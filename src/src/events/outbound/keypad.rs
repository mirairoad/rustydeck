//! What a key press does.
//!
//! Every action is implemented in-process, so this dispatches on the action's UUID rather than
//! forwarding the press to a plugin.

use crate::shared::{ActionInstance, Context};
use crate::store::profiles::{acquire_locks_mut, get_slot_mut, mark_profile_stale};

use std::sync::LazyLock;

use dashmap::DashMap;

static KEY_DOWN_TARGETS: LazyLock<DashMap<(String, u8), Context>> = LazyLock::new(DashMap::new);

/// One of an instance's stored commands, empty when unset.
pub fn command_setting(instance: &ActionInstance, key: &str) -> String {
	instance.settings.get(key).and_then(|value| value.as_str()).unwrap_or_default().to_owned()
}

/// How far a page action steps, if this is one.
///
/// The profile locks must be released before stepping: switching a page re-acquires them, so
/// stepping inline would deadlock.
fn page_step_for(uuid: &str) -> Option<i32> {
	match uuid {
		crate::shared::PAGE_LEFT_UUID => Some(-1),
		crate::shared::PAGE_RIGHT_UUID => Some(1),
		_ => None,
	}
}

pub async fn key_down(device: &str, key: u8) -> Result<(), anyhow::Error> {
	let mut locks = acquire_locks_mut().await;
	let selected_profile = locks.device_stores.get_selected_profile(device)?;
	let context = Context {
		device: device.to_owned(),
		profile: selected_profile.to_owned(),
		controller: "Keypad".to_owned(),
		position: key,
	};

	KEY_DOWN_TARGETS.insert((device.to_owned(), key), context.clone());

	let Some(instance) = get_slot_mut(&context, &mut locks).await? else { return Ok(()) };

	// The press effect goes first and does not block: it is what the key looks like while whatever
	// it does happens, so waiting for the command would defeat the point of it.
	crate::animation::press(context.clone(), instance);

	if let Some(delta) = page_step_for(&instance.action.uuid) {
		drop(locks);
		let device = device.to_owned();
		crate::spawn(async move {
			if let Err(error) = crate::pages::step(&device, delta).await {
				log::error!("Failed to change page: {error}");
			}
		});
		return Ok(());
	}

	if instance.action.uuid == crate::shared::RUN_COMMAND_UUID {
		let command = command_setting(instance, "down");
		drop(locks);
		crate::system_actions::run_shell(command);
	}

	Ok(())
}

pub async fn key_up(device: &str, key: u8) -> Result<(), anyhow::Error> {
	let mut locks = acquire_locks_mut().await;
	let selected_profile = locks.device_stores.get_selected_profile(device)?;
	let context = Context {
		device: device.to_owned(),
		profile: selected_profile.to_owned(),
		controller: "Keypad".to_owned(),
		position: key,
	};


	// Only release the key that was pressed: a press and release on different slots (the page
	// changed under the finger) is not a completed press.
	let Some((_, expected_context)) = KEY_DOWN_TARGETS.remove(&(device.to_owned(), key)) else {
		return Ok(());
	};
	if context != expected_context {
		return Ok(());
	}

	let Some(instance) = get_slot_mut(&context, &mut locks).await? else { return Ok(()) };

	// The page actions were fully handled on key-down.
	if page_step_for(&instance.action.uuid).is_some() {
		return Ok(());
	}

	// A two-state action toggles its face on release, so the key shows what it will do next.
	if instance.states.len() == 2 && !instance.action.disable_automatic_states {
		instance.current_state = (instance.current_state + 1) % (instance.states.len() as u16);
	}

	if instance.action.uuid == crate::shared::RUN_COMMAND_UUID {
		crate::system_actions::run_shell(command_setting(instance, "up"));
	}

	mark_profile_stale(device, &mut locks).await?;

	Ok(())
}
