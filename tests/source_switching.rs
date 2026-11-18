//! GNOME's input-source switching rules: the current source is remembered
//! in `mru-sources`, restored at startup, kept across list edits, sway
//! reloads and XKB option changes, and with `per-window` each window keeps
//! its own source.

mod common;

use common::{Daemon, FakeSway};

const SOURCES: &str = "org.gnome.desktop.input-sources";

/// A keyboard with GNOME's default single layout, as sway reports it.
const INPUTS: &str = r#"[
  {"identifier": "1:1:AT_Translated_Set_2_keyboard", "name": "AT Translated Set 2 keyboard",
   "vendor": 1, "product": 1, "type": "keyboard",
   "xkb_active_layout_name": "English (US)", "xkb_layout_names": ["English (US)"],
   "xkb_active_layout_index": 0, "libinput": {"send_events": "enabled"}}
]"#;

fn keyfile(sources: &str, extra: &str) -> String {
    format!("[org/gnome/desktop/input-sources]\nsources={sources}\n{extra}")
}

/// Start the daemon and wait until both its handlers and its switcher
/// follow sway's events, and the switcher has read the keyboards' state
/// (it reads the tree right after). A user's switch before that would be
/// overridden by the source the switcher starts on.
fn start(name: &str, keyfile: &str) -> (FakeSway, Daemon) {
    let (sway, daemon) = Daemon::start_with_inputs(name, keyfile, INPUTS);
    sway.wait_for_startup();
    sway.wait_for_subscribers(2);
    sway.wait_for_get_tree();
    (sway, daemon)
}

