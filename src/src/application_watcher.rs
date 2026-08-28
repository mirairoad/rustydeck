//! Switching profiles to follow the focused application.
//!
//! A second half of this used to watch processes so plugins could be told when an application
//! they cared about launched or exited; there are no plugins, so only the profile switch remains.

use crate::store::{NotProfile, Store};

use std::collections::HashMap;
use std::sync::LazyLock;

use active_win_pos_rs::get_active_window;
use tokio::sync::RwLock;

use crate::frontend_events::{FrontendEvent, emit};

pub type ApplicationProfiles = HashMap<String, HashMap<String, String>>;
impl NotProfile for ApplicationProfiles {}

pub static APPLICATION_PROFILES: LazyLock<RwLock<Store<ApplicationProfiles>>> = LazyLock::new(|| RwLock::new(Store::new("applications", &crate::shared::config_dir(), HashMap::new()).unwrap()));


pub fn init_application_watcher() {
	tokio::spawn(async move {
		let mut previous = String::new();
		loop {
			let app_name = match get_active_window() {
				Ok(win) => win.app_name,
				Err(_) => String::new(),
			};

			if app_name != previous {
				let application_profiles = &APPLICATION_PROFILES.read().await.value;
				let application = application_profiles.get(&app_name);
				let default = application_profiles.get("opendeck_default");
				for value in crate::shared::DEVICES.iter() {
					let device = value.key();
					let Some(profile) = application.and_then(|d| d.get(device)).or(default.and_then(|d| d.get(device))) else {
						continue;
					};
					if crate::store::profiles::DEVICE_STORES.write().await.get_selected_profile(device).ok().as_ref() == Some(profile) {
						continue;
					}
					emit(FrontendEvent::SwitchProfile);
				}
				previous = app_name;
			}

			tokio::time::sleep(std::time::Duration::from_millis(250)).await;
		}
	});
}
