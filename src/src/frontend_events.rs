//! In-process replacement for the old `tauri::Emitter` calls: instead of serialising an event
//! across the Tauri IPC bridge to a webview, broadcast it on a channel the GPUI shell subscribes
//! to directly.

use std::sync::LazyLock;

use tokio::sync::broadcast;

/// Something changed that the window needs to redraw for.
///
/// These carry no payload on purpose: the window re-reads whatever it needs from the store when it
/// wakes, so a payload would only be a second copy that can go stale. Variants nothing listened for
/// were removed with the plugin system.
#[derive(Clone)]
pub enum FrontendEvent {
	/// The deck switched page, either by itself or because the focused application changed.
	SwitchProfile,
	/// A device connected or disconnected.
	Devices,
}

static FRONTEND_EVENTS: LazyLock<broadcast::Sender<FrontendEvent>> = LazyLock::new(|| broadcast::channel(256).0);

pub fn subscribe() -> broadcast::Receiver<FrontendEvent> {
	FRONTEND_EVENTS.subscribe()
}

pub fn emit(event: FrontendEvent) {
	// No error if there are no subscribers yet - the event is simply not delivered.
	let _ = FRONTEND_EVENTS.send(event);
}
