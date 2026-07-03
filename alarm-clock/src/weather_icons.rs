//! Weather icon rendering from Meteocons animated SVGs.
//!
//! Slint cannot render SVG natively, so this module uses `resvg`/`usvg`/
//! `tiny-skia` to rasterize the embedded Meteocons SVGs to pixel buffers.
//!
//! Each Meteocons SVG contains SMIL `<animateTransform>`/`<animate>` elements
//! that are not supported by `usvg`. We work around this by:
//! 1. Extracting the animation parameters (type, values, duration) from the
//!    SVG text via simple string parsing.
//! 2. Stripping the `<animate*>` elements from the SVG.
//! 3. For each animation frame, injecting the computed transform/opacity as
//!    a regular `transform`/`opacity` attribute on the parent group.
//! 4. Rasterizing the modified SVG with `resvg`.
//!
//! The resulting frame sequence is exposed as `Vec<slint::Image>` and cycled
//! by a Slint timer in the UI.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;
use tiny_skia::Pixmap;
use tracing::{info, warn};

/// Number of animation frames to pre-render per icon.
const NUM_FRAMES: usize = 12;

/// Render size (pixels) for each icon frame.
const ICON_SIZE: u32 = 64;

/// A single animated icon: a sequence of pre-rendered frames.
pub struct AnimatedIcon {
    pub frames: Vec<slint::Image>,
    pub frame_duration_ms: u32,
}

/// WMO weather code → meteocons icon slug mapping.
fn wmo_to_slug(wmo_code: i32) -> &'static str {
    match wmo_code {
        0 => "clear-day",
        1 => "partly-cloudy-day",
        2 => "partly-cloudy-day",
        3 => "overcast-day",
        45 | 48 => "fog-day",
        51 | 53 | 55 => "drizzle",
        56 | 57 => "drizzle",
        61 | 63 | 65 => "rain",
        66 | 67 => "rain",
        71 | 73 | 75 => "snow",
        77 => "snow",
        80 | 81 | 82 => "rain",
        85 | 86 => "snow",
        95 => "thunderstorms",
        96 | 99 => "thunderstorms",
        _ => "clear-day",
    }
}

/// Embedded SVG sources, keyed by slug.
fn svg_sources() -> &'static HashMap<&'static str, &'static str> {
    static SVGS: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    SVGS.get_or_init(|| {
        let mut m = HashMap::new();
        m.insert("clear-day", include_str!("../assets/weather/clear-day.svg"));
        m.insert("cloudy", include_str!("../assets/weather/cloudy.svg"));
        m.insert("overcast-day", include_str!("../assets/weather/overcast-day.svg"));
        m.insert("partly-cloudy-day", include_str!("../assets/weather/partly-cloudy-day.svg"));
        m.insert("fog-day", include_str!("../assets/weather/fog-day.svg"));
        m.insert("drizzle", include_str!("../assets/weather/drizzle.svg"));
        m.insert("rain", include_str!("../assets/weather/rain.svg"));
        m.insert("snow", include_str!("../assets/weather/snow.svg"));
        m.insert("thunderstorms", include_str!("../assets/weather/thunderstorms.svg"));
        m.insert("mist", include_str!("../assets/weather/mist.svg"));
        m.insert("hail", include_str!("../assets/weather/hail.svg"));
        m.insert("overcast-day-rain", include_str!("../assets/weather/overcast-day-rain.svg"));
        m.insert("overcast-day-snow", include_str!("../assets/weather/overcast-day-snow.svg"));
        m
    })
}

// ── Animation parameter extraction ───────────────────────────────────────

/// Parsed animation parameters for a single SVG group.
struct Animation {
    /// The `transform` attribute value to inject at each frame, indexed by
    /// frame number (0..NUM_FRAMES).
    transforms: Vec<String>,
    /// The `opacity` value to inject at each frame, or `None` if no opacity
    /// animation is present.
    opacities: Vec<Option<f32>>,
}

