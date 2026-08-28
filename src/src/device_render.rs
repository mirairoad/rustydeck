//! Composites key images for physical Stream Deck hardware.
//!
//! The old Svelte frontend did this in an HTML `<canvas>` (`src/lib/rendererHelper.ts`) and handed
//! the finished bitmap to the backend as a JPEG data URI. The backend never composites anything
//! itself - `elgato::update_image` only base64-decodes what it is given and writes it to the
//! device - so this job belongs to the shell, and this module is a faithful port of that canvas
//! drawing logic (same 144px canvas, same scale/offset/baseline maths).

use crate::shared::{ActionState, config_dir};

use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, RwLock};

use ab_glyph::{Font as _, FontVec, PxScale, ScaleFont as _};
use anyhow::{Result, anyhow};
use base64::Engine as _;
use image::{DynamicImage, Rgba, RgbaImage, imageops::FilterType};

/// The canvas size the old frontend rendered at. `elgato-streamdeck` downscales to whatever the
/// specific model needs (72/80/96px depending on Kind), so this one size covers every Stream Deck.
pub const CANVAS: u32 = 144;

/// Action state images are stored either as an absolute path (user-uploaded custom images, under
/// `config_dir/images/...`) or as a path relative to `config_dir` (plugin-bundled icons, already
/// resolved to a real extension by `plugins::initialise_plugin` - note this is *not* the job of
/// `shared::convert_icon`, which only handles raw extension-less manifest paths).
pub fn resolve_image_path(image: &str) -> PathBuf {
	let path = Path::new(image);
	if path.is_absolute() { path.to_path_buf() } else { config_dir().join(path) }
}

/// Parse a `#RRGGBB` or `#RRGGBBAA` colour, as stored in an [`ActionState`].
pub fn parse_colour(colour: &str) -> Rgba<u8> {
	let hex = colour.trim_start_matches('#');
	let component = |i: usize| u8::from_str_radix(hex.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0);
	match hex.len() {
		6 => Rgba([component(0), component(2), component(4), 255]),
		8 => Rgba([component(0), component(2), component(4), component(6)]),
		_ => Rgba([0, 0, 0, 255]),
	}
}

type FontCache = HashMap<(String, String), Option<Arc<FontVec>>>;
static FONTS: LazyLock<RwLock<FontCache>> = LazyLock::new(|| RwLock::new(HashMap::new()));

/// Look up a system font by the family/style recorded in an [`ActionState`], with caching -
/// resolving and parsing a font takes long enough to matter when redrawing a whole profile.
fn load_font(family: &str, style: &str) -> Option<Arc<FontVec>> {
	let key = (family.to_owned(), style.to_owned());
	if let Some(cached) = FONTS.read().ok()?.get(&key) {
		return cached.clone();
	}

	let font = resolve_font(family, style).map(Arc::new);
	if let Ok(mut cache) = FONTS.write() {
		cache.insert(key, font.clone());
	}
	font
}

fn resolve_font(family: &str, style: &str) -> Option<FontVec> {
	use font_kit::family_name::FamilyName;
	use font_kit::properties::{Properties, Style, Weight};
	use font_kit::source::SystemSource;

	let mut properties = Properties::new();
	if style.to_lowercase().contains("bold") {
		properties.weight = Weight::BOLD;
	}
	if style.to_lowercase().contains("italic") {
		properties.style = Style::Italic;
	}

	let families = [FamilyName::Title(family.to_owned()), FamilyName::SansSerif];
	let handle = SystemSource::new().select_best_match(&families, &properties).ok()?;
	let data = handle.load().ok()?.copy_font_data()?;
	FontVec::try_from_vec(data.to_vec()).ok()
}

/// Alpha-blend a single coverage sample onto the canvas.
pub fn blend(canvas: &mut RgbaImage, x: i32, y: i32, colour: Rgba<u8>, coverage: f32) {
	if x < 0 || y < 0 || x >= canvas.width() as i32 || y >= canvas.height() as i32 {
		return;
	}
	let alpha = coverage.clamp(0.0, 1.0) * (colour[3] as f32 / 255.0);
	if alpha <= 0.0 {
		return;
	}

	let pixel = canvas.get_pixel_mut(x as u32, y as u32);
	for channel in 0..3 {
		pixel[channel] = (colour[channel] as f32 * alpha + pixel[channel] as f32 * (1.0 - alpha)).round() as u8;
	}
	pixel[3] = ((alpha + (pixel[3] as f32 / 255.0) * (1.0 - alpha)) * 255.0).round() as u8;
}

