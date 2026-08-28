// This whole module is the direct-call API surface that replaces the old Tauri IPC commands.
// Nothing calls most of it yet - the GPUI shell that will is milestone 3.
#![allow(dead_code)]

pub mod instances;
pub mod profiles;

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
	emit(FrontendEvent::Devices);
}


pub async fn get_categories() -> HashMap<String, Category> {
	CATEGORIES.read().await.clone()
}





