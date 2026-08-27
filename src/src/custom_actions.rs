//! User-defined actions: a name, a shell command, and an image.
//!
//! These are *templates*, not instances - a library the user builds up, where dragging one onto a
//! slot creates a "Run Command" instance configured from it.
//!
//! Each action owns a directory under `~/.rustydeck/customs/<name>/` holding its `config.json`,
//! the composited `picture.png`, and a copy of whatever icon it was built from. Keeping the
//! ingredients (icon, background) alongside the result means editing can recompose the artwork
//! without asking the user to find the source file again, and an action stays intact if they move
//! or delete the original.

use crate::device_render::{CANVAS, blend, parse_colour};
use crate::shared::config_dir;
use crate::store::{NotProfile, Store};

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use image::{Rgba, RgbaImage, imageops::FilterType};
use serde::{Deserialize, Serialize};

/// The on-disk shape of `config.json`.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomActionConfig {
	/// Stable identity, generated once and never changed.
	///
	/// Deliberately not the directory name: renaming an action moves its directory, so anything
	/// referencing it by slug would break exactly when the user renames it. Keys on the deck store
	/// this id to stay linked to the action they were created from.
	pub id: String,
	pub name: String,
	/// Runs on key press - written to the Run Command action's `settings.down`.
	pub command: String,
	/// For predefined entries: the UUID of the action to instantiate instead of Run Command.
	/// When set, `command` is unused.
	pub action: Option<String>,
	/// The composited key face, relative to the action's own directory.
	pub image: String,
	/// The icon it was composited from, if any (PNG/JPEG/SVG), relative to the directory.
	pub icon: Option<String>,
	/// Background colour as `#RRGGBB`, or `None` for no fill.
	pub background: Option<String>,
}

impl NotProfile for CustomActionConfig {}

/// A stored action plus the directory name that identifies it.
#[derive(Clone)]
pub struct CustomAction {
	/// Directory name within [`Self::root`] - derived from the action's name.
	pub slug: String,
	/// Which library this came from: the user's `customs/` or the shipped `predefined/`.
	pub root: PathBuf,
	pub config: CustomActionConfig,
}

impl CustomAction {
	pub fn name(&self) -> &str {
		&self.config.name
	}

	pub fn command(&self) -> &str {
		&self.config.command
	}

	pub fn id(&self) -> &str {
		&self.config.id
	}

	/// The built-in action this entry places, if it is not a shell command.
	pub fn action_uuid(&self) -> Option<&str> {
		self.config.action.as_deref()
	}

	fn dir(&self) -> PathBuf {
		self.root.join(&self.slug)
	}

	/// Absolute path to the composited key face.
	pub fn image_path(&self) -> PathBuf {
		self.dir().join(&self.config.image)
	}

	/// Absolute path to the image the key face was composited *from*, when one was kept.
	///
	/// Editing must preview against this rather than [`Self::image_path`]: the composited picture
	/// already has the old background baked into it, so showing it over a newly chosen colour just
	/// hides the new colour behind the old one.
	pub fn source_path(&self) -> Option<PathBuf> {
		self.config
			.icon
			.as_ref()
			.map(|icon| self.dir().join(icon))
			.filter(|path| std::fs::metadata(path).map(|meta| meta.len() > 0).unwrap_or(false))
	}
}

pub const PICTURE: &str = "picture.png";

fn new_id() -> String {
	use std::time::{SystemTime, UNIX_EPOCH};
	let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
	format!("ca-{nanos}")
}

pub fn customs_dir() -> PathBuf {
	config_dir().join("customs")
}

/// Predefined entries ship with the app but live on disk in the same shape as the user's own, so
/// they can be re-themed or replaced, and users can add their own.
pub fn predefined_dir() -> PathBuf {
	config_dir().join("predefined")
}

fn directory(slug: &str) -> PathBuf {
	customs_dir().join(slug)
}

/// Turn a display name into a filesystem-safe directory name, keeping it recognisable.
fn slugify(name: &str) -> String {
	let slug: String = name
		.trim()
		.to_lowercase()
		.chars()
		.map(|character| if character.is_alphanumeric() { character } else { '-' })
		.collect();

	let slug = slug.trim_matches('-').replace("--", "-");
	if slug.is_empty() { "action".to_owned() } else { slug }
}

/// A slug not already taken by another action - `lock-screen`, then `lock-screen-2`, and so on.
fn unique_slug(name: &str, except: Option<&str>) -> String {
	let base = slugify(name);
	let mut candidate = base.clone();
	let mut suffix = 2;

	while Some(candidate.as_str()) != except && directory(&candidate).exists() {
		candidate = format!("{base}-{suffix}");
		suffix += 1;
	}
	candidate
}

