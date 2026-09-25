//! Undo/redo history. A transaction stores before/after snapshots of what it touched.

use indexmap::IndexMap;

use crate::drawing::{Block, Tables};
use crate::entity::Entity;
use crate::ids::{BlockId, EntityId};
use crate::part::Part;

#[derive(Clone, Debug)]
pub(crate) struct EntityChange {
    pub id: EntityId,
    pub before: Option<Entity>,
    pub after: Option<Entity>,
}

#[derive(Clone, Debug)]
pub(crate) struct Transaction {
    pub label: String,
    pub entities: Vec<EntityChange>,
    pub tables: Option<(Tables, Tables)>,
    pub blocks: Option<(IndexMap<BlockId, Block>, IndexMap<BlockId, Block>)>,
    pub part: Option<(Part, Part)>,
}

impl Transaction {
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty() && self.tables.is_none() && self.blocks.is_none() && self.part.is_none()
    }
}

#[derive(Debug)]
pub(crate) struct History {
    pub undo: Vec<Transaction>,
    pub redo: Vec<Transaction>,
    pub limit: usize,
}

impl Default for History {
    fn default() -> Self {
        Self { undo: Vec::new(), redo: Vec::new(), limit: 200 }
    }
}

impl History {
    pub fn push(&mut self, t: Transaction) {
        self.redo.clear();
        self.undo.push(t);
        if self.undo.len() > self.limit {
            let excess = self.undo.len() - self.limit;
            self.undo.drain(..excess);
        }
    }
}
