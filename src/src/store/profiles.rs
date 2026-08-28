use super::Store;

use crate::shared::{ActionInstance, DEVICES, DeviceInfo, Profile, config_dir, copy_dir, initialise_encoder_layout};

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::LazyLock;

use anyhow::{Context, anyhow};
use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

pub struct ProfileStores {
	stores: HashMap<String, Store<Profile>>,
}

impl ProfileStores {
	/// Forget every cached profile, so the next read comes from disk.
	///
	/// For a restore: these hold the configuration that has just been replaced, and saving one
	/// would write it back over the restored file.
	pub fn clear(&mut self) {
		self.stores.clear();
	}

	fn canonical_id(device: &str, id: &str) -> String {
		if cfg!(target_os = "windows") {
			PathBuf::from(device).join(id.replace('/', "\\")).to_str().unwrap().to_owned()
		} else {
			PathBuf::from(device).join(id).to_str().unwrap().to_owned()
		}
	}

	pub fn get_profile_store(&self, device: &DeviceInfo, id: &str) -> Result<&Store<Profile>, anyhow::Error> {
		self.stores.get(&Self::canonical_id(&device.id, id)).ok_or_else(|| anyhow!("profile not found"))
	}

	pub async fn get_profile_store_mut(&mut self, device: &DeviceInfo, id: &str) -> Result<&mut Store<Profile>, anyhow::Error> {
		let canonical_id = Self::canonical_id(&device.id, id);
		if self.stores.contains_key(&canonical_id) {
			Ok(self.stores.get_mut(&canonical_id).unwrap())
		} else {
			let default = Profile {
				id: id.to_owned(),
				keys: Vec::new(),
				sliders: Vec::new(),
				infobars: Vec::new(),

				stale: false,
			};

			let mut store = Store::new(&canonical_id, &config_dir().join("profiles"), default).context(format!("Failed to create store for profile {}", canonical_id))?;
			store.value.keys.resize((device.rows * device.columns + device.touchpoints) as usize, None);
			store.value.sliders.resize(device.encoders as usize, None);
			store.value.infobars.resize(device.infobars as usize, None);

			let categories = crate::shared::CATEGORIES.read().await;
			let actions = categories.values().flat_map(|v| v.actions.iter()).collect::<Vec<_>>();
			// Commands used to be run by a bundled plugin process; they are run in-process now.
			// Remap instances that still name the old action so existing keys keep working - and
			// so they survive the check below, which would otherwise see a plugin that is gone.
			let run_command = actions.iter().find(|action| action.uuid == crate::shared::RUN_COMMAND_UUID).map(|action| (*action).clone());
			if let Some(run_command) = run_command {
				for slot in store.value.keys.iter_mut().chain(store.value.sliders.iter_mut()).chain(store.value.infobars.iter_mut()) {
					if let Some(instance) = slot
						&& instance.action.uuid == crate::shared::LEGACY_RUN_COMMAND_UUID
					{
						// Only the identity changes; the settings carrying the commands are kept.
						instance.action = run_command.clone();
					}
				}
			}

			// Every action is implemented in the app now, so a slot is worth keeping exactly when
			// its action is one we still register. This used to also test for a plugin directory on
			// disk, which silently deleted every first-party action from every profile.
			let keep_instance = |instance: &ActionInstance| -> bool { actions.iter().any(|v| v.uuid == instance.action.uuid) };
			for slot in store.value.keys.iter_mut().chain(store.value.sliders.iter_mut()).chain(store.value.infobars.iter_mut()) {
				if let Some(instance) = slot {
					if !keep_instance(instance) {
						*slot = None;
					} else if let Some(children) = &mut instance.children {
						children.retain_mut(|child| keep_instance(child));
					}
				}
			}

			// We need to populate instances from a profile without encoders or without parsed layouts with them
			for instance in store.value.sliders.iter_mut().flatten() {
				// Populate encoder data using the manifest action if missing
				if instance.action.encoder.is_none()
					&& let Some(action) = actions.iter().find(|a| a.uuid == *instance.action.uuid)
				{
					instance.action.encoder = action.encoder.clone();
				}

				// Load encoder layout if not yet parsed
				let _ = initialise_encoder_layout(&mut instance.action, None);
			}

			store.save()?;

			self.stores.insert(canonical_id.clone(), store);
			Ok(self.stores.get_mut(&canonical_id).unwrap())
		}
	}

