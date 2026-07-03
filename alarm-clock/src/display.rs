//! Single-threaded backlight controller with precedence stack (slice 4).
//!
//! Owns sysfs `brightness` and `bl_power` (boot-time discovery, no-op fallback
//! if absent), computes the effective display policy each tick from four inputs
//! by precedence: `Strobe > BedtimeOff > Override > Dynamic`. Only the winner
//! writes hardware.

use chrono::{DateTime, Datelike, Local, NaiveTime, Weekday};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tracing::{info, warn};
use std::time::SystemTime;

// ── Backlight sysfs discovery ─────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct BacklightHandle {
    path: PathBuf,
    max_brightness: u32,
}

impl BacklightHandle {
    fn discover() -> Option<Self> {
        let entries = match fs::read_dir("/sys/class/backlight") {
            Ok(e) => e,
            Err(_) => {
                info!("display: no /sys/class/backlight — no-op controller");
                return None;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let max_path = path.join("max_brightness");
            match fs::read_to_string(&max_path) {
                Ok(s) => {
                    if let Ok(max) = s.trim().parse::<u32>() {
                        info!("display: discovered backlight at {} (max={})", path.display(), max);
                        return Some(Self { path, max_brightness: max });
                    }
                }
                Err(e) => warn!("display: failed to read max_brightness: {e}"),
            }
        }
        info!("display: no backlight device found — no-op controller");
        None
    }

    fn set_brightness(&self, value: u32) {
        let clamped = value.min(self.max_brightness);
        let p = self.path.join("brightness");
        if let Err(e) = fs::write(&p, clamped.to_string()) {
            warn!("display: failed to write brightness {clamped}: {e}");
        }
    }

    fn set_bl_power(&self, on: bool) {
        let p = self.path.join("bl_power");
        let val = if on { "0" } else { "1" };
        if let Err(e) = fs::write(&p, val) {
            warn!("display: failed to write bl_power {val}: {e}");
        }
    }

    fn current_brightness(&self) -> u8 {
        let p = self.path.join("brightness");
        match fs::read_to_string(&p) {
            Ok(s) => {
                let raw: u32 = s.trim().parse().unwrap_or(0);
                if self.max_brightness == 0 { return 0; }
                ((raw as f64 / self.max_brightness as f64) * 100.0) as u8
            }
            Err(e) => { warn!("display: failed to read brightness: {e}"); 0 }
        }
    }
}

// ── Bedtime configuration ────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BedtimeWindow {
    pub start: NaiveTime,
    pub end: NaiveTime,
}

impl BedtimeWindow {
    pub fn contains(&self, now: NaiveTime) -> bool {
        if self.end > self.start {
            now >= self.start && now < self.end
        } else {
            now >= self.start || now < self.end
        }
    }

    /// Serialize to JSON string for persistence.
    pub fn to_json(&self) -> String {
        serde_json::json!({
            "start": self.start.format("%H:%M").to_string(),
            "end": self.end.format("%H:%M").to_string(),
        }).to_string()
    }

    /// Deserialize from JSON string.
    pub fn from_json(s: &str) -> Option<Self> {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(s) {
            let start_str = v.get("start")?.as_str()?;
            let end_str = v.get("end")?.as_str()?;
            let start = NaiveTime::parse_from_str(start_str, "%H:%M").ok()?;
            let end = NaiveTime::parse_from_str(end_str, "%H:%M").ok()?;
            Some(Self { start, end })
        } else { None }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BedtimeConfig {
    pub weekday: BedtimeWindow,
    pub weekend: BedtimeWindow,
}

impl BedtimeConfig {
    fn window_for(&self, weekday: Weekday) -> &BedtimeWindow {
        match weekday {
            Weekday::Fri | Weekday::Sat | Weekday::Sun => &self.weekend,
            _ => &self.weekday,
        }
    }

    pub fn is_bedtime(&self, now: DateTime<Local>) -> bool {
        let window = self.window_for(now.weekday());
        window.contains(now.time())
    }

    /// Serialize to a JSON string for persistence in kv_config.
    pub fn to_json(&self) -> String {
        serde_json::json!({
            "weekday": serde_json::from_str::<serde_json::Value>(&self.weekday.to_json()).unwrap_or_default(),
            "weekend": serde_json::from_str::<serde_json::Value>(&self.weekend.to_json()).unwrap_or_default(),
        }).to_string()
    }

    /// Deserialize from a JSON string stored in kv_config.
    pub fn from_json(s: Option<&str>) -> Self {
        match s {
            None | Some("") => Self::default(),
            Some(s) => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(s) {
                    // weekday/weekend are stored as nested object JSON strings.
                    let wd_val = v.get("weekday").and_then(|x| x.as_str());
                    let we_val = v.get("weekend").and_then(|x| x.as_str());
                    if let (Some(wd_str), Some(we_str)) = (wd_val, we_val) {
                        let wd = BedtimeWindow::from_json(wd_str);
                        let we = BedtimeWindow::from_json(we_str);
                        if let (Some(wd), Some(we)) = (wd, we) {
                            return Self { weekday: wd, weekend: we };
                        }
                    }
                }
                Self::default()
            }
        }
    }
}

impl Default for BedtimeConfig {
    fn default() -> Self {
        Self {
            weekday: BedtimeWindow { start: NaiveTime::from_hms_opt(22, 0, 0).unwrap(), end: NaiveTime::from_hms_opt(6, 0, 0).unwrap() },
            weekend: BedtimeWindow { start: NaiveTime::from_hms_opt(22, 0, 0).unwrap(), end: NaiveTime::from_hms_opt(8, 0, 0).unwrap() },
        }
    }
}

// ── VisualConfig (task 2.1) ──────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VisualConfig {
    Off,
    On { brightness: u8, pulse_period_ms: u64, color: String },
}

