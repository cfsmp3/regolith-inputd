//! Changes made in sway (with swaymsg) are mirrored back to GSettings, so
//! that the Settings panels show them. What sway does on its own (a reload
//! resetting the devices, a device being plugged in) and what the daemon
//! itself applies to sway are never written to GSettings.

mod common;

use common::{change, Daemon};
use serde_json::json;
use std::thread;
use std::time::Duration;

const MOUSE: &str = "org.gnome.desktop.peripherals.mouse";
const TOUCHPAD: &str = "org.gnome.desktop.peripherals.touchpad";
const PAD: &str = "1267:12445:ELAN0001:00_04F3:309D_Touchpad";
const MOUSE_ID: &str = "1133:49970:Logitech_Gaming_Mouse_G502";

/// The harness's keyboard and clickpad, plus a mouse.
const INPUTS: &str = r#"[
  {"identifier": "1:1:AT_Translated_Set_2_keyboard", "name": "AT Translated Set 2 keyboard",
   "vendor": 1, "product": 1, "type": "keyboard",
   "xkb_active_layout_name": "English (US)", "xkb_layout_names": ["English (US)"],
   "xkb_active_layout_index": 0, "libinput": {"send_events": "enabled"}},
  {"identifier": "1267:12445:ELAN0001:00_04F3:309D_Touchpad", "name": "ELAN0001:00 04F3:309D Touchpad",
   "vendor": 1267, "product": 12445, "type": "touchpad",
   "libinput": {"send_events": "enabled", "tap": "disabled", "tap_button_map": "lrm",
                "tap_drag": "enabled", "tap_drag_lock": "disabled", "accel_speed": 0.0,
                "accel_profile": "adaptive", "natural_scroll": "disabled", "left_handed": "disabled",
                "click_method": "button_areas", "middle_emulation": "disabled",
                "scroll_method": "two_finger", "dwt": "enabled"}},
  {"identifier": "1133:49970:Logitech_Gaming_Mouse_G502", "name": "Logitech Gaming Mouse G502",
   "vendor": 1133, "product": 49970, "type": "pointer",
   "libinput": {"send_events": "enabled", "accel_speed": 0.0, "accel_profile": "adaptive",
                "natural_scroll": "disabled", "left_handed": "disabled",
                "middle_emulation": "disabled", "scroll_method": "none"}}
]"#;

const KEYFILE: &str = "[org/gnome/desktop/input-sources]\n\
                       sources=[('xkb', 'us')]\n\
                       [org/gnome/desktop/peripherals/touchpad]\n\
                       tap-to-click=false\n";

/// Longer than the daemon waits for sway's devices to settle.
fn settle() {
    thread::sleep(Duration::from_millis(1500));
}

/// A change made in sway after startup reaches GSettings, once, and isn't
/// answered by applying every GSettings key to sway again.
#[test]
fn a_touchpad_change_in_sway_reaches_gsettings() {
    let (sway, daemon) = Daemon::start_with_inputs("sway-touchpad", KEYFILE, INPUTS);
    sway.wait_for_startup();
    let monitor = daemon.monitor(TOUCHPAD);
    let seen = sway.commands().len();

    sway.user_command("input type:touchpad tap enabled");
    assert_eq!(
        daemon.wait_for_gsetting(TOUCHPAD, "tap-to-click", "true"),
        "true",
        "sway got {:?}",
        sway.commands()
    );
    settle();
    assert_eq!(monitor.changes(), ["tap-to-click: true"]);
    let answered: Vec<String> = sway.commands().into_iter().skip(seen).collect();
    assert!(
        !answered
            .iter()
            .any(|c| c.contains("scroll_method") || c.contains("dwt")),
        "a sway change was answered with GSettings for every key: {answered:?}"
    );
}

#[test]
fn a_mouse_change_in_sway_reaches_gsettings() {
    let (sway, daemon) = Daemon::start_with_inputs("sway-mouse", KEYFILE, INPUTS);
    sway.wait_for_startup();
    sway.user_command("input type:pointer left_handed enabled");
    assert_eq!(
        daemon.wait_for_gsetting(MOUSE, "left-handed", "true"),
        "true"
    );
    sway.user_command(&format!("input \"{MOUSE_ID}\" natural_scroll enabled"));
    assert_eq!(
        daemon.wait_for_gsetting(MOUSE, "natural-scroll", "true"),
        "true"
    );
    sway.user_command("input type:pointer left_handed disabled");
    assert_eq!(
        daemon.wait_for_gsetting(MOUSE, "left-handed", "false"),
        "false"
    );
}