	pub fn remove_profile(&mut self, device: &str, id: &str) {
		self.stores.remove(&Self::canonical_id(device, id));
	}

	pub fn delete_profile(&mut self, device: &str, id: &str) {
		self.remove_profile(device, id);
		let config_dir = config_dir();
		#[cfg(target_os = "windows")]
		let id = &id.replace('/', "\\");
		let path = config_dir.join("profiles").join(device).join(format!("{id}.json"));
		let _ = fs::remove_file(&path);
		// This is safe as `remove_dir` errors if the directory is not empty.
		let _ = fs::remove_dir(path.parent().unwrap());
		let images_path = config_dir.join("images").join(device).join(id);
		let _ = fs::remove_dir_all(images_path);
	}

	pub async fn rename_profile(&mut self, device: &DeviceInfo, old_id: &str, new_id: &str, retain: bool) -> Result<(), anyhow::Error> {
		if !retain {
			// Remove from the store but don't delete the file
			self.remove_profile(&device.id, old_id);
		}

		let config_dir = config_dir();

		// Construct old and new paths (handling Windows path separators)
		#[cfg(target_os = "windows")]
		let old_path_id = old_id.replace('/', "\\");
		#[cfg(not(target_os = "windows"))]
		let old_path_id = old_id;

		#[cfg(target_os = "windows")]
		let new_path_id = new_id.replace('/', "\\");
		#[cfg(not(target_os = "windows"))]
		let new_path_id = new_id;

		let old_path = config_dir.join("profiles").join(&device.id).join(format!("{}.json", old_path_id));
		let new_path = config_dir.join("profiles").join(&device.id).join(format!("{}.json", new_path_id));

		// Create parent directory for new path if it doesn't exist
		if let Some(parent) = new_path.parent() {
			fs::create_dir_all(parent)?;
		}

		// Rename the profile file
		if !retain {
			fs::rename(&old_path, &new_path)?;

			// Clean up empty old directory if profile was in a folder
			if let Some(parent) = old_path.parent() {
				// This is safe as `remove_dir` errors if the directory is not empty.
				let _ = fs::remove_dir(parent);
			}
		} else {
			fs::copy(&old_path, &new_path)?;
		}

		// Rename images directory if it exists
		let old_images_path = config_dir.join("images").join(&device.id).join(old_path_id);
		let new_images_path = config_dir.join("images").join(&device.id).join(new_path_id);

		if old_images_path.exists() {
			if let Some(parent) = new_images_path.parent() {
				fs::create_dir_all(parent)?;
			}

			if !retain {
				fs::rename(&old_images_path, &new_images_path)?;

				// Clean up empty old images directory
				if let Some(parent) = old_images_path.parent() {
					// This is safe as `remove_dir` errors if the directory is not empty.
					let _ = fs::remove_dir(parent);
				}
			} else {
				copy_dir(&old_images_path, &new_images_path)?;
			}
		}

		// Reload the new profile
		self.get_profile_store_mut(device, new_id).await?;

		Ok(())
	}

}

/// What one physical dial does.
///
/// Dials are device-scoped rather than page-scoped: the knob is a fixed control, so it keeps doing
/// the same thing whichever page is showing. The rectangle above it is the page-scoped half - it
/// owns the artwork and the tap, and lives in the profile like any other slot.
///
/// Deliberately not an [`ActionInstance`]: a dial runs either a first-party action or the user's
/// own shell commands, both of which the app executes itself, so none of the plugin instance
/// machinery (contexts, `willAppear`, settings round-trips) applies.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DialConfig {
	/// A first-party action implemented in `system_actions`, named by its UUID.
	System { uuid: String },
	/// Shell commands run directly by the app: anticlockwise, clockwise, and a press.
	///
	/// `name` is what the knob is captioned with. It defaults in from configs written before dials
	/// could be named, and is allowed to stay blank - the UI falls back to "Custom", which is what
	/// every custom dial was called before.
	Custom {
		#[serde(default)]
		name: String,
		left: String,
		right: String,
		centre: String,
	},
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceConfig {
	pub selected_profile: String,
	/// One entry per encoder, `None` where the dial is unconfigured.
	pub dials: Vec<Option<DialConfig>>,
}

impl Default for DeviceConfig {
	fn default() -> Self {
		Self {
			selected_profile: "Default".to_owned(),
			dials: Vec::new(),
		}
	}
}

