use super::PayloadEvent;

use crate::shared::DEVICES;
use crate::store::profiles::get_device_profiles;

use serde::Deserialize;

/// A device has come online.
///
/// Devices used to be able to come from a plugin that had claimed an id namespace, which is why
/// this was an inbound event with a plugin uuid to check. Only the built-in Elgato driver calls it
/// now.
pub async fn register_device(mut event: PayloadEvent<crate::shared::DeviceInfo>) -> Result<(), anyhow::Error> {
	if let Ok(profiles) = get_device_profiles(&event.payload.id) {
		let mut profile_stores = crate::store::profiles::PROFILE_STORES.write().await;
		for profile in profiles {
			// Initialise the store for each of the device's profiles.
			if let Err(e) = profile_stores.get_profile_store_mut(&event.payload, &profile).await {
				log::error!("{}", e);
			}
		}
	}

	event.payload.plugin = crate::shared::BUILTIN_PLUGIN.to_owned();
	DEVICES.insert(event.payload.id.clone(), event.payload.clone());
	let _ = crate::device_sleep::apply_initial_device_sleep(&event.payload.id).await;
	crate::events::frontend::update_devices().await;

	let mut locks = crate::store::profiles::acquire_locks_mut().await;
	let selected_profile = locks.device_stores.get_selected_profile(&event.payload.id)?;

	// Dials used to live in the profile beside the rectangle above them. Lift any existing setup to
	// where dials live now - once, and without touching the profile itself.
	let sliders = locks
		.profile_stores
		.get_profile_store(&DEVICES.get(&event.payload.id).unwrap(), &selected_profile)?
		.value
		.sliders
		.clone();
	if let Err(error) = locks.device_stores.seed_dials(&event.payload.id, event.payload.encoders as usize, &sliders) {
		log::error!("Failed to seed dials for device {}: {error}", event.payload.id);
	}

	Ok(())
}

pub async fn deregister_device(event: PayloadEvent<String>) -> Result<(), anyhow::Error> {
	if !DEVICES.contains_key(&event.payload) {
		return Ok(());
	}

	let mut locks = crate::store::profiles::acquire_locks_mut().await;

	// Flush any pending profile writes before removing the device.
	if let Err(error) = crate::store::profiles::save_profile_now(&event.payload, &mut locks).await {
		log::error!("Failed to flush profile for device {}: {error}", event.payload);
	}

	if let Ok(profiles) = get_device_profiles(&event.payload) {
		for profile in profiles {
			locks.profile_stores.remove_profile(&event.payload, &profile);
		}
	}

	drop(locks);

	DEVICES.remove(&event.payload);
	// Nothing to animate on hardware that has gone. Left running, the player keeps compositing
	// frames for a deck that cannot take them - and lands them on the faces of the next deck to
	// come back under the same id, over stills that have not been pushed yet.
	crate::animation::stop(&event.payload).await;
	crate::device_sleep::deregister_device(&event.payload);
	crate::events::frontend::update_devices().await;

	Ok(())
}

#[derive(Deserialize)]
pub struct PressPayload {
	pub device: String,
	pub position: u8,
}

pub async fn key_down(event: PayloadEvent<PressPayload>) -> Result<(), anyhow::Error> {
	if crate::device_sleep::note_activity(&event.payload.device).await.unwrap_or(false) {
		return Ok(());
	}
	crate::events::outbound::keypad::key_down(&event.payload.device, event.payload.position).await
}

pub async fn key_up(event: PayloadEvent<PressPayload>) -> Result<(), anyhow::Error> {
	if crate::device_sleep::note_activity(&event.payload.device).await.unwrap_or(false) {
		return Ok(());
	}
	crate::events::outbound::keypad::key_up(&event.payload.device, event.payload.position).await
}

#[derive(Deserialize)]
pub struct TicksPayload {
	pub device: String,
	pub position: u8,
	pub ticks: i16,
}

pub async fn encoder_change(event: PayloadEvent<TicksPayload>) -> Result<(), anyhow::Error> {
	if crate::device_sleep::note_activity(&event.payload.device).await.unwrap_or(false) {
		return Ok(());
	}
	crate::events::outbound::encoder::dial_rotate(&event.payload.device, event.payload.position, event.payload.ticks).await
}

pub async fn encoder_down(event: PayloadEvent<PressPayload>) -> Result<(), anyhow::Error> {
	if crate::device_sleep::note_activity(&event.payload.device).await.unwrap_or(false) {
		return Ok(());
	}
	crate::events::outbound::encoder::dial_press(&event.payload.device, "dialDown", event.payload.position).await
}

pub async fn encoder_up(event: PayloadEvent<PressPayload>) -> Result<(), anyhow::Error> {
	if crate::device_sleep::note_activity(&event.payload.device).await.unwrap_or(false) {
		return Ok(());
	}
	crate::events::outbound::encoder::dial_press(&event.payload.device, "dialUp", event.payload.position).await
}

#[derive(Deserialize)]
pub struct TouchscreenPressPayload {
	pub device: String,
	pub position: u8,
	pub x: u16,
	pub y: u16,
	#[serde(default)]
	pub hold: bool,
}

pub async fn touchscreen_press(event: PayloadEvent<TouchscreenPressPayload>) -> Result<(), anyhow::Error> {
	if crate::device_sleep::note_activity(&event.payload.device).await.unwrap_or(false) {
		return Ok(());
	}
	crate::events::outbound::encoder::touch_tap(&event.payload.device, event.payload.position, event.payload.x, event.payload.y, event.payload.hold).await
}

