//! Document → `wcad_render::Batch2D` display lists.
//!
//! Model-space entities are grouped into chunks keyed by (layer, id bucket); each chunk is one
//! batch, so layer visibility toggles without rebuilding and an edit only rebuilds the chunks of
//! the touched entities ([`DisplayCache::update`] consumes `Document::take_changes` via the
//! editor). Curves are flattened with a tolerance tied to the zoom level of the last full build;
//! zooming in or out by more than 4× triggers a rebuild. Everything here is GPU-free: the output
//! is a [`DisplayDelta`] the GPU layer applies to the renderer.

use std::collections::{BTreeSet, HashMap};
use std::hash::{Hash, Hasher};

use wcad_doc::{
    ChangeSet, Color, Drawing, Entity, EntityId, EntityKind, HAlign, LayerId, LineWeight,
    LinetypeId, LinetypeRef, VAlign,
};
use wcad_geom2d::tess::{self, FillRule};
use wcad_geom2d::text::{self, TextSpec};
use wcad_geom2d::{Curve, Curve2};
use wcad_math::{BBox2, DAffine2, DVec2};
use wcad_render::{Batch2D, BatchId, PointShape, Rgba, rgb8};

use crate::select::insert_transform;
use crate::tools::Preview;
use crate::xform::scale_of;

/// Reserved batch ids (drawn in id order: grid first, highlights last).
pub const GRID_BATCH: BatchId = BatchId(0);
pub const HOVER_BATCH: BatchId = BatchId(u64::MAX - 2);
pub const SELECTION_BATCH: BatchId = BatchId(u64::MAX - 1);

/// Entity ids per chunk.
const BUCKET: u64 = 256;
const MAX_BLOCK_DEPTH: usize = 16;
/// Work limit for pathological block nesting (entity visits per build call).
const MAX_VISITS: usize = 2_000_000;
/// Default lineweight (LWDEFAULT) in millimetres.
const DEFAULT_WEIGHT_MM: f64 = 0.25;

/// View-dependent build parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplayParams {
    /// Color of ACI 7 (white on dark backgrounds, black on light ones).
    pub foreground: [u8; 3],
    /// World units per physical pixel at build time (flatten tolerance, linetype density).
    pub units_per_px: f64,
    pub show_lineweights: bool,
}

impl DisplayParams {
    /// Built at `self`, viewed at `now`: flattening too coarse (zoomed in ≥ 1.8×) or far too
    /// fine (zoomed out ≥ 8×)?
    fn zoom_stale(&self, now: &DisplayParams) -> bool {
        let ratio = self.units_per_px / now.units_per_px;
        !ratio.is_finite() || !(1.0 / 8.0..1.8).contains(&ratio)
    }
}

/// Changes for the renderer.
#[derive(Default)]
pub struct DisplayDelta {
    pub upload: Vec<(BatchId, Batch2D)>,
    pub remove: Vec<BatchId>,
    pub visibility: Vec<(BatchId, bool)>,
}

impl DisplayDelta {
    pub fn is_empty(&self) -> bool {
        self.upload.is_empty() && self.remove.is_empty() && self.visibility.is_empty()
    }
}

type ChunkKey = (LayerId, u64);

struct Chunk {
    batch: BatchId,
    entities: BTreeSet<EntityId>,
    has_inserts: bool,
}

#[derive(Default)]
pub struct DisplayCache {
    chunks: HashMap<ChunkKey, Chunk>,
    entity_chunk: HashMap<EntityId, ChunkKey>,
    next_batch: u64,
    params: Option<DisplayParams>,
    appearance: u64,
    /// Number of full rebuilds (diagnostics/tests).
    pub full_builds: u64,
}

/// Hash of everything in the tables that affects appearance (not visibility/lock flags).
fn appearance_hash(d: &Drawing) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let t = &d.tables;
    for (id, l) in &t.layers {
        (id, &l.name, l.color, l.linetype, l.lineweight).hash(&mut h);
    }
    format!(
        "{:?}{:?}{:?}{:?}",
        t.linetypes, t.text_styles, t.dim_styles, t.settings
    )
    .hash(&mut h);
    h.finish()
}

fn layer_shown(d: &Drawing, l: LayerId) -> bool {
    d.layer(l).is_some_and(|l| l.visible && !l.frozen)
}

