//! Simulated Stream Decks, for working on the app without the hardware plugged in.
//!
//! Debug builds only - `main` never calls this in a release build, so nothing here ships.
//!
//! There is one simulator rather than one per model: every layout difference between Stream Decks
//! is already described by [`Kind`], so a simulated device is just a [`DeviceInfo`] built from the
//! same accessors the real driver uses. Adding a model is one line in [`MODELS`].
//!
//! A simulated device registers through the ordinary `register_device` path, so it gets profiles,
//! pages and slots exactly like real hardware. What it does *not* get is a driver: ids are prefixed
//! [`PREFIX`] rather than `sd-`, and every hardware write in `events::outbound::devices` is guarded
//! on `sd-`, so images and brightness are silently dropped instead of going anywhere.

use crate::events::inbound::{PayloadEvent, devices};
use crate::shared::DeviceInfo;

use elgato_streamdeck::info::Kind;

/// Marks a device id as simulated. Deliberately not `sd-`, which is what routes to the driver.
pub const PREFIX: &str = "sim-";

/// Is this a simulated device rather than real hardware?
pub fn is_simulated(device_id: &str) -> bool {
	device_id.starts_with(PREFIX)
}

/// The models offered in the picker, as (id suffix, kind).
///
/// Names come from [`crate::elgato::model_name`], the same table the driver uses, so a simulated
/// deck is labelled exactly as the real one would be.
///
/// Only standalone products: the `*Module` variants are OEM boards, not something a user owns, and
/// the near-duplicate revisions (`Original`, `Mk2Scissor`, `MiniMk2`, …) have layouts identical to
/// an entry already here, so simulating them would add rows that behave the same.
const MODELS: &[(&str, Kind)] = &[
	("mini", Kind::Mini),
	("original", Kind::OriginalV2),
	("mk2", Kind::Mk2),
	("xl", Kind::Xl),
	("plus", Kind::Plus),
	("plusxl", Kind::PlusXl),
	("neo", Kind::Neo),
	("pedal", Kind::Pedal),
];

/// The `type` the Stream Deck SDK uses to identify a model.
///
/// Mirrors the mapping in `elgato::init` so a simulated device reports what the real one would.
fn device_type(kind: Kind) -> u8 {
	match kind {
		Kind::Original | Kind::OriginalV2 | Kind::Mk2 | Kind::Mk2Scissor | Kind::Mk2Module => 0,
		Kind::Mini | Kind::MiniMk2 | Kind::MiniDiscord | Kind::MiniMk2Module => 1,
		Kind::Xl | Kind::XlV2 | Kind::XlV2Module => 2,
		Kind::Pedal => 5,
		Kind::Plus => 7,
		Kind::Neo => 9,
		Kind::PlusXl => 13,
	}
}

fn describe(suffix: &str, kind: Kind) -> DeviceInfo {
	DeviceInfo {
		id: format!("{PREFIX}{suffix}"),
		plugin: crate::shared::BUILTIN_PLUGIN.to_owned(),
		name: crate::elgato::model_name(kind).to_owned(),
		rows: kind.row_count(),
		columns: kind.column_count(),
		encoders: kind.encoder_count(),
		touchpoints: kind.touchpoint_count(),
		// Only the Neo has the letterbox strip under its keys.
		infobars: if kind == Kind::Neo { 1 } else { 0 },
		r#type: device_type(kind),
	}
}

/// Register every simulated model, so they appear in the picker alongside real hardware.
pub async fn register_all() {
	for (suffix, kind) in MODELS {
		let info = describe(suffix, *kind);
		let id = info.id.clone();
		if let Err(error) = devices::register_device(PayloadEvent { payload: info }).await {
			log::error!("Failed to register simulated device {id}: {error}");
		}
	}
	log::info!("Registered {} simulated devices", MODELS.len());
}

/// Turn a simulated dial, as though the knob had been rotated.
///
/// Goes in at the same point the driver does, so everything downstream - the debounce, the device
/// dial config, the shell command - is the code that runs for real hardware.
pub fn rotate(device: &str, position: u8, ticks: i16) {
	let device = device.to_owned();
	crate::spawn(async move {
		let event = PayloadEvent {
			payload: devices::TicksPayload { device, position, ticks },
		};
		if let Err(error) = devices::encoder_change(event).await {
			log::error!("Simulated dial rotate failed: {error}");
		}
	});
}

/// Press and release a simulated dial.
pub fn press_dial(device: &str, position: u8) {
	let device = device.to_owned();
	crate::spawn(async move {
		let down = PayloadEvent {
			payload: devices::PressPayload {
				device: device.clone(),
				position,
			},
		};
		if let Err(error) = devices::encoder_down(down).await {
			log::error!("Simulated dial press failed: {error}");
			return;
		}
		let up = PayloadEvent {
			payload: devices::PressPayload { device, position },
		};
		if let Err(error) = devices::encoder_up(up).await {
			log::error!("Simulated dial release failed: {error}");
		}
	});
}

/// Tap the strip segment above a simulated dial.
pub fn tap_strip(device: &str, position: u8) {
	let device = device.to_owned();
	crate::spawn(async move {
		let event = PayloadEvent {
			payload: devices::TouchscreenPressPayload {
				device,
				position,
				// Centre of the segment; nothing downstream reads the coordinates.
				x: 100,
				y: 50,
				hold: false,
			},
		};
		if let Err(error) = devices::touchscreen_press(event).await {
			log::error!("Simulated strip tap failed: {error}");
		}
	});
}
