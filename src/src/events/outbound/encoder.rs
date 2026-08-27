use super::{Coordinates, send_to_plugin};

use crate::shared::{ActionContext, DEVICES};
use crate::store::profiles::{DialConfig, acquire_locks_mut, get_instance_mut};

use serde::Serialize;

/// The dial's configuration, if it has one.
///
/// Dials are device-scoped: the knob is a fixed control, so it keeps doing the same thing whichever
/// page is showing. That is why nothing here touches the profile - the rectangle above the dial is
/// the page-scoped half, and it owns the artwork and the tap (see [`touch_tap`]).
async fn dial_at(device: &str, index: u8) -> Option<DialConfig> {
	let encoders = DEVICES.get(device)?.encoders as usize;
	let mut locks = acquire_locks_mut().await;
	locks.device_stores.get_dial(device, encoders, index)
}

pub async fn dial_rotate(device: &str, index: u8, ticks: i16) -> Result<(), anyhow::Error> {
	let Some(config) = dial_at(device, index).await else { return Ok(()) };
	crate::system_actions::dial_rotate(&config, device, index, ticks);
	Ok(())
}

pub async fn dial_press(device: &str, event: &'static str, index: u8) -> Result<(), anyhow::Error> {
	// Act on the way down only - handling both edges fires twice per physical press.
	if event != "dialDown" {
		return Ok(());
	}
	let Some(config) = dial_at(device, index).await else { return Ok(()) };
	crate::system_actions::dial_press(&config, device, index);
	Ok(())
}

/// Page stepping for the rectangle above a dial.
///
/// Returns the step if the instance is a page command. As on the keypad, the profile locks must be
/// released before stepping - switching a page re-acquires them.
fn page_step_for(uuid: &str) -> Option<i32> {
	match uuid {
		crate::shared::PAGE_LEFT_UUID => Some(-1),
		crate::shared::PAGE_RIGHT_UUID => Some(1),
		_ => None,
	}
}

fn spawn_page_step(device: &str, delta: i32) {
	let device = device.to_owned();
	crate::spawn(async move {
		if let Err(error) = crate::pages::step(&device, delta).await {
			log::error!("Failed to change page: {error}");
		}
	});
}

#[derive(Serialize)]
#[allow(non_snake_case)]
struct TouchTapPayload {
	controller: &'static str,
	settings: serde_json::Value,
	coordinates: Coordinates,
	tapPos: (u16, u16),
	hold: bool,
}

#[derive(Serialize)]
struct TouchTapEvent {
	event: &'static str,
	action: String,
	context: ActionContext,
	device: String,
	payload: TouchTapPayload,
}

pub async fn touch_tap(device: &str, index: u8, x: u16, y: u16, hold: bool) -> Result<(), anyhow::Error> {
	let mut locks = acquire_locks_mut().await;
	let selected_profile = locks.device_stores.get_selected_profile(device)?;
	let context = ActionContext {
		device: device.to_owned(),
		profile: selected_profile.to_owned(),
		controller: "Encoder".to_owned(),
		position: index,
		index: 0,
	};
	let Some(instance) = get_instance_mut(&context, &mut locks).await? else { return Ok(()) };

	if let Some(delta) = page_step_for(&instance.action.uuid) {
		drop(locks);
		spawn_page_step(device, delta);
		return Ok(());
	}

	send_to_plugin(
		&instance.action.plugin,
		&TouchTapEvent {
			event: "touchTap",
			action: instance.action.uuid.clone(),
			context: instance.context.clone(),
			device: instance.context.device.clone(),
			payload: TouchTapPayload {
				controller: "Encoder",
				settings: instance.settings.clone(),
				coordinates: Coordinates { row: 0, column: index },
				tapPos: (x, y),
				hold,
			},
		},
	)
	.await
}