/// Extract animation parameters from an SVG string.
///
/// Looks for `<animateTransform>` (rotate/translate) and `<animate>` (opacity)
/// elements, samples their values across `NUM_FRAMES`, and returns:
/// - The SVG with `<animate*>` elements stripped.
/// - A list of per-frame transform strings and opacity values.
fn extract_animation(svg: &str) -> (String, Animation) {
    // Parse animateTransform: type, values, dur, begin.
    let (transform_type, transform_values, dur_s, begin_s) = parse_animate_transform(svg);

    // Parse animate (opacity): values, keyTimes, dur, begin.
    let (opacity_values, opacity_key_times, opacity_dur, opacity_begin) = parse_animate_opacity(svg);

    // Generate per-frame transforms.
    let transforms: Vec<String> = (0..NUM_FRAMES)
        .map(|i| {
            let t = i as f32 / NUM_FRAMES as f32; // 0.0 .. 1.0
            if let (Some(ref tv), Some(ref vals), Some(dur), Some(begin)) =
                (&transform_type, &transform_values, dur_s, begin_s)
            {
                let adj_t = adjust_time(t, dur, begin);
                let v = sample_values(vals, adj_t);
                match tv.as_str() {
                    "rotate" => {
                        // values like "0 64 64;360 64 64"
                        let parts: Vec<&str> = v.split_whitespace().collect();
                        if parts.len() >= 1 {
                            let angle: f32 = parts[0].parse().unwrap_or(0.0);
                            let cx = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(64.0);
                            let cy = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(64.0);
                            format!("rotate({} {} {})", angle, cx, cy)
                        } else {
                            String::new()
                        }
                    }
                    "translate" => {
                        // values like "0 0;0 -3;0 0"
                        let parts: Vec<&str> = v.split_whitespace().collect();
                        let x: f32 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0.0);
                        let y: f32 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0.0);
                        format!("translate({} {})", x, y)
                    }
                    _ => String::new(),
                }
            } else {
                String::new()
            }
        })
        .collect();

    // Generate per-frame opacities.
    let opacities: Vec<Option<f32>> = (0..NUM_FRAMES)
        .map(|i| {
            let t = i as f32 / NUM_FRAMES as f32;
            if let (Some(ref vals), Some(dur), Some(begin)) =
                (&opacity_values, opacity_dur, opacity_begin)
            {
                let adj_t = adjust_time(t, dur, begin);
                let key_t = opacity_key_times.as_ref();
                let v = sample_opacity(vals, key_t, adj_t);
                Some(v)
            } else {
                None
            }
        })
        .collect();

    // Strip all <animate*> elements from the SVG.
    let stripped = strip_animations(svg);

    (stripped, Animation { transforms, opacities })
}

fn adjust_time(t: f32, dur: f32, begin: f32) -> f32 {
    // Normalize begin offset into the [0, dur) range.
    let dur = if dur <= 0.0 { 1.0 } else { dur };
    let mut time = (t * dur) + begin;
    // Wrap into [0, dur).
    while time < 0.0 {
        time += dur;
    }
    time %= dur;
    time / dur // return as fraction of duration
}

fn parse_animate_transform(svg: &str) -> (Option<String>, Option<Vec<String>>, Option<f32>, Option<f32>) {
    // Find <animateTransform .../>
    let start = match svg.find("<animateTransform") {
        Some(s) => s,
        None => return (None, None, None, None),
    };
    let end = match svg[start..].find("/>") {
        Some(e) => start + e + 2,
        None => return (None, None, None, None),
    };
    let tag = &svg[start..end];

    let atype = extract_attr(tag, "type");
    let values_str = extract_attr(tag, "values");
    let dur = extract_attr(tag, "dur").and_then(|s| parse_duration(&s));
    let begin = extract_attr(tag, "begin").and_then(|s| parse_duration(&s)).or(Some(0.0));

    let values = values_str.map(|s| s.split(';').map(|v| v.trim().to_string()).collect());

    (atype, values, dur, begin)
}

fn parse_animate_opacity(svg: &str) -> (Option<Vec<String>>, Option<Vec<f32>>, Option<f32>, Option<f32>) {
    // Find <animate .../> (but not <animateTransform>)
    let mut search_start = 0;
    loop {
        let start = match svg[search_start..].find("<animate") {
            Some(s) => search_start + s,
            None => return (None, None, None, None),
        };
        // Skip if this is actually <animateTransform
        if svg[start..].starts_with("<animateTransform") {
            search_start = start + 1;
            continue;
        }
        let end = match svg[start..].find("/>") {
            Some(e) => start + e + 2,
            None => return (None, None, None, None),
        };
        let tag = &svg[start..end];

        // Only interested in opacity animations.
        let attr = extract_attr(tag, "attributeName");
        if attr.as_deref() != Some("opacity") {
            search_start = end;
            continue;
        }

        let values_str = extract_attr(tag, "values");
        let key_times_str = extract_attr(tag, "keyTimes");
        let dur = extract_attr(tag, "dur").and_then(|s| parse_duration(&s));
        let begin = extract_attr(tag, "begin").and_then(|s| parse_duration(&s)).or(Some(0.0));

        let values = values_str.map(|s| s.split(';').map(|v| v.trim().to_string()).collect());
        let key_times = key_times_str.map(|s| {
            s.split(';').filter_map(|v| v.trim().parse::<f32>().ok()).collect()
        });

        return (values, key_times, dur, begin);
    }
}

fn extract_attr(tag: &str, name: &str) -> Option<String> {
    let pattern = format!("{}=\"", name);
    let start = tag.find(&pattern)? + pattern.len();
    let end = tag[start..].find('"')?;
    Some(tag[start..start + end].to_string())
}

