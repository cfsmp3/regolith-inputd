//! GNOME's input-source switching rules for sway.
//!
//! One owner on the GLib main loop holds the switching state. sway's window,
//! workspace, input and tick events and the input-sources settings changes
//! reach it as messages through one channel and are processed in order.
//! Commands this module sends are fenced with a SEND_TICK: layout events seen
//! before the fence's tick are the daemon's own and are never taken for a
//! user's switch.
//!
//! The owner also answers the input-source service (see
//! [`crate::sources_service`]): its method calls join the same queue, and
//! after each message it announces a change of the listed sources or of the
//! current source, and keeps the `current` key on the current source's
//! position in the list. Others' writes to `current` are not followed.

use crate::input_sources::{effective_sources, EffectiveSource};
use crate::sources_service::{self, Call, Entry};
use crate::utils;
use crate::xkb_rules::XkbRules;
use gio::prelude::*;
use gio::{BusType, DBusConnection, DBusMethodInvocation, Settings};
use glib::thread_guard::ThreadGuard;
use glib::ToVariant;
use log::{debug, info, warn};
use serde::Deserialize;
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use swayipc::{
    CommandType, Connection, Error as SwayError, Event, EventType, Fallible, InputChange, Node,
    NodeType, Success, WindowChange, WorkspaceChange,
};

const FENCE: &str = "regolith-inputd:sources:";

/// sway's input event type, without the event bit.
const INPUT_EVENT: u32 = 21;

/// A source's identity: its type and id, as in the `sources` setting. The
/// list may repeat one; each occurrence is then an entry of its own,
/// selected by its layout index.
type Source = (String, String);

/// An input device as sway describes it. Decoded leniently: keyboards
/// without vendor/product ids (virtual keyboards, the wlroots X11/Wayland
/// backends' keyboards) are keyboards like any other.
#[derive(Deserialize)]
struct Device {
    identifier: String,
    #[serde(rename = "type")]
    input_type: String,
    #[serde(default)]
    xkb_layout_names: Vec<String>,
    xkb_active_layout_index: Option<i64>,
}

#[derive(Deserialize)]
struct DeviceEvent {
    change: InputChange,
    input: Device,
}

enum Msg {
    Sway(Event),
    Input(DeviceEvent),
    Setting(String),
    /// A call of the input-source service. It is answered on the thread
    /// that received it, which also runs the owner.
    Call(Call, ThreadGuard<DBusMethodInvocation>),
}

struct Keyboard {
    layouts: usize,
    index: i64,
}

struct Switcher {
    settings: Settings,
    conn: Option<Connection>,
    /// The effective source list, with each source's position in `sources`.
    listed: Vec<EffectiveSource>,
    /// Their identities.
    sources: Vec<Source>,
    /// Listed sources that were chosen or made current, most recent first,
    /// once each. The never-chosen ones are not in it.
    recent: Vec<Source>,
    /// The layout index of the current entry.
    current: Option<usize>,
    per_window: bool,
    /// The layout index of each window's entry.
    windows: HashMap<i64, usize>,
    focused: Option<i64>,
    keyboards: HashMap<String, Keyboard>,
    fence_sent: u64,
    fence_seen: u64,
    rules: XkbRules,
    bus: Option<DBusConnection>,
    /// What the service last announced: the entries and the current one.
    published: (Vec<Entry>, Option<usize>),
    /// The current entry's position in `sources` and its source, as last
    /// announced. `current` is stored when either changes.
    published_current: Option<(u32, Source)>,
}

fn is_view(n: &Node) -> bool {
    matches!(n.node_type, NodeType::Con | NodeType::FloatingCon)
        && n.nodes.is_empty()
        && n.floating_nodes.is_empty()
        && (n.pid.is_some() || n.window.is_some() || n.app_id.is_some())
}

fn find_focused(n: &Node) -> Option<&Node> {
    if n.focused {
        return Some(n);
    }
    n.nodes
        .iter()
        .chain(n.floating_nodes.iter())
        .find_map(find_focused)
}

fn has_views(n: &Node) -> bool {
    n.nodes
        .iter()
        .chain(n.floating_nodes.iter())
        .any(|c| is_view(c) || has_views(c))
}

impl Switcher {
    fn new() -> Switcher {
        let settings = Settings::new("org.gnome.desktop.input-sources");
        // The keys written for one message (`mru-sources`, `current`) are
        // applied together after it: a reader never sees one without the
        // other, and with a file backend no write lands between them.
        settings.delay();
        let conn = Connection::new().ok();
        Switcher {
            settings,
            conn,
            listed: Vec::new(),
            sources: Vec::new(),
            recent: Vec::new(),
            current: None,
            per_window: false,
            windows: HashMap::new(),
            focused: None,
            keyboards: HashMap::new(),
            fence_sent: 0,
            fence_seen: 0,
            rules: XkbRules::load(),
            bus: None,
            published: (Vec::new(), None),
            published_current: None,
        }
    }

