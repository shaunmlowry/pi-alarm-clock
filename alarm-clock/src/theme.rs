//! Runtime theme model and controller (slice 3).
//!
//! Themes are parameter sets, not compile-time components. Slint components
//! read token values from the generated `ThemeGlobal` singleton, so switching
//! themes at runtime is just re-writing those properties.

use crate::database::ConfigStore;
use crate::display::DisplayController;
use crate::WeatherSnapshot;
use chrono::{DateTime, Local, Timelike};
use rusqlite::Connection;
use std::sync::{Arc, Mutex};

// Generated Slint types live in the crate root because of
// `slint::include_modules!()` in `main.rs`.
use crate::{AppWindow, ThemeGlobal};
use slint::ComponentHandle;

pub const DEFAULT_THEME_NAME: &str = "Liquid Glass";
pub const DEFAULT_MODE_NAME: &str = "Follow-Bedtime";

const KV_THEME: &str = "theme";
const KV_THEME_MODE: &str = "theme_mode";

/// A complete set of colors / shapes / type for one mode of one theme.
#[derive(Debug, Clone, PartialEq)]
pub struct TokenSet {
    pub background: slint::Color,
    pub card_background: slint::Color,
    pub card_border: slint::Color,
    pub card_border_width: f32,
    pub card_shadow_blur: f32,
    pub card_shadow_offset: f32,
    pub card_shadow_color: slint::Color,
    pub card_radius: f32,
    pub text_color: slint::Color,
    pub accent_color: slint::Color,
    pub clock_face_background: slint::Color,
    pub hand_color: slint::Color,
    pub second_hand_color: slint::Color,
    pub font_family: &'static str,
}

/// Visual family of a theme. Components branch on this to implement the
/// differences that cannot be expressed with tokens alone (e.g. neumorphic
/// dual-extrusion shadows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentVariant {
    LiquidGlass,
    Neuromorphic,
}

/// Theme mode: manual light/dark, or follow a bedtime window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    ManualLight,
    ManualDark,
    FollowBedtime,
}

impl Mode {
    /// Parse a persisted mode value.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "Manual-Light" => Some(Mode::ManualLight),
            "Manual-Dark" => Some(Mode::ManualDark),
            "Follow-Bedtime" => Some(Mode::FollowBedtime),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::ManualLight => "Manual-Light",
            Mode::ManualDark => "Manual-Dark",
            Mode::FollowBedtime => "Follow-Bedtime",
        }
    }
}

impl Default for Mode {
    fn default() -> Self {
        Mode::FollowBedtime
    }
}

/// A theme with light and dark tokens and a component variant.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub name: &'static str,
    pub light: TokenSet,
    pub dark: TokenSet,
    pub variant: ComponentVariant,
}

/// Color helpers.
fn c(r: u8, g: u8, b: u8) -> slint::Color {
    slint::Color::from_argb_u8(255, r, g, b)
}

fn ca(r: u8, g: u8, b: u8, a: u8) -> slint::Color {
    slint::Color::from_argb_u8(a, r, g, b)
}

pub fn liquid_glass_theme() -> Theme {
    Theme {
        name: "Liquid Glass",
        variant: ComponentVariant::LiquidGlass,
        light: TokenSet {
            background: c(36, 36, 62),              // deep blue/purple
            card_background: ca(255, 255, 255, 26),   // rgba(255,255,255,0.1)
            card_border: ca(255, 255, 255, 46),       // rgba(255,255,255,0.18)
            card_border_width: 1.0,
            card_shadow_blur: 32.0,
            card_shadow_offset: 8.0,
            card_shadow_color: ca(31, 38, 135, 94),   // rgba(31,38,135,0.37)
            card_radius: 24.0,
            text_color: c(255, 255, 255),
            accent_color: c(255, 107, 107),           // #ff6b6b
            clock_face_background: ca(255, 255, 255, 60),
            hand_color: ca(255, 255, 255, 230),
            second_hand_color: ca(255, 100, 100, 204),
            font_family: "sans-serif",
        },
        dark: TokenSet {
            background: c(0, 0, 0),
            card_background: ca(50, 50, 50, 102),     // rgba(50,50,50,0.4)
            card_border: ca(255, 255, 255, 38),
            card_border_width: 1.0,
            card_shadow_blur: 32.0,
            card_shadow_offset: 8.0,
            card_shadow_color: ca(0, 0, 0, 128),
            card_radius: 24.0,
            text_color: c(255, 255, 255),
            accent_color: c(255, 107, 107),
            clock_face_background: ca(255, 255, 255, 46),
            hand_color: ca(255, 255, 255, 242),
            second_hand_color: ca(255, 100, 100, 204),
            font_family: "sans-serif",
        },
    }
}

