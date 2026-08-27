// This whole module is the direct-call API surface that replaces the old Tauri IPC commands.
// Nothing calls most of it yet - the GPUI shell that will is milestone 3.
#![allow(dead_code)]

pub mod instances;
pub mod plugins;
pub mod profiles;
pub mod property_inspector;
pub mod settings;

use crate::frontend_events::{FrontendEvent, emit};
use crate::shared::{CATEGORIES, Category, DEVICES, DeviceInfo};

use std::collections::HashMap;

#[derive(Debug, serde_with::SerializeDisplay, serde::Deserialize)]
pub struct Error {
	pub description: String,
}

impl std::fmt::Display for Error {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}", self.description)
	}
}
impl std::error::Error for Error {}

impl Error {
	fn new(description: String) -> Self {
		log::error!("{}", description);
		Self { description }
	}
}

impl From<serde_json::Error> for Error {
	fn from(error: serde_json::Error) -> Self {
		Self::new(error.to_string())
	}
}

impl From<std::io::Error> for Error {
	fn from(error: std::io::Error) -> Self {
		Self::new(error.to_string())
	}
}

impl From<anyhow::Error> for Error {
	fn from(error: anyhow::Error) -> Self {
		Self::new(error.to_string())
	}
}

pub async fn restart() {
	crate::restart_app();
}

pub async fn get_devices() -> dashmap::DashMap<String, DeviceInfo> {
	DEVICES.clone()
}

pub async fn update_devices() {
	emit(FrontendEvent::Devices(DEVICES.clone()));
}

pub async fn get_port_base() -> u16 {
	*crate::plugins::PORT_BASE
}

pub async fn get_categories() -> HashMap<String, Category> {
	CATEGORIES.read().await.clone()
}

pub async fn get_localisations(locale: &str) -> Result<HashMap<String, serde_json::Value>, Error> {
	let mut localisations: HashMap<String, serde_json::Value> = HashMap::new();

	let mut entries = match tokio::fs::read_dir(&crate::shared::config_dir().join("plugins")).await {
		Ok(entries) => entries,
		Err(error) => return Err(anyhow::Error::from(error).into()),
	};

	while let Ok(Some(entry)) = entries.next_entry().await {
		let path = match entry.metadata().await.unwrap().is_symlink() {
			true => tokio::fs::read_link(entry.path()).await.unwrap(),
			false => entry.path(),
		};
		let metadata = tokio::fs::metadata(&path).await.unwrap();
		if metadata.is_dir() {
			let Ok(locale) = tokio::fs::read(path.join(format!("{locale}.json"))).await else { continue };
			let Ok(locale): Result<serde_json::Value, _> = serde_json::from_slice(&locale) else {
				continue;
			};
			localisations.insert(path.file_name().unwrap().to_str().unwrap().to_owned(), locale);
		}
	}

	Ok(localisations)
}

pub async fn get_applications() -> Vec<String> {
	crate::application_watcher::APPLICATIONS.read().await.clone()
}

pub async fn get_application_profiles() -> crate::application_watcher::ApplicationProfiles {
	crate::application_watcher::APPLICATION_PROFILES.read().await.value.clone()
}

pub async fn set_application_profiles(value: crate::application_watcher::ApplicationProfiles) -> Result<(), Error> {
	let mut store = crate::application_watcher::APPLICATION_PROFILES.write().await;
	store.value = value;
	Ok(store.save()?)
}

pub fn get_fonts() -> Vec<String> {
	font_kit::source::SystemSource::new().all_families().unwrap_or_default()
}