impl DisplayCache {
    pub fn new() -> Self {
        Self {
            next_batch: 1,
            ..Default::default()
        }
    }

    /// Does the view change require a rebuild with new parameters?
    pub fn needs_rebuild_for(&self, params: &DisplayParams) -> bool {
        match &self.params {
            None => true,
            Some(p) => {
                p.foreground != params.foreground
                    || p.show_lineweights != params.show_lineweights
                    || p.zoom_stale(params)
            }
        }
    }

    /// Number of chunk batches.
    pub fn batch_count(&self) -> usize {
        self.chunks.len()
    }

    /// Apply `changes` (and a parameter change) to the display lists.
    pub fn update(
        &mut self,
        d: &Drawing,
        changes: &ChangeSet,
        params: &DisplayParams,
    ) -> DisplayDelta {
        let mut delta = DisplayDelta::default();
        if self.next_batch == 0 {
            self.next_batch = 1;
        }
        let appearance = if changes.tables || changes.all || self.params.is_none() {
            appearance_hash(d)
        } else {
            self.appearance
        };
        let full = changes.all
            || changes.blocks
            || self.needs_rebuild_for(params)
            || (changes.tables && appearance != self.appearance);
        self.params = Some(*params);
        self.appearance = appearance;
        if full {
            self.full_builds += 1;
            for (_, c) in self.chunks.drain() {
                delta.remove.push(c.batch);
            }
            self.entity_chunk.clear();
            for (id, e) in &d.entities {
                let key = (e.layer, id.0 / BUCKET);
                self.entity_chunk.insert(*id, key);
                let next = &mut self.next_batch;
                let ch = self.chunks.entry(key).or_insert_with(|| {
                    let b = BatchId(*next);
                    *next += 1;
                    Chunk {
                        batch: b,
                        entities: BTreeSet::new(),
                        has_inserts: false,
                    }
                });
                ch.entities.insert(*id);
            }
            let keys: Vec<ChunkKey> = self.chunks.keys().copied().collect();
            for key in keys {
                self.build_chunk(d, key, params, &mut delta);
            }
            return delta;
        }
        let mut dirty: BTreeSet<ChunkKey> = BTreeSet::new();
        if changes.tables {
            // Visibility-only change; inserts may show entities of other layers → rebuild those.
            for (key, ch) in &self.chunks {
                delta.visibility.push((ch.batch, layer_shown(d, key.0)));
                if ch.has_inserts {
                    dirty.insert(*key);
                }
            }
        }
        for id in &changes.entities {
            if let Some(old) = self.entity_chunk.remove(id) {
                if let Some(ch) = self.chunks.get_mut(&old) {
                    ch.entities.remove(id);
                }
                dirty.insert(old);
            }
            if let Some(e) = d.entities.get(id) {
                let key = (e.layer, id.0 / BUCKET);
                self.entity_chunk.insert(*id, key);
                let next = &mut self.next_batch;
                let ch = self.chunks.entry(key).or_insert_with(|| {
                    let b = BatchId(*next);
                    *next += 1;
                    Chunk {
                        batch: b,
                        entities: BTreeSet::new(),
                        has_inserts: false,
                    }
                });
                ch.entities.insert(*id);
                dirty.insert(key);
            }
        }
        for key in dirty {
            self.build_chunk(d, key, params, &mut delta);
        }
        delta
    }

    fn build_chunk(
        &mut self,
        d: &Drawing,
        key: ChunkKey,
        params: &DisplayParams,
        delta: &mut DisplayDelta,
    ) {
        let Some(ch) = self.chunks.get_mut(&key) else {
            return;
        };
        if ch.entities.is_empty() {
            delta.remove.push(ch.batch);
            self.chunks.remove(&key);
            return;
        }
        let origin = ch
            .entities
            .iter()
            .find_map(|id| d.entities.get(id))
            .map(|e| crate::select::entity_bbox(e, d))
            .filter(|b| !b.is_empty())
            .map(|b| b.center())
            .unwrap_or(DVec2::ZERO);
        let mut batch = Batch2D::new(origin);
        let mut em = Emitter::new(d, params, &mut batch, None);
        let mut has_inserts = false;
        for id in &ch.entities {
            if let Some(e) = d.entities.get(id) {
                has_inserts |= matches!(e.kind, EntityKind::Insert(_));
                em.entity(e, &Ctx::root());
            }
        }
        ch.has_inserts = has_inserts;
        delta.visibility.push((ch.batch, layer_shown(d, key.0)));
        delta.upload.push((ch.batch, batch));
    }
}

