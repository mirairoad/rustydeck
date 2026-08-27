//! First-party "System" actions - encoder actions the app implements itself rather than
//! delegating to a plugin, because they need direct access to the device driver and to system
//! services a sandboxed plugin process would not have.

use crate::store::SETTINGS_MUT;
use crate::store::profiles::DialConfig;

use std::sync::LazyLock;
use std::time::Duration;

use dashmap::DashMap;

/// The system actions offered in the dial dialog, as (label, action UUID).
///
/// This is the list of first-party dial actions that are actually wired up here - `shared.rs`
/// registers a few more (volume, microphone, display brightness) whose handlers are not written
/// yet, and offering those would give the user a dial that silently does nothing.
pub const CATALOGUE: &[(&str, &str)] = &[("Brightness", crate::shared::DEVICE_BRIGHTNESS_UUID)];

/// Is this UUID one of the first-party actions the app runs itself?
pub fn is_system_action(uuid: &str) -> bool {
	CATALOGUE.iter().any(|(_, candidate)| *candidate == uuid)
}

/// Run a command through the user's login shell, reporting a failure rather than swallowing it.
///
/// `sh -c` is deliberately avoided: a login shell resolves aliases and shell functions the way a
/// terminal does. Omarchy, for instance, provides `open` as a bash function - it works when typed
/// but is invisible to a non-interactive POSIX shell, so a perfectly good command appears to do
/// nothing at all.
pub fn run_shell(command: String) {
	if command.trim().is_empty() {
		return;
	}

	crate::spawn(async move {
		log::info!("Running command: {command}");
		let shell = std::env::var("SHELL").unwrap_or_else(|_| "bash".to_owned());
		match tokio::process::Command::new(&shell).arg("-lic").arg(&command).output().await {
			Ok(output) if output.status.success() => log::info!("Command finished: {command}"),
			Ok(output) => log::error!(
				"Command failed ({}): {} - {}",
				output.status,
				command,
				String::from_utf8_lossy(&output.stderr).trim()
			),
			Err(error) => log::error!("Could not run command {command}: {error}"),
		}
	});
}

/// Brightness change per detent of the dial.
const BRIGHTNESS_STEP: i32 = 5;

/// How long to gather dial ticks before acting on them.
///
/// A fast spin delivers a burst of individual ticks; applying each one would mean a device write
/// per tick. Coalescing lets one write cover the whole movement. 50ms is the PRD's starting guess
/// rather than a measured figure - worth tuning against real hardware.
const ROTATE_DEBOUNCE: Duration = Duration::from_millis(50);

/// Ticks accumulated per (device, encoder) awaiting a flush. The presence of an entry also marks
/// that a flush is already scheduled, so a spin schedules one task rather than one per tick.
static PENDING_TICKS: LazyLock<DashMap<(String, u8), i16>> = LazyLock::new(DashMap::new);

/// Gather a rotation and schedule its application.
pub fn rotate(device: &str, encoder: u8, ticks: i16) {
	let key = (device.to_owned(), encoder);

	if let Some(mut pending) = PENDING_TICKS.get_mut(&key) {
		// A flush is already pending; fold this movement into it.
		*pending += ticks;
		return;
	}

	PENDING_TICKS.insert(key.clone(), ticks);

	let device = device.to_owned();
	crate::spawn(async move {
		tokio::time::sleep(ROTATE_DEBOUNCE).await;
		let Some((_, ticks)) = PENDING_TICKS.remove(&key) else { return };
		if let Err(error) = adjust_brightness(&device, ticks as i32 * BRIGHTNESS_STEP).await {
			log::error!("Failed to adjust device brightness: {error}");
		}
	});
}

/// A press jumps straight to full brightness.
pub fn press(device: &str) {
	let device = device.to_owned();
	crate::spawn(async move {
		if let Err(error) = set_brightness(&device, 100).await {
			log::error!("Failed to set device brightness: {error}");
		}
	});
}

async fn adjust_brightness(device: &str, delta: i32) -> Result<(), anyhow::Error> {
	let current = SETTINGS_MUT.lock().await.value.brightness as i32;
	set_brightness(device, (current + delta).clamp(0, 100) as u8).await
}

/// Persist the new level and push it to the device.
///
/// Goes through `set_device_brightness` rather than the driver directly so it keeps honouring
/// device sleep and keeps working for plugin-provided devices.
async fn set_brightness(device: &str, brightness: u8) -> Result<(), anyhow::Error> {
	{
		let mut settings = SETTINGS_MUT.lock().await;
		settings.value.brightness = brightness;
		settings.save()?;
	}
	crate::events::outbound::devices::set_device_brightness(device, brightness).await
}

/// Act on a turn of a configured dial.
pub fn dial_rotate(config: &DialConfig, device: &str, index: u8, ticks: i16) {
	match config {
		DialConfig::System { uuid } if uuid == crate::shared::DEVICE_BRIGHTNESS_UUID => rotate(device, index, ticks),
		DialConfig::System { uuid } => log::warn!("Dial {index} is set to {uuid}, which has no handler"),
		// One command per movement rather than per detent - a fast spin would otherwise launch a
		// shell per tick.
		DialConfig::Custom { left, right, .. } => run_shell(if ticks < 0 { left.clone() } else { right.clone() }),
	}
}

/// Act on a press of a configured dial.
pub fn dial_press(config: &DialConfig, device: &str, index: u8) {
	match config {
		DialConfig::System { uuid } if uuid == crate::shared::DEVICE_BRIGHTNESS_UUID => press(device),
		DialConfig::System { uuid } => log::warn!("Dial {index} is set to {uuid}, which has no handler"),
		DialConfig::Custom { centre, .. } => run_shell(centre.clone()),
	}
}
