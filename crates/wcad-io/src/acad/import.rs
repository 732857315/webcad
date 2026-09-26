//! `acadrust::CadDocument` → `wcad_doc::Document`.

use std::collections::{BTreeMap, HashMap};
use std::f64::consts::{PI, TAU};

use acadrust::CadDocument;
use acadrust::entities::dimension::Dimension as ADim;
use acadrust::entities::hatch::{BoundaryEdge, BoundaryPath};
use acadrust::entities::mtext::AttachmentPoint;
use acadrust::entities::text::{TextHorizontalAlignment, TextVerticalAlignment};
use acadrust::entities::{EntityCommon, EntityType};
use acadrust::types::Vector3;
use wcad_doc::{
    Block, BlockId, Color, DimKind, DimStyle, DimStyleId, DocMeta, Document, Drawing, Entity,
    EntityKind, HAlign, Hatch, HatchLoop, HatchPatternRef, IdAllocator, Insert, Layer, LayerId,
    LineWeight, Linetype, LinetypeId, LinetypeRef, MText, Part, Text, TextStyle, TextStyleId,
    VAlign,
};
use wcad_geom2d::{Arc2, Circle2, Curve2, EllipseArc2, Line2, Nurbs2, PolyVertex, Polyline2};
use wcad_math::DVec2;

use super::{Flavor, color_from, layer_color_from, lineweight_from, units_from, v2, v2d};
use crate::ImportReport;

/// Extra information the caller extracted from the raw file (acadrust does not keep it).
#[derive(Default)]
pub(crate) struct Extras {
    /// Block base points by upper-case block name (DXF only).
    pub block_bases: HashMap<String, DVec2>,
}

/// Aggregated warnings: counted messages plus free-form ones.
#[derive(Default)]
struct Warnings {
    counted: BTreeMap<String, usize>,
    plain: Vec<String>,
}

impl Warnings {
    fn count(&mut self, key: impl Into<String>) {
        *self.counted.entry(key.into()).or_insert(0) += 1;
    }
    fn push(&mut self, msg: impl Into<String>) {
        if self.plain.len() < 200 {
            self.plain.push(msg.into());
        }
    }
    fn finish(self) -> Vec<String> {
        let mut out = self.plain;
        for (k, n) in self.counted {
            out.push(if n == 1 { k } else { format!("{k} (×{n})") });
        }
        out
    }
}

struct Ctx<'a> {
    cad: &'a CadDocument,
    flavor: Flavor,
    ids: IdAllocator,
    drawing: Drawing,
    warnings: Warnings,
    layers: HashMap<String, LayerId>,
    linetypes: HashMap<String, LinetypeId>,
    styles: HashMap<String, TextStyleId>,
    dimstyles: HashMap<String, DimStyleId>,
    blocks: HashMap<String, BlockId>,
}

/// Table/lookup key: `\U+XXXX` escapes (pre-2007 files) decoded, ASCII upper-cased.
fn key(s: &str) -> String {
    decode(s).to_ascii_uppercase()
}

fn decode(s: &str) -> String {
    crate::mtext::decode_unicode_escapes(s)
}

pub(crate) fn to_document(cad: &CadDocument, flavor: Flavor, extras: &Extras) -> ImportReport {
    let mut ids = IdAllocator::default();
    let drawing = Drawing::new(&mut ids);
    let mut cx = Ctx {
        cad,
        flavor,
        ids,
        drawing,
        warnings: Warnings::default(),
        layers: HashMap::new(),
        linetypes: HashMap::new(),
        styles: HashMap::new(),
        dimstyles: HashMap::new(),
        blocks: HashMap::new(),
    };
    for (&id, l) in &cx.drawing.tables.linetypes {
        cx.linetypes.insert(key(&l.name), id);
    }
    for (&id, l) in &cx.drawing.tables.layers {
        cx.layers.insert(key(&l.name), id);
    }
    for (&id, s) in &cx.drawing.tables.text_styles {
        cx.styles.insert(key(&s.name), id);
    }
    for (&id, s) in &cx.drawing.tables.dim_styles {
        cx.dimstyles.insert(key(&s.name), id);
    }

    let mut meta = DocMeta::default();
    match units_from(cad.header.insertion_units) {
        Some(u) => meta.units = u,
        None => {
            meta.units = wcad_doc::Units::Unitless;
            cx.warnings.push(format!(
                "unsupported drawing units ($INSUNITS = {}), imported as unitless",
                cad.header.insertion_units
            ));
        }
    }
    let lts = cad.header.linetype_scale;
    if lts.is_finite() && lts > 0.0 {
        cx.drawing.tables.settings.ltscale = lts;
    }

    cx.import_linetypes();
    cx.import_layers();
    cx.import_text_styles();
    cx.import_dim_styles();
    cx.import_blocks(extras);

    // Model space, in file order (= draw order).
    let model: Vec<&EntityType> = cad.model_space_entities().collect();
    for e in model {
        let mut out = Vec::new();
        cx.map_entity(e, &mut out);
        for ent in out {
            cx.drawing.entities.insert(ent.id, ent);
        }
    }
    let paper: usize = cad
        .block_records
        .iter()
        .filter(|br| br.is_paper_space())
        .map(|br| br.entity_handles.len())
        .sum();
    if paper > 0 {
        cx.warnings.push(format!(
            "{paper} paper-space entities were not imported (model space only)"
        ));
    }

    let Ctx {
        ids,
        drawing,
        warnings,
        ..
    } = cx;
    ImportReport {
        document: Document::from_parts(meta, drawing, Part::default(), ids),
        warnings: warnings.finish(),
    }
}