pub fn neuromorphic_theme() -> Theme {
    Theme {
        name: "Neuromorphic",
        variant: ComponentVariant::Neuromorphic,
        light: TokenSet {
            background: c(234, 239, 246),             // #eaeff6
            card_background: c(234, 239, 246),
            card_border: ca(234, 239, 246, 0),
            card_border_width: 0.0,
            card_shadow_blur: 12.0,
            card_shadow_offset: 4.0,
            // For neuromorphic cards the drop-shadow token is the dark shadow;
            // the light extrusion shadow is derived from the card background.
            card_shadow_color: ca(163, 177, 198, 128), // rgba(163,177,198,0.5)
            card_radius: 16.0,
            text_color: c(74, 74, 74),
            accent_color: c(255, 107, 107),
            clock_face_background: c(234, 239, 246),
            hand_color: c(74, 74, 74),
            second_hand_color: c(255, 107, 107),
            font_family: "sans-serif",
        },
        dark: TokenSet {
            background: c(36, 36, 36),                // #242424
            card_background: c(36, 36, 36),
            card_border: ca(36, 36, 36, 0),
            card_border_width: 0.0,
            card_shadow_blur: 12.0,
            card_shadow_offset: 4.0,
            card_shadow_color: ca(0, 0, 0, 128),
            card_radius: 16.0,
            text_color: c(255, 255, 255),
            accent_color: c(255, 107, 107),
            clock_face_background: c(36, 36, 36),
            hand_color: c(255, 255, 255),
            second_hand_color: c(255, 107, 107),
            font_family: "sans-serif",
        },
    }
}

/// Find a built-in theme by name. Returns the default if unknown.
pub fn theme_by_name(name: &str) -> Theme {
    match name {
        "Neuromorphic" => neuromorphic_theme(),
        _ => liquid_glass_theme(),
    }
}

/// Theme controller owned by main. It knows the active theme + mode, resolves
/// the effective token set, writes it into the Slint `ThemeGlobal`, and
/// persists the selection via `ConfigStore`.
pub struct ThemeController {
    shared: Arc<Mutex<Connection>>,
    active_theme: Theme,
    mode: Mode,
    /// Reference to the DisplayController for Follow-Bedtime resolution (slice 4).
    display: Option<Arc<Mutex<DisplayController>>>,
}

impl ThemeController {
    /// Create a controller and immediately load any persisted selection.
    pub fn new(shared: Arc<Mutex<Connection>>) -> Self {
        let mut this = Self {
            shared,
            active_theme: liquid_glass_theme(),
            mode: Mode::default(),
            display: None,
        };
        this.load();
        this
    }

    /// Map WMO weather code to a short text label (emoji fonts are not
    /// available on the Pi's minimal font set, so we use plain text).
    fn wmo_code_to_icon(wmo_code: i32) -> String {
        match wmo_code {
            0 => "Clear".to_string(),
            1 => "Mainly Clear".to_string(),
            2 => "Partly Cloudy".to_string(),
            3 => "Cloudy".to_string(),
            45 | 48 => "Fog".to_string(),
            51 | 53 | 55 => "Drizzle".to_string(),
            56 | 57 => "Freezing Drizzle".to_string(),
            61 | 63 | 65 => "Rain".to_string(),
            66 | 67 => "Freezing Rain".to_string(),
            71 | 73 | 75 => "Snow".to_string(),
            77 => "Snow Grains".to_string(),
            80 | 81 | 82 => "Showers".to_string(),
            85 | 86 => "Snow Showers".to_string(),
            95 => "Thunderstorm".to_string(),
            96 | 99 => "Thunderstorm + Hail".to_string(),
            _ => "Clear".to_string(),
        }
    }