/// Lay a single line out left-to-right, returning each glyph with its x offset plus the total
/// advance width (needed to centre the line, and to size the underline rule).
fn layout_line(font: &FontVec, scale: PxScale, line: &str) -> (Vec<(ab_glyph::Glyph, f32)>, f32) {
	let scaled = font.as_scaled(scale);
	let mut glyphs = Vec::new();
	let mut width = 0.0;
	let mut previous: Option<ab_glyph::GlyphId> = None;

	for character in line.chars() {
		let glyph = scaled.scaled_glyph(character);
		if let Some(previous) = previous {
			width += scaled.kern(previous, glyph.id);
		}
		previous = Some(glyph.id);
		let advance = scaled.h_advance(glyph.id);
		glyphs.push((glyph, width));
		width += advance;
	}

	(glyphs, width)
}

fn draw_line(canvas: &mut RgbaImage, font: &FontVec, scale: PxScale, line: &str, origin_x: f32, top_y: f32, colour: Rgba<u8>) {
	let scaled = font.as_scaled(scale);
	let baseline = top_y + scaled.ascent();
	let (glyphs, _) = layout_line(font, scale, line);

	for (glyph, offset) in glyphs {
		let mut glyph = glyph;
		glyph.position = ab_glyph::point(origin_x + offset, baseline);
		if let Some(outline) = font.outline_glyph(glyph) {
			let bounds = outline.px_bounds();
			outline.draw(|x, y, coverage| {
				blend(canvas, bounds.min.x as i32 + x as i32, bounds.min.y as i32 + y as i32, colour, coverage);
			});
		}
	}
}

/// Render one action state to a JPEG data URI, ready to hand to
/// `events::frontend::instances::update_image`.
///
/// `width`/`height` are the canvas to draw into: square for keys and encoders, but the Neo's
/// infobar is a wide letterbox, and (as in the old renderer) text and stroke sizes scale with the
/// canvas height while the icon is stretched to the canvas aspect.
pub fn render_state(state: &ActionState, width: u32, height: u32) -> Result<String> {
	let _timed = crate::shared::Timed::start(format!("render_state {width}x{height}"));
	let mut canvas = RgbaImage::new(width, height);

	// The old renderer deliberately leaves a pure-black background transparent so that only
	// explicitly chosen colours are painted; JPEG encoding flattens it to black either way.
	if !state.background_colour.starts_with("#000000") {
		let background = parse_colour(&state.background_colour);
		for pixel in canvas.pixels_mut() {
			*pixel = background;
		}
	}

	if let Err(error) = draw_icon(&mut canvas, &state.image, state.image_scale) {
		log::warn!("Failed to draw icon {}: {error}", state.image);
	}

	if state.show && !state.text.trim().is_empty() {
		draw_title(&mut canvas, state);
	}

	encode_jpeg(canvas)
}

fn draw_icon(canvas: &mut RgbaImage, image: &str, image_scale: u8) -> Result<()> {
	if image.is_empty() {
		return Ok(());
	}

	let path = resolve_image_path(image);
	if path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("svg")) {
		// Rasterising SVG needs a full renderer (resvg); plugin action icons are PNG in practice.
		return Err(anyhow!("SVG icons are not supported yet"));
	}

	let icon = image::open(&path)?;

	// Matches the old renderer: clamp the scale to a 10% floor, stretch the icon to the canvas
	// aspect, then centre it.
	let scale = image_scale.max(10) as f32 / 100.0;
	let scaled_width = (canvas.width() as f32 * scale).round().max(1.0) as u32;
	let scaled_height = (canvas.height() as f32 * scale).round().max(1.0) as u32;
	let icon = icon.resize_exact(scaled_width, scaled_height, FilterType::Lanczos3).to_rgba8();
	let offset_x = (canvas.width() as i32 - scaled_width as i32) / 2;
	let offset_y = (canvas.height() as i32 - scaled_height as i32) / 2;

	for (x, y, pixel) in icon.enumerate_pixels() {
		blend(canvas, offset_x + x as i32, offset_y + y as i32, *pixel, pixel[3] as f32 / 255.0);
	}

	Ok(())
}

