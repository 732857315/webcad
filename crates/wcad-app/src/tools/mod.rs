//! Interactive tool framework (ARCHITECTURE §5.8).
//!
//! A [`Tool`] is a small state machine driven by [`ToolInput`]s: points (clicked, snapped or typed),
//! numbers, keywords, text, selections, Enter and Escape. It reads the document and mutates it only
//! through [`ToolCx::transact`] (one undo step per call), and describes rubber-band geometry in
//! [`Tool::preview`]. The [`crate::editor::Editor`] owns the active tool and translates viewport
//! and command-line events into inputs: it resolves relative/polar coordinates against the last
//! point, applies object snaps, ortho, polar tracking and direct-distance entry, and runs the
//! selection prompt for tools that [`Accept::selection`].
//!
//! Feature modules add tools by registering commands (see [`crate::commands::CommandRegistry`]):
//! [`draw`], [`modify`], [`annotate`] are theirs; [`core`] holds the proof tools.

pub mod annotate;
pub mod core;
pub mod draw;
pub mod modify;

use std::collections::BTreeSet;

use wcad_doc::{Document, Drawing, Entity, EntityId, EntityKind, Tx};
use wcad_geom2d::Curve2;
use wcad_math::DVec2;

use crate::editor::{AppRequest, LogKind, LogLine};
use crate::i18n::Lang;
use crate::select::{self, Selection};
use crate::settings::DraftSettings;
use crate::snap::SpatialIndex;

/// A prompt option. The user can type `key` (shortcut letters), the `id`, a prefix of the `id`, or
/// the localized `label`, or click it in the command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Keyword {
    /// Stable identifier sent back as [`ToolInput::Keyword`] (English, e.g. `"Close"`).
    pub id: &'static str,
    /// Shortcut letters (e.g. `"C"`).
    pub key: &'static str,
    /// Localized label shown in the prompt.
    pub label: &'static str,
}

/// Input delivered to a tool.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolInput {
    /// A point: clicked (snapped/constrained) or typed (resolved to world coordinates).
    Point(DVec2),
    /// Cursor moved (already snapped/constrained).
    Hover(DVec2),
    /// A number typed while the tool [`Accept::value`]s.
    Value(f64),
    /// Free text typed while the tool [`Accept::text`]s.
    Text(String),
    /// One of the tool's [`Tool::keywords`] (its id).
    Keyword(&'static str),
    /// The finished selection (for tools that [`Accept::selection`]); also sent at start when
    /// something was pre-selected.
    Selection(Vec<EntityId>),
    Enter,
    Escape,
}

/// Result of handling an input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolFlow {
    Continue,
    /// Finished normally.
    Done,
    /// Aborted (prints `*Cancel*`).
    Cancel,
}

/// What kinds of input the current step accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Accept {
    pub point: bool,
    /// Bare numbers are values (radius, distance, count …) instead of direct distance entry.
    pub value: bool,
    pub text: bool,
    /// Viewport clicks/windows edit the selection; Enter sends [`ToolInput::Selection`].
    pub selection: bool,
    /// Clicks are object picks (TRIM, FILLET, …): delivered as [`ToolInput::Point`] at the raw
    /// cursor position, without object snaps, ortho or polar; the pick box is shown and the
    /// entity under the cursor is highlighted. Use [`ToolCx::pick`] to resolve the entity.
    pub pick: bool,
}

impl Accept {
    pub const POINT: Accept = Accept {
        point: true,
        value: false,
        text: false,
        selection: false,
        pick: false,
    };
    pub const POINT_OR_VALUE: Accept = Accept {
        point: true,
        value: true,
        text: false,
        selection: false,
        pick: false,
    };
    pub const VALUE: Accept = Accept {
        point: false,
        value: true,
        text: false,
        selection: false,
        pick: false,
    };
    pub const TEXT: Accept = Accept {
        point: false,
        value: false,
        text: true,
        selection: false,
        pick: false,
    };
    pub const SELECTION: Accept = Accept {
        point: false,
        value: false,
        text: false,
        selection: true,
        pick: false,
    };
    /// Object pick (see [`Accept::pick`]).
    pub const PICK: Accept = Accept {
        point: true,
        value: false,
        text: false,
        selection: false,
        pick: true,
    };
    pub const NONE: Accept = Accept {
        point: false,
        value: false,
        text: false,
        selection: false,
        pick: false,
    };
}

/// Rubber-band output of [`Tool::preview`], drawn in the preview color on top of the drawing.
#[derive(Clone, Debug, Default)]
pub struct Preview {
    /// Temporary curves (world coordinates).
    pub curves: Vec<Curve2>,
    /// Ghost entities (e.g. the selection displaced by MOVE), drawn with full display logic.
    pub ghosts: Vec<Entity>,
    /// Marker points.
    pub points: Vec<DVec2>,
    /// Draw a dashed line from the tool's base point to the cursor.
    pub rubber_band: bool,
}

impl Preview {
    pub fn is_empty(&self) -> bool {
        self.curves.is_empty()
            && self.ghosts.is_empty()
            && self.points.is_empty()
            && !self.rubber_band
    }
    pub fn clear(&mut self) {
        *self = Preview::default();
    }
}