/// sway keeps the speed as a float: 0.1 comes back as 0.10000000149011612.
/// It is mirrored as 0.1, written once, and the daemon's own speed
/// command doesn't come back to GSettings either.
#[test]
fn speeds_are_mirrored_without_float_noise() {
    let (sway, daemon) = Daemon::start_with_inputs("sway-speed", KEYFILE, INPUTS);
    sway.wait_for_startup();
    let monitor = daemon.monitor(MOUSE);
    sway.user_command("input type:pointer pointer_accel 0.1");
    // GVariant prints 0.1 as 0.10000000000000001.
    assert_eq!(
        daemon.wait_for_gsetting(MOUSE, "speed", "0.10000000000000001"),
        "0.10000000000000001"
    );
    settle();
    // A value from a Settings slider, with more digits than a float has.
    daemon.gsettings_set(MOUSE, "speed", "0.31578947368421");
    settle();
    assert_eq!(
        monitor.changes(),
        ["speed: 0.10000000000000001", "speed: 0.31578947368421001"]
    );
    assert_eq!(
        sway.libinput(MOUSE_ID, "accel_speed"),
        json!(0.31578946_f32 as f64)
    );
}

/// Applying a GSettings change doesn't stop sway changes from being
/// mirrored afterwards, and vice versa.
#[test]
fn both_directions_keep_working_after_each_other() {
    let (sway, daemon) = Daemon::start_with_inputs("sway-both", KEYFILE, INPUTS);
    sway.wait_for_startup();
    assert_eq!(
        change(
            &sway,
            &daemon,
            (TOUCHPAD, "tap-and-drag", "false"),
            "input type:touchpad drag"
        )
        .as_deref(),
        Some("input type:touchpad drag disabled")
    );
    // The daemon reads sway's state right after its command.
    thread::sleep(Duration::from_millis(100));
    sway.user_command("input type:touchpad tap enabled");
    assert_eq!(
        daemon.wait_for_gsetting(TOUCHPAD, "tap-to-click", "true"),
        "true"
    );
    settle();
    assert_eq!(
        change(
            &sway,
            &daemon,
            (TOUCHPAD, "natural-scroll", "true"),
            "input type:touchpad natural_scroll"
        )
        .as_deref(),
        Some("input type:touchpad natural_scroll enabled")
    );
    thread::sleep(Duration::from_millis(100));
    sway.user_command("input type:touchpad drag enabled");
    assert_eq!(
        daemon.wait_for_gsetting(TOUCHPAD, "tap-and-drag", "true"),
        "true"
    );
}

/// sway spells it disabled_on_external_mouse; GNOME disabled-on-external-mouse.
/// Mirroring it as "disabled" would switch the touchpad off.
#[test]
fn touchpad_off_with_an_external_mouse_is_mirrored_as_such() {
    let (sway, daemon) = Daemon::start_with_inputs("sway-send-events", KEYFILE, INPUTS);
    sway.wait_for_startup();
    sway.user_command("input type:touchpad events disabled_on_external_mouse");
    assert_eq!(
        daemon.wait_for_gsetting(TOUCHPAD, "send-events", "'disabled-on-external-mouse'"),
        "'disabled-on-external-mouse'"
    );
}

/// Events about devices the daemon has no settings for (here a lid switch)
/// are ignored, and the daemon keeps mirroring the others.
#[test]
fn other_devices_are_ignored() {
    let (sway, mut daemon) = Daemon::start_with_inputs("sway-switch", KEYFILE, INPUTS);
    sway.wait_for_startup();
    sway.add_device(json!({"identifier": "0:5:Lid_Switch", "name": "Lid Switch",
                           "vendor": 0, "product": 5, "type": "switch",
                           "libinput": {"send_events": "enabled"}}));
    settle();
    sway.user_command("input type:touchpad tap enabled");
    assert_eq!(
        daemon.wait_for_gsetting(TOUCHPAD, "tap-to-click", "true"),
        "true"
    );
    assert!(daemon.is_running());
}