impl super::NotProfile for DeviceConfig {}

pub struct DeviceStores {
	stores: HashMap<String, Store<DeviceConfig>>,
}

impl DeviceStores {
	/// Forget every cached device configuration - see [`ProfileStores::clear`].
	pub fn clear(&mut self) {
		self.stores.clear();
	}

	pub fn get_selected_profile(&mut self, device: &str) -> Result<String, anyhow::Error> {
		if !self.stores.contains_key(device) {
			let default = DeviceConfig::default();

			let store = Store::new(device, &config_dir().join("profiles"), default).context(format!("Failed to create store for device config {}", device))?;
			store.save()?;

			self.stores.insert(device.to_owned(), store);
		}

		let from_store = &self.stores.get(device).unwrap().value.selected_profile;
		let all = get_device_profiles(device)?;
		if all.contains(from_store) { Ok(from_store.clone()) } else { Ok(all.first().unwrap().clone()) }
	}

	/// Ensure the device's store exists and its dial array is the right length, returning it.
	fn dials_mut(&mut self, device: &str, encoders: usize) -> Result<&mut Store<DeviceConfig>, anyhow::Error> {
		if !self.stores.contains_key(device) {
			let store = Store::new(device, &config_dir().join("profiles"), DeviceConfig::default())
				.context(format!("Failed to create store for device config {device}"))?;
			self.stores.insert(device.to_owned(), store);
		}

		let store = self.stores.get_mut(device).unwrap();
		if store.value.dials.len() != encoders {
			store.value.dials.resize_with(encoders, || None);
		}
		Ok(store)
	}

	/// What every dial on this device is set to.
	pub fn get_dials(&mut self, device: &str, encoders: usize) -> Result<Vec<Option<DialConfig>>, anyhow::Error> {
		Ok(self.dials_mut(device, encoders)?.value.dials.clone())
	}

	/// What one dial is set to, or `None` if it is unconfigured or out of range.
	pub fn get_dial(&mut self, device: &str, encoders: usize, index: u8) -> Option<DialConfig> {
		self.dials_mut(device, encoders).ok()?.value.dials.get(index as usize).cloned().flatten()
	}

	pub fn set_dial(&mut self, device: &str, encoders: usize, index: u8, config: Option<DialConfig>) -> Result<(), anyhow::Error> {
		let store = self.dials_mut(device, encoders)?;
		let Some(slot) = store.value.dials.get_mut(index as usize) else {
			return Err(anyhow!("dial {index} is out of range for device {device}"));
		};
		*slot = config;
		store.save()
	}

	/// Exchange what two dials do, so a layout can be rearranged without retyping it.
	///
	/// Moving a dial is always a swap: a dial is identified by its position on the device, so the
	/// destination's config has to go somewhere and the source is the only place free to take it.
	/// Dragging onto an unconfigured dial is therefore how a dial gets moved rather than exchanged.
	pub fn swap_dials(&mut self, device: &str, encoders: usize, a: u8, b: u8) -> Result<(), anyhow::Error> {
		if a == b {
			return Ok(());
		}

		let store = self.dials_mut(device, encoders)?;
		let count = store.value.dials.len();
		if a as usize >= count || b as usize >= count {
			return Err(anyhow!("dial {a} or {b} is out of range for device {device}"));
		}

		store.value.dials.swap(a as usize, b as usize);
		store.save()
	}

	/// Copy the rotate and press commands off a page's encoder slots onto the device's dials, once.
	///
	/// Dials used to live in the profile alongside the rectangle above them, so an existing setup
	/// has its commands there. This lifts them to where dials now live, and deliberately changes
	/// nothing in the profile: the slot keeps its artwork and its tap, and its now-unread `down`
	/// and `rotate_*` keys are left alone rather than rewritten.
	///
	/// Only runs while every dial is still unconfigured, so it cannot overwrite a real setup.
	pub fn seed_dials(&mut self, device: &str, encoders: usize, sliders: &[Option<ActionInstance>]) -> Result<(), anyhow::Error> {
		let store = self.dials_mut(device, encoders)?;
		if store.value.dials.iter().any(|dial| dial.is_some()) {
			return Ok(());
		}

		let command = |instance: &ActionInstance, key: &str| {
			instance.settings.get(key).and_then(|value| value.as_str()).unwrap_or_default().to_owned()
		};

		let mut seeded = false;
		for (index, slot) in sliders.iter().enumerate().take(encoders) {
			let Some(instance) = slot else { continue };

			let dial = if crate::system_actions::is_system_action(&instance.action.uuid) {
				DialConfig::System {
					uuid: instance.action.uuid.clone(),
				}
			} else {
				let (left, right, centre) = (command(instance, "rotate_left"), command(instance, "rotate_right"), command(instance, "down"));
				if left.is_empty() && right.is_empty() && centre.is_empty() {
					continue;
				}
				DialConfig::Custom {
					name: String::new(),
					left,
					right,
					centre,
				}
			};

			store.value.dials[index] = Some(dial);
			seeded = true;
		}

		if seeded { store.save() } else { Ok(()) }
	}