/// Inheritance context (block inserts).
#[derive(Clone)]
struct Ctx {
    m: DAffine2,
    /// Layer that entities on layer "0" inherit (the insert's layer).
    layer: Option<LayerId>,
    color: Color,
    linetype: Option<LinetypeId>,
    lineweight: LineWeight,
    ltscale: f64,
    depth: usize,
}

impl Ctx {
    fn root() -> Self {
        Ctx {
            m: DAffine2::IDENTITY,
            layer: None,
            color: Color::WHITE,
            linetype: None,
            lineweight: LineWeight::Default,
            ltscale: 1.0,
            depth: 0,
        }
    }
}

struct Style {
    color: Rgba,
    width: f32,
    dash: Option<Vec<f64>>,
}

/// Emits entity graphics into a batch.
pub struct Emitter<'a> {
    d: &'a Drawing,
    params: &'a DisplayParams,
    batch: &'a mut Batch2D,
    override_color: Option<Rgba>,
    /// Extra width added to every stroke (highlights).
    pub extra_width: f32,
    layer0: Option<LayerId>,
    visits: usize,
}

impl<'a> Emitter<'a> {
    pub fn new(
        d: &'a Drawing,
        params: &'a DisplayParams,
        batch: &'a mut Batch2D,
        override_color: Option<Rgba>,
    ) -> Self {
        Self {
            d,
            params,
            batch,
            override_color,
            extra_width: 0.0,
            layer0: d.layer_by_name("0"),
            visits: 0,
        }
    }

    /// Emit a model-space entity (or a ghost copy of one).
    pub fn emit(&mut self, e: &Entity) {
        self.entity(e, &Ctx::root());
    }

    /// Emit a bare curve with an explicit color/width.
    pub fn curve(&mut self, c: &Curve2, color: Rgba, width: f32) {
        self.stroke(
            c,
            &Style {
                color,
                width,
                dash: None,
            },
            &DAffine2::IDENTITY,
        );
    }

    fn tol(&self, m: &DAffine2) -> f64 {
        (self.params.units_per_px * 0.25 / scale_of(m)).max(1e-12)
    }

