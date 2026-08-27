//! In-process replacement for the old `tauri::Emitter` calls: instead of serialising an
//! event across the Tauri IPC bridge to a webview, broadcast it on a channel that the
//! (future) GPUI shell subscribes to directly.

use crate::shared::{ActionContext, ActionInstance, Context, DeviceInfo};

use std::sync::LazyLock;

use tokio::sync::broadcast;

#[derive(Clone)]
pub enum FrontendEvent {
	Applications(Vec<String>),
	SwitchProfile { device: String, profile: String },
	ShowAlert(ActionContext),
	ShowOk(ActionContext),
	DeviceBrightness { action: String, value: u8 },
	Devices(dashmap::DashMap<String, DeviceInfo>),
	RerenderImages,
	UpdateState { context: ActionContext, contents: Option<ActionInstance> },
	KeyMoved { context: Context, pressed: bool },
	PluginReloaded(String),
}

static FRONTEND_EVENTS: LazyLock<broadcast::Sender<FrontendEvent>> = LazyLock::new(|| broadcast::channel(256).0);

pub fn subscribe() -> broadcast::Receiver<FrontendEvent> {
	FRONTEND_EVENTS.subscribe()
}

pub fn emit(event: FrontendEvent) {
	// No error if there are no subscribers yet - the event is simply not delivered.
	let _ = FRONTEND_EVENTS.send(event);
}