fn draw_title(canvas: &mut RgbaImage, state: &ActionState) {
	let Some(font) = load_font(&state.family, &state.style) else {
		log::warn!("No system font matched {} ({}); skipping title", state.family, state.style);
		return;
	};

	// The canvas renderer used `state.size * 2` because it drew into a 144px canvas while the
	// stored size is expressed against a 72px key, and scaled that by the canvas height so a
	// letterboxed infobar gets proportionally smaller text.
	let canvas_scale = canvas.height() as f32 / CANVAS as f32;
	let size = state.size.0 as f32 * 2.0 * canvas_scale;
	let scale = PxScale::from(size);
	let stroke = state.stroke_size.0 as f32 * canvas_scale;
	let colour = parse_colour(&state.colour);
	let stroke_colour = parse_colour(&state.stroke_colour);

	let lines: Vec<&str> = state.text.split('\n').collect();
	let centre_x = canvas.width() as f32 / 2.0;
	let top_y = match state.alignment.as_str() {
		"top" => stroke,
		"bottom" => canvas.height() as f32 - size * lines.len() as f32 - stroke,
		_ => canvas.height() as f32 / 2.0 - size * lines.len() as f32 * 0.5,
	};

	for (index, line) in lines.iter().enumerate() {
		let line_y = top_y + size * index as f32;
		let (_, width) = layout_line(&font, scale, line);
		let origin_x = centre_x - width / 2.0;

		// Canvas `strokeText` centres a stroke of `lineWidth` on the glyph outline. Redrawing the
		// glyphs around a small circle approximates that closely enough at key resolution.
		if stroke > 0.0 {
			let radius = stroke / 2.0;
			for step in 0..8 {
				let angle = std::f32::consts::TAU * step as f32 / 8.0;
				draw_line(canvas, &font, scale, line, origin_x + radius * angle.cos(), line_y + radius * angle.sin(), stroke_colour);
			}
		}

		draw_line(canvas, &font, scale, line, origin_x, line_y, colour);

		if state.underline {
			// Black outline rule first, then the text-coloured rule inset within it.
			fill_rect(canvas, origin_x - 3.0, line_y + size, width + 6.0, 9.0, Rgba([0, 0, 0, 255]));
			fill_rect(canvas, origin_x, line_y + size + 4.0, width, 3.0, colour);
		}
	}
}

fn fill_rect(canvas: &mut RgbaImage, x: f32, y: f32, width: f32, height: f32, colour: Rgba<u8>) {
	for offset_y in 0..height.round().max(0.0) as i32 {
		for offset_x in 0..width.round().max(0.0) as i32 {
			blend(canvas, x.round() as i32 + offset_x, y.round() as i32 + offset_y, colour, 1.0);
		}
	}
}

/// Flatten onto black (JPEG has no alpha channel) and encode as a data URI, the same shape the
/// backend already expects from the old `canvas.toDataURL("image/jpeg")` call.
fn encode_jpeg(canvas: RgbaImage) -> Result<String> {
	let mut flattened = image::RgbImage::new(canvas.width(), canvas.height());
	for (x, y, pixel) in canvas.enumerate_pixels() {
		let alpha = pixel[3] as f32 / 255.0;
		flattened.put_pixel(
			x,
			y,
			image::Rgb([
				(pixel[0] as f32 * alpha).round() as u8,
				(pixel[1] as f32 * alpha).round() as u8,
				(pixel[2] as f32 * alpha).round() as u8,
			]),
		);
	}

	let mut buffer = Vec::new();
	DynamicImage::ImageRgb8(flattened).write_to(&mut Cursor::new(&mut buffer), image::ImageFormat::Jpeg)?;
	Ok(format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(&buffer)))
}