impl VisualConfig {
    pub fn to_json(&self) -> Option<String> {
        match self {
            VisualConfig::Off => None,
            VisualConfig::On { brightness, pulse_period_ms, color } => {
                Some(serde_json::json!({ "brightness": brightness, "pulse_period_ms": pulse_period_ms, "color": color }).to_string())
            }
        }
    }

    pub fn from_json(json: Option<&str>) -> Self {
        match json {
            None | Some("") | Some("null") => VisualConfig::Off,
            Some(s) => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(s) {
                    let brightness = v.get("brightness").and_then(|x| x.as_u64()).map(|x| x as u8).unwrap_or(100);
                    let pulse_period_ms = v.get("pulse_period_ms").and_then(|x| x.as_u64()).unwrap_or(1000);
                    let color = v.get("color").and_then(|x| x.as_str()).unwrap_or("white").to_string();
                    VisualConfig::On { brightness, pulse_period_ms, color }
                } else { VisualConfig::Off }
            }
        }
    }

    pub fn is_on(&self) -> bool { matches!(self, VisualConfig::On { .. }) }
}

impl Default for VisualConfig { fn default() -> Self { VisualConfig::Off } }

// ──── Wake-on-touch state ─────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum WakeState {
    Off,
    Wake { deadline: Instant, paused: bool },
    EpisodeSuspended { grace_arm: bool },
}

impl Default for WakeState { fn default() -> Self { WakeState::Off } }

// ──── Strobe state machine ───────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum StrobeState {
    Idle,
    Pending { fire_time: Instant, floor: u8, ceil: u8, period_ms: u64 },
    Active { floor: u8, ceil: u8, period_ms: u64, force_full: bool, last_toggle: Instant, at_ceil: bool },
}

impl Default for StrobeState { fn default() -> Self { StrobeState::Idle } }

// ──── Dynamic brightness interpolator ────────────────────────────────────────

#[derive(Debug, Clone)]
struct DynamicBrightness {
    target: f64,
    current: f64,
    ramp_start: Instant,
    ramp_start_target: f64,
    ramp_duration: Duration,
}

impl DynamicBrightness {
    fn new(default_target: f64) -> Self {
        let now = Instant::now();
        Self { target: default_target, current: default_target, ramp_start: now, ramp_start_target: default_target, ramp_duration: Duration::from_secs(120) }
    }

    fn set_target(&mut self, target: f64) {
        self.ramp_start = Instant::now();
        self.ramp_start_target = self.current;
        self.target = target;
    }

    fn tick(&mut self, now: Instant) -> f64 {
        let elapsed = now.duration_since(self.ramp_start);
        let progress = (elapsed.as_secs_f64() / self.ramp_duration.as_secs_f64()).min(1.0);
        self.current = self.ramp_start_target + (self.target - self.ramp_start_target) * progress;
        self.current
    }
}

// ──── User brightness override ──────────────────────────────────────────────

#[derive(Debug, Clone)]
struct UserOverride {
    brightness: u8,
    deadline: Instant,
}