fn parse_duration(s: &str) -> Option<f32> {
    let s = s.trim();
    if s.ends_with('s') {
        s[..s.len() - 1].parse::<f32>().ok()
    } else {
        s.parse::<f32>().ok()
    }
}

fn sample_values(values: &[String], t: f32) -> String {
    if values.is_empty() {
        return String::new();
    }
    if values.len() == 1 {
        return values[0].clone();
    }
    // Linear interpolation between values at fraction t.
    let idx_f = t * (values.len() - 1) as f32;
    let idx = idx_f.floor() as usize;
    let frac = idx_f - idx as f32;
    let next = (idx + 1).min(values.len() - 1);

    // For transform values, just pick the nearest (they're usually small sets).
    // Round to nearest to avoid weird interpolation of transform strings.
    if frac < 0.5 {
        values[idx].clone()
    } else {
        values[next].clone()
    }
}

fn sample_opacity(values: &[String], key_times: Option<&Vec<f32>>, t: f32) -> f32 {
    if values.is_empty() {
        return 1.0;
    }
    let floats: Vec<f32> = values.iter().filter_map(|v| v.parse().ok()).collect();
    if floats.is_empty() {
        return 1.0;
    }
    if let Some(kt) = key_times {
        if kt.len() == floats.len() && kt.len() >= 2 {
            // Interpolate using keyTimes.
            for i in 0..kt.len() - 1 {
                if t >= kt[i] && t <= kt[i + 1] {
                    let range = kt[i + 1] - kt[i];
                    let frac = if range > 0.0 { (t - kt[i]) / range } else { 0.0 };
                    return floats[i] + (floats[i + 1] - floats[i]) * frac;
                }
            }
        }
    }
    // Fallback: linear across values.
    let idx_f = t * (floats.len() - 1) as f32;
    let idx = idx_f.floor() as usize;
    let frac = idx_f - idx as f32;
    let next = (idx + 1).min(floats.len() - 1);
    floats[idx] + (floats[next] - floats[idx]) * frac
}

fn strip_animations(svg: &str) -> String {
    let mut result = svg.to_string();
    // Remove <animateTransform .../> and <animate .../> elements.
    loop {
        let removed = remove_first_tag(&mut result, "<animateTransform");
        if !removed {
            break;
        }
    }
    loop {
        // Remove <animate .../> but not <animateTransform (already removed).
        let removed = remove_first_tag(&mut result, "<animate");
        if !removed {
            break;
        }
    }
    result
}

fn remove_first_tag(svg: &mut String, tag_start: &str) -> bool {
    if let Some(start) = svg.find(tag_start) {
        if let Some(end_rel) = svg[start..].find("/>") {
            let end = start + end_rel + 2;
            svg.replace_range(start..end, "");
            return true;
        }
        // Handle </animate> style closing (shouldn't happen for SMIL, but just in case).
        if let Some(end_rel) = svg[start..].find('>') {
            let end = start + end_rel + 1;
            svg.replace_range(start..end, "");
            return true;
        }
    }
    false
}

// ── SVG rasterization ──────────────────────────────────────────────────

/// Rasterize an SVG string to a `slint::Image` at `ICON_SIZE`×`ICON_SIZE`.
fn render_svg(svg: &str) -> Option<slint::Image> {
    let tree = usvg::Tree::from_str(svg, &usvg::Options::default()).ok()?;

    let mut pixmap = Pixmap::new(ICON_SIZE, ICON_SIZE)?;
    // The SVGs have a 0 0 128 128 viewBox.
    let scale = ICON_SIZE as f32 / 128.0;
    let transform = tiny_skia::Transform::from_scale(scale, scale);

    resvg::render(&tree, transform, &mut pixmap.as_mut());

    Some(slint::Image::from_rgba8_premultiplied(
        slint::SharedPixelBuffer::clone_from_slice(
            pixmap.data(),
            ICON_SIZE,
            ICON_SIZE,
        ),
    ))
}