/// Wait until `mru-sources` is `expected`.
fn wait_for_mru(daemon: &Daemon, expected: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let mru = daemon.gsettings_get(SOURCES, "mru-sources");
        if mru == expected {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "mru-sources is {mru}, expected {expected}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Let the daemon react to whatever it was sent, then check the keyboard
/// stays where it is.
fn assert_stays(sway: &FakeSway, layouts: &[&str], index: usize) {
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert_eq!(
        sway.layouts(),
        (layouts.iter().map(|l| l.to_string()).collect(), index),
        "sway got {:?}",
        sway.commands()
    );
}

#[test]
fn user_switches_are_recorded_most_recent_first() {
    let (sway, daemon) = start(
        "mru-record",
        &keyfile("[('xkb', 'us'), ('xkb', 'de'), ('xkb', 'fr')]", ""),
    );
    sway.wait_for_layout(&["us", "de", "fr"], 0);
    sway.user_switch(2);
    wait_for_mru(&daemon, "[('xkb', 'fr'), ('xkb', 'us'), ('xkb', 'de')]");
    sway.user_switch(1);
    wait_for_mru(&daemon, "[('xkb', 'de'), ('xkb', 'fr'), ('xkb', 'us')]");
    // The daemon's own reaction is not taken for another switch.
    assert_stays(&sway, &["us", "de", "fr"], 1);
}

#[test]
fn startup_restores_the_most_recent_source() {
    let (sway, daemon) = start(
        "mru-startup",
        &keyfile(
            "[('xkb', 'us'), ('xkb', 'de')]",
            "mru-sources=[('xkb', 'de'), ('xkb', 'us')]\n",
        ),
    );
    sway.wait_for_layout(&["us", "de"], 1);
    assert_stays(&sway, &["us", "de"], 1);
    // Restoring is not a user's choice: the history is left as it was.
    assert_eq!(
        daemon.gsettings_get(SOURCES, "mru-sources"),
        "[('xkb', 'de'), ('xkb', 'us')]"
    );
}

#[test]
fn xkb_option_changes_keep_the_current_source() {
    let (sway, daemon) = start(
        "mru-options",
        &keyfile("[('xkb', 'us'), ('xkb', 'de')]", ""),
    );
    sway.wait_for_layout(&["us", "de"], 0);
    sway.user_switch(1);
    wait_for_mru(&daemon, "[('xkb', 'de'), ('xkb', 'us')]");
    // sway compiles a new keymap, which starts on the first layout.
    daemon.gsettings_set(SOURCES, "xkb-options", "['compose:ralt']");
    sway.wait_for(0, "input type:keyboard xkb_options 'compose:ralt'");
    sway.wait_for_layout(&["us", "de"], 1);
    assert_stays(&sway, &["us", "de"], 1);
    assert_eq!(
        daemon.gsettings_get(SOURCES, "mru-sources"),
        "[('xkb', 'de'), ('xkb', 'us')]"
    );
}

#[test]
fn a_sway_reload_keeps_the_current_source() {
    let (sway, daemon) = start("mru-reload", &keyfile("[('xkb', 'us'), ('xkb', 'de')]", ""));
    sway.wait_for_layout(&["us", "de"], 0);
    sway.user_switch(1);
    wait_for_mru(&daemon, "[('xkb', 'de'), ('xkb', 'us')]");
    let seen = sway.commands().len();
    sway.send_tick(r#"{"status": "reload_done"}"#);
    sway.wait_for(seen, "input type:keyboard xkb_layout");
    sway.wait_for_layout(&["us", "de"], 1);
    assert_stays(&sway, &["us", "de"], 1);
}

#[test]
fn list_edits_keep_the_current_source_at_its_new_position() {
    let (sway, daemon) = start(
        "mru-edit",
        &keyfile("[('xkb', 'us'), ('xkb', 'de'), ('xkb', 'fr')]", ""),
    );
    sway.wait_for_layout(&["us", "de", "fr"], 0);
    sway.user_switch(1);
    wait_for_mru(&daemon, "[('xkb', 'de'), ('xkb', 'us'), ('xkb', 'fr')]");
    // Moved and joined by a new source: de stays current.
    daemon.gsettings_set(
        SOURCES,
        "sources",
        "[('xkb', 'ru'), ('xkb', 'fr'), ('xkb', 'us'), ('xkb', 'de')]",
    );
    sway.wait_for_layout(&["ru", "fr", "us", "de"], 3);
    assert_stays(&sway, &["ru", "fr", "us", "de"], 3);
    // Removed: the most recently used source still listed takes over.
    daemon.gsettings_set(
        SOURCES,
        "sources",
        "[('xkb', 'ru'), ('xkb', 'fr'), ('xkb', 'us')]",
    );
    sway.wait_for_layout(&["ru", "fr", "us"], 2);
    assert_stays(&sway, &["ru", "fr", "us"], 2);
    // The next user switch writes the history in the new list's terms:
    // chosen sources first, the never-chosen ones in list order.
    sway.user_switch(1);
    wait_for_mru(&daemon, "[('xkb', 'fr'), ('xkb', 'us'), ('xkb', 'ru')]");
}

#[test]
fn per_window_sources_follow_the_focus() {
    let (sway, daemon) = start(
        "mru-per-window",
        &keyfile("[('xkb', 'us'), ('xkb', 'de')]", "per-window=true\n"),
    );
    sway.wait_for_layout(&["us", "de"], 0);
    sway.window_event("focus", 1);
    sway.user_switch(1);
    wait_for_mru(&daemon, "[('xkb', 'de'), ('xkb', 'us')]");
    // A window that never had a source gets the first one.
    sway.window_event("focus", 2);
    sway.wait_for_layout(&["us", "de"], 0);
    sway.window_event("focus", 1);
    sway.wait_for_layout(&["us", "de"], 1);
    // A closed window's source is forgotten.
    sway.window_event("close", 1);
    sway.window_event("focus", 2);
    sway.wait_for_layout(&["us", "de"], 0);
    assert_stays(&sway, &["us", "de"], 0);
}

#[test]
fn without_per_window_the_source_is_global() {
    let (sway, daemon) = start("mru-global", &keyfile("[('xkb', 'us'), ('xkb', 'de')]", ""));
    sway.wait_for_layout(&["us", "de"], 0);
    sway.window_event("focus", 1);
    sway.user_switch(1);
    wait_for_mru(&daemon, "[('xkb', 'de'), ('xkb', 'us')]");
    sway.window_event("focus", 2);
    assert_stays(&sway, &["us", "de"], 1);
}

/// Wait until `current` is `expected`.
fn wait_for_current(daemon: &Daemon, expected: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let current = daemon.gsettings_get(SOURCES, "current");
        if current == expected {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "current is {current}, expected {expected}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn a_repeated_source_is_one_entry_per_occurrence_and_once_in_the_history() {
    let (sway, daemon) = start(
        "dup-switch",
        &keyfile("[('xkb', 'us'), ('xkb', 'de'), ('xkb', 'us')]", ""),
    );
    sway.wait_for_layout(&["us", "de", "us"], 0);
    wait_for_current(&daemon, "uint32 0");
    // The second us is an entry of its own, not the first one again.
    sway.user_switch(2);
    wait_for_current(&daemon, "uint32 2");
    wait_for_mru(&daemon, "[('xkb', 'us'), ('xkb', 'de')]");
    assert_stays(&sway, &["us", "de", "us"], 2);
    // Back to the first us: a change of entry, with the same history.
    sway.user_switch(0);
    wait_for_current(&daemon, "uint32 0");
    assert_stays(&sway, &["us", "de", "us"], 0);
    assert_eq!(
        daemon.gsettings_get(SOURCES, "mru-sources"),
        "[('xkb', 'us'), ('xkb', 'de')]"
    );
}

#[test]
fn startup_takes_the_first_entry_of_the_most_recent_source() {
    let (sway, daemon) = start(
        "dup-startup",
        &keyfile(
            "[('xkb', 'us'), ('xkb', 'de'), ('xkb', 'fr'), ('xkb', 'de')]",
            "mru-sources=[('xkb', 'de'), ('xkb', 'us'), ('xkb', 'de')]\n",
        ),
    );
    sway.wait_for_layout(&["us", "de", "fr", "de"], 1);
    wait_for_current(&daemon, "uint32 1");
    assert_stays(&sway, &["us", "de", "fr", "de"], 1);
    assert_eq!(
        daemon.gsettings_get(SOURCES, "mru-sources"),
        "[('xkb', 'de'), ('xkb', 'us'), ('xkb', 'de')]"
    );
}

#[test]
fn list_edits_take_the_first_entry_of_a_repeated_source() {
    let (sway, daemon) = start(
        "dup-edit",
        &keyfile(
            "[('xkb', 'us'), ('xkb', 'de'), ('xkb', 'us')]",
            "per-window=true\n",
        ),
    );
    sway.wait_for_layout(&["us", "de", "us"], 0);
    // Window 1 on the second us, window 2 on de.
    sway.window_event("focus", 1);
    sway.user_switch(2);
    wait_for_current(&daemon, "uint32 2");
    sway.window_event("focus", 2);
    sway.wait_for_layout(&["us", "de", "us"], 0);
    sway.user_switch(1);
    wait_for_current(&daemon, "uint32 1");
    // de, current, follows to its new position; window 1's us goes to the
    // first us, as in GNOME Shell.
    daemon.gsettings_set(
        SOURCES,
        "sources",
        "[('xkb', 'fr'), ('xkb', 'us'), ('xkb', 'de'), ('xkb', 'us')]",
    );
    sway.wait_for_layout(&["fr", "us", "de", "us"], 2);
    wait_for_current(&daemon, "uint32 2");
    sway.window_event("focus", 1);
    sway.wait_for_layout(&["fr", "us", "de", "us"], 1);
    wait_for_current(&daemon, "uint32 1");
    // The current entry, a repeated source, also goes to its first entry.
    sway.window_event("focus", 2);
    sway.wait_for_layout(&["fr", "us", "de", "us"], 2);
    sway.user_switch(3);
    wait_for_current(&daemon, "uint32 3");
    daemon.gsettings_set(
        SOURCES,
        "sources",
        "[('xkb', 'de'), ('xkb', 'us'), ('xkb', 'us')]",
    );
    sway.wait_for_layout(&["de", "us", "us"], 1);
    wait_for_current(&daemon, "uint32 1");
    assert_stays(&sway, &["de", "us", "us"], 1);
}

#[test]
fn per_window_sources_keep_their_entry_of_a_repeated_source() {
    let (sway, daemon) = start(
        "dup-per-window",
        &keyfile(
            "[('xkb', 'us'), ('xkb', 'de'), ('xkb', 'us')]",
            "per-window=true\n",
        ),
    );
    sway.wait_for_layout(&["us", "de", "us"], 0);
    sway.window_event("focus", 1);
    sway.user_switch(2);
    wait_for_mru(&daemon, "[('xkb', 'us'), ('xkb', 'de')]");
    // Window 2 never had an entry: the first one, the other us.
    sway.window_event("focus", 2);
    sway.wait_for_layout(&["us", "de", "us"], 0);
    wait_for_current(&daemon, "uint32 0");
    sway.window_event("focus", 1);
    sway.wait_for_layout(&["us", "de", "us"], 2);
    wait_for_current(&daemon, "uint32 2");
    assert_stays(&sway, &["us", "de", "us"], 2);
    // Focus changes alone don't write the history.
    assert_eq!(
        daemon.gsettings_get(SOURCES, "mru-sources"),
        "[('xkb', 'us'), ('xkb', 'de')]"
    );
}