	pub fn set_selected_profile(&mut self, device: &str, id: String) -> Result<(), anyhow::Error> {
		if self.stores.contains_key(device) {
			let store = self.stores.get_mut(device).unwrap();
			store.value.selected_profile = id;
			store.save()?;
		} else {
			let default = DeviceConfig {
				selected_profile: id,
				..Default::default()
			};

			let store = Store::new(device, &config_dir().join("profiles"), default).context(format!("Failed to create store for device config {}", device))?;
			store.save()?;

			self.stores.insert(device.to_owned(), store);
		}
		Ok(())
	}
}

pub fn get_device_profiles(device: &str) -> Result<Vec<String>, anyhow::Error> {
	let mut profiles: Vec<String> = vec![];

	let device_path = config_dir().join("profiles").join(device);
	fs::create_dir_all(&device_path)?;
	let entries = fs::read_dir(device_path)?;

	for entry in entries.flatten() {
		if entry.metadata()?.is_file() {
			let mut id = entry.file_name().to_string_lossy().into_owned();
			if id.ends_with(".json") {
				id.truncate(id.len() - 5);
			} else if id.ends_with(".json.bak") {
				id.truncate(id.len() - 9);
			} else if id.ends_with(".json.temp") {
				id.truncate(id.len() - 10);
			} else {
				continue;
			}
			profiles.push(id);
		} else if entry.metadata()?.is_dir() {
			let entries = fs::read_dir(entry.path())?;
			for subentry in entries.flatten() {
				if subentry.metadata()?.is_file() {
					let mut id = format!("{}/{}", entry.file_name().to_string_lossy(), subentry.file_name().to_string_lossy());
					if id.ends_with(".json") {
						id.truncate(id.len() - 5);
					} else if id.ends_with(".json.bak") {
						id.truncate(id.len() - 9);
					} else if id.ends_with(".json.temp") {
						id.truncate(id.len() - 10);
					} else {
						continue;
					}
					profiles.push(id);
				}
			}
		}
	}

	if profiles.is_empty() {
		profiles.push("Default".to_owned());
	}

	Ok(profiles)
}

/// A singleton object to contain all active Store instances that hold a profile.
pub static PROFILE_STORES: LazyLock<RwLock<ProfileStores>> = LazyLock::new(|| RwLock::new(ProfileStores { stores: HashMap::new() }));

/// A singleton object to manage Store instances for device configurations.
pub static DEVICE_STORES: LazyLock<RwLock<DeviceStores>> = LazyLock::new(|| RwLock::new(DeviceStores { stores: HashMap::new() }));

/// Read every connected device's profiles back into the cache.
///
/// A restore replaces the files these caches were built from, so they are dropped as part of the
/// swap - but dropping them is not enough on its own. [`ProfileStores::get_profile_store`] is a
/// read-only accessor that returns "profile not found" rather than creating what is missing, so
/// every read path stays broken until something primes the cache. Registration is what normally
/// does that, and after a restore there is no registration to wait for: the device never went
/// away.
///
/// Device stores need no equivalent - [`DeviceStores::get_selected_profile`] and
/// [`DeviceStores::dials_mut`] both create theirs on demand.
pub async fn prime_stores() {
	// Collected first, so no DashMap guard is held across an await.
	let devices: Vec<DeviceInfo> = DEVICES.iter().map(|entry| entry.value().clone()).collect();

	for device in devices {
		let Ok(profiles) = get_device_profiles(&device.id) else { continue };
		let mut profile_stores = PROFILE_STORES.write().await;
		for profile in profiles {
			if let Err(error) = profile_stores.get_profile_store_mut(&device, &profile).await {
				log::error!("Failed to read profile {profile} of {} back: {error}", device.id);
			}
		}
	}
}