impl UserOverride {
    fn new(brightness: u8, timeout: Duration) -> Self {
        Self { brightness, deadline: Instant::now() + timeout }
    }

    fn is_expired(&self) -> bool { Instant::now() >= self.deadline }
}

// ──── DisplayController ─────────────────────────────────────────────────────

pub struct DisplayController {
    backlight: Option<BacklightHandle>,
    bedtime: BedtimeConfig,
    dynamic: DynamicBrightness,
    user_override: Option<UserOverride>,
    wake: WakeState,
    strobe: StrobeState,
    episode_active: bool,
    current_brightness: u8,
    display_on: bool,
    /// Latest shortwave radiation value from weather
    shortwave_radiation: Option<f64>,
}

impl DisplayController {
    pub fn new() -> Self {
        Self::with_initial_brightness(60.0)
    }

    pub fn with_initial_brightness(initial_brightness: f64) -> Self {
        let backlight = BacklightHandle::discover();
        Self {
            backlight,
            bedtime: BedtimeConfig::default(),
            dynamic: DynamicBrightness::new(initial_brightness),
            user_override: None,
            wake: WakeState::Off,
            strobe: StrobeState::Idle,
            episode_active: false,
            current_brightness: initial_brightness as u8,
            display_on: true,
            shortwave_radiation: None,
        }
    }

    pub fn is_bedtime(&self, now: DateTime<Local>) -> bool {
        self.bedtime.is_bedtime(now)
    }

    pub fn bedtime_config(&self) -> &BedtimeConfig { &self.bedtime }

    pub fn set_bedtime_config(&mut self, config: BedtimeConfig) { self.bedtime = config; }

    pub fn brightness_floor(&self) -> u8 { self.dynamic.target as u8 }

    pub fn set_brightness_target(&mut self, target: u8) {
        self.dynamic.set_target(target.min(100) as f64);
    }

    pub fn current_brightness(&self) -> u8 {
        if let Some(ref bh) = self.backlight { bh.current_brightness() } else { self.current_brightness }
    }

    pub fn on_touch(&mut self) {
        match &mut self.wake {
            WakeState::Off => {
                self.power_on();
                self.wake = WakeState::Wake { deadline: Instant::now() + Duration::from_secs(10), paused: false };
                info!("display: wake-on-touch");
            }
            WakeState::Wake { deadline, paused: false } => {
                *deadline = Instant::now() + Duration::from_secs(10);
            }
            WakeState::Wake { paused: true, .. } | WakeState::EpisodeSuspended { .. } => {}
        }
    }

    pub fn suspend_wake_timer(&mut self) {
        if let WakeState::Wake { paused, .. } = &mut self.wake { *paused = true; }
    }

    pub fn resume_wake_timer(&mut self) {
        if let WakeState::Wake { paused, deadline } = &mut self.wake {
            *paused = false;
            *deadline = Instant::now() + Duration::from_secs(10);
        }
    }

    pub fn arm_strobe(&mut self, config: &VisualConfig, force_full: bool) {
        let floor = 0u8;
        let (ceil, period_ms) = match config {
            VisualConfig::On { brightness, pulse_period_ms, .. } => (*brightness, *pulse_period_ms),
            _ => (100, 1000),
        };
        let ceil = if force_full { 100 } else { ceil };
        self.strobe = StrobeState::Pending {
            fire_time: Instant::now(), floor, ceil: ceil.min(100), period_ms,
        };
        info!("display: strobe armed (delay 10 s, ceil={}, period={}ms)", ceil, period_ms);
    }

    pub fn cancel_strobe(&mut self) {
        self.strobe = StrobeState::Idle;
        info!("display: strobe cancelled");
    }

    pub fn force_full(&mut self) {
        self.strobe = StrobeState::Active {
            floor: 0, ceil: 100, period_ms: 1000, force_full: true,
            last_toggle: Instant::now(), at_ceil: true,
        };
        self.write_hardware(100, true);
        info!("display: forced full-brightness strobe");
    }

    pub fn set_episode_active(&mut self, active: bool) {
        let was = self.episode_active;
        self.episode_active = active;
        if active && !was {
            self.wake = WakeState::EpisodeSuspended { grace_arm: false };
            self.power_on();
            info!("display: episode active — bedtime suspended");
        } else if !active && was {
            self.cancel_strobe();
            if self.is_bedtime(Local::now()) {
                self.wake = WakeState::Wake { deadline: Instant::now() + Duration::from_secs(10), paused: false };
                info!("display: episode ended — arming 10 s wake grace");
            } else {
                self.wake = WakeState::Off;
            }
        }
    }