    /// Read the effective source list: what layout index i of sway's
    /// layouts is.
    fn read_sources(&mut self) {
        self.listed = effective_sources(self.settings.get::<Vec<Source>>("sources"));
        self.sources = self.listed.iter().map(|e| e.source.clone()).collect();
    }

    /// The layout index of a source's first entry.
    fn index_of(&self, s: &Source) -> Option<usize> {
        self.sources.iter().position(|x| x == s)
    }

    /// Keep only the listed sources of `history`, once each, in order.
    fn set_recent(&mut self, history: Vec<Source>) {
        let mut recent: Vec<Source> = Vec::new();
        for s in history {
            if self.sources.contains(&s) && !recent.contains(&s) {
                recent.push(s);
            }
        }
        self.recent = recent;
    }

    /// The value `mru-sources` gets: every listed source once, the most
    /// recently chosen first, then the never-chosen ones in list order.
    fn history(&self) -> Vec<Source> {
        let mut h = self.recent.clone();
        for s in &self.sources {
            if !h.contains(s) {
                h.push(s.clone());
            }
        }
        h
    }

    fn startup(&mut self) {
        self.read_sources();
        self.per_window = self.settings.boolean("per-window");
        let saved: Vec<Source> = self.settings.get::<Vec<Source>>("mru-sources");
        self.set_recent(saved);
        self.refresh_keyboards();
        self.focused = self.query_focused();
        let first = match self.recent.first() {
            Some(s) => self.index_of(s),
            None => (!self.sources.is_empty()).then_some(0),
        };
        if let Some(first) = first {
            info!("Starting on input source {:?}", self.sources[first]);
            self.activate(first, false);
        }
    }

    fn query_focused(&mut self) -> Option<i64> {
        let tree = self.conn.as_mut()?.get_tree().ok()?;
        let n = find_focused(&tree)?;
        if is_view(n) {
            Some(n.id)
        } else {
            None
        }
    }

    fn refresh_keyboards(&mut self) {
        let inputs = match utils::get_inputs_json() {
            Ok(i) => i,
            Err(e) => {
                warn!("{e}");
                return;
            }
        };
        self.keyboards.clear();
        let devices = inputs
            .into_iter()
            .filter_map(|i| serde_json::from_value::<Device>(i).ok());
        for i in devices.filter(|i| i.input_type == "keyboard") {
            self.keyboards.insert(
                i.identifier,
                Keyboard {
                    layouts: i.xkb_layout_names.len(),
                    index: i.xkb_active_layout_index.unwrap_or(0),
                },
            );
        }
    }

    fn fenced(&self) -> bool {
        self.fence_seen < self.fence_sent
    }

    fn send_fence(&mut self) {
        self.fence_sent += 1;
        let payload = format!("{FENCE}{}", self.fence_sent);
        if let Some(c) = self.conn.as_mut() {
            if c.send_tick(&payload).is_err() {
                // No tick support (or no sway): nothing will clear it.
                self.fence_seen = self.fence_sent;
            }
        } else {
            self.fence_seen = self.fence_sent;
        }
    }

    /// Put every keyboard that has the source list's layouts on `index`.
    fn switch_all(&mut self, index: usize) {
        let n = self.sources.len();
        let needed = self
            .keyboards
            .values()
            .any(|k| k.layouts == n && k.index != index as i64);
        if !needed {
            return;
        }
        let cmd = format!("input type:keyboard xkb_switch_layout {index}");
        debug!("{cmd}");
        if let Some(c) = self.conn.as_mut() {
            if let Err(e) = c.run_command(&cmd) {
                warn!("{e}");
            }
        }
        self.send_fence();
    }

    /// Make the entry at layout index `index` the current one. Its source
    /// becomes the most recent, even when another entry of it was current.
    fn activate(&mut self, index: usize, interactive: bool) {
        let Some(s) = self.sources.get(index).cloned() else {
            return;
        };
        self.current = Some(index);
        self.recent.retain(|x| x != &s);
        self.recent.insert(0, s);
        if interactive {
            let v = self.history().to_variant();
            if let Err(e) = self.settings.set_value("mru-sources", &v) {
                warn!("{e}");
            }
        }
        if self.per_window {
            if let Some(w) = self.focused {
                self.windows.insert(w, index);
            }
        }
        self.switch_all(index);
    }

