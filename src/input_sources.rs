use crate::input_method::InputMethod;
use crate::quoting::{sway_argument, Unquotable};
use crate::InputHandler;
use gio::{prelude::SettingsExtManual, Settings};
use log::{info, warn};
use std::collections::HashMap;
use std::env;
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use swayipc::Connection as SwayConnection;

/// The layout GNOME applies when no source is usable, and the one IBus
/// gives an engine whose description has none.
const DEFAULT_LAYOUT: &str = "us";

/// A source of the effective source list: its identity in the `sources`
/// setting, the XKB layout and variant it applies, and for an IBus source
/// its engine.
#[derive(Clone)]
pub struct EffectiveSource {
    pub source: (String, String),
    pub layout: String,
    pub variant: String,
    pub engine: Option<String>,
}

/// The sources in effect, in list order, as in GNOME Shell: an xkb source
/// applies its own layout (`layout+variant`), an IBus engine the layout and
/// variant of its installed description. IBus engines with no installed
/// description, sources of any other type, and sources whose layout or
/// variant sway can't receive intact are left out.
pub fn effective_sources(sources: Vec<(String, String)>) -> Vec<EffectiveSource> {
    let mut engines: Option<HashMap<String, (String, String)>> = None;
    let mut effective = Vec::new();
    for (kind, id) in sources {
        let ((layout, variant), engine) = match kind.as_str() {
            "xkb" => match id.split_once('+') {
                Some((layout, variant)) => ((layout.to_string(), variant.to_string()), None),
                None => ((id.clone(), String::new()), None),
            },
            "ibus" => match engines.get_or_insert_with(ibus_engines).get(&id) {
                Some(desc) => (desc.clone(), Some(id.clone())),
                None => continue,
            },
            _ => continue,
        };
        let unsendable = [("layout", &layout), ("variant", &variant)]
            .into_iter()
            .find_map(|(what, value)| Some((what, value, sway_layout_argument(value).err()?)));
        if let Some((what, value, e)) = unsendable {
            warn!("Input source ({kind}, {id:?}) left out: sway can't receive its {what} {value:?}, {e}");
            continue;
        }
        effective.push(EffectiveSource {
            source: (kind, id),
            layout,
            variant,
            engine,
        });
    }
    effective
}

/// The installed IBus engine descriptions, by engine name: the layout and
/// variant of each. They are read from IBus's component directories
/// (`$IBUS_COMPONENT_PATH`, else `/usr/share/ibus/component`).
pub(crate) fn ibus_engines() -> HashMap<String, (String, String)> {
    let dirs: Vec<PathBuf> = match env::var("IBUS_COMPONENT_PATH") {
        Ok(path) => env::split_paths(&path).collect(),
        Err(_) => vec![PathBuf::from("/usr/share/ibus/component")],
    };
    let mut engines = HashMap::new();
    for dir in dirs {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for path in entries.flatten().map(|e| e.path()) {
            if path.extension().map_or(true, |e| e != "xml") {
                continue;
            }
            if let Ok(xml) = fs::read_to_string(&path) {
                engines.extend(parse_engines(&xml));
            }
        }
    }
    engines
}

/// The `<engine>` elements of an IBus component description.
fn parse_engines(xml: &str) -> Vec<(String, (String, String))> {
    let mut text = String::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<!--") {
        text.push_str(&rest[..start]);
        rest = rest[start..]
            .find("-->")
            .map_or("", |end| &rest[start + end + 3..]);
    }
    text.push_str(rest);
    let mut engines = Vec::new();
    for block in text.split("<engine>").skip(1) {
        let block = block.split("</engine>").next().unwrap_or_default();
        let Some(name) = element(block, "name") else {
            continue;
        };
        let layout = element(block, "layout")
            .filter(|l| !l.is_empty() && l != "default")
            .unwrap_or_else(|| String::from(DEFAULT_LAYOUT));
        let variant = element(block, "layout_variant").unwrap_or_default();
        engines.push((name, (layout, variant)));
    }
    engines
}