    fn entity(&mut self, e: &Entity, cx: &Ctx) {
        self.visits += 1;
        if self.visits > MAX_VISITS {
            return;
        }
        let layer_id = match (cx.layer, self.layer0) {
            (Some(l), Some(l0)) if e.layer == l0 => l,
            _ => e.layer,
        };
        let Some(layer) = self.d.layer(layer_id) else {
            return;
        };
        if cx.depth > 0 && (!layer.visible || layer.frozen) {
            return;
        }
        let rgb = e
            .color
            .resolve(layer.color, cx.color, self.params.foreground);
        let color = self
            .override_color
            .unwrap_or_else(|| rgb8(rgb[0], rgb[1], rgb[2]));
        let weight = match e.lineweight {
            LineWeight::ByLayer => layer.lineweight,
            LineWeight::ByBlock => cx.lineweight,
            w => w,
        };
        let weight_mm = match weight {
            LineWeight::Mm100(n) => n as f64 / 100.0,
            _ => DEFAULT_WEIGHT_MM,
        };
        let width = if self.params.show_lineweights {
            (weight_mm / DEFAULT_WEIGHT_MM).clamp(1.0, 12.0) as f32
        } else {
            1.0
        } + self.extra_width;
        let linetype = match e.linetype {
            LinetypeRef::ByLayer => Some(layer.linetype),
            LinetypeRef::ByBlock => cx.linetype,
            LinetypeRef::Id(id) => Some(id),
        };
        let ltscale = self.d.tables.settings.ltscale.abs().max(1e-12)
            * e.linetype_scale.abs().max(1e-12)
            * cx.ltscale;
        let dash = linetype
            .and_then(|id| self.d.tables.linetypes.get(&id))
            .and_then(|lt| {
                let len = lt.pattern_length() * ltscale;
                // Too dense to see at this zoom: draw continuous (AutoCAD does the same).
                (!lt.is_continuous() && len.is_finite() && len > self.params.units_per_px * 4.0)
                    .then(|| lt.pattern.iter().map(|v| v * ltscale).collect())
            });
        let style = Style { color, width, dash };
        let m = cx.m;
        match &e.kind {
            EntityKind::Point { p } => {
                let size = self.d.tables.settings.point_size_px.clamp(1.0, 64.0) as f32;
                self.batch
                    .push_point(m.transform_point2(*p), size, PointShape::Cross, color);
            }
            EntityKind::Text(t) => {
                let plain = text_plain(&t.text);
                let spec = TextSpec {
                    text: &plain,
                    pos: t.pos,
                    height: t.height,
                    rotation: t.rotation,
                    width_factor: t.width_factor,
                    oblique: t.oblique,
                    halign: t.halign,
                    valign: t.valign,
                    line_spacing: 1.0,
                    wrap_width: None,
                };
                self.text(&spec, color, &m);
            }
            EntityKind::MText(t) => {
                let plain = text::mtext_to_plain(&t.text);
                let (halign, valign) = attachment_align(t.attachment);
                let spec = TextSpec {
                    text: &plain,
                    pos: t.pos,
                    height: t.height,
                    rotation: t.rotation,
                    width_factor: 1.0,
                    oblique: 0.0,
                    halign,
                    valign,
                    line_spacing: t.line_spacing,
                    wrap_width: (t.width > 0.0).then_some(t.width),
                };
                self.text(&spec, color, &m);
            }
            EntityKind::Dimension(dim) => {
                let st = crate::dimgen::style_of(dim, self.d);
                let g = crate::dimgen::dimension_geometry(dim, &st, &self.d.tables);
                let solid = Style {
                    dash: None,
                    ..style
                };
                for c in g.curves() {
                    self.stroke(&c, &solid, &m);
                }
                for t in &g.arrows {
                    let p = t.map(|q| m.transform_point2(q));
                    self.batch.push_triangle(p[0], p[1], p[2], color);
                }
                if !g.text.is_empty() {
                    let plain = text::mtext_to_plain(&g.text);
                    let mut spec = TextSpec::simple(&plain, g.text_pos, g.text_height);
                    spec.rotation = g.text_rotation;
                    spec.halign = g.halign;
                    spec.valign = g.valign;
                    self.text(&spec, color, &m);
                }
            }
            EntityKind::Hatch(h) => {
                let loops: Vec<&[Curve2]> = h.loops.iter().map(|l| l.curves.as_slice()).collect();
                if loops.is_empty() {
                    return;
                }
                if h.is_solid() {
                    let (v, i) = tess::fill_loops(&loops, FillRule::EvenOdd, self.tol(&m));
                    let v: Vec<DVec2> = v.into_iter().map(|p| m.transform_point2(p)).collect();
                    let mut c = color;
                    if self.override_color.is_some() {
                        c[3] *= 0.35;
                    }
                    self.batch.push_triangles(&v, &i, c);
                } else {
                    let pat = wcad_geom2d::hatch::builtin(&h.pattern.name)
                        .or_else(|| wcad_geom2d::hatch::builtin("ANSI31"));
                    let solid = Style {
                        dash: None,
                        ..style
                    };
                    match pat {
                        Some(pat)
                            if !wcad_geom2d::hatch::hatch_too_dense(
                                &loops,
                                &pat,
                                h.pattern.scale,
                            ) =>
                        {
                            for l in wcad_geom2d::hatch_lines(
                                &loops,
                                &pat,
                                h.pattern.scale,
                                h.pattern.angle,
                            ) {
                                let (a, b) = (m.transform_point2(l.a), m.transform_point2(l.b));
                                if a == b {
                                    self.batch.push_point(
                                        a,
                                        solid.width + 1.0,
                                        PointShape::Circle,
                                        color,
                                    );
                                } else {
                                    self.batch.push_line(a, b, solid.width, color);
                                }
                            }
                        }
                        _ => {
                            for c in loops.iter().flat_map(|l| l.iter()) {
                                self.stroke(c, &solid, &m);
                            }
                        }
                    }
                }
            }
            EntityKind::Insert(ins) => {
                if cx.depth >= MAX_BLOCK_DEPTH {
                    return;
                }
                let Some(block) = self.d.blocks.get(&ins.block) else {
                    return;
                };
                let local = insert_transform(ins, block.base);
                let s = (ins.scale.x * ins.scale.y).abs().sqrt();
                let child = Ctx {
                    m: m * local,
                    layer: Some(layer_id),
                    color: match e.color {
                        Color::ByLayer => layer.color,
                        Color::ByBlock => cx.color,
                        c => c,
                    },
                    linetype,
                    lineweight: weight,
                    ltscale: cx.ltscale * if s.is_finite() && s > 0.0 { s } else { 1.0 },
                    depth: cx.depth + 1,
                };
                for be in block.entities.values() {
                    self.entity(be, &child);
                }
            }
            other => {
                if let Some(c) = other.as_curve() {
                    self.stroke(&c, &style, &m);
                }
            }
        }
    }

