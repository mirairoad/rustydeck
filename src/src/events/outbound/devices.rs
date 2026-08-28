//! Driving the physical device.
//!
//! Every device is handled by the built-in Elgato driver. There used to be a second path here, in
//! which a plugin could claim a device-id namespace and receive `setImage`/`setBrightness` events
//! instead; nothing owns a namespace any more, so these go straight to the driver.

pub async fn update_image(context: crate::shared::Context, image: Option<String>) -> Result<(), anyhow::Error> {
	if context.device.starts_with("sd-") {
		crate::elgato::update_image(&context, image.as_deref()).await?;
	}
	Ok(())
}

pub async fn clear_screen(device: String) -> Result<(), anyhow::Error> {
	if device.starts_with("sd-") {
		crate::elgato::clear_screen(&device).await?;
	}
	Ok(())
}


/// Set the brightness for a specific device.
pub async fn set_device_brightness(device: &str, brightness: u8) -> Result<(), anyhow::Error> {
	if crate::device_sleep::is_device_sleeping(device) {
		return Ok(());
	}
	if device.starts_with("sd-") {
		crate::elgato::set_brightness(device, brightness).await;
	}
	Ok(())
}