/// The text of the first `<tag>` element in `xml`, unescaped.
fn element(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = start + xml[start..].find(&close)?;
    let text = xml[start..end].trim();
    Some(
        text.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&"),
    )
}

pub struct InputSourcesHandler {
    settings: Settings,
    sway_connection: SwayConnection,
    input_method: InputMethod,
}
impl InputSourcesHandler {
    pub fn new() -> InputSourcesHandler {
        let settings = Settings::new("org.gnome.desktop.input-sources");
        let sway_connection = SwayConnection::new().unwrap();
        InputSourcesHandler {
            settings,
            sway_connection,
            input_method: InputMethod::start(),
        }
    }
    fn apply_input_sources(&mut self) -> Result<(), Box<dyn Error>> {
        let sources: Vec<(String, String)> = self.settings().get("sources");
        if sources.is_empty() {
            info!("No input sources set in GNOME, keeping sway's keyboard layout");
            self.input_method.set_sources(Vec::new());
            return Ok(());
        }
        let effective = effective_sources(sources);
        let (layouts, variants) = if effective.is_empty() {
            info!("No usable input source, applying the default layout");
            (String::from(DEFAULT_LAYOUT), String::new())
        } else {
            let layouts: Vec<&str> = effective.iter().map(|e| e.layout.as_str()).collect();
            let variants: Vec<&str> = effective.iter().map(|e| e.variant.as_str()).collect();
            (layouts.join(","), variants.join(","))
        };
        // sway compiles every input command against the other stored field,
        // so a new variant next to the old layouts (or new layouts next to
        // the old variants) can be rejected. Clear the variants, then set the
        // layouts, then their variants: each step is valid on its own.
        let cmds = [
            String::from("input type:keyboard xkb_variant ''"),
            format!(
                "input type:keyboard xkb_layout {}",
                sway_layout_argument(&layouts)?
            ),
            format!(
                "input type:keyboard xkb_variant {}",
                sway_layout_argument(&variants)?
            ),
        ];
        for cmd in cmds {
            info!("{cmd}");
            for outcome in self.sway_connection().run_command(&cmd)? {
                if let Err(e) = outcome {
                    warn!("{cmd}: {e}");
                }
            }
        }
        // The layouts are in: the input method follows the new list.
        self.input_method.set_sources(effective);
        Ok(())
    }
    /// Apply GNOME's XKB options (compose key, alternate characters key and
    /// the like). An empty list is only sent when the user changes the key:
    /// at startup and after a sway reload it means GNOME has no options, and
    /// any set in the user's sway config are kept.
    fn apply_xkb_options(&mut self, changed: bool) -> Result<(), Box<dyn Error>> {
        let options: Vec<String> = self.settings().get("xkb-options");
        if options.is_empty() && !changed {
            return Ok(());
        }
        let cmd = xkb_options_command(&options)?;
        info!("{cmd}");
        self.sway_connection().run_command(cmd)?;
        Ok(())
    }
}

/// Layouts, variants and options, alone or comma-joined, as the last
/// argument of their command. They are quoted because sway splits an
/// unquoted command at commas and would run every value after the first as
/// a command.
fn sway_layout_argument(value: &str) -> Result<String, Unquotable> {
    sway_argument(value, '\'', true)
}

/// The command for GNOME's XKB options. An option sway can't receive intact
/// is left out; if none is left, the options are cleared.
fn xkb_options_command(options: &[String]) -> Result<String, Unquotable> {
    let options: Vec<&str> = options
        .iter()
        .map(String::as_str)
        .filter(|option| match sway_layout_argument(option) {
            Ok(_) => true,
            Err(e) => {
                warn!("XKB option {option:?} left out: sway can't receive it, {e}");
                false
            }
        })
        .collect();
    let options = sway_layout_argument(&options.join(","))?;
    Ok(format!("input type:keyboard xkb_options {options}"))
}

