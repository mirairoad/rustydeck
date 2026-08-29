//! Animation effects, and what one frame of one does to the artwork.
//!
//! An animation here is a sequence of stills, because that is all the hardware understands: a key
//! is a small screen you write a JPEG to, and everything that looks like motion is the app pushing
//! a new frame. Elgato's own SDK refuses animated formats outright for the same reason.
//!
//! Every effect is computed from the artwork the action already has, rather than needing a second
//! file. That is what makes them work on a library someone has already built, and it is why the
//! rest of the pipeline needs no new decoder.
//!
//! **Frame zero is the resting state.** Every effect starts from the identity transform, so the
//! first frame is byte-for-byte the still the action would have had anyway. A sleeping deck, a page
//! you are not looking at, the sidebar icon and a deck that is over its frame budget all show that
//! frame - they simply stop advancing, with no separate still to keep in step.

use serde::{Deserialize, Serialize};

/// How many frames one cycle of a generated effect is drawn with.
///
/// A whole cycle, so it loops without a seam. Twenty-four is smooth at every rate offered and keeps
/// a save to well under a second: each frame is composited twice, once per face.
pub const CYCLE: usize = 24;

/// What one frame does to the artwork, relative to the resting still.
///
/// Deliberately only three knobs. They compose - `Pulse` is scale alone, `Fade` is alpha alone,
/// `Drift` is offset alone - and between them they cover every effect worth generating without the
/// pipeline having to know which effect it is drawing.
#[derive(Clone, Copy)]
pub struct Transform {
	/// Scale about the centre of the face. 1.0 is the resting size.
	pub scale: f32,
	/// Opacity of the artwork over the background. 1.0 is fully opaque.
	pub alpha: f32,
	/// Offset as a fraction of the face's own width and height.
	pub offset: (f32, f32),
}

impl Transform {
	/// The resting state: the still as it would be with no animation at all.
	pub const IDENTITY: Self = Self {
		scale: 1.0,
		alpha: 1.0,
		offset: (0.0, 0.0),
	};
}

/// A generated effect, computed from the artwork rather than imported.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
	/// Breathe between fully opaque and part way out.
	Fade,
	/// Swell slightly and settle.
	Pulse,
	/// Draw in and back out, like a key being pressed.
	Shrink,
	/// Drift side to side.
	Drift,
	/// Hold, then cut out briefly - an attention effect, not a smooth one.
	Blink,
}

impl Effect {
	/// Every effect, in the order the picker offers them.
	pub const ALL: &'static [Effect] = &[Effect::Fade, Effect::Pulse, Effect::Shrink, Effect::Drift, Effect::Blink];

	pub fn label(&self) -> &'static str {
		match self {
			Effect::Fade => "Fade",
			Effect::Pulse => "Pulse",
			Effect::Shrink => "Shrink",
			Effect::Drift => "Drift",
			Effect::Blink => "Blink",
		}
	}

	/// The transform for frame `index` of [`CYCLE`].
	///
	/// Every effect is phrased so frame zero is the identity, which is what lets the still and the
	/// animation be the same thing. The smooth ones ride a sine from zero for that reason - a
	/// cosine would start at the extreme and make the still the wrong picture.
	pub fn transform(&self, index: usize) -> Transform {
		// Position within the cycle, 0.0 at the start and approaching 1.0 at the end.
		let phase = (index % CYCLE) as f32 / CYCLE as f32;
		// Out to one at the middle of the cycle and back, for the effects that swell once and
		// settle. Zero at both ends, so frame zero rests and the loop has no seam.
		let swell = (phase * std::f32::consts::PI).sin();
		// A full period for the effects that go both ways: out, back through rest, out the other
		// side, back. Also zero at both ends.
		let wave = (phase * std::f32::consts::TAU).sin();

		match self {
			// Dips to a quarter rather than vanishing: a key that goes fully blank mid-cycle reads
			// as broken hardware rather than as an effect.
			Effect::Fade => Transform {
				alpha: 1.0 - 0.75 * swell,
				..Transform::IDENTITY
			},
			Effect::Pulse => Transform {
				scale: 1.0 + 0.10 * swell,
				..Transform::IDENTITY
			},
			Effect::Shrink => Transform {
				scale: 1.0 - 0.14 * swell,
				..Transform::IDENTITY
			},
			Effect::Drift => Transform {
				offset: (0.06 * wave, 0.0),
				..Transform::IDENTITY
			},
			// Not a wave: on for most of the cycle, off for a beat at the end. The gap sits at the
			// end rather than the start so frame zero is still the resting image.
			Effect::Blink => Transform {
				alpha: if index >= CYCLE - CYCLE / 6 { 0.15 } else { 1.0 },
				..Transform::IDENTITY
			},
		}
	}
}

