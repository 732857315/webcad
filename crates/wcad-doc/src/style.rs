//! Layers, colors, linetypes, lineweights, text and dimension styles.

use serde::{Deserialize, Serialize};

use crate::ids::{LinetypeId, TextStyleId};

/// Entity color. Layer colors are never `ByLayer`/`ByBlock`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "v")]
pub enum Color {
    #[default]
    ByLayer,
    ByBlock,
    /// AutoCAD Color Index 1..=255 (7 = white/black depending on background).
    Aci(u8),
    Rgb(u8, u8, u8),
}

impl Color {
    pub const WHITE: Color = Color::Aci(7);

    /// Resolve to RGB given the layer color and the block color (for entities inside inserts).
    /// ACI 7 resolves to the supplied foreground (white on dark backgrounds, black on light ones).
    pub fn resolve(self, layer: Color, block: Color, foreground: [u8; 3]) -> [u8; 3] {
        match self {
            Color::ByLayer => layer.resolve(Color::WHITE, Color::WHITE, foreground),
            Color::ByBlock => block.resolve(layer, Color::WHITE, foreground),
            Color::Aci(7) => foreground,
            Color::Aci(i) => crate::aci::rgb(i),
            Color::Rgb(r, g, b) => [r, g, b],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(tag = "type", content = "v")]
pub enum LinetypeRef {
    #[default]
    ByLayer,
    ByBlock,
    Id(LinetypeId),
}

/// Line weight. `Mm100(n)` = n/100 mm (DXF convention, e.g. 25 = 0.25 mm).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(tag = "type", content = "v")]
pub enum LineWeight {
    #[default]
    ByLayer,
    ByBlock,
    Default,
    Mm100(u16),
}

/// A dash pattern in drawing units: positive = dash, negative = gap, 0 = dot.
/// An empty pattern is a continuous line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Linetype {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub pattern: Vec<f64>,
}

impl Linetype {
    pub fn continuous() -> Self {
        Self {
            name: "Continuous".into(),
            description: "Solid line".into(),
            pattern: vec![],
        }
    }
    pub fn is_continuous(&self) -> bool {
        self.pattern.is_empty()
    }
    pub fn pattern_length(&self) -> f64 {
        self.pattern.iter().map(|d| d.abs()).sum()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    pub name: String,
    pub color: Color,
    pub linetype: LinetypeId,
    pub lineweight: LineWeight,
    #[serde(default = "yes")]
    pub visible: bool,
    #[serde(default)]
    pub frozen: bool,
    #[serde(default)]
    pub locked: bool,
    #[serde(default = "yes")]
    pub plot: bool,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextStyle {
    pub name: String,
    /// Font key understood by the app's font registry ("default" = embedded CJK font).
    pub font: String,
    /// Fixed height; 0 = height given per entity.
    #[serde(default)]
    pub height: f64,
    #[serde(default = "one")]
    pub width_factor: f64,
    /// Oblique angle in radians.
    #[serde(default)]
    pub oblique: f64,
}

fn one() -> f64 {
    1.0
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DimStyle {
    pub name: String,
    pub text_style: TextStyleId,
    pub text_height: f64,
    pub arrow_size: f64,
    /// Gap between the measured point and the start of the extension line.
    pub ext_offset: f64,
    /// Extension line overshoot beyond the dimension line.
    pub ext_extend: f64,
    /// Gap between dimension line and text.
    pub text_gap: f64,
    /// Overall scale applied to sizes above.
    pub scale: f64,
    /// Decimal places for linear values.
    pub decimals: u8,
    /// Decimal places for angles (degrees).
    pub angle_decimals: u8,
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub suffix: String,
}

impl DimStyle {
    pub fn standard(text_style: TextStyleId) -> Self {
        Self {
            name: "Standard".into(),
            text_style,
            text_height: 2.5,
            arrow_size: 2.5,
            ext_offset: 0.625,
            ext_extend: 1.25,
            text_gap: 0.625,
            scale: 1.0,
            decimals: 2,
            angle_decimals: 0,
            prefix: String::new(),
            suffix: String::new(),
        }
    }
}