/// An interactive command. See the module docs.
pub trait Tool {
    /// Command name, e.g. `"LINE"`.
    fn name(&self) -> &'static str;
    /// Prompt of the current step (without keywords or trailing colon).
    fn prompt(&self, lang: Lang) -> String;
    /// Options of the current step.
    fn keywords(&self, _lang: Lang) -> Vec<Keyword> {
        Vec::new()
    }
    /// Accepted input kinds of the current step.
    fn accepts(&self) -> Accept {
        Accept::POINT
    }
    /// Reference point of the current step: origin of the rubber band, ortho/polar tracking,
    /// direct distance entry and perpendicular/tangent snaps.
    fn base_point(&self) -> Option<DVec2> {
        None
    }
    /// Called once when the tool becomes active. Returning `Done`/`Cancel` ends it immediately.
    fn start(&mut self, _cx: &mut ToolCx<'_>) -> ToolFlow {
        ToolFlow::Continue
    }
    fn on_input(&mut self, input: ToolInput, cx: &mut ToolCx<'_>) -> ToolFlow;
    /// Rubber-band geometry for the current cursor position (`cx.cursor()`).
    fn preview(&self, _cx: &ToolCx<'_>, _out: &mut Preview) {}
}

/// Everything a tool may touch.
pub struct ToolCx<'a> {
    pub(crate) doc: &'a mut Document,
    pub(crate) selection: &'a mut Selection,
    pub(crate) draft: &'a DraftSettings,
    pub(crate) index: &'a SpatialIndex,
    pub(crate) log: &'a mut Vec<LogLine>,
    pub(crate) requests: &'a mut Vec<AppRequest>,
    pub(crate) lang: Lang,
    pub(crate) cursor: Option<DVec2>,
    pub(crate) last_point: Option<DVec2>,
    pub(crate) units_per_px: f64,
}

impl<'a> ToolCx<'a> {
    pub fn doc(&self) -> &Document {
        self.doc
    }
    pub fn drawing(&self) -> &Drawing {
        &self.doc.drawing
    }
    pub fn entity(&self, id: EntityId) -> Option<&Entity> {
        self.doc.drawing.entities.get(&id)
    }
    pub fn lang(&self) -> Lang {
        self.lang
    }
    pub fn draft(&self) -> &DraftSettings {
        self.draft
    }
    /// Current (snapped/constrained) cursor position in world coordinates.
    pub fn cursor(&self) -> Option<DVec2> {
        self.cursor
    }
    /// The last point entered in any command (base for `@` input).
    pub fn last_point(&self) -> Option<DVec2> {
        self.last_point
    }
    /// World units per logical pixel of the active viewport (for pick tolerances).
    pub fn units_per_px(&self) -> f64 {
        self.units_per_px
    }
    pub fn index(&self) -> &SpatialIndex {
        self.index
    }

    /// Run `f` as one undoable step.
    pub fn transact<R>(&mut self, label: impl Into<String>, f: impl FnOnce(&mut Tx<'_>) -> R) -> R {
        self.doc.transact(label, f)
    }

    /// Undo the last document step (e.g. a LINE segment for the in-command Undo keyword).
    pub fn undo(&mut self) -> Option<String> {
        self.doc.undo()
    }

    pub fn selection(&self) -> &Selection {
        self.selection
    }
    pub fn selection_mut(&mut self) -> &mut Selection {
        self.selection
    }

    /// Selected entity ids, dropping ids that no longer exist.
    pub fn selected_ids(&self) -> Vec<EntityId> {
        self.selection
            .iter()
            .filter(|id| self.doc.drawing.entities.contains_key(id))
            .collect()
    }

    /// The topmost editable entity under `p` within the pick box, if any.
    pub fn pick(&self, p: DVec2) -> Option<EntityId> {
        let tol = self.draft.pickbox_px as f64 * self.units_per_px;
        select::pick(&self.doc.drawing, self.index, p, tol)
    }

    /// Of `ids`, the ones on editable (unlocked, visible) layers; prints a note about the rest.
    pub fn editable(&mut self, ids: &[EntityId]) -> Vec<EntityId> {
        let d = &self.doc.drawing;
        let (ok, locked): (Vec<EntityId>, Vec<EntityId>) = ids
            .iter()
            .copied()
            .filter(|id| d.entities.contains_key(id))
            .partition(|id| {
                d.entities
                    .get(id)
                    .is_some_and(|e| d.is_layer_editable(e.layer))
            });
        if !locked.is_empty() {
            let s = crate::i18n::core(self.lang);
            self.message(crate::i18n::fmt(s.layer_locked, &[&locked.len()]));
        }
        ok
    }

    /// Print an informational line in the command history.
    pub fn message(&mut self, text: impl Into<String>) {
        self.log.push(LogLine {
            kind: LogKind::Info,
            text: text.into(),
        });
    }
    /// Print an error line in the command history.
    pub fn error(&mut self, text: impl Into<String>) {
        self.log.push(LogLine {
            kind: LogKind::Error,
            text: text.into(),
        });
    }
    /// Ask the application for something outside the editor (zoom, file dialogs …).
    pub fn request(&mut self, r: AppRequest) {
        self.requests.push(r);
    }
}

/// Entities of `ids` as owned values (for previews and transforms).
pub fn entities_of(d: &Drawing, ids: &[EntityId]) -> Vec<Entity> {
    ids.iter()
        .filter_map(|id| d.entities.get(id).cloned())
        .collect()
}

/// Deduplicate while keeping order.
pub fn dedup_ids(ids: impl IntoIterator<Item = EntityId>) -> Vec<EntityId> {
    let mut seen = BTreeSet::new();
    ids.into_iter().filter(|id| seen.insert(*id)).collect()
}

/// Convenience: add a curve entity on the current layer.
pub fn add_curve(tx: &mut Tx<'_>, c: Curve2) -> EntityId {
    tx.add(EntityKind::from_curve(c))
}