/// How fast an animation runs, as a named choice rather than a number.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Speed {
	Slow,
	Normal,
	Fast,
}

impl Speed {
	pub const ALL: &'static [Speed] = &[Speed::Slow, Speed::Normal, Speed::Fast];

	pub fn label(&self) -> &'static str {
		match self {
			Speed::Slow => "Slow",
			Speed::Normal => "Normal",
			Speed::Fast => "Fast",
		}
	}

	pub fn fps(&self) -> u16 {
		match self {
			Speed::Slow => 10,
			Speed::Normal => 20,
			Speed::Fast => 30,
		}
	}
}

/// What makes an animation play.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
	/// Loops for as long as the page is showing.
	Always,
	/// Runs once when the physical control is pressed, then settles back to the still.
	OnPress,
}

impl Trigger {
	pub const ALL: &'static [Trigger] = &[Trigger::Always, Trigger::OnPress];

	pub fn label(&self) -> &'static str {
		match self {
			Trigger::Always => "Always",
			Trigger::OnPress => "On press",
		}
	}
}

/// What an action's animation is, stored alongside the rest of its configuration.
#[derive(Clone, Serialize, Deserialize)]
pub struct Animation {
	pub effect: Effect,
	pub speed: Speed,
	/// Older configs predate triggers and were all ambient, so that is what they default to.
	#[serde(default = "always")]
	pub trigger: Trigger,
}

fn always() -> Trigger {
	Trigger::Always
}

impl Animation {
	pub fn frame_count(&self) -> usize {
		CYCLE
	}

	/// How long one frame is held.
	pub fn interval(&self) -> std::time::Duration {
		std::time::Duration::from_secs_f64(1.0 / self.speed.fps() as f64)
	}
}

// ---------------------------------------------------------------------------
// Playback
// ---------------------------------------------------------------------------

use crate::shared::{Context as SlotContext, DeviceInfo, Profile};

/// The pixel sizes the renderer composites each surface at, matching `ui`'s own constants.
const KEY_IMAGE: (u32, u32) = (144, 144);
const ENCODER_IMAGE: (u32, u32) = (200, 100);

use std::collections::HashMap;
use std::sync::LazyLock;

use tokio::sync::Mutex;
use tokio::task::JoinHandle;

/// Measured cost of getting one frame onto one surface, in milliseconds.
///
/// From a bench against a connected Stream Deck +: a key write and flush is 3.6 ms sustained, and a
/// strip segment is a 200x100 region write at 9.7 ms. Four keys together came to 3.5 ms each, so
/// the cost is linear in the number of animating surfaces and lives on the wire, not in the CPU.
/// These are what the frame rate is chosen against.
const KEY_COST_MS: f64 = 3.5;
const STRIP_COST_MS: f64 = 9.7;

/// One animating slot: where its frames are, and where they go.
struct Slot {
	context: SlotContext,
	directory: std::path::PathBuf,
	strip: bool,
	/// The resting state of the slot, reused per frame with only its image swapped. Carrying it
	/// keeps the background colour and title the still had, so a frame is the same picture the
	/// still renderer would draw.
	state: crate::shared::ActionState,
	/// The size the device wants this surface at.
	size: (u32, u32),
	frames: usize,
	/// This slot's own rate, which may be slower than the device's tick.
	interval: std::time::Duration,
	elapsed: std::time::Duration,
	index: usize,
}

impl Slot {
	fn cost_ms(&self) -> f64 {
		if self.strip { STRIP_COST_MS } else { KEY_COST_MS }
	}
}

