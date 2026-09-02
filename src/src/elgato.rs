use crate::encoder_layouts::generate_encoder_image;
use crate::events::inbound;

use std::collections::HashMap;
use std::sync::LazyLock;

use base64::Engine as _;
use elgato_streamdeck::{
	AsyncStreamDeck, DeviceStateUpdate,
	images::{ImageRect, convert_image_with_format_async},
	info::{ImageRotation, Kind},
};
use image::GenericImageView as _;
use tokio::sync::RwLock;

static ELGATO_DEVICES: LazyLock<RwLock<HashMap<String, AsyncStreamDeck>>> = LazyLock::new(|| RwLock::new(HashMap::new()));
static HIDAPI: LazyLock<RwLock<Option<hidapi::HidApi>>> = LazyLock::new(|| RwLock::new(None));

/// Extract the average colour from an image.
fn extract_average_colour(img: &image::DynamicImage) -> (u8, u8, u8) {
	let (r_sum, g_sum, b_sum) = img
		.pixels()
		.fold((0u64, 0u64, 0u64), |(r, g, b), (_, _, pixel)| (r + pixel[0] as u64, g + pixel[1] as u64, b + pixel[2] as u64));
	let count = (img.width() * img.height()).max(1) as u64;
	((r_sum / count) as u8, (g_sum / count) as u8, (b_sum / count) as u8)
}

pub async fn update_image(context: &crate::shared::Context, image: Option<&str>) -> Result<(), anyhow::Error> {
	if let Some(device) = ELGATO_DEVICES.read().await.get(&context.device) {
		let kind = device.kind();
		if !kind.is_visual() {
			return Ok(());
		}
		let key_count = kind.key_count();
		let is_touch_point = context.controller == "Keypad" && context.position >= key_count;

		if let Some(image) = image {
			let data = image.split_once(',').unwrap().1;
			let bytes = base64::engine::general_purpose::STANDARD.decode(data)?;
			if context.controller == "Encoder" {
				let mut img = generate_encoder_image(context, &bytes).await?;
				let Some(format) = device.kind().lcd_image_format() else {
					return Err(anyhow::anyhow!("Failed to get LCD image format"));
				};
				img = match format.rotation {
					ImageRotation::Rot0 => img,
					ImageRotation::Rot90 => img.rotate90(),
					ImageRotation::Rot180 => img.rotate180(),
					ImageRotation::Rot270 => img.rotate270(),
				};
				device.write_lcd(context.position as u16 * 200, 0, &ImageRect::from_image_async(img)?).await?;
			} else if context.controller == "Infobar" {
				let img = image::load_from_memory(&bytes)?;
				let Some(format) = device.kind().lcd_image_format() else {
					return Err(anyhow::anyhow!("Failed to get LCD image format"));
				};
				let data = convert_image_with_format_async(format, img.resize_exact(248, 58, image::imageops::FilterType::Lanczos3))?;
				device.write_lcd_fill(&data).await?;
			} else if is_touch_point {
				let (r, g, b) = extract_average_colour(&image::load_from_memory(&bytes)?);
				device.set_touchpoint_color(context.position - key_count, r, g, b).await?;
			} else {
				device.set_button_image(context.position, image::load_from_memory(&bytes)?).await?;
			}
		} else if context.controller == "Encoder" {
			let mut img = image::DynamicImage::new_rgb8(200, 100);
			let Some(format) = device.kind().lcd_image_format() else {
				return Err(anyhow::anyhow!("Failed to get LCD image format"));
			};
			img = match format.rotation {
				ImageRotation::Rot0 => img,
				ImageRotation::Rot90 => img.rotate90(),
				ImageRotation::Rot180 => img.rotate180(),
				ImageRotation::Rot270 => img.rotate270(),
			};
			device.write_lcd(context.position as u16 * 200, 0, &ImageRect::from_image_async(img)?).await?;
		} else if context.controller == "Infobar" {
			let Some(format) = device.kind().lcd_image_format() else {
				return Err(anyhow::anyhow!("Failed to get LCD image format"));
			};
			let data = convert_image_with_format_async(format, image::DynamicImage::new_rgb8(248, 58))?;
			device.write_lcd_fill(&data).await?;
		} else if is_touch_point {
			device.set_touchpoint_color(context.position - key_count, 0, 0, 0).await?;
		} else {
			device.clear_button_image(context.position).await?;
		}
		device.flush().await?;
	}
	Ok(())
}

