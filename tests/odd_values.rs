//! Layouts, variants, XKB options and cursor themes with quote marks,
//! command separators and `$` must reach sway as exactly one command each,
//! carrying the value unchanged, and values sway can't receive are left out.

mod common;

use common::{change, Daemon};

const SOURCES: &str = "org.gnome.desktop.input-sources";
const INTERFACE: &str = "org.gnome.desktop.interface";

#[test]
fn odd_values_reach_sway_as_one_command_each() {
    let (sway, mut daemon) = Daemon::start(
        "odd-values",
        "[org/gnome/desktop/input-sources]\nsources=[('xkb', 'us')]\n",
    );
    sway.wait_for_startup();

    // The source with a line feed in its variant is left out; the others
    // keep their order. sway reads `",o'dd;exec x,$$mod"` as `,o'dd;exec x,$mod`.
    let seen = sway.commands().len();
    daemon.gsettings_set(
        SOURCES,
        "sources",
        r#"[('xkb', 'us'), ('xkb', "fr+o'dd;exec x"), ('xkb', 'de+bad\nvariant'), ('xkb', 'jp+$mod')]"#,
    );
    assert_eq!(
        sway.wait_for(seen, "input type:keyboard xkb_layout")
            .as_deref(),
        Some("input type:keyboard xkb_layout 'us,fr,jp'")
    );
    assert_eq!(
        sway.wait_for(seen + 2, "input type:keyboard xkb_variant")
            .as_deref(),
        Some(r#"input type:keyboard xkb_variant ",o'dd;exec x,$$mod""#)
    );

    assert_eq!(
        change(
            &sway,
            &daemon,
            (
                SOURCES,
                "xkb-options",
                r#"["it's;$x", 'bad\noption', 'compose:ralt']"#
            ),
            "input type:keyboard xkb_options"
        )
        .as_deref(),
        Some(r#"input type:keyboard xkb_options "it's;$$x,compose:ralt""#)
    );

    assert_eq!(
        change(
            &sway,
            &daemon,
            (INTERFACE, "cursor-theme", r#"'My "odd" $theme'"#),
            "seat * xcursor_theme"
        )
        .as_deref(),
        Some(r#"seat * xcursor_theme 'My "odd" $$theme' 24"#)
    );

    // A theme ending in one backslash would escape the size after it: no
    // command, and the next theme sway can receive is applied as usual.
    let seen = sway.commands().len();
    daemon.gsettings_set(INTERFACE, "cursor-theme", r"'Bad\\'");
    assert_eq!(
        change(
            &sway,
            &daemon,
            (INTERFACE, "cursor-theme", "'Yaru'"),
            "seat * xcursor_theme"
        )
        .as_deref(),
        Some(r#"seat * xcursor_theme "Yaru" 24"#)
    );
    let commands = sway.commands();
    assert!(
        !commands[seen..].iter().any(|c| c.contains("Bad")),
        "sway got {commands:?}"
    );
    assert!(daemon.is_running());
}
