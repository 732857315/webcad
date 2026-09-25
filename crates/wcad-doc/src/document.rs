//! [`Document`] and the transaction API.
//!
//! The public fields are for **reading**. UI code must mutate only through [`Document::transact`]
//! so undo/redo and change tracking stay correct. Importers that build a fresh document may use
//! [`Document::from_parts`].

use std::collections::{BTreeMap, BTreeSet};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::drawing::{Block, Drawing, Tables};
use crate::entity::{Entity, EntityKind};
use crate::history::{EntityChange, History, Transaction};
use crate::ids::*;
use crate::part::Part;
use crate::style::{Color, LineWeight, LinetypeRef};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Units {
    #[default]
    Millimeter,
    Centimeter,
    Meter,
    Inch,
    Foot,
    Unitless,
}

impl Units {
    /// Short suffix shown after values ("mm", "in", ...).
    pub fn suffix(self) -> &'static str {
        match self {
            Units::Millimeter => "mm",
            Units::Centimeter => "cm",
            Units::Meter => "m",
            Units::Inch => "in",
            Units::Foot => "ft",
            Units::Unitless => "",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocMeta {
    pub title: String,
    pub units: Units,
    /// App version that last saved the document.
    #[serde(default)]
    pub app_version: String,
}

impl Default for DocMeta {
    fn default() -> Self {
        Self { title: String::new(), units: Units::Millimeter, app_version: String::new() }
    }
}

/// What changed since the last [`Document::take_changes`]. `all` = rebuild everything.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChangeSet {
    pub entities: BTreeSet<EntityId>,
    pub tables: bool,
    pub blocks: bool,
    pub part: bool,
    pub all: bool,
}

impl ChangeSet {
    pub fn is_empty(&self) -> bool {
        !self.all && !self.tables && !self.blocks && !self.part && self.entities.is_empty()
    }