impl Ctx<'_> {
    // ----------------------------------------------------------------------------- tables

    fn import_linetypes(&mut self) {
        for lt in self.cad.line_types.iter() {
            let k = key(&lt.name);
            if k == "BYLAYER" || k == "BYBLOCK" || lt.name.is_empty() {
                continue;
            }
            let pattern: Vec<f64> = lt
                .elements
                .iter()
                .map(|e| e.length)
                .filter(|l| l.is_finite())
                .collect();
            if lt.elements.iter().any(|e| e.complex.is_some()) {
                self.warnings
                    .count("complex linetype shapes/text were simplified to dashes");
            }
            let def = Linetype {
                name: decode(&lt.name),
                description: decode(&lt.description),
                pattern,
            };
            match self.linetypes.get(&k) {
                Some(&id) => {
                    // Keep "Continuous" continuous even if a file defines it oddly.
                    if k != "CONTINUOUS"
                        && let Some(slot) = self.drawing.tables.linetypes.get_mut(&id)
                    {
                        *slot = def;
                    }
                }
                None => {
                    let id = self.ids.linetype();
                    self.drawing.tables.linetypes.insert(id, def);
                    self.linetypes.insert(k, id);
                }
            }
        }
    }

    fn continuous(&self) -> LinetypeId {
        self.linetypes
            .get("CONTINUOUS")
            .copied()
            .or_else(|| self.drawing.tables.linetypes.keys().next().copied())
            .unwrap_or(LinetypeId(1))
    }

    fn import_layers(&mut self) {
        for l in self.cad.layers.iter() {
            if l.name.is_empty() {
                continue;
            }
            let lt = self
                .linetypes
                .get(&key(&l.line_type))
                .copied()
                .unwrap_or_else(|| self.continuous());
            let lineweight = match lineweight_from(l.line_weight) {
                LineWeight::ByLayer | LineWeight::ByBlock => LineWeight::Default,
                w => w,
            };
            let def = Layer {
                name: decode(&l.name),
                color: layer_color_from(l.color),
                linetype: lt,
                lineweight,
                visible: !l.flags.off,
                frozen: l.flags.frozen,
                locked: l.flags.locked,
                plot: l.is_plottable,
            };
            let k = key(&l.name);
            match self.layers.get(&k) {
                Some(&id) => {
                    if let Some(slot) = self.drawing.tables.layers.get_mut(&id) {
                        *slot = def;
                    }
                }
                None => {
                    let id = self.ids.layer();
                    self.drawing.tables.layers.insert(id, def);
                    self.layers.insert(k, id);
                }
            }
        }
    }

    fn layer_id(&mut self, name: &str) -> LayerId {
        let k = key(if name.is_empty() { "0" } else { name });
        if let Some(&id) = self.layers.get(&k) {
            return id;
        }
        // Entities may reference layers missing from the table: create them like AutoCAD does.
        let id = self.ids.layer();
        let lt = self.continuous();
        self.drawing.tables.layers.insert(
            id,
            Layer {
                name: if name.is_empty() {
                    "0".into()
                } else {
                    decode(name)
                },
                color: Color::WHITE,
                linetype: lt,
                lineweight: LineWeight::Default,
                visible: true,
                frozen: false,
                locked: false,
                plot: true,
            },
        );
        self.layers.insert(k, id);
        id
    }

    fn import_text_styles(&mut self) {
        for s in self.cad.text_styles.iter() {
            if s.name.is_empty() || s.is_shape_file {
                continue;
            }
            let oblique = match self.flavor {
                Flavor::Dxf => s.oblique_angle.to_radians(),
                Flavor::Dwg => s.oblique_angle,
            };
            let font = if !s.true_type_font.is_empty() {
                s.true_type_font.clone()
            } else {
                s.font_file.clone()
            };
            // AutoCAD's generic default shape font maps to our embedded default font.
            let generic = font.is_empty()
                || font.eq_ignore_ascii_case("txt")
                || font.eq_ignore_ascii_case("txt.shx");
            let def = TextStyle {
                name: decode(&s.name),
                font: if generic { "default".into() } else { font },
                height: if s.height.is_finite() && s.height > 0.0 {
                    s.height
                } else {
                    0.0
                },
                width_factor: if s.width_factor.is_finite() && s.width_factor > 0.0 {
                    s.width_factor
                } else {
                    1.0
                },
                oblique: if oblique.is_finite() { oblique } else { 0.0 },
            };
            let k = key(&s.name);
            match self.styles.get(&k) {
                Some(&id) => {
                    if let Some(slot) = self.drawing.tables.text_styles.get_mut(&id) {
                        *slot = def;
                    }
                }
                None => {
                    let id = self.ids.text_style();
                    self.drawing.tables.text_styles.insert(id, def);
                    self.styles.insert(k, id);
                }
            }
        }
    }

    fn style_id(&self, name: &str) -> TextStyleId {
        self.styles
            .get(&key(name))
            .copied()
            .unwrap_or(self.drawing.tables.current_text_style)
    }

    fn import_dim_styles(&mut self) {
        for d in self.cad.dim_styles.iter() {
            if d.name.is_empty() {
                continue;
            }
            let pos = |v: f64, dflt: f64| if v.is_finite() && v >= 0.0 { v } else { dflt };
            let (prefix, suffix) = match d.dimpost.split_once("<>") {
                Some((p, s)) => (p.to_string(), s.to_string()),
                None => (String::new(), d.dimpost.clone()),
            };
            let decimals = d.dimdec.clamp(0, 8) as u8;
            let def = DimStyle {
                name: decode(&d.name),
                text_style: self.style_id(&d.dimtxsty),
                text_height: pos(d.dimtxt, 2.5),
                arrow_size: pos(d.dimasz, 2.5),
                ext_offset: pos(d.dimexo, 0.625),
                ext_extend: pos(d.dimexe, 1.25),
                text_gap: pos(d.dimgap.abs(), 0.625),
                scale: if d.dimscale.is_finite() && d.dimscale > 0.0 {
                    d.dimscale
                } else {
                    1.0
                },
                decimals,
                angle_decimals: if d.dimadec < 0 {
                    decimals
                } else {
                    d.dimadec.clamp(0, 8) as u8
                },
                prefix,
                suffix,
            };
            let k = key(&d.name);
            match self.dimstyles.get(&k) {
                Some(&id) => {
                    if let Some(slot) = self.drawing.tables.dim_styles.get_mut(&id) {
                        *slot = def;
                    }
                }
                None => {
                    let id = self.ids.dim_style();
                    self.drawing.tables.dim_styles.insert(id, def);
                    self.dimstyles.insert(k, id);
                }
            }
        }
    }

    // ----------------------------------------------------------------------------- blocks

    fn import_blocks(&mut self, extras: &Extras) {
        let cad = self.cad;
        let mut records = Vec::new();
        for br in cad.block_records.iter() {
            let name = br.name.as_str();
            let upper = key(name);
            if br.is_model_space() || br.is_paper_space() || upper.starts_with("*MODEL_SPACE") {
                continue;
            }
            // Anonymous dimension blocks (*D1, *D2, ...): we regenerate dimension graphics ourselves.
            if is_dimension_block(&upper) {
                continue;
            }
            if br.flags.is_xref || br.flags.is_xref_overlay {
                self.warnings
                    .count("external references (xrefs) are not supported");
                continue;
            }
            let id = self.ids.block();
            self.blocks.insert(upper, id);
            records.push((id, br));
        }
        for (id, br) in records {
            let mut base = v2(br.base_point);
            if base == DVec2::ZERO {
                if let Some(b) = extras.block_bases.get(&key(&br.name)) {
                    base = *b;
                } else if let Some(EntityType::Block(b)) = cad.get_entity(br.block_entity_handle) {
                    base = v2(b.base_point);
                }
            }
            if !(base.x.is_finite() && base.y.is_finite()) {
                base = DVec2::ZERO;
            }
            let mut entities = BTreeMap::new();
            let members: Vec<&EntityType> = cad.entities_in_block(&br.name).collect();
            for e in members {
                let mut out = Vec::new();
                self.map_entity(e, &mut out);
                for ent in out {
                    entities.insert(ent.id, ent);
                }
            }
            self.drawing.blocks.insert(
                id,
                Block {
                    name: decode(&br.name),
                    base,
                    entities,
                },
            );
        }
    }

    // ----------------------------------------------------------------------------- entities

    fn base_entity(&mut self, c: &EntityCommon, kind: EntityKind) -> Entity {
        let linetype = {
            let k = key(&c.linetype);
            if k.is_empty() || k == "BYLAYER" {
                LinetypeRef::ByLayer
            } else if k == "BYBLOCK" {
                LinetypeRef::ByBlock
            } else if let Some(&id) = self.linetypes.get(&k) {
                LinetypeRef::Id(id)
            } else {
                self.warnings.count(format!(
                    "unknown linetype '{}' replaced by ByLayer",
                    c.linetype
                ));
                LinetypeRef::ByLayer
            }
        };
        let lts = c.linetype_scale;
        Entity {
            id: self.ids.entity(),
            layer: self.layer_id(&c.layer),
            color: color_from(c.color),
            linetype,
            linetype_scale: if lts.is_finite() && lts > 0.0 {
                lts
            } else {
                1.0
            },
            lineweight: lineweight_from(c.line_weight),
            kind,
        }
    }

    /// Whether an OCS normal is flipped (−Z). Non-planar normals are projected with a warning.
    fn ocs_flip(&mut self, n: Vector3) -> bool {
        let len = (n.x * n.x + n.y * n.y + n.z * n.z).sqrt();
        if !(len > 0.0) {
            return false;
        }
        if (n.x / len).abs() > 1e-6 || (n.y / len).abs() > 1e-6 {
            self.warnings
                .count("entities outside the XY plane were projected onto it");
        }
        n.z < 0.0
    }

    fn push(&mut self, c: &EntityCommon, kind: EntityKind, flip: bool, out: &mut Vec<Entity>) {
        let kind = if flip { mirror_x(kind) } else { kind };
        let e = self.base_entity(c, kind);
        out.push(e);
    }

    fn map_entity(&mut self, e: &EntityType, out: &mut Vec<Entity>) {
        let c = e.common();
        if c.invisible {
            return;
        }
        match e {
            EntityType::Point(p) => {
                self.push(c, EntityKind::Point { p: v2(p.location) }, false, out)
            }
            EntityType::Line(l) => {
                self.push(
                    c,
                    EntityKind::Line(Line2::new(v2(l.start), v2(l.end))),
                    false,
                    out,
                );
            }
            EntityType::Circle(ci) => {
                if !(ci.radius.is_finite() && ci.radius > 0.0) {
                    self.warnings.count("degenerate circles skipped");
                    return;
                }
                let flip = self.ocs_flip(ci.normal);
                self.push(
                    c,
                    EntityKind::Circle(Circle2::new(v2(ci.center), ci.radius)),
                    flip,
                    out,
                );
            }
            EntityType::Arc(a) => {
                if !(a.radius.is_finite() && a.radius > 0.0) {
                    self.warnings.count("degenerate arcs skipped");
                    return;
                }
                let flip = self.ocs_flip(a.normal);
                let arc = Arc2::new(v2(a.center), a.radius, a.start_angle, a.end_angle);
                self.push(c, EntityKind::Arc(arc), flip, out);
            }
            EntityType::Ellipse(el) => {
                let major = v2(el.major_axis);
                if major.length_squared() < 1e-24 || !(el.minor_axis_ratio > 0.0) {
                    self.warnings.count("degenerate ellipses skipped");
                    return;
                }
                let flip = self.ocs_flip(el.normal);
                let (mut start, mut end) = (el.start_parameter, el.end_parameter);
                if flip {
                    // WCS geometry, but the parameter runs clockwise.
                    (start, end) = (-end, -start);
                }
                let ratio = el.minor_axis_ratio.min(1.0);
                let e2 = EllipseArc2 {
                    c: v2(el.center),
                    major,
                    ratio,
                    start,
                    end,
                };
                self.push(c, EntityKind::Ellipse(e2), false, out);
            }
            EntityType::LwPolyline(p) => {
                let verts: Vec<PolyVertex> = p
                    .vertices
                    .iter()
                    .map(|v| {
                        PolyVertex::with_bulge(
                            v2d(v.location),
                            if v.bulge.is_finite() { v.bulge } else { 0.0 },
                        )
                    })
                    .collect();
                if verts.is_empty() {
                    return;
                }
                let flip = self.ocs_flip(p.normal);
                self.push(
                    c,
                    EntityKind::Polyline(Polyline2 {
                        verts,
                        closed: p.is_closed,
                    }),
                    flip,
                    out,
                );
            }
            EntityType::Polyline2D(p) => {
                let verts: Vec<PolyVertex> = p
                    .vertices
                    .iter()
                    .filter(|v| v.flags.bits() & 16 == 0) // skip spline frame control points
                    .map(|v| {
                        PolyVertex::with_bulge(
                            v2(v.location),
                            if v.bulge.is_finite() { v.bulge } else { 0.0 },
                        )
                    })
                    .collect();
                if verts.is_empty() {
                    return;
                }
                let flip = self.ocs_flip(p.normal);
                self.push(
                    c,
                    EntityKind::Polyline(Polyline2 {
                        verts,
                        closed: p.is_closed(),
                    }),
                    flip,
                    out,
                );
            }
            EntityType::Polyline3D(p) => {
                if p.flags.is_3d_mesh || p.flags.is_polyface_mesh {
                    self.warnings.count("polygon meshes are not supported");
                    return;
                }
                let verts: Vec<PolyVertex> = p
                    .vertices
                    .iter()
                    .filter(|v| v.flags & 16 == 0)
                    .map(|v| PolyVertex::new(v2(v.position)))
                    .collect();
                if verts.is_empty() {
                    return;
                }
                self.warnings
                    .count("3D polylines were projected onto the XY plane");
                self.push(
                    c,
                    EntityKind::Polyline(Polyline2 {
                        verts,
                        closed: p.flags.closed,
                    }),
                    false,
                    out,
                );
            }
            EntityType::Polyline(p) => {
                let verts: Vec<PolyVertex> = p
                    .vertices
                    .iter()
                    .filter(|v| v.flags.bits() & 16 == 0)
                    .map(|v| PolyVertex::new(v2(v.location)))
                    .collect();
                if verts.is_empty() {
                    return;
                }
                self.warnings
                    .count("3D polylines were projected onto the XY plane");
                self.push(
                    c,
                    EntityKind::Polyline(Polyline2 {
                        verts,
                        closed: p.is_closed(),
                    }),
                    false,
                    out,
                );
            }
            EntityType::Spline(s) => {
                let rational = s.flags.rational && s.weights.len() == s.control_points.len();
                let n = Nurbs2 {
                    degree: s.degree.clamp(1, 32) as u32,
                    ctrl: s.control_points.iter().map(|p| v2(*p)).collect(),
                    weights: if rational {
                        s.weights.clone()
                    } else {
                        Vec::new()
                    },
                    knots: s.knots.clone(),
                    fit_points: s.fit_points.iter().map(|p| v2(*p)).collect(),
                    closed: s.flags.closed || s.flags.periodic,
                };
                if n.ctrl.len() < 2 && n.fit_points.len() < 2 {
                    self.warnings.count("degenerate splines skipped");
                    return;
                }
                self.push(c, EntityKind::Spline(n), false, out);
            }
            EntityType::Text(t) => {
                let (halign, valign, fit) =
                    text_align(&t.horizontal_alignment, &t.vertical_alignment);
                let left_baseline = halign == HAlign::Left && valign == VAlign::Baseline && !fit;
                let pos = if left_baseline {
                    v2(t.insertion_point)
                } else {
                    v2(t.alignment_point.unwrap_or(t.insertion_point))
                };
                let mut rotation = t.rotation;
                if fit && let Some(ap) = t.alignment_point {
                    let d = v2(ap) - v2(t.insertion_point);
                    if d.length_squared() > 1e-24 {
                        rotation = d.y.atan2(d.x);
                    }
                }
                let pos = if fit { v2(t.insertion_point) } else { pos };
                let style = self.style_id(&t.style);
                let text = Text {
                    pos,
                    height: self.text_height(t.height, style),
                    rotation: finite_or(rotation, 0.0),
                    width_factor: positive_or(t.width_factor, 1.0),
                    oblique: finite_or(t.oblique_angle, 0.0),
                    style,
                    halign,
                    valign,
                    text: crate::mtext::decode_unicode_escapes(&t.value),
                };
                let flip = self.ocs_flip(t.normal);
                self.push(c, EntityKind::Text(text), flip, out);
            }
            EntityType::MText(m) => {
                let style = self.style_id(&m.style);
                let mt = MText {
                    pos: v2(m.insertion_point),
                    height: self.text_height(m.height, style),
                    width: if m.rectangle_width.is_finite() && m.rectangle_width > 0.0 {
                        m.rectangle_width
                    } else {
                        0.0
                    },
                    rotation: finite_or(m.rotation, 0.0),
                    line_spacing: positive_or(m.line_spacing_factor, 1.0),
                    attachment: attachment_u8(m.attachment_point),
                    style,
                    text: crate::mtext::decode_unicode_escapes(&m.value),
                };
                self.push(c, EntityKind::MText(mt), false, out);
            }
            EntityType::Dimension(d) => self.map_dimension(c, d, out),
            EntityType::Hatch(h) => {
                let flip = self.ocs_flip(h.normal);
                let mut loops = Vec::new();
                for path in &h.paths {
                    let lp = self.hatch_loop(path);
                    if !lp.curves.is_empty() {
                        loops.push(lp);
                    }
                }
                if loops.is_empty() {
                    self.warnings
                        .count("hatches without usable boundaries skipped");
                    return;
                }
                if h.gradient_color.enabled {
                    self.warnings
                        .count("gradient fills were imported as solid fills");
                }
                let solid = h.is_solid
                    || h.gradient_color.enabled
                    || h.pattern.name.eq_ignore_ascii_case("SOLID");
                let pattern = HatchPatternRef {
                    name: if solid {
                        "SOLID".into()
                    } else if h.pattern.name.is_empty() {
                        "ANSI31".into()
                    } else {
                        h.pattern.name.clone()
                    },
                    angle: finite_or(h.pattern_angle, 0.0),
                    scale: positive_or(h.pattern_scale, 1.0),
                };
                self.push(c, EntityKind::Hatch(Hatch { loops, pattern }), flip, out);
            }
            EntityType::Solid(s) => {
                let pts = [
                    v2(s.first_corner),
                    v2(s.second_corner),
                    v2(s.fourth_corner),
                    v2(s.third_corner),
                ];
                let mut verts: Vec<PolyVertex> = Vec::new();
                for p in pts {
                    if verts.last().is_none_or(|v| v.p.distance_squared(p) > 1e-24) {
                        verts.push(PolyVertex::new(p));
                    }
                }
                if verts.len() > 3 && verts[0].p.distance_squared(verts[verts.len() - 1].p) <= 1e-24
                {
                    verts.pop();
                }
                if verts.len() < 3 {
                    return;
                }
                let flip = self.ocs_flip(s.normal);
                let hatch = Hatch {
                    loops: vec![HatchLoop {
                        curves: vec![Curve2::Polyline(Polyline2 {
                            verts,
                            closed: true,
                        })],
                    }],
                    pattern: HatchPatternRef {
                        name: "SOLID".into(),
                        angle: 0.0,
                        scale: 1.0,
                    },
                };
                self.push(c, EntityKind::Hatch(hatch), flip, out);
            }
            EntityType::Leader(l) => {
                let verts: Vec<PolyVertex> =
                    l.vertices.iter().map(|p| PolyVertex::new(v2(*p))).collect();
                if verts.len() >= 2 {
                    self.warnings.count("leaders were imported as polylines");
                    self.push(
                        c,
                        EntityKind::Polyline(Polyline2 {
                            verts,
                            closed: false,
                        }),
                        false,
                        out,
                    );
                }
            }
            EntityType::Insert(ins) => self.map_insert(c, ins, out),
            EntityType::AttributeDefinition(_) => {} // template only; values come with INSERTs
            EntityType::Block(_)
            | EntityType::BlockEnd(_)
            | EntityType::Seqend(_)
            | EntityType::Viewport(_) => {}
            EntityType::Unknown(u) => self
                .warnings
                .count(format!("unsupported entity {} skipped", u.dxf_name)),
            other => self
                .warnings
                .count(format!("unsupported entity {} skipped", entity_name(other))),
        }
    }

    fn text_height(&self, h: f64, style: TextStyleId) -> f64 {
        if h.is_finite() && h > 0.0 {
            return h;
        }
        self.drawing
            .tables
            .text_styles
            .get(&style)
            .map(|s| s.height)
            .filter(|h| *h > 0.0)
            .unwrap_or(2.5)
    }

    fn map_insert(
        &mut self,
        c: &EntityCommon,
        ins: &acadrust::entities::Insert,
        out: &mut Vec<Entity>,
    ) {
        let Some(&block) = self.blocks.get(&key(&ins.block_name)) else {
            if !is_dimension_block(&key(&ins.block_name)) {
                self.warnings.count(format!(
                    "reference to missing block '{}' skipped",
                    ins.block_name
                ));
            }
            return;
        };
        let flip = self.ocs_flip(ins.normal);
        let sx = if ins.x_scale().is_finite() && ins.x_scale() != 0.0 {
            ins.x_scale()
        } else {
            1.0
        };
        let sy = if ins.y_scale().is_finite() && ins.y_scale() != 0.0 {
            ins.y_scale()
        } else {
            1.0
        };
        let rotation = finite_or(ins.rotation, 0.0);
        let cols = ins.column_count.max(1) as usize;
        let rows = ins.row_count.max(1) as usize;
        if cols * rows > 10_000 {
            self.warnings
                .count("very large block arrays (MINSERT) were truncated");
        }
        let (sin, cos) = rotation.sin_cos();
        let mut n = 0usize;
        'grid: for r in 0..rows {
            for col in 0..cols {
                if n >= 10_000 {
                    break 'grid;
                }
                n += 1;
                let off = DVec2::new(col as f64 * ins.column_spacing, r as f64 * ins.row_spacing);
                let off = DVec2::new(off.x * cos - off.y * sin, off.x * sin + off.y * cos);
                let kind = EntityKind::Insert(Insert {
                    block,
                    pos: v2(ins.insert_point) + off,
                    scale: DVec2::new(sx, sy),
                    rotation,
                });
                self.push(c, kind, flip, out);
            }
        }
        // Attribute values become plain text in the owner space.
        for att in &ins.attributes {
            if att.flags.invisible || att.common.invisible || att.value.is_empty() {
                continue;
            }
            let h = att.horizontal_alignment as i32;
            let v = att.vertical_alignment as i32;
            let halign = match h {
                1 | 4 => HAlign::Center,
                2 => HAlign::Right,
                _ => HAlign::Left,
            };
            let valign = match (h, v) {
                (4, _) => VAlign::Middle,
                (_, 1) => VAlign::Bottom,
                (_, 2) => VAlign::Middle,
                (_, 3) => VAlign::Top,
                _ => VAlign::Baseline,
            };
            let pos = if halign == HAlign::Left && valign == VAlign::Baseline {
                v2(att.insertion_point)
            } else {
                v2(att.alignment_point)
            };
            let style = self.style_id(&att.text_style);
            let text = Text {
                pos,
                height: self.text_height(att.height, style),
                rotation: finite_or(att.rotation, 0.0),
                width_factor: positive_or(att.width_factor, 1.0),
                oblique: finite_or(att.oblique_angle, 0.0),
                style,
                halign,
                valign,
                text: crate::mtext::decode_unicode_escapes(&att.value),
            };
            let flip = self.ocs_flip(att.normal);
            self.push(&att.common, EntityKind::Text(text), flip, out);
        }
    }

    fn map_dimension(&mut self, c: &EntityCommon, d: &ADim, out: &mut Vec<Entity>) {
        let base = d.base();
        let kind = match d {
            ADim::Linear(l) => DimKind::Linear {
                p1: v2(l.first_point),
                p2: v2(l.second_point),
                line_point: v2(l.definition_point),
                rotation: finite_or(l.rotation, 0.0),
            },
            ADim::Aligned(a) => DimKind::Aligned {
                p1: v2(a.first_point),
                p2: v2(a.second_point),
                line_point: v2(a.definition_point),
            },
            ADim::Radius(r) => DimKind::Radius {
                center: v2(r.angle_vertex),
                point: v2(r.definition_point),
            },
            ADim::Diameter(dd) => {
                let a = v2(dd.angle_vertex);
                let b = v2(dd.definition_point);
                DimKind::Diameter {
                    center: (a + b) * 0.5,
                    point: a,
                }
            }
            ADim::Angular3Pt(a) => DimKind::Angular {
                vertex: v2(a.angle_vertex),
                p1: v2(a.first_point),
                p2: v2(a.second_point),
                arc_point: v2(a.definition_point),
            },
            ADim::Angular2Ln(a) => {
                // Line 1: first_point → second_point; line 2: angle_vertex → definition_point.
                let (s1, e1) = (v2(a.first_point), v2(a.second_point));
                let (s2, e2) = (v2(a.angle_vertex), v2(a.definition_point));
                let arc = v2(a.dimension_arc);
                let Some(vertex) = line_line(s1, e1, s2, e2) else {
                    self.warnings
                        .count("parallel-line angular dimensions skipped");
                    return;
                };
                let far = |a: DVec2, b: DVec2| {
                    if a.distance_squared(vertex) > b.distance_squared(vertex) {
                        a
                    } else {
                        b
                    }
                };
                let (mut p1, mut p2) = (far(s1, e1), far(s2, e2));
                // Choose the rays whose sector contains the arc point.
                let ang = |p: DVec2| (p - vertex).y.atan2((p - vertex).x);
                let inside = |a: DVec2, b: DVec2| {
                    wcad_math::ccw_between(ang(a), ang(b), ang(arc), 1e-9)
                        && wcad_math::ccw_sweep(ang(a), ang(b)) <= PI
                };
                let mut found = false;
                'search: for flip1 in [false, true] {
                    for flip2 in [false, true] {
                        let q1 = if flip1 { vertex * 2.0 - p1 } else { p1 };
                        let q2 = if flip2 { vertex * 2.0 - p2 } else { p2 };
                        for (x, y) in [(q1, q2), (q2, q1)] {
                            if inside(x, y) {
                                p1 = x;
                                p2 = y;
                                found = true;
                                break 'search;
                            }
                        }
                    }
                }
                if !found {
                    self.warnings.count("angular dimension sector guessed");
                }
                DimKind::Angular {
                    vertex,
                    p1,
                    p2,
                    arc_point: arc,
                }
            }
            ADim::Ordinate(o) => DimKind::Ordinate {
                origin: v2(o.definition_point),
                point: v2(o.feature_location),
                leader_end: v2(o.leader_endpoint),
                x_axis: o.is_ordinate_type_x,
            },
            ADim::Arc(_) => {
                self.warnings
                    .count("unsupported entity ARC_DIMENSION skipped");
                return;
            }
            ADim::LargeRadial(_) => {
                self.warnings
                    .count("unsupported jogged radius dimension skipped");
                return;
            }
        };
        let style = self
            .dimstyles
            .get(&key(&base.style_name))
            .copied()
            .unwrap_or(self.drawing.tables.current_dim_style);
        let text_override = base
            .text_override()
            .filter(|t| !t.is_empty() && *t != "<>")
            .map(crate::mtext::decode_unicode_escapes);
        let text_pos = base
            .text_user_positioned
            .then(|| v2(base.text_middle_point));
        let dim = wcad_doc::Dimension {
            kind,
            style,
            text_override,
            text_pos,
        };
        self.push(c, EntityKind::Dimension(dim), false, out);
    }

    fn hatch_loop(&mut self, path: &BoundaryPath) -> HatchLoop {
        let mut curves = Vec::new();
        for edge in &path.edges {
            match edge {
                BoundaryEdge::Polyline(p) => {
                    let verts: Vec<PolyVertex> = p
                        .vertices
                        .iter()
                        .map(|v| {
                            PolyVertex::with_bulge(
                                DVec2::new(v.x, v.y),
                                if v.z.is_finite() { v.z } else { 0.0 },
                            )
                        })
                        .collect();
                    if verts.len() >= 2 {
                        curves.push(Curve2::Polyline(Polyline2 {
                            verts,
                            closed: true,
                        }));
                    }
                }
                BoundaryEdge::Line(l) => {
                    curves.push(Curve2::Line(Line2::new(v2d(l.start), v2d(l.end))))
                }
                BoundaryEdge::CircularArc(a) => {
                    if !(a.radius > 0.0) {
                        continue;
                    }
                    let (s, e) = if a.counter_clockwise {
                        (a.start_angle, a.end_angle)
                    } else {
                        (-a.end_angle, -a.start_angle)
                    };
                    if (e - s).abs() >= TAU - 1e-9 {
                        curves.push(Curve2::Circle(Circle2::new(v2d(a.center), a.radius)));
                    } else {
                        curves.push(Curve2::Arc(Arc2::new(v2d(a.center), a.radius, s, e)));
                    }
                }
                BoundaryEdge::EllipticArc(el) => {
                    let major = v2d(el.major_axis_endpoint);
                    let ratio = el.minor_axis_ratio;
                    if major.length_squared() < 1e-24 || !(ratio > 0.0) {
                        continue;
                    }
                    let (mut s, mut e) = match self.flavor {
                        Flavor::Dxf => (el.start_angle.to_radians(), el.end_angle.to_radians()),
                        Flavor::Dwg => (el.start_angle, el.end_angle),
                    };
                    if !el.counter_clockwise {
                        (s, e) = (-e, -s);
                    }
                    let full = (e - s).abs() >= TAU - 1e-9;
                    let (s, e) = if full {
                        (0.0, TAU)
                    } else {
                        (
                            super::ellipse_angle_to_param(ratio, s),
                            super::ellipse_angle_to_param(ratio, e),
                        )
                    };
                    curves.push(Curve2::Ellipse(EllipseArc2 {
                        c: v2d(el.center),
                        major,
                        ratio: ratio.min(1.0),
                        start: s,
                        end: e,
                    }));
                }
                BoundaryEdge::Spline(sp) => {
                    let n = Nurbs2 {
                        degree: sp.degree.clamp(1, 32) as u32,
                        ctrl: sp
                            .control_points
                            .iter()
                            .map(|p| DVec2::new(p.x, p.y))
                            .collect(),
                        weights: if sp.rational {
                            sp.control_points.iter().map(|p| p.z).collect()
                        } else {
                            Vec::new()
                        },
                        knots: sp.knots.clone(),
                        fit_points: sp.fit_points.iter().map(|p| v2d(*p)).collect(),
                        closed: sp.periodic,
                    };
                    curves.push(Curve2::Spline(n));
                }
            }
        }
        HatchLoop { curves }
    }
}

