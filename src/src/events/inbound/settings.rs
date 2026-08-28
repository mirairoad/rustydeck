//! Writing an instance's settings.
//!
//! This was one end of a plugin protocol - a plugin or its property inspector would push settings
//! and the other side would be told. Only the window writes settings now, so this is a store write
//! that marks the profile dirty.

use crate::store::profiles::{acquire_locks_mut, get_instance_mut, mark_profile_stale};

/// `_from_property_inspector` is kept so call sites read the same; there is no inspector to tell.
pub async fn set_settings(event: super::ContextAndPayloadEvent<serde_json::Value>, _from_property_inspector: bool) -> Result<(), anyhow::Error> {
	let mut locks = acquire_locks_mut().await;

	if let Some(instance) = get_instance_mut(&event.context, &mut locks).await? {
		instance.settings = event.payload;
		mark_profile_stale(&event.context.device, &mut locks).await?;
	}

	Ok(())
}
