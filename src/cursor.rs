use crate::quoting::{sway_argument, Unquotable};
use crate::InputHandler;
use gio::{prelude::SettingsExtManual, Settings};
use log::{info, warn};
use std::error::Error;
use swayipc::{Connection as SwayConnection, Input};

/// Mirrors GNOME's cursor theme and size to sway's seats.
pub struct CursorHandler {
    settings: Settings,
    sway_connection: SwayConnection,
}

impl CursorHandler {
    pub fn new() -> CursorHandler {
        let settings = Settings::new("org.gnome.desktop.interface");
        let sway_connection = SwayConnection::new().unwrap();
        CursorHandler {
            settings,
            sway_connection,
        }
    }
    fn apply_cursor_theme(&mut self) -> Result<(), Box<dyn Error>> {
        let theme: String = self.settings().get("cursor-theme");
        let size: i32 = self.settings().get("cursor-size");
        let cmd = match xcursor_theme_command(&theme, size) {
            Ok(cmd) => cmd,
            Err(e) => {
                warn!("Cursor theme {theme:?} not applied: sway can't receive it, {e}");
                return Ok(());
            }
        };
        info!("{cmd}");
        self.sway_connection().run_command(cmd)?;
        Ok(())
    }
}

/// sway's command for a cursor theme and size. sway also hands both to
/// XWayland and, as XCURSOR_THEME and XCURSOR_SIZE, to the programs it
/// starts afterwards.
fn xcursor_theme_command(theme: &str, size: i32) -> Result<String, Unquotable> {
    let theme = sway_argument(theme, '"', false)?;
    Ok(format!("seat * xcursor_theme {theme} {}", size.max(1)))
}

impl InputHandler for CursorHandler {
    fn apply_changes(&mut self, key: &str) -> Result<(), Box<dyn Error>> {
        match key {
            "cursor-theme" | "cursor-size" => {
                info!("org.gnome.desktop.interface -> Key: {key} changed");
                self.apply_cursor_theme()?
            }
            _ => (),
        };
        Ok(())
    }
    fn settings(&self) -> &Settings {
        &self.settings
    }
    fn sway_connection(&mut self) -> &mut SwayConnection {
        &mut self.sway_connection
    }
    fn apply_all(&mut self) -> Result<(), Box<dyn Error>> {
        self.apply_cursor_theme()
    }
    fn sync_gsettings(&mut self, _: &Input) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
}

unsafe impl Send for CursorHandler {}

#[cfg(test)]
mod tests {
    use super::xcursor_theme_command;
    use crate::quoting::Unquotable;

    #[test]
    fn theme_and_size_map_to_seat_command() {
        assert_eq!(
            xcursor_theme_command("Adwaita", 24).unwrap(),
            "seat * xcursor_theme \"Adwaita\" 24"
        );
        // sway keeps backslashes: quote marks go in runs of the other mark.
        assert_eq!(
            xcursor_theme_command("My \"odd\" theme", 0).unwrap(),
            "seat * xcursor_theme 'My \"odd\" theme' 1"
        );
        assert_eq!(
            xcursor_theme_command("it's \"$x\"", 32).unwrap(),
            "seat * xcursor_theme \"it's \"'\"$$x\"' 32"
        );
        assert_eq!(
            xcursor_theme_command("theme\\\\", 24).unwrap(),
            "seat * xcursor_theme \"theme\"\\\\ 24"
        );
    }

    #[test]
    fn themes_sway_cant_receive_have_no_command() {
        assert_eq!(
            xcursor_theme_command("bad\ntheme", 24),
            Err(Unquotable::LineFeed)
        );
        assert_eq!(
            xcursor_theme_command("theme\\", 24),
            Err(Unquotable::TrailingBackslash)
        );
    }
}