    /// Push weather data to the ThemeGlobal
    pub fn push_weather(&self, window: &AppWindow, weather_available: bool, weather_data: Option<WeatherSnapshot>) {
        let global = window.global::<ThemeGlobal>();
        global.set_weather_available(weather_available);
        
        if let Some(snapshot) = weather_data {
            // Set the weather data properties
            global.set_weather_temp(snapshot.current_temp.to_string().into());
            global.set_weather_high(snapshot.today_high.to_string().into());
            global.set_weather_low(snapshot.today_low.to_string().into());
            global.set_weather_tomorrow_high(snapshot.tomorrow_high.to_string().into());
            global.set_weather_tomorrow_low(snapshot.tomorrow_low.to_string().into());
            global.set_weather_wind(snapshot.wind_speed.to_string().into());
            global.set_weather_humidity(snapshot.humidity.to_string().into());
            global.set_weather_wmo_code(snapshot.wmo_code);
            global.set_weather_icon(Self::wmo_code_to_icon(snapshot.wmo_code).into());
            
            // Push the first frame of the animated icon.
            if let Some(first_frame) = crate::weather_icons::get_icon_first_frame(snapshot.wmo_code) {
                global.set_weather_icon_image(first_frame);
            }
        }
    }

    /// Attach a `DisplayController` for Follow-Bedtime resolution.
    pub fn with_display(mut self, display: Arc<Mutex<DisplayController>>) -> Self {
        self.display = Some(display);
        self
    }

