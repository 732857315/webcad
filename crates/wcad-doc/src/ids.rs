//! Stable identifiers. Allocated from per-document counters, never reused, persisted.

use serde::{Deserialize, Serialize};

macro_rules! id_type {
    ($(#[$m:meta])* $name:ident($t:ty)) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub $t);
    };
}

id_type!(/// A 2D drawing entity (model space or inside a block).
    EntityId(u64));
id_type!(LayerId(u32));
id_type!(BlockId(u32));
id_type!(LinetypeId(u32));
id_type!(TextStyleId(u32));
id_type!(DimStyleId(u32));
id_type!(/// A feature in the 3D part history.
    FeatureId(u64));

/// Monotonic id counters. Persisted with the document so ids stay unique after reload.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct IdAllocator {
    next_entity: u64,
    next_layer: u32,
    next_block: u32,
    next_linetype: u32,
    next_text_style: u32,
    next_dim_style: u32,
    next_feature: u64,
}

impl IdAllocator {
    pub fn entity(&mut self) -> EntityId {
        self.next_entity += 1;
        EntityId(self.next_entity)
    }
    pub fn layer(&mut self) -> LayerId {
        self.next_layer += 1;
        LayerId(self.next_layer)
    }
    pub fn block(&mut self) -> BlockId {
        self.next_block += 1;
        BlockId(self.next_block)
    }
    pub fn linetype(&mut self) -> LinetypeId {
        self.next_linetype += 1;
        LinetypeId(self.next_linetype)
    }
    pub fn text_style(&mut self) -> TextStyleId {
        self.next_text_style += 1;
        TextStyleId(self.next_text_style)
    }
    pub fn dim_style(&mut self) -> DimStyleId {
        self.next_dim_style += 1;
        DimStyleId(self.next_dim_style)
    }
    pub fn feature(&mut self) -> FeatureId {
        self.next_feature += 1;
        FeatureId(self.next_feature)
    }

    /// Make sure future ids are larger than ids already present (after importing foreign data).
    pub fn bump_entity(&mut self, seen: EntityId) {
        self.next_entity = self.next_entity.max(seen.0);
    }
    pub fn bump_feature(&mut self, seen: FeatureId) {
        self.next_feature = self.next_feature.max(seen.0);
    }
}