/// Clear all touchpoint LEDs on a device by setting them to black.
async fn clear_all_touchpoints(device: &AsyncStreamDeck) {
	for i in 0..device.kind().touchpoint_count() {
		let _ = device.set_touchpoint_color(i, 0, 0, 0).await;
	}
}

pub async fn clear_screen(id: &str) -> Result<(), anyhow::Error> {
	if let Some(device) = ELGATO_DEVICES.read().await.get(id) {
		device.clear_all_button_images().await?;
		if let Some(lcd_format) = device.kind().lcd_image_format() {
			device
				.write_lcd_fill(&convert_image_with_format_async(
					lcd_format,
					image::DynamicImage::new_rgb8(lcd_format.size.0 as u32, lcd_format.size.1 as u32),
				)?)
				.await?;
		}
		clear_all_touchpoints(device).await;
		device.flush().await?;
	}
	Ok(())
}

pub async fn set_brightness(id: &str, brightness: u8) {
	if let Some(device) = ELGATO_DEVICES.read().await.get(id) {
		let _ = device.set_brightness(brightness.clamp(0, 100)).await;
		let _ = device.flush().await;
	}
}

pub async fn reset_devices() {
	for device in ELGATO_DEVICES.read().await.values() {
		let _ = device.reset().await;
		let _ = device.flush().await;
	}
}

/// The display name for a Stream Deck model.
///
/// Deliberately not the USB product string: that reports "Stream Deck Plus" while the product is
/// branded "Stream Deck +", and the simulator would then disagree with the hardware it imitates.
/// Naming every model here keeps the two consistent, and the exhaustive match means a model added
/// upstream is a compile error rather than a device that quietly shows the wrong name.
pub fn model_name(kind: Kind) -> &'static str {
	match kind {
		Kind::Original | Kind::OriginalV2 => "Stream Deck",
		Kind::Mk2 | Kind::Mk2Scissor | Kind::Mk2Module => "Stream Deck MK.2",
		Kind::Mini | Kind::MiniMk2 | Kind::MiniMk2Module => "Stream Deck Mini",
		Kind::MiniDiscord => "Stream Deck Mini Discord",
		Kind::Xl | Kind::XlV2 | Kind::XlV2Module => "Stream Deck XL",
		Kind::Plus => "Stream Deck +",
		Kind::PlusXl => "Stream Deck XL+",
		Kind::Neo => "Stream Deck Neo",
		Kind::Pedal => "Stream Deck Pedal",
	}
}