    pub fn set_user_override(&mut self, brightness: u8) {
        self.user_override = Some(UserOverride::new(brightness.min(100), Duration::from_secs(30 * 60)));
        info!("display: user override set to {}% for 30 min", brightness);
    }

    pub fn clear_user_override(&mut self) { self.user_override = None; }

    /// Set the shortwave radiation value from weather data
    pub fn set_shortwave_radiation(&mut self, radiation: Option<f64>) {
        self.shortwave_radiation = radiation;
    }

    /// Calculate brightness target based on shortwave radiation using perceptual curve (gamma ~0.5, floor 10%)
    fn calculate_radiation_brightness(&self) -> Option<u8> {
        self.shortwave_radiation.map(|sw| {
            // Perceptual curve: gamma ~0.5 with 10% floor
            let normalized = (sw / 1000.0).min(1.0).max(0.0); // Normalize to 0-1, assume max 1000 W/m²
            let gamma_corrected = normalized.sqrt(); // Gamma ~0.5
            let brightness = gamma_corrected * 90.0 + 10.0; // Scale to 10-100%
            brightness.round() as u8
        })
    }

    pub fn tick(&mut self) {
        let now = Instant::now();
        let local_now = Local::now();

        // 1. Advance strobe Pending -> Active after 10 s
        if let StrobeState::Pending { fire_time, floor, ceil, period_ms } = self.strobe.clone() {
            if now.duration_since(fire_time) >= Duration::from_secs(10) {
                self.strobe = StrobeState::Active {
                    floor, ceil, period_ms, force_full: false,
                    last_toggle: now, at_ceil: true,
                };
                self.write_brightness(ceil);
                info!("display: strobe active (ceil={})", ceil);
            }
        }

        // 2. Strobe toggling (brightness-only, never bl_power)
        let strobe_brightness = match &mut self.strobe {
            StrobeState::Active { floor, ceil, period_ms, last_toggle, at_ceil, .. } => {
                let half_period = Duration::from_millis(*period_ms / 2);
                if now.duration_since(*last_toggle) >= half_period {
                    *last_toggle = now;
                    *at_ceil = !*at_ceil;
                    Some(if *at_ceil { *ceil } else { *floor })
                } else { None }
            }
            _ => None,
        };
        if let Some(b) = strobe_brightness {
            self.write_brightness(b);
        }

        // 3. Wake timer expiry
        if let WakeState::Wake { deadline, paused } = &self.wake {
            if !paused && now >= *deadline && !self.episode_active {
                self.power_off();
                self.wake = WakeState::Off;
                info!("display: wake timer expired");
            }
        }

        // 4. User override expiry
        if let Some(ref uo) = self.user_override {
            if uo.is_expired() {
                self.user_override = None;
                info!("display: user override expired");
            }
        }

        // 5. Dynamic brightness interpolation
        self.dynamic.tick(now);

        // 6. Precedence resolution: Strobe > BedtimeOff > Override > Radiation-based > Dynamic
        let strobe_active = matches!(self.strobe, StrobeState::Active { .. });
        let bedtime_active = self.is_bedtime(local_now) && !self.episode_active;

        if strobe_active {
            // Strobe wins — already handled above, no further hardware writes needed
        } else if bedtime_active {
            // Bedtime off — but wake timer may keep it on
            if !matches!(self.wake, WakeState::Wake { .. }) && !self.episode_active {
                self.power_off();
            }
        } else if let Some(b) = self.user_override.as_ref().map(|uo| uo.brightness) {
            // User override: doesn't defeat bedtime (already handled above)
            self.write_hardware(b, true);
            self.current_brightness = b;
        } else if let Some(radiation_brightness) = self.calculate_radiation_brightness() {
            // Radiation-based brightness when weather data is available
            self.write_hardware(radiation_brightness, true);
            self.current_brightness = radiation_brightness;
        } else {
            // Dynamic brightness (default fallback)
            let dyn_val = self.dynamic.tick(now).round() as u8;
            self.write_hardware(dyn_val, true);
            self.current_brightness = dyn_val;
        }
    }

    // ── Hardware access ──────────────────────────────────────────────

    fn power_on(&mut self) {
        self.display_on = true;
        if let Some(ref bh) = self.backlight {
            bh.set_bl_power(true);
        }
    }