    fn stroke(&mut self, c: &Curve2, style: &Style, m: &DAffine2) {
        let bb = c.bbox();
        if bb.is_empty() {
            return;
        }
        // Never more than ~20k points per curve, whatever the zoom.
        let tol = self.tol(m).max(bb.size().max_element() * 2e-5);
        let pts: Vec<DVec2> = c
            .flatten(tol)
            .into_iter()
            .map(|p| m.transform_point2(p))
            .collect();
        if pts.len() < 2 {
            return;
        }
        match &style.dash {
            Some(pattern) => {
                for piece in wcad_geom2d::dash_polyline(&pts, pattern, 1.0, 0.0) {
                    if piece.len() == 2 && piece[0] == piece[1] {
                        self.batch.push_point(
                            piece[0],
                            style.width + 1.0,
                            PointShape::Circle,
                            style.color,
                        );
                    } else {
                        self.batch
                            .push_polyline(&piece, false, style.width, style.color);
                    }
                }
            }
            None => self
                .batch
                .push_polyline(&pts, false, style.width, style.color),
        }
    }

    fn text(&mut self, spec: &TextSpec<'_>, color: Rgba, m: &DAffine2) {
        if !(spec.height > 0.0) || !spec.height.is_finite() || spec.text.trim().is_empty() {
            return;
        }
        let Some(font) = crate::fonts::cad_font() else {
            return;
        };
        let out = text::layout(font, spec);
        // Text smaller than a pixel: draw its frame outline only.
        if spec.height * scale_of(m) < self.params.units_per_px * 2.0 {
            let f: Vec<DVec2> = out.frame.iter().map(|p| m.transform_point2(*p)).collect();
            self.batch.push_polyline(
                &f,
                true,
                1.0,
                [color[0], color[1], color[2], color[3] * 0.5],
            );
            return;
        }
        let tol = (spec.height / 40.0).max(self.tol(m));
        let (v, i) = tess::fill_paths(&out.glyphs, FillRule::NonZero, tol);
        let v: Vec<DVec2> = v.into_iter().map(|p| m.transform_point2(p)).collect();
        self.batch.push_triangles(&v, &i, color);
    }
}

/// TEXT content with `%%c`/`%%d`/`%%p` codes replaced.
fn text_plain(s: &str) -> String {
    if s.contains("%%") {
        text::mtext_to_plain(s)
    } else {
        s.to_owned()
    }
}

/// MTEXT attachment point (1..=9) → alignment.
pub fn attachment_align(a: u8) -> (HAlign, VAlign) {
    let a = a.clamp(1, 9) - 1;
    let h = [HAlign::Left, HAlign::Center, HAlign::Right][(a % 3) as usize];
    let v = [VAlign::Top, VAlign::Middle, VAlign::Bottom][(a / 3) as usize];
    (h, v)
}

/// Highlight batch for `ids` (selection or hover): thicker translucent strokes in `color`.
pub fn highlight_batch(
    d: &Drawing,
    ids: &[EntityId],
    params: &DisplayParams,
    color: Rgba,
    extra: f32,
) -> Batch2D {
    let origin = ids
        .first()
        .and_then(|id| d.entities.get(id))
        .map(|e| crate::select::entity_bbox(e, d).center())
        .filter(|c| c.is_finite())
        .unwrap_or(DVec2::ZERO);
    let mut b = Batch2D::new(origin);
    let mut em = Emitter::new(d, params, &mut b, Some(color));
    em.extra_width = extra;
    for id in ids.iter().take(20_000) {
        if let Some(e) = d.entities.get(id) {
            em.emit(e);
        }
    }
    b
}

