//! IBus in the sway session is opt-in: the daemon runs it only when trawld
//! answers `true` for the resource `regolith.experimental.ibus`, and
//! otherwise only applies the layouts. When it runs, the daemon runs IBus's panel itself,
//! and starts it again when it dies and when an output appears or is turned
//! on, since IBus 1.5.32 to 1.5.34 stop committing text after a new
//! `wl_output` (ibus/ibus#2870). Every other panel on the session's display,
//! and every `ibus-daemon` that would start one, is stopped.

mod common;

use common::{Daemon, FakeSway, Ibus, Trawl};
use std::process::Command;
use std::thread;
use std::time::Duration;

const KEYFILE: &str = "[org/gnome/desktop/input-sources]\nsources=[('xkb', 'us')]\n";
/// Two sources, so that the layouts applied show in sway's commands.
const TWO_SOURCES: &str = "[org/gnome/desktop/input-sources]\n\
                           sources=[('xkb', 'us'), ('xkb', 'de+nodeadkeys')]\n";
/// What sway gets for `TWO_SOURCES`, with or without IBus.
const TWO_LAYOUTS: [&str; 3] = [
    "input type:keyboard xkb_variant ''",
    "input type:keyboard xkb_layout 'us,de'",
    "input type:keyboard xkb_variant ',nodeadkeys'",
];

/// The laptop's panel alone, as at startup.
const LAPTOP: &str = r#"[{"name": "eDP-1", "active": true, "power": true}]"#;
/// A monitor plugged in next to the panel.
const DOCKED: &str = r#"[
  {"name": "eDP-1", "active": true, "power": true},
  {"name": "DP-1", "active": true, "power": true}
]"#;
/// A second monitor plugged in.
const DOCKED_TWICE: &str = r#"[
  {"name": "eDP-1", "active": true, "power": true},
  {"name": "DP-1", "active": true, "power": true},
  {"name": "HDMI-A-1", "active": true, "power": true}
]"#;
/// Docked, with the laptop's panel turned off, as a display profile does.
const DOCKED_PANEL_OFF: &str = r#"[
  {"name": "eDP-1", "active": false, "power": false},
  {"name": "DP-1", "active": true, "power": true}
]"#;

/// Longer than the daemon lets outputs settle before it starts the panel
/// again.
const SETTLE: Duration = Duration::from_millis(2500);

/// Start the daemon for a user who opted in, with a stand-in IBus, and wait
/// until its panel runs and it has read the outputs it starts with. Returns
/// the panel's ID.
fn start(name: &str) -> (FakeSway, Daemon, u32) {
    let (sway, daemon) = Daemon::start_with_ibus(name, KEYFILE, Ibus::opted_in());
    sway.wait_for_startup();
    let panels = daemon.wait_for_panels(1);
    sway.wait_for_get_outputs();
    let panel = *panels.last().unwrap();
    (sway, daemon, panel)
}

#[test]
fn the_daemon_runs_the_wayland_panel_and_ibus_daemon_none() {
    let (_sway, daemon, _) = start("im-panel");
    let log = daemon.ibus_log();
    let daemons: Vec<&String> = log.iter().filter(|l| l.starts_with("daemon ")).collect();
    assert_eq!(daemons.len(), 1, "IBus log: {log:?}");
    assert!(
        daemons[0].ends_with(" --replace --panel disable"),
        "ibus-daemon must not start a panel the daemon can't restart: {}",
        daemons[0]
    );
    assert_eq!(daemon.panels_started().len(), 1, "IBus log: {log:?}");
}

#[test]
fn plugging_a_monitor_starts_the_panel_again_once() {
    let (sway, daemon, first) = start("im-plug");
    sway.set_outputs(DOCKED);
    thread::sleep(Duration::from_millis(200));
    sway.set_outputs(DOCKED_TWICE);
    let panels = daemon.wait_for_panels(2);
    assert_eq!(
        daemon.stopped(),
        [first],
        "IBus log: {:?}",
        daemon.ibus_log()
    );
    assert_ne!(panels[1], first);
    // Both monitors came within the settling time: one new panel.
    thread::sleep(SETTLE);
    assert_eq!(daemon.panels_started().len(), 2, "{:?}", daemon.ibus_log());
}

#[test]
fn undocking_starts_the_panel_again_when_the_laptop_panel_comes_back() {
    let (sway, daemon, _) = start("im-undock");
    sway.set_outputs(DOCKED);
    daemon.wait_for_panels(2);
    // The display profile turns the laptop's panel off: no new output.
    sway.set_outputs(DOCKED_PANEL_OFF);
    thread::sleep(SETTLE);
    assert_eq!(daemon.panels_started().len(), 2, "{:?}", daemon.ibus_log());
    // Unplugging the monitor turns the laptop's panel back on.
    sway.set_outputs(LAPTOP);
    daemon.wait_for_panels(3);
    assert_eq!(daemon.stopped().len(), 2, "{:?}", daemon.ibus_log());
}