/// A device that is plugged in comes with its own settings. They are not
/// the user's choice and must not reach GSettings, also not when it is
/// unplugged again.
#[test]
fn a_plugged_in_device_does_not_change_gsettings() {
    let (sway, daemon) = Daemon::start_with_inputs("sway-hotplug", KEYFILE, INPUTS);
    sway.wait_for_startup();
    let monitor = daemon.monitor(MOUSE);
    let second = json!({"identifier": "1:2:Second_Mouse", "name": "Second Mouse",
                        "vendor": 1, "product": 2, "type": "pointer",
                        "libinput": {"send_events": "enabled", "accel_speed": 0.5,
                                     "natural_scroll": "enabled", "left_handed": "enabled"}});
    sway.add_device(second.clone());
    settle();
    sway.remove_device(MOUSE_ID);
    settle();
    sway.remove_device("1:2:Second_Mouse");
    sway.add_device(second);
    settle();
    assert_eq!(monitor.changes(), Vec::<String>::new());
    // Changes to all pointers still reach GSettings.
    sway.user_command("input type:pointer left_handed disabled");
    settle();
    assert_eq!(daemon.gsettings_get(MOUSE, "left-handed"), "false");
    assert_eq!(monitor.changes(), Vec::<String>::new());
    sway.user_command("input type:pointer natural_scroll disabled");
    sway.user_command("input type:pointer left_handed enabled");
    assert_eq!(
        daemon.wait_for_gsetting(MOUSE, "left-handed", "true"),
        "true"
    );
    settle();
    assert_eq!(monitor.changes(), ["left-handed: true"]);
}

/// An input event can arrive after later changes; what it reports is never
/// mirrored, only sway's actual state is.
#[test]
fn stale_events_are_not_mirrored() {
    let (sway, daemon) = Daemon::start_with_inputs(
        "sway-stale",
        "[org/gnome/desktop/input-sources]\nsources=[('xkb', 'us')]\n\
         [org/gnome/desktop/peripherals/touchpad]\ntap-to-click=true\n",
        INPUTS,
    );
    sway.wait_for_startup();
    let monitor = daemon.monitor(TOUCHPAD);
    let mut stale: serde_json::Value = serde_json::from_str(INPUTS).unwrap();
    sway.send_input_event("libinput_config", stale[1].take());
    settle();
    assert_eq!(monitor.changes(), Vec::<String>::new());
    assert_eq!(daemon.gsettings_get(TOUCHPAD, "tap-to-click"), "true");
}

/// The reload's resets: every touchpad and mouse setting back to libinput's
/// defaults.
fn reset_devices(sway: &common::FakeSway) {
    sway.user_command("input type:touchpad tap disabled");
    sway.user_command("input type:touchpad natural_scroll disabled");
    sway.user_command("input type:pointer left_handed disabled");
    sway.user_command("input type:pointer pointer_accel 0");
}

const CHOSEN: &str = "[org/gnome/desktop/input-sources]\nsources=[('xkb', 'us')]\n\
                      [org/gnome/desktop/peripherals/touchpad]\n\
                      tap-to-click=true\nnatural-scroll=true\n\
                      [org/gnome/desktop/peripherals/mouse]\n\
                      left-handed=true\nspeed=-0.5\n";

/// After a reload sway has the user's GSettings again, and GSettings was
/// never written.
fn assert_reload_kept_gsettings(sway: &common::FakeSway, daemon: &Daemon) {
    assert_eq!(daemon.gsettings_get(TOUCHPAD, "tap-to-click"), "true");
    assert_eq!(daemon.gsettings_get(TOUCHPAD, "natural-scroll"), "true");
    assert_eq!(daemon.gsettings_get(MOUSE, "left-handed"), "true");
    assert_eq!(daemon.gsettings_get(MOUSE, "speed"), "-0.5");
    assert_eq!(sway.libinput(PAD, "tap"), "enabled");
    assert_eq!(sway.libinput(PAD, "natural_scroll"), "enabled");
    assert_eq!(sway.libinput(MOUSE_ID, "left_handed"), "enabled");
    assert_eq!(sway.libinput(MOUSE_ID, "accel_speed"), -0.5);
}