/// Draw a simple arrow key face: a solid background with a white arrow.
///
/// Generated rather than shipped as a binary asset so the artwork stays editable - the files land
/// in `predefined/` like any other action, and can be re-themed or replaced.
fn arrow_picture(background: &str, pointing_right: bool) -> RgbaImage {
	let mut canvas = RgbaImage::from_pixel(CANVAS, CANVAS, parse_colour(background));
	let white = Rgba([255, 255, 255, 255]);

	let mid = CANVAS as i32 / 2;
	let shaft_half = (CANVAS as f32 * 0.045).round() as i32;
	let (shaft_start, shaft_end) = ((CANVAS as f32 * 0.28) as i32, (CANVAS as f32 * 0.66) as i32);
	let head = (CANVAS as f32 * 0.20) as i32;

	for y in (mid - shaft_half)..=(mid + shaft_half) {
		for x in shaft_start..=shaft_end {
			let x = if pointing_right { x } else { CANVAS as i32 - x };
			blend(&mut canvas, x, y, white, 1.0);
		}
	}

	// Triangular head: each step in from the tip widens the arm by one pixel.
	let tip = if pointing_right { (CANVAS as f32 * 0.76) as i32 } else { CANVAS as i32 - (CANVAS as f32 * 0.76) as i32 };
	for step in 0..head {
		let x = if pointing_right { tip - step } else { tip + step };
		for y in (mid - step)..=(mid + step) {
			blend(&mut canvas, x, y, white, 1.0);
		}
	}

	canvas
}

/// Create the shipped page commands on first run, if they are not already there.
fn ensure_predefined() {
	let root = predefined_dir();
	for (slug, name, uuid, colour, pointing_right) in [
		("page-left", "Page Left", crate::shared::PAGE_LEFT_UUID, "#3B82F6", false),
		("page-right", "Page Right", crate::shared::PAGE_RIGHT_UUID, "#22C55E", true),
	] {
		let directory = root.join(slug);
		if directory.join("config.json").exists() {
			continue;
		}
		if let Err(error) = std::fs::create_dir_all(&directory) {
			log::error!("Failed to create {}: {error}", directory.display());
			continue;
		}

		if let Err(error) = write_picture(arrow_picture(colour, pointing_right), &directory.join(PICTURE)) {
			log::error!("Failed to draw {name} artwork: {error}");
			continue;
		}

		let config = CustomActionConfig {
			id: new_id(),
			name: name.to_owned(),
			command: String::new(),
			action: Some(uuid.to_owned()),
			image: PICTURE.to_owned(),
			icon: None,
			background: Some(colour.to_owned()),
		};
		if let Err(error) = write_config(&directory, &config) {
			log::error!("Failed to write {name} config: {error}");
		}
	}
}

/// The user's own action library.
pub fn load() -> Vec<CustomAction> {
	load_from(customs_dir())
}

/// The shipped page commands, generating them on first run.
pub fn load_predefined() -> Vec<CustomAction> {
	ensure_predefined();
	load_from(predefined_dir())
}

/// Read every action in a library, skipping any directory whose config will not parse.
fn load_from(root: PathBuf) -> Vec<CustomAction> {
	let Ok(entries) = std::fs::read_dir(&root) else {
		return Vec::new();
	};

	let mut actions: Vec<CustomAction> = entries
		.flatten()
		.filter(|entry| entry.path().is_dir())
		.filter_map(|entry| {
			let slug = entry.file_name().to_string_lossy().into_owned();
			let mut config: CustomActionConfig = serde_json::from_slice(&std::fs::read(entry.path().join("config.json")).ok()?).ok()?;

			// Actions written before ids existed get one now, so they can be linked from here on.
			if config.id.is_empty() {
				config.id = new_id();
				let _ = write_config(&root.join(&slug), &config);
			}
			Some(CustomAction {
				slug,
				root: root.clone(),
				config,
			})
		})
		.collect();

	actions.sort_by(|a, b| a.config.name.cmp(&b.config.name));
	actions
}

/// How the user chose to picture the action: a background colour, an image, or both.
///
/// There is deliberately no separate "icon" pick - one image covers both cases, and how it is
/// placed is decided from the image itself (see [`compose`]).
#[derive(Clone, Default)]
pub struct ImageSpec {
	pub file: Option<PathBuf>,
	pub background: Option<String>,
}

impl ImageSpec {
	pub fn is_empty(&self) -> bool {
		self.file.is_none() && self.background.is_none()
	}
}

