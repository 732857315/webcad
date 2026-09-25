//! User settings, persisted as JSON in the platform key-value store ([`crate::platform::KvStore`]).

use serde::{Deserialize, Serialize};

use crate::i18n::Lang;

/// Storage key of the settings JSON.
pub const SETTINGS_KEY: &str = "settings";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Theme {
    #[default]
    Dark,
    Light,
}

/// Which object snaps are enabled (OSNAP modes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OsnapModes {
    pub endpoint: bool,
    pub midpoint: bool,
    pub center: bool,
    pub quadrant: bool,
    pub intersection: bool,
    pub perpendicular: bool,
    pub tangent: bool,
    pub nearest: bool,
    pub node: bool,
}

impl Default for OsnapModes {
    fn default() -> Self {
        Self {
            endpoint: true,
            midpoint: true,
            center: true,
            quadrant: false,
            intersection: true,
            perpendicular: true,
            tangent: false,
            nearest: false,
            node: true,
        }
    }
}

/// Drafting aids (status bar toggles) and pick sizes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DraftSettings {
    pub osnap_on: bool,
    pub osnap: OsnapModes,
    pub ortho: bool,
    pub polar: bool,
    /// Polar tracking increment in degrees.
    pub polar_increment_deg: f64,
    /// Grid snap (SNAP).
    pub snap_on: bool,
    /// Grid display (GRID).
    pub grid_on: bool,
    /// Object snap aperture radius in logical pixels.
    pub aperture_px: f32,
    /// Pick box half size in logical pixels.
    pub pickbox_px: f32,
    /// Grip half size in logical pixels.
    pub grip_px: f32,
    /// Show lineweights (LWDISPLAY).
    pub show_lineweights: bool,
}

impl Default for DraftSettings {
    fn default() -> Self {
        Self {
            osnap_on: true,
            osnap: OsnapModes::default(),
            ortho: false,
            polar: true,
            polar_increment_deg: 45.0,
            snap_on: false,
            grid_on: true,
            aperture_px: 10.0,
            pickbox_px: 5.0,
            grip_px: 4.0,
            show_lineweights: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub lang: Lang,
    pub theme: Theme,
    pub draft: DraftSettings,
    /// Autosave the drawing for crash recovery.
    pub autosave: bool,
    pub autosave_seconds: f32,
    /// Docks visible (on narrow screens the app starts with both collapsed).
    pub left_panel: bool,
    pub right_panel: bool,
    /// Show text under ribbon icons.
    pub ribbon_labels: bool,
    /// Perspective projection in the 3D view.
    pub perspective: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            lang: Lang::Zh,
            theme: Theme::Dark,
            draft: DraftSettings::default(),
            autosave: true,
            autosave_seconds: 60.0,
            left_panel: true,
            right_panel: true,
            ribbon_labels: true,
            perspective: true,
        }
    }
}

impl Settings {
    /// Parse persisted settings; unknown or broken JSON falls back to defaults (never fails).
    pub fn from_json(s: &str) -> Self {
        serde_json::from_str(s).unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_garbage() {
        let s = Settings {
            lang: Lang::En,
            draft: DraftSettings {
                ortho: true,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(Settings::from_json(&s.to_json()), s);
        assert_eq!(Settings::from_json("not json"), Settings::default());
        // Missing fields take defaults.
        let partial = Settings::from_json(r#"{"lang":"En"}"#);
        assert_eq!(partial.lang, Lang::En);
        assert_eq!(partial.draft, DraftSettings::default());
    }
}