    fn absorb(&mut self, t: &Transaction) {
        self.entities.extend(t.entities.iter().map(|c| c.id));
        self.tables |= t.tables.is_some();
        self.blocks |= t.blocks.is_some();
        self.part |= t.part.is_some();
    }
}

#[derive(Debug)]
pub struct Document {
    pub meta: DocMeta,
    pub drawing: Drawing,
    pub part: Part,
    pub(crate) ids: IdAllocator,
    history: History,
    changes: ChangeSet,
    revision: u64,
    saved_revision: u64,
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl Document {
    pub fn new() -> Self {
        let mut ids = IdAllocator::default();
        let drawing = Drawing::new(&mut ids);
        Self::from_parts(DocMeta::default(), drawing, Part::default(), ids)
    }

    /// Assemble a document from loaded or imported data. History starts empty.
    pub fn from_parts(meta: DocMeta, drawing: Drawing, part: Part, mut ids: IdAllocator) -> Self {
        // Guard against foreign data whose ids exceed the allocator.
        for id in drawing.entities.keys().chain(drawing.blocks.values().flat_map(|b| b.entities.keys())) {
            ids.bump_entity(*id);
        }
        for f in &part.features {
            ids.bump_feature(f.id);
        }
        Self {
            meta,
            drawing,
            part,
            ids,
            history: History::default(),
            changes: ChangeSet { all: true, ..Default::default() },
            revision: 0,
            saved_revision: 0,
        }
    }

    pub fn ids(&self) -> &IdAllocator {
        &self.ids
    }

    /// Run `f` as one undoable step labelled `label`. Nothing is recorded if `f` changes nothing.
    pub fn transact<R>(&mut self, label: impl Into<String>, f: impl FnOnce(&mut Tx<'_>) -> R) -> R {
        let mut tx = Tx {
            drawing: &mut self.drawing,
            part: &mut self.part,
            ids: &mut self.ids,
            touched: BTreeMap::new(),
            tables_before: None,
            blocks_before: None,
            part_before: None,
        };
        let out = f(&mut tx);
        let t = tx.finish(label.into());
        if !t.is_empty() {
            self.changes.absorb(&t);
            self.history.push(t);
            self.revision += 1;
        }
        out
    }

    pub fn can_undo(&self) -> bool {
        !self.history.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.history.redo.is_empty()
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.history.undo.last().map(|t| t.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.history.redo.last().map(|t| t.label.as_str())
    }

    /// Undo the last transaction; returns its label.
    pub fn undo(&mut self) -> Option<String> {
        let t = self.history.undo.pop()?;
        self.apply(&t, false);
        let label = t.label.clone();
        self.history.redo.push(t);
        Some(label)
    }

    /// Redo the last undone transaction; returns its label.
    pub fn redo(&mut self) -> Option<String> {
        let t = self.history.redo.pop()?;
        self.apply(&t, true);
        let label = t.label.clone();
        self.history.undo.push(t);
        Some(label)
    }

    fn apply(&mut self, t: &Transaction, forward: bool) {
        for c in &t.entities {
            let v = if forward { &c.after } else { &c.before };
            match v {
                Some(e) => {
                    self.drawing.entities.insert(c.id, e.clone());
                }
                None => {
                    self.drawing.entities.remove(&c.id);
                }
            }
        }
        if let Some((b, a)) = &t.tables {
            self.drawing.tables = if forward { a.clone() } else { b.clone() };
        }
        if let Some((b, a)) = &t.blocks {
            self.drawing.blocks = if forward { a.clone() } else { b.clone() };
        }
        if let Some((b, a)) = &t.part {
            self.part = if forward { a.clone() } else { b.clone() };
        }
        self.changes.absorb(t);
        self.revision += 1;
    }

    /// Monotonic counter bumped by every change (transaction, undo, redo).
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn is_dirty(&self) -> bool {
        self.revision != self.saved_revision
    }

    pub fn mark_saved(&mut self) {
        self.saved_revision = self.revision;
    }

    /// Changes since the previous call (for incremental display-list updates).
    pub fn take_changes(&mut self) -> ChangeSet {
        std::mem::take(&mut self.changes)
    }

    /// Clear undo/redo history (e.g. after loading).
    pub fn clear_history(&mut self) {
        self.history = History::default();
    }
}

/// Mutation handle passed to [`Document::transact`].
pub struct Tx<'a> {
    drawing: &'a mut Drawing,
    part: &'a mut Part,
    ids: &'a mut IdAllocator,
    touched: BTreeMap<EntityId, Option<Entity>>,
    tables_before: Option<Tables>,
    blocks_before: Option<IndexMap<BlockId, Block>>,
    part_before: Option<Part>,
}

impl<'a> Tx<'a> {
    pub fn drawing(&self) -> &Drawing {
        self.drawing
    }

    pub fn part(&self) -> &Part {
        self.part
    }

    pub fn ids(&mut self) -> &mut IdAllocator {
        self.ids
    }

    pub fn entity(&self, id: EntityId) -> Option<&Entity> {
        self.drawing.entities.get(&id)
    }

    fn touch(&mut self, id: EntityId) {
        if !self.touched.contains_key(&id) {
            let before = self.drawing.entities.get(&id).cloned();
            self.touched.insert(id, before);
        }
    }

    /// Add an entity on the current layer with ByLayer properties.
    pub fn add(&mut self, kind: EntityKind) -> EntityId {
        let e = Entity {
            id: EntityId(0),
            layer: self.drawing.tables.current_layer,
            color: Color::ByLayer,
            linetype: LinetypeRef::ByLayer,
            linetype_scale: 1.0,
            lineweight: LineWeight::ByLayer,
            kind,
        };
        self.insert(e)
    }

    /// Insert a fully specified entity. `EntityId(0)` means "allocate a new id".
    pub fn insert(&mut self, mut e: Entity) -> EntityId {
        if e.id == EntityId(0) {
            e.id = self.ids.entity();
        } else {
            self.ids.bump_entity(e.id);
        }
        let id = e.id;
        self.touch(id);
        self.drawing.entities.insert(id, e);
        id
    }

    /// Modify an entity in place. Returns `false` if it does not exist.
    pub fn modify(&mut self, id: EntityId, f: impl FnOnce(&mut Entity)) -> bool {
        if !self.drawing.entities.contains_key(&id) {
            return false;
        }
        self.touch(id);
        if let Some(e) = self.drawing.entities.get_mut(&id) {
            f(e);
            e.id = id;
        }
        true
    }

    pub fn remove(&mut self, id: EntityId) -> Option<Entity> {
        if !self.drawing.entities.contains_key(&id) {
            return None;
        }
        self.touch(id);
        self.drawing.entities.remove(&id)
    }

    pub fn tables_mut(&mut self) -> &mut Tables {
        if self.tables_before.is_none() {
            self.tables_before = Some(self.drawing.tables.clone());
        }
        &mut self.drawing.tables
    }

    pub fn blocks_mut(&mut self) -> &mut IndexMap<BlockId, Block> {
        if self.blocks_before.is_none() {
            self.blocks_before = Some(self.drawing.blocks.clone());
        }
        &mut self.drawing.blocks
    }

    pub fn part_mut(&mut self) -> &mut Part {
        if self.part_before.is_none() {
            self.part_before = Some(self.part.clone());
        }
        self.part
    }

    fn finish(self, label: String) -> Transaction {
        let mut entities = Vec::with_capacity(self.touched.len());
        for (id, before) in self.touched {
            let after = self.drawing.entities.get(&id).cloned();
            if before != after {
                entities.push(EntityChange { id, before, after });
            }
        }
        let tables = self.tables_before.filter(|b| *b != self.drawing.tables).map(|b| (b, self.drawing.tables.clone()));
        let blocks = self.blocks_before.filter(|b| *b != self.drawing.blocks).map(|b| (b, self.drawing.blocks.clone()));
        let part = self.part_before.filter(|b| b != &*self.part).map(|b| (b, self.part.clone()));
        Transaction { label, entities, tables, blocks, part }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wcad_geom2d::Line2;
    use wcad_math::DVec2;

    #[test]
    fn undo_redo_round_trip() {
        let mut doc = Document::new();
        let id = doc.transact("line", |tx| tx.add(EntityKind::Line(Line2::new(DVec2::ZERO, DVec2::X))));
        assert_eq!(doc.drawing.entities.len(), 1);
        doc.transact("move", |tx| {
            tx.modify(id, |e| {
                if let EntityKind::Line(l) = &mut e.kind {
                    l.b = DVec2::new(2.0, 0.0);
                }
            })
        });
        doc.transact("layer", |tx| tx.tables_mut().settings.ltscale = 2.0);
        assert_eq!(doc.undo().as_deref(), Some("layer"));
        assert_eq!(doc.drawing.tables.settings.ltscale, 1.0);
        doc.undo();
        doc.undo();
        assert!(doc.drawing.entities.is_empty());
        doc.redo();
        doc.redo();
        match &doc.drawing.entities[&id].kind {
            EntityKind::Line(l) => assert_eq!(l.b, DVec2::new(2.0, 0.0)),
            _ => panic!("expected line"),
        }
        // A transaction that changes nothing is not recorded.
        let before = doc.revision();
        doc.transact("noop", |tx| {
            tx.modify(id, |_| {});
        });
        assert_eq!(doc.revision(), before);
    }
}