#[test]
fn unplugging_powering_off_or_reconfiguring_leave_the_panel_alone() {
    let (sway, daemon, _) = start("im-quiet");
    sway.set_outputs(DOCKED);
    daemon.wait_for_panels(2);
    sway.set_outputs(LAPTOP);
    sway.set_outputs(r#"[{"name": "eDP-1", "active": true, "power": false}]"#);
    sway.set_outputs(r#"[{"name": "eDP-1", "active": true, "power": true, "scale": 2.0}]"#);
    thread::sleep(SETTLE);
    assert_eq!(daemon.panels_started().len(), 2, "{:?}", daemon.ibus_log());
}

#[test]
fn a_laptop_panel_created_again_on_resume_starts_the_panel_again() {
    let (sway, daemon, first) = start("im-resume");
    sway.set_outputs(r#"[{"name": "eDP-1", "id": 3, "active": true, "power": true}]"#);
    thread::sleep(SETTLE);
    let started = daemon.panels_started().len();
    // On resume sway destroys the panel's output and creates it again, under
    // the same name and with a new ID, before the daemon reads the outputs.
    sway.set_outputs(r#"[{"name": "eDP-1", "id": 9, "active": true, "power": true}]"#);
    let panels = daemon.wait_for_panels(started + 1);
    assert_ne!(*panels.last().unwrap(), first);
    // A change that keeps its ID, such as a new scale, adds no global.
    sway.set_outputs(
        r#"[{"name": "eDP-1", "id": 9, "active": true, "power": true, "scale": 2.0}]"#,
    );
    thread::sleep(SETTLE);
    assert_eq!(
        daemon.panels_started().len(),
        started + 1,
        "{:?}",
        daemon.ibus_log()
    );
}

#[test]
fn a_panel_that_dies_is_started_again() {
    let (_sway, daemon, first) = start("im-crash");
    let status = Command::new("kill")
        .args(["-KILL", &first.to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let panels = daemon.wait_for_panels(2);
    assert_ne!(panels[1], first);
}

#[test]
fn a_panel_the_daemon_did_not_start_is_replaced() {
    let ibus = Ibus {
        foreign_panel: true,
        ..Ibus::opted_in()
    };
    let (_sway, daemon) = Daemon::start_with_ibus("im-foreign", KEYFILE, ibus);
    let panels = daemon.wait_for_panels(2);
    assert_eq!(daemon.stopped(), [panels[0]], "{:?}", daemon.ibus_log());
}

#[test]
fn a_panel_started_next_to_the_daemons_own_is_stopped() {
    // `ibus restart` starts a panel of its own a few seconds after it stops
    // ibus-daemon, by which time the daemon has started its own again.
    let (_sway, daemon, ours) = start("im-second");
    let wayland = daemon.start_foreign_panel(&[
        "--enable-wayland-im",
        "--exec-daemon",
        "--daemon-args",
        "--xim --panel disable",
    ]);
    // The daemon's own panel is stopped first, as IBus's crashes when a
    // second one exits, and started again once the other one is gone.
    let second = daemon.wait_for_panels(2)[1];
    assert_eq!(daemon.stopped(), [ours, wayland], "{:?}", daemon.ibus_log());
    // One started by hand without the Wayland input method still takes
    // IBus's panel from the daemon's.
    let plain = daemon.start_foreign_panel(&[]);
    let third = daemon.wait_for_panels(3)[2];
    thread::sleep(SETTLE);
    assert_eq!(
        daemon.stopped(),
        [ours, wayland, second, plain],
        "{:?}",
        daemon.ibus_log()
    );
    assert_eq!(
        daemon.panels_started(),
        [ours, second, third],
        "{:?}",
        daemon.ibus_log()
    );
}

#[test]
fn an_ibus_daemon_running_its_own_panel_is_replaced() {
    for_each_value(&["-drx", "--replace --panel default", ""], |i, args| {
        let ibus = Ibus {
            foreign_daemon: Some(args),
            ..Ibus::opted_in()
        };
        let (_sway, daemon) =
            Daemon::start_with_ibus(&format!("im-panel-daemon-{i}"), KEYFILE, ibus);
        daemon.wait_for_panels(1);
        let log = daemon.ibus_log();
        let daemons: Vec<&String> = log.iter().filter(|l| l.starts_with("daemon ")).collect();
        assert_eq!(daemons.len(), 2, "{args:?}: IBus log: {log:?}");
        assert!(
            daemons[1].ends_with(" --replace --panel disable"),
            "{log:?}"
        );
        let foreign: u32 = daemons[0].split(' ').nth(1).unwrap().parse().unwrap();
        assert!(daemon.stopped().contains(&foreign), "{args:?}: {log:?}");
    });
}

#[test]
fn an_ibus_daemon_without_a_panel_is_used() {
    let ibus = Ibus {
        foreign_daemon: Some("--xim --panel disable"),
        ..Ibus::opted_in()
    };
    let (_sway, daemon) = Daemon::start_with_ibus("im-panelless-daemon", KEYFILE, ibus);
    daemon.wait_for_panels(1);
    thread::sleep(SETTLE);
    let log = daemon.ibus_log();
    let daemons = log.iter().filter(|l| l.starts_with("daemon ")).count();
    assert_eq!(daemons, 1, "IBus log: {log:?}");
    assert_eq!(daemon.stopped(), Vec::<u32>::new(), "{log:?}");
}

/// Wait until sway got the layouts of `TWO_SOURCES`. The input method's
/// own subscription to sway's events can come first, so this waits for the
/// commands rather than for the daemon's subscription.
fn assert_layouts_applied(sway: &FakeSway) {
    sway.wait_for(0, TWO_LAYOUTS[2]);
    let commands = sway.commands();
    let applied: Vec<&str> = commands
        .iter()
        .map(String::as_str)
        .skip_while(|c| *c != TWO_LAYOUTS[0])
        .take(3)
        .collect();
    assert_eq!(applied, TWO_LAYOUTS, "sway got {commands:?}");
}

/// Start the daemon with trawld as given, and check that it applies the
/// layouts and starts no IBus program.
fn assert_ibus_off(name: &str, trawl: Trawl) {
    let missing = matches!(trawl, Trawl::Missing);
    let ibus = Ibus {
        trawl,
        ..Ibus::default()
    };
    let (sway, mut daemon) = Daemon::start_with_ibus(name, TWO_SOURCES, ibus);
    assert_layouts_applied(&sway);
    if !missing {
        assert_eq!(
            daemon.wait_for_trawl(),
            ["regolith.experimental.ibus"],
            "the resource is read once"
        );
    }
    thread::sleep(SETTLE);
    assert_eq!(daemon.ibus_log(), Vec::<String>::new(), "{name}");
    assert!(daemon.is_running(), "{name}: the daemon exited");
}

/// Run `check` for each value, at once, and pass on the first failure.
fn for_each_value(values: &[&'static str], check: fn(usize, &'static str)) {
    let checks: Vec<_> = values
        .iter()
        .enumerate()
        .map(|(i, &value)| thread::spawn(move || check(i, value)))
        .collect();
    // Every check finishes, and cleans up after itself, before a failure
    // is passed on.
    let failures: Vec<_> = checks.into_iter().filter_map(|c| c.join().err()).collect();
    if let Some(failure) = failures.into_iter().next() {
        std::panic::resume_unwind(failure);
    }
}

#[test]
fn ibus_is_off_unless_the_user_opted_in() {
    assert_ibus_off("im-default", Trawl::Unset);
}

#[test]
fn ibus_stays_off_for_values_other_than_true() {
    for_each_value(
        &[
            "false", "yes", "1", "on", "no", "off", "0", "enabled", "truee", "t",
        ],
        |i, value| assert_ibus_off(&format!("im-value-{i}"), Trawl::Value(value)),
    );
}

#[test]
fn ibus_stays_off_when_trawld_fails() {
    assert_ibus_off("im-trawl-fails", Trawl::Fails);
}

#[test]
fn ibus_stays_off_without_trawld() {
    assert_ibus_off("im-no-trawl", Trawl::Missing);
}

#[test]
fn true_in_any_case_turns_ibus_on() {
    for_each_value(&["true", " TRUE ", "True", "true\n"], |i, value| {
        let ibus = Ibus {
            trawl: Trawl::Value(value),
            ..Ibus::default()
        };
        let (sway, daemon) = Daemon::start_with_ibus(&format!("im-on-{i}"), TWO_SOURCES, ibus);
        assert_layouts_applied(&sway);
        daemon.wait_for_panels(1);
        let log = daemon.ibus_log();
        assert!(
            log.iter().any(|l| l.starts_with("daemon ")),
            "{value:?}: no ibus-daemon; IBus log: {log:?}"
        );
        assert_eq!(
            daemon.trawl_calls(),
            ["regolith.experimental.ibus"],
            "the resource is read once"
        );
    });
}