/// How a source image is made to fit the square key face.
#[derive(Clone, Copy, PartialEq)]
pub enum Fit {
	/// Fill the key, cropping the overflowing edges - a photo of any shape becomes a square key
	/// without distortion, keeping the middle.
	Cover,
	/// Scale to fit inside the key, leaving the rest transparent - right for icons, which must not
	/// be cropped.
	Contain,
}

/// Decode any supported image at the requested size, rasterising SVG through resvg since the
/// `image` crate cannot.
fn decode(path: &Path, width: u32, height: u32, fit: Fit) -> Result<RgbaImage> {
	if path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("svg")) {
		return decode_svg(path, width, height, fit);
	}

	let source = image::open(path)?;
	let (source_width, source_height) = (source.width().max(1), source.height().max(1));

	let scale_x = width as f32 / source_width as f32;
	let scale_y = height as f32 / source_height as f32;
	// Cover takes the larger scale so the image overflows and gets cropped; contain takes the
	// smaller so all of it fits.
	let scale = if fit == Fit::Cover { scale_x.max(scale_y) } else { scale_x.min(scale_y) };

	let scaled_width = (source_width as f32 * scale).round().max(1.0) as u32;
	let scaled_height = (source_height as f32 * scale).round().max(1.0) as u32;
	let scaled = source.resize_exact(scaled_width, scaled_height, FilterType::Lanczos3).to_rgba8();

	// Centre the result, cropping or padding as needed.
	let mut canvas = RgbaImage::new(width, height);
	let offset_x = (width as i32 - scaled_width as i32) / 2;
	let offset_y = (height as i32 - scaled_height as i32) / 2;
	for (x, y, pixel) in scaled.enumerate_pixels() {
		let (target_x, target_y) = (offset_x + x as i32, offset_y + y as i32);
		if target_x >= 0 && target_y >= 0 && (target_x as u32) < width && (target_y as u32) < height {
			canvas.put_pixel(target_x as u32, target_y as u32, *pixel);
		}
	}
	Ok(canvas)
}

fn decode_svg(path: &Path, width: u32, height: u32, fit: Fit) -> Result<RgbaImage> {
	let data = std::fs::read(path)?;
	let tree = resvg::usvg::Tree::from_data(&data, &resvg::usvg::Options::default())?;

	let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height).ok_or_else(|| anyhow!("invalid pixmap size {width}x{height}"))?;
	let size = tree.size();
	let scale_x = width as f32 / size.width();
	let scale_y = height as f32 / size.height();
	let scale = if fit == Fit::Cover { scale_x.max(scale_y) } else { scale_x.min(scale_y) };

	let transform =
		resvg::tiny_skia::Transform::from_translate((width as f32 - size.width() * scale) / 2.0, (height as f32 - size.height() * scale) / 2.0)
			.pre_scale(scale, scale);
	resvg::render(&tree, transform, &mut pixmap.as_mut());

	// tiny-skia hands back premultiplied RGBA; undo that so it composites like any other image.
	let mut rgba = RgbaImage::new(width, height);
	for (pixel, target) in pixmap.pixels().iter().zip(rgba.pixels_mut()) {
		let alpha = pixel.alpha();
		*target = if alpha == 0 {
			Rgba([0, 0, 0, 0])
		} else {
			let unmultiply = |channel: u8| ((channel as u32 * 255) / alpha as u32).min(255) as u8;
			Rgba([unmultiply(pixel.red()), unmultiply(pixel.green()), unmultiply(pixel.blue()), alpha])
		};
	}
	Ok(rgba)
}

/// Write the finished key face, dropping the alpha channel when nothing is transparent - a fully
/// opaque key is a third smaller as RGB, and these files are written per action.
fn write_picture(canvas: RgbaImage, path: &Path) -> Result<()> {
	if canvas.pixels().all(|pixel| pixel[3] == 255) {
		image::DynamicImage::ImageRgba8(canvas).to_rgb8().save(path)?;
	} else {
		canvas.save(path)?;
	}
	Ok(())
}

/// Does this image have transparency worth preserving - i.e. is it an icon rather than a picture?
pub fn has_transparency(path: &Path) -> bool {
	if path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("svg")) {
		return true;
	}
	image::open(path).map(|image| image.to_rgba8().pixels().any(|pixel| pixel[3] < 250)).unwrap_or(false)
}