static PLAYERS: LazyLock<Mutex<HashMap<String, JoinHandle<()>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Frames already rendered for the wire, keyed by what makes one unique.
///
/// Shared rather than per-slot so a press effect is not slow the first time it fires: the ambient
/// warm-up, a second slot using the same action, and every later press all draw from the same
/// entries. Rendering a frame costs about 7 ms for a key and 10 ms for a strip segment, against
/// 3.5 ms and 9.7 ms to put it on the wire, so a cache miss is the expensive case by a distance.
type FrameKey = (std::path::PathBuf, bool, usize);
static FRAME_CACHE: LazyLock<Mutex<HashMap<FrameKey, String>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Forget every rendered frame. Called when the library changes, since an edited action's frames
/// are rewritten underneath the cache and the old ones would otherwise keep playing.
pub async fn forget_frames() {
	FRAME_CACHE.lock().await.clear();
}

/// One frame of one face, rendered for the device and remembered.
async fn frame_image(directory: &std::path::Path, strip: bool, index: usize, state: &crate::shared::ActionState, size: (u32, u32)) -> Option<String> {
	let key = (directory.to_path_buf(), strip, index);
	if let Some(cached) = FRAME_CACHE.lock().await.get(&key) {
		return Some(cached.clone());
	}

	// Through the still renderer, not around it: `update_image` takes an encoded data URL, and
	// going the same way the stills do means the background, the scale and the encoder region
	// handling are whatever the resting frame already got right.
	let mut state = state.clone();
	state.image = crate::custom_actions::frame_file(directory, strip, index).to_string_lossy().into_owned();

	let (width, height) = size;
	let rendered = tokio::task::spawn_blocking(move || crate::device_render::render_state(&state, width, height)).await;
	let image = match rendered {
		Ok(Ok(image)) => image,
		Ok(Err(error)) => {
			log::warn!("Failed to render animation frame: {error}");
			return None;
		}
		Err(error) => {
			log::warn!("Animation frame render panicked: {error}");
			return None;
		}
	};

	FRAME_CACHE.lock().await.insert(key, image.clone());
	Some(image)
}

/// Stop whatever is animating on a device.
///
/// Every slot keeps whatever frame it was showing. That is safe because frame zero is the resting
/// still and every other frame is a legitimate picture of the same artwork - stopping mid-cycle
/// looks like a pause, not a glitch. The next push of the profile restores the still.
pub async fn stop(device_id: &str) {
	if let Some(handle) = PLAYERS.lock().await.remove(device_id) {
		handle.abort();
	}
}

/// Begin animating every slot on this profile that has an effect, replacing any running player.
///
/// One task for the whole device rather than one per slot: the driver batches image writes and
/// sends them on `flush`, so a timer per slot would flush per slot and throw that batching away.
pub async fn start(device: DeviceInfo, profile: Profile) {
	stop(&device.id).await;

	let slots = collect_slots(&device, &profile);
	if slots.is_empty() {
		return;
	}

	// What one frame of all of this costs, against the fastest rate anything asked for.
	let cost_ms: f64 = slots.iter().map(Slot::cost_ms).sum();
	let wanted_fps = slots
		.iter()
		.map(|slot| (1.0 / slot.interval.as_secs_f64()).round() as u16)
		.max()
		.unwrap_or(20);

	// Never ask for more than the wire can carry. Falling behind does not degrade gracefully - the
	// writes queue and the deck stutters - so the rate is capped up front and said out loud.
	let affordable_fps = (1000.0 / cost_ms).floor().max(1.0) as u16;
	let fps = wanted_fps.min(affordable_fps);
	if fps < wanted_fps {
		log::info!(
			"Animating {} slots on {} costs {cost_ms:.1}ms a frame; holding {fps} fps rather than the {wanted_fps} asked for",
			slots.len(),
			device.id
		);
	}

	let device_id = device.id.clone();
	let handle = crate::spawn(async move {
		run(slots, fps).await;
	});
	PLAYERS.lock().await.insert(device_id, handle);
}