    /// The layout index of the entry a window had, or the first entry.
    fn window_source(&self, id: i64) -> Option<usize> {
        match self.windows.get(&id) {
            Some(&i) => Some(i),
            None => (!self.sources.is_empty()).then_some(0),
        }
    }

    fn reassert(&mut self) {
        if let Some(i) = self.current {
            self.switch_all(i);
        }
    }

    /// Follow an edit of the source list. As in GNOME Shell, the current
    /// entry and the windows' entries follow their source to its first
    /// entry in the new list; a window whose source is gone gets the first
    /// entry when it is focused again.
    fn sources_changed(&mut self) {
        let old = std::mem::take(&mut self.sources);
        let current = self.current.take().and_then(|i| old.get(i).cloned());
        self.read_sources();
        let windows = std::mem::take(&mut self.windows);
        for (w, i) in windows {
            if let Some(i) = old.get(i).and_then(|s| self.index_of(s)) {
                self.windows.insert(w, i);
            }
        }
        let history = std::mem::take(&mut self.recent);
        self.set_recent(history);
        let keep = current.and_then(|s| self.index_of(&s));
        let fallback = || match self.recent.first() {
            Some(s) => self.index_of(s),
            None => (!self.sources.is_empty()).then_some(0),
        };
        if let Some(i) = keep.or_else(fallback) {
            self.activate(i, false);
        }
        // The keymap event (or the fence) re-asserts once sway has the new list.
        self.send_fence();
    }

    fn per_window_changed(&mut self) {
        self.per_window = self.settings.boolean("per-window");
        self.windows.clear();
    }

    /// Announce a change of the listed entries or of the current source.
    /// When the current entry's position or its source changes, `current`
    /// follows it.
    fn publish(&mut self) {
        let current = self.current;
        let active = current.map(|i| (self.listed[i].position, self.sources[i].clone()));
        if active != self.published_current {
            if let Some((position, _)) = &active {
                self.store_position(*position);
            }
            self.published_current = active;
        }
        let view = (sources_service::entries(&self.listed, &self.rules), current);
        if view != self.published {
            self.published = view;
            if let Some(bus) = self.bus.as_ref() {
                sources_service::emit_changed(bus);
            }
        }
    }

    /// Store the current source's position in `current`, unless it is there.
    fn store_position(&self, position: u32) {
        if self.settings.uint("current") == position {
            return;
        }
        debug!("current = {position}");
        if let Err(e) = self.settings.set_uint("current", position) {
            warn!("{e}");
        }
    }

    /// Answer a call of the service. An activation that switched is
    /// returned, to be answered once the settings it changed are stored.
    fn on_call(
        &mut self,
        call: Call,
        invocation: DBusMethodInvocation,
    ) -> Option<DBusMethodInvocation> {
        match call {
            Call::GetInputSources => {
                let (entries, current) = &self.published;
                let reply = sources_service::input_sources_reply(entries, *current);
                invocation.return_value(Some(&reply));
            }
            Call::Activate(position) => {
                let Some(i) = self.listed.iter().position(|e| e.position == position) else {
                    sources_service::refuse_index(invocation, position);
                    return None;
                };
                if self.current == Some(i) {
                    invocation.return_value(None);
                    return None;
                }
                info!("Activating {:?} at {position}", self.sources[i]);
                self.activate(i, true);
                return Some(invocation);
            }
            Call::ActivateProperty => sources_service::refuse_property(invocation),
        }
        None
    }