    /// Current theme name.
    pub fn theme_name(&self) -> &'static str {
        self.active_theme.name
    }

    /// Current mode name.
    pub fn mode_name(&self) -> &'static str {
        self.mode.as_str()
    }

    fn load(&mut self) {
        if let Ok(conn_guard) = self.shared.lock() {
            let store = ConfigStore::new(&*conn_guard);
            if let Ok(Some(name)) = store.get(KV_THEME) {
                self.active_theme = theme_by_name(&name);
            }
            if let Ok(Some(mode)) = store.get(KV_THEME_MODE) {
                if let Some(m) = Mode::from_str(&mode) {
                    self.mode = m;
                }
            }
        }
    }

    fn persist(&self) {
        if let Ok(conn_guard) = self.shared.lock() {
            let store = ConfigStore::new(&*conn_guard);
            let _ = store.set(
                KV_THEME,
                self.active_theme.name,
            );
            let _ = store.set(
                KV_THEME_MODE,
                self.mode.as_str(),
            );
        }
    }

    /// Cycle Liquid Glass -> Neuromorphic -> Liquid Glass.
    pub fn cycle_theme(&mut self) {
        self.active_theme = match self.active_theme.name {
            "Liquid Glass" => neuromorphic_theme(),
            _ => liquid_glass_theme(),
        };
        self.persist();
    }

    /// Cycle Follow-Bedtime -> Manual-Light -> Manual-Dark -> Follow-Bedtime.
    pub fn cycle_mode(&mut self) {
        self.mode = match self.mode {
            Mode::FollowBedtime => Mode::ManualLight,
            Mode::ManualLight => Mode::ManualDark,
            Mode::ManualDark => Mode::FollowBedtime,
        };
        self.persist();
    }

    /// True if the currently active mode selects the dark side of the theme.
    fn is_dark(&self, now: DateTime<Local>) -> bool {
        match self.mode {
            Mode::ManualLight => false,
            Mode::ManualDark => true,
            Mode::FollowBedtime => {
                // Use DisplayController for precise bedtime window resolution.
                if let Some(ref dc) = self.display {
                    if let Ok(d) = dc.lock() {
                        d.is_bedtime(now)
                    } else {
                        let hour = now.hour();
                        hour >= 22 || hour < 6
                    }
                } else {
                    let hour = now.hour();
                    hour >= 22 || hour < 6
                }
            }
        }
    }

    /// Return the effective token set for the current theme and the given time.
    pub fn effective_tokens(&self, now: DateTime<Local>) -> &TokenSet {
        if self.is_dark(now) {
            &self.active_theme.dark
        } else {
            &self.active_theme.light
        }
    }

    /// Push the effective token set into the Slint `ThemeGlobal` and refresh
    /// the theme/mode labels.
    pub fn push(&self, window: &AppWindow) {
        let now = Local::now();
        let tokens = self.effective_tokens(now);
        let global = window.global::<ThemeGlobal>();

        global.set_background(tokens.background);
        global.set_card_background(tokens.card_background);
        global.set_card_border(tokens.card_border);
        global.set_card_border_width(tokens.card_border_width);
        global.set_card_shadow_blur(tokens.card_shadow_blur);
        global.set_card_shadow_offset(tokens.card_shadow_offset);
        global.set_card_shadow_color(tokens.card_shadow_color);
        global.set_card_radius(tokens.card_radius);
        global.set_text_color(tokens.text_color);
        global.set_accent_color(tokens.accent_color);
        global.set_clock_face_background(tokens.clock_face_background);
        global.set_hand_color(tokens.hand_color);
        global.set_second_hand_color(tokens.second_hand_color);
        global.set_font_family(slint::SharedString::from(tokens.font_family));
        global.set_active_theme_name(slint::SharedString::from(self.active_theme.name));
        global.set_active_mode_name(slint::SharedString::from(self.mode.as_str()));

        global.set_is_neuromorphic(
            self.active_theme.variant == ComponentVariant::Neuromorphic,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn in_memory() -> Arc<Mutex<Connection>> {
        let conn = Connection::open_in_memory().expect("in-memory db");
        crate::database::run_migrations(&conn).expect("migrations");
        Arc::new(Mutex::new(conn))
    }

    #[test]
    fn follow_bedtime_selects_dark_at_2300() {
        let mut ctl = ThemeController::new(in_memory());
        ctl.mode = Mode::FollowBedtime;
        let now = Local.with_ymd_and_hms(2026, 6, 1, 23, 0, 0).unwrap();
        assert!(ctl.is_dark(now));
    }

    #[test]
    fn follow_bedtime_selects_dark_at_0300() {
        let mut ctl = ThemeController::new(in_memory());
        ctl.mode = Mode::FollowBedtime;
        let now = Local.with_ymd_and_hms(2026, 6, 1, 3, 0, 0).unwrap();
        assert!(ctl.is_dark(now));
    }

    #[test]
    fn follow_bedtime_selects_light_at_noon() {
        let mut ctl = ThemeController::new(in_memory());
        ctl.mode = Mode::FollowBedtime;
        let now = Local.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap();
        assert!(!ctl.is_dark(now));
    }

    #[test]
    fn manual_light_overrides_bedtime() {
        let mut ctl = ThemeController::new(in_memory());
        ctl.mode = Mode::ManualLight;
        let now = Local.with_ymd_and_hms(2026, 6, 1, 23, 0, 0).unwrap();
        assert!(!ctl.is_dark(now));
    }

    #[test]
    fn manual_dark_overrides_daytime() {
        let mut ctl = ThemeController::new(in_memory());
        ctl.mode = Mode::ManualDark;
        let now = Local.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap();
        assert!(ctl.is_dark(now));
    }

    #[test]
    fn persistence_round_trip() {
        let shared = in_memory();

        {
            let mut ctl = ThemeController::new(Arc::clone(&shared));
            assert_eq!(ctl.theme_name(), "Liquid Glass");
            assert_eq!(ctl.mode_name(), "Follow-Bedtime");

            ctl.cycle_theme();
            ctl.cycle_mode(); // Follow -> Manual-Light
            assert_eq!(ctl.theme_name(), "Neuromorphic");
            assert_eq!(ctl.mode_name(), "Manual-Light");
        }

        {
            let ctl = ThemeController::new(Arc::clone(&shared));
            assert_eq!(ctl.theme_name(), "Neuromorphic");
            assert_eq!(ctl.mode_name(), "Manual-Light");
        }
    }

    #[test]
    fn cycle_theme_returns_to_liquid_glass() {
        let mut ctl = ThemeController::new(in_memory());
        assert_eq!(ctl.theme_name(), "Liquid Glass");
        ctl.cycle_theme();
        assert_eq!(ctl.theme_name(), "Neuromorphic");
        ctl.cycle_theme();
        assert_eq!(ctl.theme_name(), "Liquid Glass");
    }

    #[test]
    fn mode_cycle_order() {
        let mut ctl = ThemeController::new(in_memory());
        assert_eq!(ctl.mode, Mode::FollowBedtime);
        ctl.cycle_mode();
        assert_eq!(ctl.mode, Mode::ManualLight);
        ctl.cycle_mode();
        assert_eq!(ctl.mode, Mode::ManualDark);
        ctl.cycle_mode();
        assert_eq!(ctl.mode, Mode::FollowBedtime);
    }
}
