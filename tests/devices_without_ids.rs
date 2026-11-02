//! sway reports `vendor` and `product` only for libinput devices. Virtual
//! keyboards (on-screen keyboards, remote-desktop servers, wtype) and the
//! devices of sway's X11 and Wayland backends have neither, and must not
//! stop the daemon.

mod common;

use common::{change, Daemon};

const TOUCHPAD: &str = "org.gnome.desktop.peripherals.touchpad";

/// A libinput keyboard and clickpad, plus a virtual keyboard as sway 1.11
/// reports it.
const INPUTS: &str = r#"[
  {"identifier": "1:1:AT_Translated_Set_2_keyboard", "name": "AT Translated Set 2 keyboard",
   "vendor": 1, "product": 1, "type": "keyboard",
   "xkb_active_layout_name": "English (US)", "xkb_layout_names": ["English (US)"],
   "xkb_active_layout_index": 0, "libinput": {"send_events": "enabled"}},
  {"identifier": "0:0:wlr_virtual_keyboard_v1", "name": "wlr_virtual_keyboard_v1",
   "type": "keyboard", "repeat_delay": 600, "repeat_rate": 25,
   "xkb_active_layout_name": "English (US)", "xkb_layout_names": ["English (US)"],
   "xkb_active_layout_index": 0},
  {"identifier": "1267:12445:ELAN0001:00_04F3:309D_Touchpad", "name": "ELAN0001:00 04F3:309D Touchpad",
   "vendor": 1267, "product": 12445, "type": "touchpad",
   "libinput": {"send_events": "enabled", "tap": "disabled", "tap_button_map": "lrm",
                "tap_drag": "enabled", "tap_drag_lock": "disabled", "accel_speed": 0.0,
                "accel_profile": "adaptive", "natural_scroll": "disabled", "left_handed": "disabled",
                "click_method": "button_areas", "middle_emulation": "disabled",
                "scroll_method": "two_finger", "dwt": "enabled"}}
]"#;

const KEYFILE: &str = "[org/gnome/desktop/input-sources]\n\
                       sources=[('xkb', 'us'), ('xkb', 'de')]\n\
                       [org/gnome/desktop/peripherals/touchpad]\n\
                       click-method='fingers'\n";

#[test]
fn a_virtual_keyboard_does_not_stop_the_daemon_at_startup() {
    let (sway, mut daemon) = Daemon::start_with_inputs("noids-startup", KEYFILE, INPUTS);
    sway.wait_for_startup();
    assert!(daemon.is_running());
    // The touchpad's own default is still found next to the virtual keyboard.
    assert_eq!(
        change(
            &sway,
            &daemon,
            (TOUCHPAD, "click-method", "default"),
            "input \""
        )
        .as_deref(),
        Some("input \"1267:12445:ELAN0001:00_04F3:309D_Touchpad\" click_method button_areas")
    );
}

#[test]
fn a_virtual_keyboard_does_not_stop_reload_handling() {
    let (sway, mut daemon) = Daemon::start_with_inputs("noids-reload", KEYFILE, INPUTS);
    sway.wait_for_startup();
    // Two reloads: the first must re-apply the settings, and must not end
    // the handling of the second.
    for _ in 0..2 {
        let seen = sway.commands().len();
        sway.send_tick(r#"{"status": "reload_done"}"#);
        assert_eq!(
            sway.wait_for(seen, "input type:keyboard xkb_layout")
                .as_deref(),
            Some("input type:keyboard xkb_layout 'us,de'")
        );
    }
    assert!(daemon.is_running());
}