/// Play a slot's effect once, from the still and back to it.
///
/// The launcher feel: the control is pressed, the artwork does its thing, and it settles. Nothing
/// is left running afterwards, and frame zero is the still, so settling is just showing it again.
///
/// Deliberately fire-and-forget on the runtime rather than awaited by the press handler - the
/// command a key runs must not wait for its animation to finish.
pub fn press(context: SlotContext, instance: &crate::shared::ActionInstance) {
	let Some(id) = instance.settings.get("rustydeck_custom").and_then(|value| value.as_str()) else {
		return;
	};

	let mut library = crate::custom_actions::load();
	library.extend(crate::custom_actions::load_predefined());
	let Some(action) = library.into_iter().find(|candidate| candidate.id() == id) else { return };
	let Some(animation) = action.config.animation.clone().filter(|a| a.trigger == Trigger::OnPress) else {
		return;
	};

	let Some(state) = instance.states.get(instance.current_state as usize).cloned() else { return };
	let strip = context.controller == "Encoder";
	let size = if strip { ENCODER_IMAGE } else { KEY_IMAGE };
	let directory = action.directory();

	crate::spawn(async move {
		let mut timer = tokio::time::interval(animation.interval());
		// Frame zero is the still already on the key, so the run starts at one and ends by putting
		// it back - that final write is what settles the key rather than leaving it mid-effect.
		for index in 1..animation.frame_count() {
			timer.tick().await;
			let Some(image) = frame_image(&directory, strip, index, &state, size).await else { break };
			crate::events::frontend::instances::update_image(context.clone(), Some(image)).await;
		}

		timer.tick().await;
		if let Some(image) = frame_image(&directory, strip, 0, &state, size).await {
			crate::events::frontend::instances::update_image(context, Some(image)).await;
		}
	});
}

/// Every slot on the profile whose action carries an effect.
fn collect_slots(device: &DeviceInfo, profile: &Profile) -> Vec<Slot> {
	let mut library = crate::custom_actions::load();
	library.extend(crate::custom_actions::load_predefined());

	let keypad_count = (device.rows as usize) * (device.columns as usize) + device.touchpoints as usize;
	let groups = [("Keypad", keypad_count), ("Encoder", device.encoders as usize)];

	let mut slots = Vec::new();
	for (controller, count) in groups {
		let instances = match controller {
			"Encoder" => &profile.sliders,
			_ => &profile.keys,
		};

		for (position, instance) in instances.iter().enumerate().take(count) {
			let Some(instance) = instance else { continue };
			// The slot remembers which library entry it was made from; that is where the effect is.
			let Some(id) = instance.settings.get("rustydeck_custom").and_then(|value| value.as_str()) else {
				continue;
			};
			let Some(action) = library.iter().find(|candidate| candidate.id() == id) else { continue };
			let Some(animation) = action.config.animation.as_ref() else { continue };
			// A press effect is not part of the loop - it runs once, when the control is pressed,
			// and spends no budget in between.
			if animation.trigger != Trigger::Always {
				continue;
			}

			let Some(state) = instance.states.get(instance.current_state as usize).cloned() else { continue };

			slots.push(Slot {
				context: SlotContext {
					device: device.id.clone(),
					profile: profile.id.clone(),
					controller: controller.to_owned(),
					position: position as u8,
				},
				directory: action.directory(),
				strip: controller == "Encoder",
				state,
				size: if controller == "Encoder" { ENCODER_IMAGE } else { KEY_IMAGE },
				frames: animation.frame_count(),
				interval: animation.interval(),
				elapsed: std::time::Duration::ZERO,
				index: 0,
			});
		}
	}
	slots
}

/// The device's tick. Advances every slot that is due and pushes what changed.
async fn run(mut slots: Vec<Slot>, fps: u16) {
	let tick = std::time::Duration::from_secs_f64(1.0 / fps as f64);
	let mut timer = tokio::time::interval(tick);
	// The rate is already capped to what the wire can carry; if a tick still runs long, skip ahead
	// rather than trying to catch up, which would send a burst the deck cannot drain.
	timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

	loop {
		timer.tick().await;

		for slot in slots.iter_mut() {
			slot.elapsed += tick;
			if slot.elapsed < slot.interval {
				continue;
			}
			slot.elapsed = std::time::Duration::ZERO;
			slot.index = (slot.index + 1) % slot.frames.max(1);

			let Some(image) = frame_image(&slot.directory, slot.strip, slot.index, &slot.state, slot.size).await else {
				continue;
			};
			crate::events::frontend::instances::update_image(slot.context.clone(), Some(image)).await;
		}
	}
}