pub struct Locks<'a> {
	#[allow(dead_code)]
	pub device_stores: RwLockReadGuard<'a, DeviceStores>,
	pub profile_stores: RwLockReadGuard<'a, ProfileStores>,
}

pub async fn acquire_locks() -> Locks<'static> {
	let device_stores = DEVICE_STORES.read().await;
	let profile_stores = PROFILE_STORES.read().await;
	Locks { device_stores, profile_stores }
}

pub struct LocksMut<'a> {
	pub device_stores: RwLockWriteGuard<'a, DeviceStores>,
	pub profile_stores: RwLockWriteGuard<'a, ProfileStores>,
}

pub async fn acquire_locks_mut() -> LocksMut<'static> {
	let device_stores = DEVICE_STORES.write().await;
	let profile_stores = PROFILE_STORES.write().await;
	LocksMut { device_stores, profile_stores }
}

pub async fn get_slot<'a>(context: &crate::shared::Context, locks: &'a Locks<'_>) -> Result<&'a Option<crate::shared::ActionInstance>, anyhow::Error> {
	let device = DEVICES.get(&context.device).ok_or_else(|| anyhow!("device not found"))?;
	let store = locks.profile_stores.get_profile_store(&device, &context.profile)?;

	let configured = match &context.controller[..] {
		"Encoder" => store.value.sliders.get(context.position as usize).ok_or_else(|| anyhow!("index out of bounds"))?,
		"Infobar" => store.value.infobars.get(context.position as usize).ok_or_else(|| anyhow!("index out of bounds"))?,
		_ => store.value.keys.get(context.position as usize).ok_or_else(|| anyhow!("index out of bounds"))?,
	};

	Ok(configured)
}

pub async fn get_slot_mut<'a>(context: &crate::shared::Context, locks: &'a mut LocksMut<'_>) -> Result<&'a mut Option<crate::shared::ActionInstance>, anyhow::Error> {
	let device = DEVICES.get(&context.device).ok_or_else(|| anyhow!("device not found"))?;
	let store = locks.profile_stores.get_profile_store_mut(&device, &context.profile).await?;

	let configured = match &context.controller[..] {
		"Encoder" => store.value.sliders.get_mut(context.position as usize).ok_or_else(|| anyhow!("index out of bounds"))?,
		"Infobar" => store.value.infobars.get_mut(context.position as usize).ok_or_else(|| anyhow!("index out of bounds"))?,
		_ => store.value.keys.get_mut(context.position as usize).ok_or_else(|| anyhow!("index out of bounds"))?,
	};

	Ok(configured)
}


pub async fn get_instance_mut<'a>(context: &crate::shared::ActionContext, locks: &'a mut LocksMut<'_>) -> Result<Option<&'a mut crate::shared::ActionInstance>, anyhow::Error> {
	let slot = get_slot_mut(&(context.into()), locks).await?;
	if let Some(instance) = slot {
		if instance.context == *context {
			return Ok(Some(instance));
		} else if let Some(children) = &mut instance.children {
			for child in children {
				if child.context == *context {
					return Ok(Some(child));
				}
			}
		}
	}
	Ok(None)
}

pub async fn mark_profile_stale(device_id: &str, locks: &mut LocksMut<'_>) -> Result<(), anyhow::Error> {
	let selected_profile = locks.device_stores.get_selected_profile(device_id)?;
	let device = DEVICES.get(device_id).ok_or_else(|| anyhow!("device not found"))?;
	let store = locks.profile_stores.get_profile_store_mut(&device, &selected_profile).await?;
	store.value.stale = true;
	Ok(())
}

pub async fn save_profile_now(device_id: &str, locks: &mut LocksMut<'_>) -> Result<(), anyhow::Error> {
	let selected_profile = locks.device_stores.get_selected_profile(device_id)?;
	let device = DEVICES.get(device_id).ok_or_else(|| anyhow!("device not found"))?;
	let store = locks.profile_stores.get_profile_store_mut(&device, &selected_profile).await?;

	store.save()?;
	store.value.stale = false;

	Ok(())
}

pub async fn flush_stale_profiles() -> Result<(), anyhow::Error> {
	let mut locks = acquire_locks_mut().await;
	for store in locks.profile_stores.stores.values_mut() {
		if store.value.stale {
			store.save()?;
			store.value.stale = false;
		}
	}
	Ok(())
}