async fn init(device: AsyncStreamDeck, device_id: String) {
	if ELGATO_DEVICES.read().await.contains_key(&device_id) {
		return;
	}

	let kind = device.kind();
	let device_type = match kind {
		Kind::Original | Kind::OriginalV2 | Kind::Mk2 | Kind::Mk2Scissor | Kind::Mk2Module => 0,
		Kind::Mini | Kind::MiniMk2 | Kind::MiniDiscord | Kind::MiniMk2Module => 1,
		Kind::Xl | Kind::XlV2 | Kind::XlV2Module => 2,
		Kind::Pedal => 5,
		Kind::Plus => 7,
		Kind::Neo => 9,
		Kind::PlusXl => 13,
	};
	let _ = device.clear_all_button_images().await;
	clear_all_touchpoints(&device).await;
	let _ = device.set_brightness(crate::store::get_settings().value.brightness).await;
	let _ = device.flush().await;

	let reader = device.get_reader();
	ELGATO_DEVICES.write().await.insert(device_id.clone(), device);
	let _ = clear_screen(&device_id).await;

	crate::events::inbound::devices::register_device(
		crate::events::inbound::PayloadEvent {
			payload: crate::shared::DeviceInfo {
				id: device_id.clone(),
				plugin: String::new(),
				name: model_name(kind).to_owned(),
				rows: kind.row_count(),
				columns: kind.column_count(),
				encoders: kind.encoder_count(),
				touchpoints: kind.touchpoint_count(),
				infobars: if kind == Kind::Neo { 1 } else { 0 },
				r#type: device_type,
			},
		},
	)
	.await
	.unwrap();

	// Every face was blanked above, and a deck is only ever painted by something pushing a page to
	// it. Without this, a deck unplugged and plugged back in comes up dark and stays dark: the
	// window is already showing it, so nothing up there thinks anything has changed.
	crate::spawn(crate::device_render::repaint(device_id.clone()));

	let press = |position| inbound::PayloadEvent {
		payload: inbound::devices::PressPayload { device: device_id.clone(), position },
	};
	let encoder = |position, ticks: i8| inbound::PayloadEvent {
		payload: inbound::devices::TicksPayload {
			device: device_id.clone(),
			position,
			ticks: ticks.into(),
		},
	};
	let touchscreen_press = |position, x, y, hold| inbound::PayloadEvent {
		payload: inbound::devices::TouchscreenPressPayload {
			device: device_id.clone(),
			position,
			x,
			y,
			hold,
		},
	};
	loop {
		let updates = match reader.read(100.0).await {
			Ok(updates) => updates,
			Err(_) => break,
		};
		for update in updates {
			match match update {
				DeviceStateUpdate::ButtonDown(key) => inbound::devices::key_down(press(key)).await,
				DeviceStateUpdate::ButtonUp(key) => inbound::devices::key_up(press(key)).await,
				DeviceStateUpdate::TouchPointDown(point) => inbound::devices::key_down(press(kind.key_count() + point)).await,
				DeviceStateUpdate::TouchPointUp(point) => inbound::devices::key_up(press(kind.key_count() + point)).await,
				DeviceStateUpdate::EncoderTwist(dial, ticks) => inbound::devices::encoder_change(encoder(dial, ticks)).await,
				DeviceStateUpdate::EncoderDown(dial) => inbound::devices::encoder_down(press(dial)).await,
				DeviceStateUpdate::EncoderUp(dial) => inbound::devices::encoder_up(press(dial)).await,
				DeviceStateUpdate::TouchScreenPress(x, y) => {
					let (position, x, y) = match kind {
						Kind::Plus => ((x / 200) as u8, x % 200, y),
						_ => continue,
					};
					inbound::devices::touchscreen_press(touchscreen_press(position, x, y, false)).await
				}
				DeviceStateUpdate::TouchScreenLongPress(x, y) => {
					let (position, x, y) = match kind {
						Kind::Plus => ((x / 200) as u8, x % 200, y),
						_ => continue,
					};
					inbound::devices::touchscreen_press(touchscreen_press(position, x, y, true)).await
				}
				_ => Ok(()),
			} {
				Ok(_) => (),
				Err(error) => log::warn!("Failed to process device event {update:?}: {error}"),
			}
		}
	}

	ELGATO_DEVICES.write().await.remove(&device_id);
	crate::events::inbound::devices::deregister_device(crate::events::inbound::PayloadEvent { payload: device_id })
		.await
		.unwrap();
}

/// Attempt to initialise all connected devices.
pub async fn initialise_devices() {
	// `disableelgato` used to hand the "sd" namespace to a plugin-provided driver. There are no
	// plugins now, so disabling the built-in driver just means handling no devices at all.
	if crate::store::get_settings().value.disableelgato {
		return;
	}

	// Iterate through detected Elgato devices and attempt to register them.
	let mut hidapi = HIDAPI.write().await;
	if hidapi.is_none() {
		match elgato_streamdeck::new_hidapi() {
			Ok(hid) => *hidapi = Some(hid),
			Err(error) => {
				log::warn!("Failed to initialise hidapi: {error}");
				return;
			}
		}
	}
	let Some(hid) = hidapi.as_mut() else { return };

	// hidapi enumerates once when its context is created and then serves that list from cache, so
	// without this a deck plugged in after startup is never seen - the poll would re-scan the same
	// frozen list every ten seconds forever.
	if let Err(error) = hid.refresh_devices() {
		log::warn!("Failed to refresh the HID device list: {error}");
		return;
	}

	for (kind, serial) in elgato_streamdeck::asynchronous::list_devices_async(hid) {
		let device_id = format!("sd-{serial}");
		if ELGATO_DEVICES.read().await.contains_key(&device_id) {
			continue;
		}
		match elgato_streamdeck::AsyncStreamDeck::connect(hid, kind, &serial) {
			Ok(device) => {
				tokio::spawn(init(device, device_id));
			}
			Err(error) => log::warn!("Failed to connect to Elgato device: {error}"),
		}
	}
}
