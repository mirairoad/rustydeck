use super::{ContextEvent, PayloadEvent};

use crate::frontend_events::{FrontendEvent, emit};

use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct OpenUrlEvent {
	pub url: String,
}

pub async fn open_url(event: PayloadEvent<OpenUrlEvent>) -> Result<(), anyhow::Error> {
	log::debug!("Opening URL {}", event.payload.url);
	open::that_detached(event.payload.url)?;
	Ok(())
}

#[derive(Deserialize)]
pub struct LogMessageEvent {
	pub message: String,
}

pub async fn log_message(uuid: Option<&str>, mut event: PayloadEvent<LogMessageEvent>) -> Result<(), anyhow::Error> {
	if let Some(uuid) = uuid
		&& let Ok(manifest) = crate::plugins::manifest::read_manifest(&crate::shared::config_dir().join("plugins").join(uuid))
	{
		event.payload.message = format!("[{}] {}", manifest.name, event.payload.message);
	}
	log::info!("{}", event.payload.message.trim());
	Ok(())
}

pub async fn show_alert(event: ContextEvent) -> Result<(), anyhow::Error> {
	emit(FrontendEvent::ShowAlert(event.context));
	Ok(())
}

pub async fn show_ok(event: ContextEvent) -> Result<(), anyhow::Error> {
	emit(FrontendEvent::ShowOk(event.context));
	Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SwitchProfileEvent {
	device: String,
	profile: String,
}

pub async fn switch_profile(event: SwitchProfileEvent) -> Result<(), anyhow::Error> {
	emit(FrontendEvent::SwitchProfile {
		device: event.device,
		profile: event.profile,
	});
	Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
pub struct DeviceBrightnessEvent {
	action: String,
	value: u8,
}

pub async fn device_brightness(event: DeviceBrightnessEvent) -> Result<(), anyhow::Error> {
	emit(FrontendEvent::DeviceBrightness {
		action: event.action,
		value: event.value,
	});
	Ok(())
}