impl InputHandler for InputSourcesHandler {
    fn apply_changes(&mut self, key: &str) -> Result<(), Box<dyn Error>> {
        info!("org.gnome.desktop.input-sources -> Key: {key} chaged");
        match key {
            "sources" => self.apply_input_sources()?,
            "xkb-options" => self.apply_xkb_options(true)?,
            _ => (),
        };
        Ok(())
    }
    fn apply_all(&mut self) -> Result<(), Box<dyn Error>> {
        self.apply_input_sources()?;
        self.apply_xkb_options(false)
    }
    fn settings(&self) -> &Settings {
        &self.settings
    }
    fn sync_gsettings(&mut self, input: &swayipc::Input) -> Result<(), Box<dyn Error>> {
        info!("xkb_layout: {}", input.xkb_layout_names[0]);
        Ok(())
    }
    fn sway_connection(&mut self) -> &mut swayipc::Connection {
        &mut self.sway_connection
    }
}
unsafe impl Send for InputSourcesHandler {}

#[cfg(test)]
mod tests {
    use super::{effective_sources, parse_engines, xkb_options_command};

    #[test]
    fn xkb_sources_are_split_into_layout_and_variant() {
        let sources = vec![
            ("xkb".into(), "us".into()),
            ("other".into(), "de".into()),
            ("xkb".into(), "de+nodeadkeys".into()),
        ];
        let effective: Vec<(String, String)> = effective_sources(sources)
            .into_iter()
            .map(|e| (e.layout, e.variant))
            .collect();
        assert_eq!(
            effective,
            vec![("us".into(), "".into()), ("de".into(), "nodeadkeys".into())]
        );
    }

    #[test]
    fn sources_sway_cant_receive_are_left_out() {
        let sources = vec![
            ("xkb".into(), "us\nbad".into()),
            ("xkb".into(), "de+bad\nvariant".into()),
            ("xkb".into(), "fr+o'dd\\".into()),
            ("xkb".into(), "jp".into()),
        ];
        let effective: Vec<(String, String)> = effective_sources(sources)
            .into_iter()
            .map(|e| (e.layout, e.variant))
            .collect();
        assert_eq!(
            effective,
            vec![("fr".into(), "o'dd\\".into()), ("jp".into(), "".into())]
        );
    }

    #[test]
    fn ibus_engine_descriptions_are_parsed() {
        let xml = "<component><name>c</name><!-- <engine><name>x</name></engine> -->\
            <engines><engine><name>xkb:jp::jpn</name><layout>jp</layout></engine>\
            <engine><name>e</name><layout_variant>dvorak</layout_variant></engine>\
            </engines></component>";
        let engines = parse_engines(xml);
        assert_eq!(
            engines,
            vec![
                ("xkb:jp::jpn".into(), ("jp".into(), "".into())),
                ("e".into(), ("us".into(), "dvorak".into())),
            ]
        );
    }

    #[test]
    fn xkb_options_are_joined_and_quoted() {
        let options = ["compose:ralt".to_string(), "lv3:menu_switch".to_string()];
        assert_eq!(
            xkb_options_command(&options).unwrap(),
            "input type:keyboard xkb_options 'compose:ralt,lv3:menu_switch'"
        );
        assert_eq!(
            xkb_options_command(&[]).unwrap(),
            "input type:keyboard xkb_options ''"
        );
    }

    #[test]
    fn xkb_options_are_sent_intact() {
        let options = [
            "bad\noption".to_string(),
            "it's;$x".to_string(),
            "end\\".to_string(),
        ];
        assert_eq!(
            xkb_options_command(&options).unwrap(),
            "input type:keyboard xkb_options \"it's;$$x,end\"\\"
        );
        assert_eq!(
            xkb_options_command(&options[..1]).unwrap(),
            "input type:keyboard xkb_options ''"
        );
    }
}