/// Build the action's artwork inside its directory, copying the source image in alongside it.
///
/// The background is always painted first, then the image composited over it. How the image is
/// placed depends on the image itself, so one picker covers both cases: a transparent icon is
/// scaled to fit and inset so the colour reads as a border around it, while an opaque picture is
/// centre-cropped to fill the key (where the background simply ends up hidden behind it).
///
/// Returns the copied source's filename, if there was one.
fn compose(directory: &Path, spec: &ImageSpec) -> Result<Option<String>> {
	std::fs::create_dir_all(directory)?;

	let mut canvas = RgbaImage::new(CANVAS, CANVAS);
	if let Some(background) = &spec.background {
		let colour = parse_colour(background);
		for pixel in canvas.pixels_mut() {
			*pixel = colour;
		}
	}

	let mut source_name = None;

	if let Some(file) = &spec.file {
		// Keep the source so a later edit can recompose it against a different background.
		let extension = file.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_else(|| "png".to_owned());
		let name = format!("icon.{extension}");
		let stored = directory.join(&name);

		// On a background-only edit the source *is* the stored copy; copying a file onto itself
		// truncates it to nothing, destroying the artwork.
		if file != &stored {
			std::fs::copy(file, &stored)?;
		}
		source_name = Some(name);

		let (size, fit) = if has_transparency(file) {
			((CANVAS as f32 * 0.8).round() as u32, Fit::Contain)
		} else {
			(CANVAS, Fit::Cover)
		};

		let decoded = decode(file, size, size, fit)?;
		let offset = (CANVAS as i32 - size as i32) / 2;
		for (x, y, pixel) in decoded.enumerate_pixels() {
			blend(&mut canvas, offset + x as i32, offset + y as i32, *pixel, pixel[3] as f32 / 255.0);
		}
	}

	write_picture(canvas, &directory.join(PICTURE))?;
	Ok(source_name)
}

fn write_config(directory: &Path, config: &CustomActionConfig) -> Result<()> {
	let mut store = Store::new("config", directory, CustomActionConfig::default())?;
	store.value = config.clone();
	store.save()
}

fn save_config(slug: &str, config: &CustomActionConfig) -> Result<()> {
	write_config(&directory(slug), config)
}

/// Create a custom action, giving it its own directory.
pub fn create(name: String, command: String, spec: &ImageSpec) -> Result<CustomAction> {
	let slug = unique_slug(&name, None);
	let icon = compose(&directory(&slug), spec)?;

	let config = CustomActionConfig {
		id: new_id(),
		name,
		command,
		action: None,
		image: PICTURE.to_owned(),
		icon,
		background: spec.background.clone(),
	};

	save_config(&slug, &config)?;
	Ok(CustomAction {
		slug,
		root: customs_dir(),
		config,
	})
}

/// Update an existing action, moving its directory if the name changed.
///
/// The artwork is only recomposed when the user picked something new, so editing just the name or
/// command leaves the existing image alone.
pub fn update(slug: &str, name: String, command: String, spec: &ImageSpec) -> Result<CustomAction> {
	let mut config: CustomActionConfig = serde_json::from_slice(&std::fs::read(directory(slug).join("config.json"))?)?;

	// Keep the directory named after the action.
	let new_slug = if config.name == name { slug.to_owned() } else { unique_slug(&name, Some(slug)) };
	if new_slug != slug {
		std::fs::rename(directory(slug), directory(&new_slug))?;
	}

	config.name = name;
	config.command = command;

	if !spec.is_empty() {
		// Changing only the background must not discard the artwork: fall back to the source image
		// copied into the action's directory when the user did not pick a new one. This is exactly
		// why the source is kept alongside the composited result.
		//
		// A zero-length file is treated as absent - an earlier bug could truncate the stored source,
		// and recomposing from it would replace the picture with a blank colour block.
		let mut spec = spec.clone();
		if spec.file.is_none() {
			spec.file = config
				.icon
				.as_ref()
				.map(|icon| directory(&new_slug).join(icon))
				.filter(|path| std::fs::metadata(path).map(|meta| meta.len() > 0).unwrap_or(false));
		}
		// A colour left unset in the form keeps whatever the action already had.
		if spec.background.is_none() {
			spec.background = config.background.clone();
		}

		config.icon = compose(&directory(&new_slug), &spec)?;
		config.image = PICTURE.to_owned();
		config.background = spec.background.clone();
	}

	save_config(&new_slug, &config)?;
	Ok(CustomAction {
		slug: new_slug,
		root: customs_dir(),
		config,
	})
}

/// Remove an action and everything it owns.
pub fn delete(slug: &str) -> Result<()> {
	std::fs::remove_dir_all(directory(slug))?;
	Ok(())
}
