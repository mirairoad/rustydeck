//! Device input, and the small event structs it travels in.
//!
//! These were once decoded off a plugin's WebSocket, which is where the name and the wrapper
//! structs come from. Nothing is decoded any more - the Elgato driver calls straight into
//! [`devices`], and the window calls [`settings::set_settings`] - so all that remains is the
//! shape those calls are made in.

pub(crate) mod devices;
pub mod settings;

use crate::shared::ActionContext;

pub struct PayloadEvent<T> {
	pub payload: T,
}

pub struct ContextAndPayloadEvent<T, C = ActionContext> {
	pub context: C,
	pub payload: T,
}
