//! The 2D drawing: tables (layers, linetypes, styles), blocks and model-space entities.

use std::collections::BTreeMap;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use wcad_math::DVec2;

use crate::entity::Entity;
use crate::ids::*;
use crate::style::*;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DrawingSettings {
    /// Global linetype scale (LTSCALE).
    pub ltscale: f64,
    pub grid_spacing: f64,
    pub snap_spacing: f64,
    /// Decimal places shown for coordinates and lengths in the UI.
    pub display_decimals: u8,
    /// Point display size in pixels.
    pub point_size_px: f64,
}

impl Default for DrawingSettings {
    fn default() -> Self {
        Self {
            ltscale: 1.0,
            grid_spacing: 10.0,
            snap_spacing: 1.0,
            display_decimals: 4,
            point_size_px: 5.0,
        }
    }
}

/// Named tables. Snapshotted as a whole by transactions (they are small).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tables {
    pub layers: IndexMap<LayerId, Layer>,
    pub current_layer: LayerId,
    pub linetypes: IndexMap<LinetypeId, Linetype>,
    pub text_styles: IndexMap<TextStyleId, TextStyle>,
    pub current_text_style: TextStyleId,
    pub dim_styles: IndexMap<DimStyleId, DimStyle>,
    pub current_dim_style: DimStyleId,
    pub settings: DrawingSettings,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Block {
    pub name: String,
    pub base: DVec2,
    /// Block-local entities (ids come from the same allocator as model space).
    pub entities: BTreeMap<EntityId, Entity>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Drawing {
    pub tables: Tables,
    pub blocks: IndexMap<BlockId, Block>,
    /// Model-space entities. Iteration order (ascending id) is the draw order.
    pub entities: BTreeMap<EntityId, Entity>,
}

impl Drawing {
    /// A new drawing with layer "0", the Continuous/Dashed/Center/Hidden linetypes and Standard styles.
    pub fn new(ids: &mut IdAllocator) -> Self {
        let mut linetypes = IndexMap::new();
        let continuous = ids.linetype();
        linetypes.insert(continuous, Linetype::continuous());
        for (name, desc, pattern) in [
            ("DASHED", "Dashed __ __ __", vec![12.7, -6.35]),
            (
                "CENTER",
                "Center ____ _ ____ _",
                vec![31.75, -6.35, 6.35, -6.35],
            ),
            ("HIDDEN", "Hidden __ __ __", vec![6.35, -3.175]),
            (
                "PHANTOM",
                "Phantom ____ _ _ ____",
                vec![31.75, -6.35, 6.35, -6.35, 6.35, -6.35],
            ),
            ("DOT", "Dot . . . .", vec![0.0, -6.35]),
            (
                "DASHDOT",
                "Dash dot __ . __ .",
                vec![12.7, -6.35, 0.0, -6.35],
            ),
        ] {
            linetypes.insert(
                ids.linetype(),
                Linetype {
                    name: name.into(),
                    description: desc.into(),
                    pattern,
                },
            );
        }

        let layer0 = ids.layer();
        let mut layers = IndexMap::new();
        layers.insert(
            layer0,
            Layer {
                name: "0".into(),
                color: Color::WHITE,
                linetype: continuous,
                lineweight: LineWeight::Default,
                visible: true,
                frozen: false,
                locked: false,
                plot: true,
            },
        );

        let standard_text = ids.text_style();
        let mut text_styles = IndexMap::new();
        text_styles.insert(
            standard_text,
            TextStyle {
                name: "Standard".into(),
                font: "default".into(),
                height: 0.0,
                width_factor: 1.0,
                oblique: 0.0,
            },
        );

        let standard_dim = ids.dim_style();
        let mut dim_styles = IndexMap::new();
        dim_styles.insert(standard_dim, DimStyle::standard(standard_text));

        Self {
            tables: Tables {
                layers,
                current_layer: layer0,
                linetypes,
                text_styles,
                current_text_style: standard_text,
                dim_styles,
                current_dim_style: standard_dim,
                settings: DrawingSettings::default(),
            },
            blocks: IndexMap::new(),
            entities: BTreeMap::new(),
        }
    }

    pub fn layer(&self, id: LayerId) -> Option<&Layer> {
        self.tables.layers.get(&id)
    }

    pub fn layer_by_name(&self, name: &str) -> Option<LayerId> {
        self.tables
            .layers
            .iter()
            .find(|(_, l)| l.name.eq_ignore_ascii_case(name))
            .map(|(&id, _)| id)
    }

    pub fn linetype_by_name(&self, name: &str) -> Option<LinetypeId> {
        self.tables
            .linetypes
            .iter()
            .find(|(_, l)| l.name.eq_ignore_ascii_case(name))
            .map(|(&id, _)| id)
    }

    pub fn block_by_name(&self, name: &str) -> Option<BlockId> {
        self.blocks
            .iter()
            .find(|(_, b)| b.name.eq_ignore_ascii_case(name))
            .map(|(&id, _)| id)
    }

    /// Visible, editable layers.
    pub fn is_layer_editable(&self, id: LayerId) -> bool {
        self.layer(id)
            .is_some_and(|l| l.visible && !l.frozen && !l.locked)
    }
}