fn is_dimension_block(upper: &str) -> bool {
    upper
        .strip_prefix("*D")
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

fn finite_or(v: f64, d: f64) -> f64 {
    if v.is_finite() { v } else { d }
}

fn positive_or(v: f64, d: f64) -> f64 {
    if v.is_finite() && v > 0.0 { v } else { d }
}

/// (halign, valign, is_fit_or_aligned)
fn text_align(h: &TextHorizontalAlignment, v: &TextVerticalAlignment) -> (HAlign, VAlign, bool) {
    let valign = match v {
        TextVerticalAlignment::Baseline => VAlign::Baseline,
        TextVerticalAlignment::Bottom => VAlign::Bottom,
        TextVerticalAlignment::Middle => VAlign::Middle,
        TextVerticalAlignment::Top => VAlign::Top,
    };
    match h {
        TextHorizontalAlignment::Left => (HAlign::Left, valign, false),
        TextHorizontalAlignment::Center => (HAlign::Center, valign, false),
        TextHorizontalAlignment::Right => (HAlign::Right, valign, false),
        TextHorizontalAlignment::Middle => (HAlign::Center, VAlign::Middle, false),
        TextHorizontalAlignment::Aligned | TextHorizontalAlignment::Fit => {
            (HAlign::Left, VAlign::Baseline, true)
        }
    }
}

fn attachment_u8(a: AttachmentPoint) -> u8 {
    (a as i32).clamp(1, 9) as u8
}

fn line_line(a0: DVec2, a1: DVec2, b0: DVec2, b1: DVec2) -> Option<DVec2> {
    let da = a1 - a0;
    let db = b1 - b0;
    let den = wcad_math::cross2(da, db);
    if den.abs() < 1e-12 * da.length() * db.length() || den == 0.0 {
        return None;
    }
    let t = wcad_math::cross2(b0 - a0, db) / den;
    Some(a0 + da * t)
}

/// Mirror an entity kind across the Y axis (x → −x): maps OCS geometry with normal −Z to WCS.
pub(crate) fn mirror_x(kind: EntityKind) -> EntityKind {
    let m = |p: DVec2| DVec2::new(-p.x, p.y);
    match kind {
        EntityKind::Point { p } => EntityKind::Point { p: m(p) },
        EntityKind::Text(mut t) => {
            t.pos = m(t.pos);
            EntityKind::Text(t)
        }
        EntityKind::MText(mut t) => {
            t.pos = m(t.pos);
            EntityKind::MText(t)
        }
        EntityKind::Insert(mut i) => {
            i.pos = m(i.pos);
            i.rotation = -i.rotation;
            i.scale.x = -i.scale.x;
            EntityKind::Insert(i)
        }
        EntityKind::Hatch(mut h) => {
            for lp in &mut h.loops {
                for c in &mut lp.curves {
                    *c = mirror_curve(c.clone());
                }
            }
            h.pattern.angle = PI - h.pattern.angle;
            EntityKind::Hatch(h)
        }
        EntityKind::Dimension(d) => EntityKind::Dimension(d),
        other => match other.as_curve() {
            Some(c) => EntityKind::from_curve(mirror_curve(c)),
            None => other,
        },
    }
}

fn mirror_curve(c: Curve2) -> Curve2 {
    let m = |p: DVec2| DVec2::new(-p.x, p.y);
    match c {
        Curve2::Line(l) => Curve2::Line(Line2::new(m(l.a), m(l.b))),
        Curve2::Circle(ci) => Curve2::Circle(Circle2::new(m(ci.c), ci.r)),
        Curve2::Arc(a) => Curve2::Arc(Arc2::new(m(a.c), a.r, PI - a.end, PI - a.start)),
        Curve2::Ellipse(e) => Curve2::Ellipse(EllipseArc2 {
            c: m(e.c),
            major: m(e.major),
            ratio: e.ratio,
            start: -e.end,
            end: -e.start,
        }),
        Curve2::Polyline(p) => Curve2::Polyline(Polyline2 {
            verts: p
                .verts
                .iter()
                .map(|v| PolyVertex::with_bulge(m(v.p), -v.bulge))
                .collect(),
            closed: p.closed,
        }),
        Curve2::Spline(mut s) => {
            for p in &mut s.ctrl {
                *p = m(*p);
            }
            for p in &mut s.fit_points {
                *p = m(*p);
            }
            Curve2::Spline(s)
        }
    }
}

fn entity_name(e: &EntityType) -> &'static str {
    match e {
        EntityType::Helix(_) => "HELIX",
        EntityType::Face3D(_) => "3DFACE",
        EntityType::Ray(_) => "RAY",
        EntityType::XLine(_) => "XLINE",
        EntityType::AttributeEntity(_) => "ATTRIB",
        EntityType::MultiLeader(_) => "MULTILEADER",
        EntityType::MLine(_) => "MLINE",
        EntityType::Mesh(_) => "MESH",
        EntityType::RasterImage(_) => "IMAGE",
        EntityType::Solid3D(_) => "3DSOLID",
        EntityType::Region(_) => "REGION",
        EntityType::Body(_) => "BODY",
        EntityType::Surface(_) => "SURFACE",
        EntityType::Table(_) => "ACAD_TABLE",
        EntityType::Tolerance(_) => "TOLERANCE",
        EntityType::PolyfaceMesh(_) => "POLYFACE MESH",
        EntityType::Wipeout(_) => "WIPEOUT",
        EntityType::Shape(_) => "SHAPE",
        EntityType::Underlay(_) => "UNDERLAY",
        EntityType::Ole2Frame(_) => "OLE2FRAME",
        EntityType::PolygonMesh(_) => "POLYGON MESH",
        EntityType::Light(_) => "LIGHT",
        _ => "OTHER",
    }
}