/// Overlay batch for a tool preview.
pub fn preview_batch(
    d: &Drawing,
    preview: &Preview,
    base: Option<DVec2>,
    cursor: Option<DVec2>,
    params: &DisplayParams,
    color: Rgba,
) -> Batch2D {
    let origin = cursor
        .or(base)
        .filter(|p| p.is_finite())
        .unwrap_or(DVec2::ZERO);
    let mut b = Batch2D::new(origin);
    {
        let mut em = Emitter::new(d, params, &mut b, Some(color));
        for g in preview.ghosts.iter().take(5_000) {
            em.emit(g);
        }
        for c in &preview.curves {
            em.curve(c, color, 1.0);
        }
    }
    if preview.rubber_band
        && let (Some(a), Some(c)) = (base, cursor)
        && a.distance(c) > 0.0
    {
        let upp = params.units_per_px;
        b.push_dashed_polyline(
            &[a, c],
            false,
            &[6.0 * upp, -4.0 * upp],
            0.0,
            1.0,
            [color[0], color[1], color[2], 0.8],
        );
    }
    for p in &preview.points {
        b.push_point(*p, 6.0, PointShape::SquareOutline, color);
    }
    b
}

/// Grid lines covering `area` (world), `spacing` adapted to at least `min_px` pixels, major lines
/// every 5 cells, axes through the origin.
pub fn grid_batch(area: &BBox2, spacing: f64, units_per_px: f64, dark: bool) -> (Batch2D, f64) {
    let mut b = Batch2D::new(area.center());
    if area.is_empty() || !(spacing > 0.0) || !(units_per_px > 0.0) {
        return (b, spacing);
    }
    let min_world = units_per_px * 12.0;
    let mut s = spacing;
    let mut guard = 0;
    while s < min_world && guard < 60 {
        s *= if guard % 2 == 0 { 5.0 } else { 2.0 };
        guard += 1;
    }
    let (minor, major) = if dark {
        ([1.0, 1.0, 1.0, 0.06], [1.0, 1.0, 1.0, 0.13])
    } else {
        ([0.0, 0.0, 0.0, 0.06], [0.0, 0.0, 0.0, 0.14])
    };
    let n0x = (area.min.x / s).floor() as i64;
    let n1x = (area.max.x / s).ceil() as i64;
    let n0y = (area.min.y / s).floor() as i64;
    let n1y = (area.max.y / s).ceil() as i64;
    if (n1x - n0x) > 2000 || (n1y - n0y) > 2000 {
        return (b, s);
    }
    for i in n0x..=n1x {
        let x = i as f64 * s;
        let c = if i % 5 == 0 { major } else { minor };
        if i != 0 {
            b.push_line(DVec2::new(x, area.min.y), DVec2::new(x, area.max.y), 1.0, c);
        }
    }
    for j in n0y..=n1y {
        let y = j as f64 * s;
        let c = if j % 5 == 0 { major } else { minor };
        if j != 0 {
            b.push_line(DVec2::new(area.min.x, y), DVec2::new(area.max.x, y), 1.0, c);
        }
    }
    if area.min.y <= 0.0 && area.max.y >= 0.0 {
        b.push_line(
            DVec2::new(area.min.x, 0.0),
            DVec2::new(area.max.x, 0.0),
            1.0,
            [0.85, 0.3, 0.3, 0.5],
        );
    }
    if area.min.x <= 0.0 && area.max.x >= 0.0 {
        b.push_line(
            DVec2::new(0.0, area.min.y),
            DVec2::new(0.0, area.max.y),
            1.0,
            [0.3, 0.75, 0.35, 0.5],
        );
    }
    (b, s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wcad_doc::{Document, Hatch, HatchLoop, HatchPatternRef, Text};
    use wcad_geom2d::{Circle2, Line2};

    fn params() -> DisplayParams {
        DisplayParams {
            foreground: [255, 255, 255],
            units_per_px: 0.05,
            show_lineweights: false,
        }
    }

    #[test]
    fn incremental_chunks() {
        let mut doc = Document::new();
        let ids = doc.transact("t", |tx| {
            vec![
                tx.add(EntityKind::Line(Line2::new(
                    DVec2::ZERO,
                    DVec2::new(10.0, 0.0),
                ))),
                tx.add(EntityKind::Circle(Circle2::new(DVec2::ZERO, 3.0))),
            ]
        });
        let mut cache = DisplayCache::new();
        let ch = doc.take_changes();
        let d = cache.update(&doc.drawing, &ch, &params());
        assert_eq!(d.upload.len(), 1, "one layer, one bucket");
        assert!(!d.upload[0].1.lines.is_empty());
        assert_eq!(cache.full_builds, 1);
        // Edit one entity: only its chunk is rebuilt, no full build.
        doc.transact("m", |tx| tx.remove(ids[0]));
        let ch = doc.take_changes();
        let d = cache.update(&doc.drawing, &ch, &params());
        assert_eq!(d.upload.len(), 1);
        assert_eq!(cache.full_builds, 1);
        // Layer visibility change → visibility only.
        let l0 = doc.drawing.tables.current_layer;
        doc.transact("vis", |tx| {
            if let Some(l) = tx.tables_mut().layers.get_mut(&l0) {
                l.visible = false;
            }
        });
        let ch = doc.take_changes();
        let d = cache.update(&doc.drawing, &ch, &params());
        assert!(d.upload.is_empty());
        assert_eq!(d.visibility.len(), 1);
        assert!(!d.visibility[0].1);
        // Zooming in 16x forces a rebuild.
        let mut p = params();
        p.units_per_px /= 16.0;
        assert!(cache.needs_rebuild_for(&p));
        // Zooming out 4x keeps the (finer) display lists.
        let mut p = params();
        p.units_per_px *= 4.0;
        assert!(!cache.needs_rebuild_for(&p));
        // Remove everything → batch removed.
        doc.transact("rm", |tx| tx.remove(ids[1]));
        let ch = doc.take_changes();
        let d = cache.update(&doc.drawing, &ch, &params());
        assert_eq!(d.remove.len(), 1);
        assert_eq!(cache.batch_count(), 0);
    }

    #[test]
    fn text_hatch_dims_emit_geometry() {
        let mut doc = Document::new();
        let style = doc.drawing.tables.current_text_style;
        let dstyle = doc.drawing.tables.current_dim_style;
        doc.transact("t", |tx| {
            tx.add(EntityKind::Text(Text {
                pos: DVec2::ZERO,
                height: 5.0,
                rotation: 0.0,
                width_factor: 1.0,
                oblique: 0.0,
                style,
                halign: HAlign::Left,
                valign: VAlign::Baseline,
                text: "图纸 A1".into(),
            }));
            let sq = wcad_geom2d::Polyline2::from_points(
                [
                    DVec2::ZERO,
                    DVec2::new(10.0, 0.0),
                    DVec2::new(10.0, 10.0),
                    DVec2::new(0.0, 10.0),
                ],
                true,
            );
            for name in ["SOLID", "ANSI31"] {
                tx.add(EntityKind::Hatch(Hatch {
                    loops: vec![HatchLoop {
                        curves: vec![Curve2::Polyline(sq.clone())],
                    }],
                    pattern: HatchPatternRef {
                        name: name.into(),
                        angle: 0.0,
                        scale: 1.0,
                    },
                }));
            }
            tx.add(EntityKind::Dimension(wcad_doc::Dimension {
                kind: wcad_doc::DimKind::Aligned {
                    p1: DVec2::ZERO,
                    p2: DVec2::new(10.0, 0.0),
                    line_point: DVec2::new(0.0, -5.0),
                },
                style: dstyle,
                text_override: None,
                text_pos: None,
            }));
        });
        let mut cache = DisplayCache::new();
        let ch = doc.take_changes();
        let d = cache.update(&doc.drawing, &ch, &params());
        let b = &d.upload[0].1;
        assert!(
            b.fill_indices.len() > 100,
            "text + solid hatch + arrows triangulated"
        );
        assert!(
            b.lines.len() >= 6,
            "hatch pattern and dimension lines: {}",
            b.lines.len()
        );
    }

    #[test]
    fn grid_adapts_spacing() {
        let area = BBox2::new(DVec2::splat(-100.0), DVec2::splat(100.0));
        let (b, s) = grid_batch(&area, 10.0, 0.1, true);
        assert_eq!(s, 10.0);
        assert!(!b.lines.is_empty());
        let (_, s) = grid_batch(&area, 10.0, 10.0, true);
        assert!(s >= 120.0);
    }
}