/// Inject a transform and/or opacity into the SVG by modifying the first `<g>`
/// element that precedes the animation (the parent group).
fn inject_frame(svg: &str, transform: &str, opacity: Option<f32>) -> String {
    // Strategy: find the first `<g` that doesn't have a `transform` yet, or
    // just modify the root `<g>`. Actually, the simplest reliable approach:
    // wrap the entire content in an extra `<g transform="..." opacity="...">`
    // right after the root `<svg>` tag. But this would transform the whole
    // icon, not just the animated group.
    //
    // For correctness we should target the specific animated group. The SMIL
    // `<animateTransform>` is a child of the group it animates. After stripping
    // the animation element, the parent group remains. We find the first `<g`
    // and inject the transform there.
    //
    // However, some SVGs have nested groups. The animation element is always
    // the last child of its parent group before `</g>`. We can find the
    // position where the animation was, and target the enclosing `<g>`.

    // Simple approach: find the first `<g` tag and inject transform/opacity.
    if let Some(g_pos) = svg.find("<g") {
        // Find the end of this <g ...> tag.
        if let Some(g_end) = svg[g_pos..].find('>') {
            let insert_pos = g_pos + g_end;
            let g_tag = &svg[g_pos..=insert_pos];

            let mut new_tag = g_tag.to_string();
            // Add transform if provided.
            if !transform.is_empty() && !g_tag.contains("transform=") {
                new_tag = new_tag.replace(">", &format!(" transform=\"{}\">", transform));
            } else if !transform.is_empty() {
                // Replace existing transform.
                new_tag = replace_or_add_attr(&new_tag, "transform", transform);
            }
            // Add opacity if provided.
            if let Some(op) = opacity {
                new_tag = replace_or_add_attr(&new_tag, "opacity", &format!("{:.2}", op));
            }

            let mut result = svg.to_string();
            result.replace_range(g_pos..=insert_pos, &new_tag);
            return result;
        }
    }

    // Fallback: no group found, return as-is.
    svg.to_string()
}

fn replace_or_add_attr(tag: &str, name: &str, value: &str) -> String {
    let attr_pattern = format!("{}=\"", name);
    if tag.contains(&attr_pattern) {
        // Replace existing value.
        let start = tag.find(&attr_pattern).unwrap() + attr_pattern.len();
        let end = tag[start..].find('"').unwrap() + start;
        let mut result = tag.to_string();
        result.replace_range(start..end, value);
        result
    } else {
        // Add new attribute before closing >.
        tag.replace(">", &format!(" {}=\"{}\">", name, value))
    }
}

// ── Icon cache ──────────────────────────────────────────────────────────

/// Global cache of pre-rendered animated icons, keyed by WMO code.
/// Uses `thread_local!` because `slint::Image` is not `Sync` (it holds a
/// reference-counted pixel buffer tied to the Slint runtime).
thread_local! {
    static ICON_CACHE: RefCell<HashMap<i32, AnimatedIcon>> = RefCell::new(HashMap::new());
}

fn ensure_cache_populated() {
    ICON_CACHE.with(|cache| {
        if cache.borrow().is_empty() {
            let mut cache = cache.borrow_mut();
            let codes = [0, 1, 2, 3, 45, 48, 51, 53, 55, 56, 57, 61, 63, 65, 66, 67, 71, 73, 75, 77, 80, 81, 82, 85, 86, 95, 96, 99];
            for code in codes {
                if let Some(icon) = render_icon(code) {
                    cache.insert(code, icon);
                }
            }
            info!("weather icons: pre-rendered {} icons", cache.len());
        }
    });
}

/// Render all animation frames for a given WMO code.
fn render_icon(wmo_code: i32) -> Option<AnimatedIcon> {
    let slug = wmo_to_slug(wmo_code);
    let sources = svg_sources();
    let svg = sources.get(slug).or_else(|| sources.get("clear-day"))?;

    let (stripped, anim) = extract_animation(svg);

    let mut frames = Vec::with_capacity(NUM_FRAMES);
    for i in 0..NUM_FRAMES {
        let frame_svg = inject_frame(&stripped, &anim.transforms[i], anim.opacities[i]);
        match render_svg(&frame_svg) {
            Some(img) => frames.push(img),
            None => {
                warn!(wmo_code, frame = i, "failed to render weather icon frame");
                return None;
            }
        }
    }

    // Estimate frame duration: animations are typically 1-6s; use 1s default
    // so the full cycle plays at a reasonable speed.
    let frame_duration_ms = 1000 / NUM_FRAMES as u32;

    Some(AnimatedIcon {
        frames,
        frame_duration_ms,
    })
}

/// Get the animated icon frames for a WMO code, returning a cloned `Vec`.
/// Returns frames for the fallback (clear-day) icon if the code is unknown.
pub fn get_icon_frames(wmo_code: i32) -> Vec<slint::Image> {
    ensure_cache_populated();
    ICON_CACHE.with(|cache| {
        let cache = cache.borrow();
        cache
            .get(&wmo_code)
            .or_else(|| cache.get(&0))
            .map(|icon| icon.frames.clone())
            .unwrap_or_default()
    })
}

/// Get a single frame (first) for a WMO code. Convenience for initial display.
pub fn get_icon_first_frame(wmo_code: i32) -> Option<slint::Image> {
    ensure_cache_populated();
    ICON_CACHE.with(|cache| {
        let cache = cache.borrow();
        cache
            .get(&wmo_code)
            .or_else(|| cache.get(&0))
            .and_then(|icon| icon.frames.first().cloned())
    })
}