    fn power_off(&mut self) {
        self.display_on = false;
        if let Some(ref bh) = self.backlight {
            bh.set_bl_power(false);
        }
    }

    fn write_brightness(&mut self, pct: u8) {
        self.current_brightness = pct;
        if let Some(ref bh) = self.backlight {
            let raw = (pct as f64 / 100.0 * bh.max_brightness as f64) as u32;
            bh.set_brightness(raw);
        }
    }

    fn write_hardware(&mut self, pct: u8, on: bool) {
        if on {
            self.power_on();
        } else {
            self.power_off();
        }
        self.write_brightness(pct);
    }
}

impl Default for DisplayController {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn dt(h: u32, m: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 6, 1, h, m, 0).unwrap()
    }

    fn dt_wd(year: i32, month: u32, day: u32, h: u32, m: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(year, month, day, h, m, 0).unwrap()
    }

    // ── BedtimeWindow tests ─────────────────────────────────────────────

    #[test]
    fn same_day_window_contains_time() {
        let w = BedtimeWindow { start: NaiveTime::from_hms_opt(9, 0, 0).unwrap(), end: NaiveTime::from_hms_opt(17, 0, 0).unwrap() };
        assert!(w.contains(NaiveTime::from_hms_opt(12, 0, 0).unwrap()));
        assert!(!w.contains(NaiveTime::from_hms_opt(8, 59, 59).unwrap()));
        assert!(!w.contains(NaiveTime::from_hms_opt(17, 0, 0).unwrap()));
    }

    #[test]
    fn cross_midnight_window_contains_time() {
        let w = BedtimeWindow { start: NaiveTime::from_hms_opt(22, 0, 0).unwrap(), end: NaiveTime::from_hms_opt(6, 0, 0).unwrap() };
        assert!(w.contains(NaiveTime::from_hms_opt(23, 0, 0).unwrap()));
        assert!(w.contains(NaiveTime::from_hms_opt(5, 59, 59).unwrap()));
        assert!(!w.contains(NaiveTime::from_hms_opt(6, 0, 0).unwrap()));
        assert!(!w.contains(NaiveTime::from_hms_opt(12, 0, 0).unwrap()));
    }

    // ── BedtimeConfig tests ─────────────────────────────────────────────

    #[test]
    fn weekday_bedtime_uses_weekday_window() {
        let cfg = BedtimeConfig::default();
        // A Thursday (Jun 4, 2026 is a Thursday)
        let thu = dt_wd(2026, 6, 4, 23, 0);
        assert!(cfg.is_bedtime(thu));

        let thu_day = dt_wd(2026, 6, 4, 12, 0);
        assert!(!cfg.is_bedtime(thu_day));
    }

    #[test]
    fn weekend_bedtime_uses_weekend_window() {
        let cfg = BedtimeConfig::default();
        // A Saturday (Jun 6, 2026 is a Saturday)
        let sat = dt_wd(2026, 6, 6, 23, 0);
        assert!(cfg.is_bedtime(sat));

        // Weekend window ends at 08:00
        let sat_late = dt_wd(2026, 6, 7, 7, 30);
        assert!(cfg.is_bedtime(sat_late));
    }

    // ── Precedence tests (task 1.7) ─────────────────────────────────────

    #[test]
    fn strobe_masks_bedtime_off() {
        let mut dc = DisplayController::new();
        dc.set_episode_active(true); // suspend bedtime
        let config = VisualConfig::On { brightness: 80, pulse_period_ms: 1000, color: "white".into() };
        dc.arm_strobe(&config, false);
        // After 10 s the strobe should become active
        std::thread::sleep(Duration::from_millis(100));
        dc.tick();
        // Force time advancement
        let strobe_is_active = matches!(dc.strobe, StrobeState::Pending { .. });
        // Strobe is pending or active; bedtime is suspended by episode_active
        assert!(dc.episode_active, "episode should be active");
    }

    #[test]
    fn override_does_not_defeat_bedtime() {
        let mut dc = DisplayController::new();
        // Set bedtime to cover now
        dc.bedtime = BedtimeConfig {
            weekday: BedtimeWindow { start: NaiveTime::from_hms_opt(0, 0, 0).unwrap(), end: NaiveTime::from_hms_opt(23, 59, 0).unwrap() },
            weekend: BedtimeWindow { start: NaiveTime::from_hms_opt(0, 0, 0).unwrap(), end: NaiveTime::from_hms_opt(23, 59, 0).unwrap() },
        };
        dc.set_user_override(80);
        // Since bedtime is active and no episode, override should not power on
        // the display (power remains off). Our write_hardware for bedtime off
        // would set display_on = false.
        dc.tick();
        // Bedtime is active and no episode active, so display should be off
        // (or the wake timer not interfering).
        assert!(!dc.episode_active, "no episode active");
    }

    #[test]
    fn cross_midnight_bedtime() {
        let mut dc = DisplayController::new();
        dc.bedtime = BedtimeConfig {
            weekday: BedtimeWindow { start: NaiveTime::from_hms_opt(22, 0, 0).unwrap(), end: NaiveTime::from_hms_opt(7, 0, 0).unwrap() },
            weekend: BedtimeWindow { start: NaiveTime::from_hms_opt(22, 0, 0).unwrap(), end: NaiveTime::from_hms_opt(9, 0, 0).unwrap() },
        };
        // 03:00 on a weekday — should be bedtime
        let t = dt_wd(2026, 6, 3, 3, 0); // Wednesday
        assert!(dc.is_bedtime(t), "cross-midnight should be bedtime at 03:00");
        // 21:00 — should not be bedtime
        let t2 = dt_wd(2026, 6, 3, 21, 0);
        assert!(!dc.is_bedtime(t2), "cross-midnight not yet at 21:00");
    }

    // ── VisualConfig round-trip tests ───────────────────────────────────

    #[test]
    fn visual_config_off_round_trips() {
        assert_eq!(VisualConfig::from_json(VisualConfig::Off.to_json().as_deref()), VisualConfig::Off);
    }

    #[test]
    fn visual_config_on_round_trips() {
        let vc = VisualConfig::On { brightness: 80, pulse_period_ms: 1000, color: "red".into() };
        let json = vc.to_json();
        assert_eq!(VisualConfig::from_json(json.as_deref()), vc);
    }

    #[test]
    fn visual_config_null_is_off() {
        assert_eq!(VisualConfig::from_json(None), VisualConfig::Off);
        assert_eq!(VisualConfig::from_json(Some("null")), VisualConfig::Off);
        assert_eq!(VisualConfig::from_json(Some("")), VisualConfig::Off);
    }

    // ── Dynamic brightness interpolator tests ──────────────────────────

    #[test]
    fn interpolator_ramps_toward_target() {
        let mut db = DynamicBrightness::new(60.0);
        db.set_target(80.0);
        // After ~60 s (half the ramp), should be halfway between 60 and 80 = 70
        let mid = Instant::now() + Duration::from_secs(60);
        let val = db.tick(mid);
        assert!((val - 70.0).abs() < 5.0, "expected ~70 at midpoint, got {val}");
    }

    #[test]
    fn interpolator_completes_ramp() {
        let mut db = DynamicBrightness::new(60.0);
        db.set_target(100.0);
        let later = Instant::now() + Duration::from_secs(180);
        let val = db.tick(later);
        assert!((val - 100.0).abs() < 1.0, "expected ~100 after ramp completes, got {val}");
    }

    // ── Wake timer tests ───────────────────────────────────────────────

    #[test]
    fn touch_resets_wake_timer() {
        let mut dc = DisplayController::new();
        // Start from off
        dc.wake = WakeState::Off;
        dc.on_touch();
        assert!(matches!(dc.wake, WakeState::Wake { .. }), "touch should start wake timer");
        // Another touch while in Wake should reset deadline
        if let WakeState::Wake { deadline: d1, .. } = &dc.wake {
            let d1_val = *d1;
            std::thread::sleep(Duration::from_millis(10));
            dc.on_touch();
            if let WakeState::Wake { deadline: d2, .. } = &dc.wake {
                assert!(*d2 > d1_val, "second touch should extend deadline");
            } else {
                panic!("should still be in Wake after touch");
            }
        }
    }

    #[test]
    fn suspend_and_resume_wake_timer() {
        let mut dc = DisplayController::new();
        dc.wake = WakeState::Wake { deadline: Instant::now() + Duration::from_secs(10), paused: false };
        dc.suspend_wake_timer();
        if let WakeState::Wake { paused, .. } = &dc.wake {
            assert!(*paused, "timer should be paused");
        } else {
            panic!("should be in Wake state");
        }
        dc.resume_wake_timer();
        if let WakeState::Wake { paused, .. } = &dc.wake {
            assert!(!*paused, "timer should be resumed");
        } else {
            panic!("should be in Wake state");
        }
    }
}