/// Regolith's reload binding: a reload_pending tick, the reload, which
/// resets the devices and ends with sway's workspace "reload" event, then
/// the reload_done tick from the config. sway can report some resets only
/// after the reload_done tick.
#[test]
fn a_regolith_reload_keeps_gsettings() {
    let (sway, daemon) = Daemon::start_with_inputs("sway-reload", CHOSEN, INPUTS);
    sway.wait_for_startup();
    let touchpad = daemon.monitor(TOUCHPAD);
    let mouse = daemon.monitor(MOUSE);
    for _ in 0..3 {
        let seen = sway.commands().len();
        sway.send_tick(r#"{"status": "reload_pending"}"#);
        reset_devices(&sway);
        sway.send_workspace_reload();
        sway.send_tick(r#"{"status": "reload_done"}"#);
        // A late report of a reset that already happened.
        let mut late: serde_json::Value = serde_json::from_str(INPUTS).unwrap();
        sway.send_input_event("libinput_config", late[1].take());
        assert_eq!(
            sway.wait_for(seen, "input type:touchpad tap").as_deref(),
            Some("input type:touchpad tap enabled"),
            "GSettings were not applied after the reload"
        );
        settle();
        assert_reload_kept_gsettings(&sway, &daemon);
    }
    assert_eq!(touchpad.changes(), Vec::<String>::new());
    assert_eq!(mouse.changes(), Vec::<String>::new());
}

/// `swaymsg reload` sends no tick before the reload; sway's resets come
/// first and only then its workspace "reload" event.
#[test]
fn a_bare_reload_keeps_gsettings() {
    let (sway, daemon) = Daemon::start_with_inputs("sway-bare-reload", CHOSEN, INPUTS);
    sway.wait_for_startup();
    let touchpad = daemon.monitor(TOUCHPAD);
    let mouse = daemon.monitor(MOUSE);
    let seen = sway.commands().len();
    reset_devices(&sway);
    sway.send_workspace_reload();
    assert_eq!(
        sway.wait_for(seen, "input type:touchpad tap").as_deref(),
        Some("input type:touchpad tap enabled"),
        "GSettings were not applied after the reload"
    );
    settle();
    assert_reload_kept_gsettings(&sway, &daemon);
    assert_eq!(touchpad.changes(), Vec::<String>::new());
    assert_eq!(mouse.changes(), Vec::<String>::new());
    // The user's changes are mirrored again after the reload.
    sway.user_command("input type:touchpad tap disabled");
    assert_eq!(
        daemon.wait_for_gsetting(TOUCHPAD, "tap-to-click", "false"),
        "false"
    );
}

/// A device that lacks a setting can report it as null, as an absolute
/// pointer does for its acceleration. It takes no part in the agreement,
/// so a speed change to the other pointers is still mirrored.
#[test]
fn a_setting_reported_as_null_does_not_block_mirroring() {
    let mut inputs: serde_json::Value = serde_json::from_str(INPUTS).unwrap();
    inputs.as_array_mut().unwrap().push(json!({
        "identifier": "1:3:QEMU_QEMU_USB_Tablet", "name": "QEMU QEMU USB Tablet",
        "vendor": 1, "product": 3, "type": "pointer",
        "libinput": {"send_events": "enabled", "accel_speed": null,
                     "natural_scroll": "disabled", "left_handed": "disabled"}}));
    // The harness keeps the devices for the whole test.
    let inputs: &'static str = Box::leak(inputs.to_string().into_boxed_str());
    let (sway, daemon) = Daemon::start_with_inputs("sway-null", KEYFILE, inputs);
    sway.wait_for_startup();
    let monitor = daemon.monitor(MOUSE);
    sway.user_command("input type:pointer pointer_accel 0.25");
    assert_eq!(daemon.wait_for_gsetting(MOUSE, "speed", "0.25"), "0.25");
    settle();
    assert_eq!(monitor.changes(), ["speed: 0.25"]);
    assert_eq!(
        sway.libinput("1:3:QEMU_QEMU_USB_Tablet", "accel_speed"),
        serde_json::Value::Null
    );
}

/// A change made right after a device is plugged in, before sway's devices
/// have settled, is mirrored: the new device is known at once. What the
/// new device came with (here natural scrolling) is still not mirrored.
#[test]
fn a_change_right_after_a_plug_in_is_mirrored() {
    let (sway, daemon) = Daemon::start_with_inputs("sway-hotplug-change", KEYFILE, INPUTS);
    sway.wait_for_startup();
    let monitor = daemon.monitor(MOUSE);
    sway.add_device(
        json!({"identifier": "1:2:Second_Mouse", "name": "Second Mouse",
                           "vendor": 1, "product": 2, "type": "pointer",
                           "libinput": {"send_events": "enabled", "accel_speed": 0.0,
                                        "natural_scroll": "enabled",
                                        "left_handed": "disabled"}}),
    );
    thread::sleep(Duration::from_millis(300));
    sway.user_command("input type:pointer left_handed enabled");
    assert_eq!(
        daemon.wait_for_gsetting(MOUSE, "left-handed", "true"),
        "true"
    );
    settle();
    assert_eq!(monitor.changes(), ["left-handed: true"]);
}

/// A sway change that hasn't settled yet is not lost when a Settings change
/// to another key comes in before it settles.
#[test]
fn a_pending_sway_change_survives_a_settings_change() {
    let (sway, daemon) = Daemon::start_with_inputs("sway-pending", KEYFILE, INPUTS);
    sway.wait_for_startup();
    let touchpad = daemon.monitor(TOUCHPAD);
    sway.user_command("input type:touchpad tap enabled");
    // Older than a reload's device changes, but not settled yet.
    thread::sleep(Duration::from_millis(300));
    daemon.gsettings_set(MOUSE, "left-handed", "true");
    assert_eq!(
        daemon.wait_for_gsetting(TOUCHPAD, "tap-to-click", "true"),
        "true",
        "sway got {:?}",
        sway.commands()
    );
    settle();
    assert_eq!(touchpad.changes(), ["tap-to-click: true"]);
    assert_eq!(sway.libinput(MOUSE_ID, "left_handed"), "enabled");
    assert_eq!(sway.libinput(PAD, "tap"), "enabled");
}

/// A Settings change to the same setting as a pending sway change wins:
/// the sway change is not written back over it.
#[test]
fn a_settings_change_wins_over_a_pending_sway_change_to_the_same_key() {
    let (sway, daemon) = Daemon::start_with_inputs("sway-pending-same", KEYFILE, INPUTS);
    sway.wait_for_startup();
    let mouse = daemon.monitor(MOUSE);
    sway.user_command("input type:pointer pointer_accel 0.5");
    thread::sleep(Duration::from_millis(300));
    daemon.gsettings_set(MOUSE, "speed", "-0.5");
    settle();
    assert_eq!(daemon.gsettings_get(MOUSE, "speed"), "-0.5");
    assert_eq!(sway.libinput(MOUSE_ID, "accel_speed"), -0.5);
    assert_eq!(mouse.changes(), ["speed: -0.5"]);
}

/// A reload announced while an earlier one is still pending (its
/// configuration failed to load, so it never ended) is waited for from its
/// own reload_pending tick. Here the second reload resets the devices more
/// than ten seconds after the first tick, and takes a while to end.
#[test]
fn a_chained_reload_is_waited_for_from_its_own_tick() {
    let (sway, daemon) = Daemon::start_with_inputs("sway-reload-chain", CHOSEN, INPUTS);
    sway.wait_for_startup();
    let touchpad = daemon.monitor(TOUCHPAD);
    let mouse = daemon.monitor(MOUSE);
    sway.send_tick(r#"{"status": "reload_pending"}"#);
    thread::sleep(Duration::from_secs(8));
    sway.send_tick(r#"{"status": "reload_pending"}"#);
    thread::sleep(Duration::from_millis(2500));
    reset_devices(&sway);
    // Longer than the daemon waits for devices to settle.
    thread::sleep(Duration::from_secs(1));
    let seen = sway.commands().len();
    sway.send_workspace_reload();
    sway.send_tick(r#"{"status": "reload_done"}"#);
    assert_eq!(
        sway.wait_for(seen, "input type:touchpad tap").as_deref(),
        Some("input type:touchpad tap enabled"),
        "GSettings were not applied after the reload"
    );
    settle();
    assert_reload_kept_gsettings(&sway, &daemon);
    assert_eq!(touchpad.changes(), Vec::<String>::new());
    assert_eq!(mouse.changes(), Vec::<String>::new());
}
