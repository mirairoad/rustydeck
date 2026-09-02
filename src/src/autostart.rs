//! Starting RustyDeck with the desktop session.
//!
//! A desktop entry in the XDG autostart directory is the one mechanism every desktop agrees on:
//! GNOME and KDE read it themselves, and a bare compositor session managed by systemd - which is
//! what a Hyprland session usually is - gets it through systemd's xdg-autostart generator. Nothing
//! here is specific to a desktop environment, and nothing here needs root.
//!
//! The entry is written from [`std::env::current_exe`] rather than a fixed path, so it points at
//! whichever copy of RustyDeck turned the setting on.

use anyhow::{Context, Result};

use std::path::PathBuf;

const FILE: &str = "rustydeck.desktop";

fn directory() -> PathBuf {
	std::env::var_os("XDG_CONFIG_HOME")
		.map(PathBuf::from)
		.filter(|path| path.is_absolute())
		.unwrap_or_else(|| dirs::home_dir().expect("a home directory").join(".config"))
		.join("autostart")
}

/// Where the autostart entry lives.
pub fn path() -> PathBuf {
	directory().join(FILE)
}

/// Whether RustyDeck is set to start with the session.
pub fn is_enabled() -> bool {
	path().is_file()
}

fn entry() -> Result<String> {
	let executable = std::env::current_exe().context("finding this executable")?;
	let executable = executable.display();

	// Started hidden, because a login is not a request to be looked at: the deck is served from the
	// tray and the window is one click away when it is wanted.
	Ok(format!(
		"[Desktop Entry]\n\
		 Type=Application\n\
		 Name={name}\n\
		 Comment=Control an Elgato Stream Deck\n\
		 Exec=\"{executable}\" --hidden\n\
		 Icon=rustydeck\n\
		 Terminal=false\n\
		 Categories=Utility;\n\
		 X-GNOME-Autostart-enabled=true\n",
		name = crate::shared::PRODUCT_NAME,
	))
}

/// Turn starting at login on or off.
pub fn set(enabled: bool) -> Result<()> {
	let path = path();

	if !enabled {
		return match std::fs::remove_file(&path) {
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
			result => result.with_context(|| format!("removing {}", path.display())),
		};
	}

	std::fs::create_dir_all(directory()).context("creating the autostart directory")?;
	std::fs::write(&path, entry()?).with_context(|| format!("writing {}", path.display()))
}