    fn on_event(&mut self, ev: Event) {
        match ev {
            Event::Window(w) => match w.change {
                WindowChange::Focus => {
                    self.focused = Some(w.container.id);
                    if self.per_window {
                        if let Some(i) = self.window_source(w.container.id) {
                            self.activate(i, false);
                        }
                    }
                }
                WindowChange::Close => {
                    self.windows.remove(&w.container.id);
                    if self.focused == Some(w.container.id) {
                        self.focused = None;
                    }
                }
                _ => {}
            },
            Event::Workspace(ws) => {
                if ws.change == WorkspaceChange::Focus {
                    if let Some(cur) = ws.current.as_ref() {
                        if !has_views(cur) {
                            self.focused = None;
                        }
                    }
                }
            }
            Event::Tick(t) => {
                if let Some(n) = t.payload.strip_prefix(FENCE) {
                    if let Ok(n) = n.parse::<u64>() {
                        self.fence_seen = self.fence_seen.max(n);
                        if !self.fenced() {
                            self.reassert();
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn on_input(&mut self, ie: DeviceEvent) {
        if ie.input.input_type != "keyboard" {
            return;
        }
        let id = ie.input.identifier;
        let layouts = ie.input.xkb_layout_names.len();
        let index = ie.input.xkb_active_layout_index.unwrap_or(0);
        match ie.change {
            InputChange::Removed => {
                self.keyboards.remove(&id);
            }
            InputChange::Added | InputChange::XkbKeymap => {
                self.keyboards.insert(id, Keyboard { layouts, index });
                // Hotplug or a new keymap: sway may have reset layouts.
                self.send_fence();
            }
            InputChange::XkbLayout => {
                self.keyboards.insert(id, Keyboard { layouts, index });
                if self.fenced() || layouts != self.sources.len() {
                    return;
                }
                let index = index as usize;
                if self.current != Some(index) {
                    if let Some(s) = self.sources.get(index) {
                        info!("User switched to {s:?} at layout {index}");
                        self.activate(index, true);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Subscribe to the events the switcher follows. The stream is read by hand
/// rather than through swayipc's `EventStream`, whose typed input events
/// require vendor/product ids.
fn subscribe() -> Fallible<UnixStream> {
    let mut stream: UnixStream = Connection::new()?.into();
    let events = serde_json::to_string(&[
        EventType::Window,
        EventType::Workspace,
        EventType::Input,
        EventType::Tick,
    ])?;
    stream.write_all(&CommandType::Subscribe.encode_with(&events))?;
    let reply: Success = CommandType::Subscribe.decode(utils::receive_message(&mut stream)?)?;
    if !reply.success {
        return Err(SwayError::SubscriptionFailed(events));
    }
    Ok(stream)
}

fn decode((kind, payload): (u32, Vec<u8>)) -> Fallible<Msg> {
    if kind & 0x7fff_ffff == INPUT_EVENT {
        Ok(Msg::Input(serde_json::from_slice(&payload)?))
    } else {
        Ok(Msg::Sway(Event::decode((kind, payload))?))
    }
}

/// Start following GNOME's input-source rules. Must run on the thread that
/// runs the default GLib main loop, after the source list was applied.
pub fn start() {
    let sw = Rc::new(RefCell::new(Switcher::new()));
    let (tx, rx) = glib::MainContext::channel::<Msg>(glib::PRIORITY_DEFAULT);

    // Subscribe before reading the initial state, so no event is missed.
    let first = subscribe();
    let sway_tx = tx.clone();
    thread::spawn(move || {
        let mut stream = first;
        loop {
            match stream.as_mut() {
                Ok(events) => loop {
                    match utils::receive_message(events) {
                        // A message that doesn't decode was still read
                        // whole: skip it, the stream stays in step.
                        Ok(message) => match decode(message) {
                            Ok(msg) => {
                                if sway_tx.send(msg).is_err() {
                                    return;
                                }
                            }
                            Err(e) => debug!("{e}"),
                        },
                        Err(e) => {
                            warn!("{e}");
                            break;
                        }
                    }
                },
                Err(e) => warn!("{e}"),
            }
            thread::sleep(Duration::from_millis(500));
            stream = subscribe();
        }
    });

    // The input-source service's method calls join the queue as well.
    match gio::bus_get_sync(BusType::Session, gio::Cancellable::NONE) {
        Ok(bus) => {
            let call_tx = Mutex::new(tx.clone());
            let exported = sources_service::export(&bus, move |call, invocation| {
                let msg = Msg::Call(call, ThreadGuard::new(invocation));
                let _ = call_tx.lock().map(|tx| tx.send(msg));
            });
            match exported {
                Ok(()) => sw.borrow_mut().bus = Some(bus),
                Err(e) => warn!("Input-source service not published: {e}"),
            }
        }
        Err(e) => warn!("No session bus, input-source service not published: {e}"),
    }

    // Settings changes join the same queue. The owner's own mru-sources write
    // emits "changed" too; only the keys it follows are queued.
    sw.borrow().settings.connect_changed(None, move |_, key| {
        if key == "sources" || key == "per-window" {
            let _ = tx.send(Msg::Setting(key.to_string()));
        }
    });

    let owner = sw.clone();
    rx.attach(None, move |msg| {
        let mut s = owner.borrow_mut();
        let mut activated = None;
        match msg {
            Msg::Sway(ev) => s.on_event(ev),
            Msg::Input(ev) => s.on_input(ev),
            Msg::Setting(key) if key == "sources" => s.sources_changed(),
            Msg::Setting(_) => s.per_window_changed(),
            Msg::Call(call, invocation) => activated = s.on_call(call, invocation.into_inner()),
        }
        s.publish();
        s.settings.apply();
        if let Some(invocation) = activated {
            // The caller finds the new source stored when the call returns.
            Settings::sync();
            invocation.return_value(None);
        }
        glib::Continue(true)
    });
    let mut s = sw.borrow_mut();
    s.startup();
    s.publish();
    s.settings.apply();
}
