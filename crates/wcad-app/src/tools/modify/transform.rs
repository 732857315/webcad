//! Selection-based, transactional COPY, ROTATE, SCALE, MIRROR and rectangular arrays.

use wcad_doc::{DimKind, Entity, EntityId, EntityKind};
use wcad_geom2d::{Curve, Curve2, Line2};
use wcad_math::{DAffine2, DVec2};

use crate::commands::{CommandKind, CommandRegistry, CommandSpec, RibbonTab};
use crate::i18n::{Lang, core, fmt};
use crate::tools::{Accept, Keyword, Preview, Tool, ToolCx, ToolFlow, ToolInput, dedup_ids};
use crate::xform;

/// Includes the original cell, so large selections cannot multiply past the limit.
const MAX_ARRAY_ENTITIES: usize = 10_000;
const MAX_ARRAY_PREVIEW: usize = 256;

pub(super) fn register(r: &mut CommandRegistry) {
    for (name, aliases, label, icon, ctor) in [
        (
            "COPY",
            &["CO", "CP"][..],
            (|l| strings(l).copy) as fn(Lang) -> &'static str,
            "CP",
            (|| Box::new(TransformTool::new(Operation::Copy)) as Box<dyn Tool>)
                as fn() -> Box<dyn Tool>,
        ),
        (
            "ROTATE",
            &["RO"][..],
            |l| strings(l).rotate,
            "R",
            || Box::new(TransformTool::new(Operation::Rotate)),
        ),
        (
            "SCALE",
            &["SC"][..],
            |l| strings(l).scale,
            "SC",
            || Box::new(TransformTool::new(Operation::Scale)),
        ),
        (
            "MIRROR",
            &["MI"][..],
            |l| strings(l).mirror,
            "|",
            || Box::new(TransformTool::new(Operation::Mirror)),
        ),
        (
            "ARRAYRECT",
            &["AR"][..],
            |l| strings(l).array,
            "#",
            || Box::new(TransformTool::new(Operation::Array)),
        ),
    ] {
        r.add(CommandSpec {
            name,
            aliases,
            label,
            icon,
            tab: Some(RibbonTab::Modify),
            group: "transform",
            kind: CommandKind::Tool(ctor),
        });
    }
}

struct Strings {
    copy: &'static str,
    rotate: &'static str,
    scale: &'static str,
    mirror: &'static str,
    array: &'static str,
    base: &'static str,
    destination: &'static str,
    angle: &'static str,
    factor: &'static str,
    reference: &'static str,
    reference_length: &'static str,
    new_length: &'static str,
    axis_first: &'static str,
    axis_second: &'static str,
    delete_source: &'static str,
    yes: &'static str,
    no: &'static str,
    rows: &'static str,
    columns: &'static str,
    spacing_base: &'static str,
    row_spacing: &'static str,
    column_spacing: &'static str,
    invalid: &'static str,
    array_limit: &'static str,
    copy_enabled: &'static str,
    changed: &'static str,
}

fn strings(lang: Lang) -> &'static Strings {
    static EN: Strings = Strings {
        copy: "Copy",
        rotate: "Rotate",
        scale: "Scale",
        mirror: "Mirror",
        array: "Rectangular Array",
        base: "Specify base point",
        destination: "Specify copy destination (from original base), or Enter to finish",
        angle: "Specify rotation angle (degrees CCW) or target direction from +X",
        factor: "Enter positive scale factor or pick reference endpoint from base",
        reference: "Reference",
        reference_length: "Specify positive reference length or endpoint from base",
        new_length: "Specify positive new length or endpoint from base",
        axis_first: "Specify first point of mirror axis",
        axis_second: "Specify a distinct second point of mirror axis",
        delete_source: "Delete source objects? <No>",
        yes: "Yes",
        no: "No",
        rows: "Enter number of rows (positive integer)",
        columns: "Enter number of columns (positive integer; at least two cells)",
        spacing_base: "Specify reference point for array spacing",
        row_spacing: "Specify nonzero row spacing along Y (signed value or endpoint)",
        column_spacing: "Specify nonzero column spacing along X (signed value or endpoint)",
        invalid: "Invalid or out-of-range geometry/value; enter a finite, nondegenerate value",
        array_limit: "Array requires 2 or more cells; rows * columns * source objects must be <= {0}",
        copy_enabled: "Copy enabled: source objects will be retained",
        changed: "{0}: {1} object(s) transformed",
    };
    static ZH: Strings = Strings {
        copy: "复制",
        rotate: "旋转",
        scale: "缩放",
        mirror: "镜像",
        array: "矩形阵列",
        base: "指定基点",
        destination: "指定复制目标点（相对原基点），或按回车结束",
        angle: "输入逆时针旋转角度（度），或指定相对正X轴的目标方向",
        factor: "输入正缩放比例，或拾取相对基点的参考长度端点",
        reference: "参照",
        reference_length: "输入正参考长度，或拾取相对基点的端点",
        new_length: "输入正新长度，或拾取相对基点的新端点",
        axis_first: "指定镜像轴第一点",
        axis_second: "指定镜像轴上不重合的第二点",
        delete_source: "是否删除源对象？<否>",
        yes: "是",
        no: "否",
        rows: "输入行数（正整数）",
        columns: "输入列数（正整数，至少两个单元）",
        spacing_base: "指定阵列间距的参考点",
        row_spacing: "指定Y方向非零行间距（有符号数值或端点）",
        column_spacing: "指定X方向非零列间距（有符号数值或端点）",
        invalid: "几何或数值无效或超出范围；请输入有限且非退化的值",
        array_limit: "阵列至少需要两个单元；行数 * 列数 * 源对象数不得超过 {0}",
        copy_enabled: "已启用复制：保留源对象",
        changed: "{0}：已变换 {1} 个对象",
    };
    lang.pick(&ZH, &EN)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operation {
    Copy,
    Rotate,
    Scale,
    Mirror,
    Array,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Step {
    Select,
    Base,
    CopyTo(DVec2),
    Angle(DVec2),
    Factor(DVec2),
    Reference(DVec2),
    NewLength {
        base: DVec2,
        reference: f64,
    },
    Axis(DVec2),
    Delete {
        a: DVec2,
        b: DVec2,
    },
    Rows,
    Columns(usize),
    ArrayBase {
        rows: usize,
        cols: usize,
    },
    RowSpacing {
        rows: usize,
        cols: usize,
        base: DVec2,
    },
    ColumnSpacing {
        rows: usize,
        cols: usize,
        base: DVec2,
        dy: f64,
    },
}

struct TransformTool {
    operation: Operation,
    ids: Vec<EntityId>,
    step: Step,
    copy: bool,
}

fn positive(v: f64) -> bool {
    v.is_finite() && v > 0.0
}

fn matrix_valid(m: &DAffine2) -> bool {
    let det = m.matrix2.determinant().abs();
    m.is_finite() && positive(det) && det >= 1e-300
}

fn about(base: DVec2, mut m: DAffine2) -> Option<DAffine2> {
    m.translation = base - m.transform_vector2(base);
    (base.is_finite() && matrix_valid(&m)).then_some(m)
}

fn rotation(base: DVec2, radians: f64) -> Option<DAffine2> {
    if !radians.is_finite() {
        return None;
    }
    about(base, DAffine2::from_angle(radians))
}

fn scaling(base: DVec2, factor: f64) -> Option<DAffine2> {
    if !positive(factor) {
        return None;
    }
    about(base, DAffine2::from_scale(DVec2::splat(factor)))
}

fn reflection(a: DVec2, b: DVec2) -> Option<DAffine2> {
    let d = b - a;
    let magnitude = d.abs().max_element();
    if !a.is_finite() || !b.is_finite() || !d.is_finite() || !positive(magnitude) {
        return None;
    }
    let d = d / magnitude;
    let u = d / d.x.hypot(d.y);
    let x = DVec2::new(u.x * u.x - u.y * u.y, 2.0 * u.x * u.y);
    about(
        a,
        DAffine2::from_cols(x, DVec2::new(x.y, -x.x), DVec2::ZERO),
    )
}

fn distance(base: DVec2, p: DVec2) -> f64 {
    let d = p - base;
    d.x.hypot(d.y)
}

fn array_size(rows: usize, cols: usize, sources: usize) -> Option<usize> {
    if rows == 0 || cols == 0 || sources == 0 {
        return None;
    }
    rows.checked_mul(cols)?
        .checked_mul(sources)
        .filter(|n| *n <= MAX_ARRAY_ENTITIES)
}

fn count(v: f64) -> Option<usize> {
    (positive(v) && v.fract() == 0.0 && v <= MAX_ARRAY_ENTITIES as f64).then_some(v as usize)
}

fn array_matrices(rows: usize, cols: usize, dx: f64, dy: f64) -> Option<Vec<DAffine2>> {
    let cells = array_size(rows, cols, 1)?;
    if cells < 2
        || !dx.is_finite()
        || !dy.is_finite()
        || (cols > 1 && dx == 0.0)
        || (rows > 1 && dy == 0.0)
    {
        return None;
    }
    let corner = DVec2::new(dx * (cols - 1) as f64, dy * (rows - 1) as f64);
    if !corner.is_finite() {
        return None;
    }
    Some(
        (0..rows)
            .flat_map(|row| (0..cols).map(move |col| (row, col)))
            .filter(|&(row, col)| row != 0 || col != 0)
            .map(|(row, col)| {
                DAffine2::from_translation(DVec2::new(col as f64 * dx, row as f64 * dy))
            })
            .collect(),
    )
}

// Check stored fields as well as bounds: bbox helpers can silently ignore NaNs, and invalid
// splines otherwise fall back to a control polyline during transformation.
fn curve_valid(c: &Curve2) -> bool {
    let fields = match c {
        Curve2::Line(l) => {
            l.a.is_finite() && l.b.is_finite() && l.a != l.b && (l.b - l.a).is_finite()
        }
        Curve2::Circle(c) => c.c.is_finite() && positive(c.r),
        Curve2::Arc(a) => {
            a.c.is_finite() && positive(a.r) && a.start.is_finite() && a.end.is_finite()
        }
        Curve2::Ellipse(e) => {
            e.c.is_finite()
                && e.major.is_finite()
                && positive(e.major.length())
                && positive(e.ratio)
                && e.ratio <= 1.0
                && e.start.is_finite()
                && e.end.is_finite()
        }
        Curve2::Polyline(p) => {
            !p.verts.is_empty()
                && p.verts
                    .iter()
                    .all(|v| v.p.is_finite() && v.bulge.is_finite())
                && (0..p.segment_count()).all(|i| {
                    let a = p.verts[i];
                    let b = p.verts[(i + 1) % p.verts.len()];
                    if a.bulge.abs() < wcad_geom2d::bulge::BULGE_EPS {
                        (b.p - a.p).is_finite()
                    } else {
                        wcad_geom2d::bulge_to_arc(a.p, b.p, a.bulge)
                            .is_some_and(|a| curve_valid(&Curve2::Arc(a.to_arc())))
                    }
                })
        }
        Curve2::Spline(s) => {
            return s.validate().is_ok()
                && s.knots.iter().all(|v| v.is_finite())
                && s.fit_points.iter().all(|p| p.is_finite());
        }
    };
    if !fields {
        return false;
    }
    let bounds = c.bbox();
    bounds.min.is_finite() && bounds.max.is_finite() && bounds.size().is_finite()
}

fn kind_valid(kind: &EntityKind) -> bool {
    match kind {
        EntityKind::Point { p } => p.is_finite(),
        EntityKind::Text(t) => {
            t.pos.is_finite()
                && positive(t.height)
                && t.rotation.is_finite()
                && t.width_factor.is_finite()
                && t.oblique.is_finite()
        }
        EntityKind::MText(t) => {
            t.pos.is_finite()
                && positive(t.height)
                && t.rotation.is_finite()
                && t.width.is_finite()
                && t.width >= 0.0
                && positive(t.line_spacing)
        }
        EntityKind::Dimension(d) => {
            let points = match d.kind {
                DimKind::Linear {
                    p1,
                    p2,
                    line_point,
                    rotation,
                } => {
                    p1.is_finite()
                        && p2.is_finite()
                        && line_point.is_finite()
                        && rotation.is_finite()
                }
                DimKind::Aligned { p1, p2, line_point } => {
                    p1.is_finite() && p2.is_finite() && line_point.is_finite()
                }
                DimKind::Radius { center, point } | DimKind::Diameter { center, point } => {
                    center.is_finite() && point.is_finite()
                }
                DimKind::Angular {
                    vertex,
                    p1,
                    p2,
                    arc_point,
                } => {
                    vertex.is_finite() && p1.is_finite() && p2.is_finite() && arc_point.is_finite()
                }
                DimKind::Ordinate {
                    origin,
                    point,
                    leader_end,
                    ..
                } => origin.is_finite() && point.is_finite() && leader_end.is_finite(),
            };
            points && d.text_pos.is_none_or(|p| p.is_finite())
        }
        EntityKind::Hatch(h) => {
            h.pattern.angle.is_finite()
                && positive(h.pattern.scale)
                && h.loops.iter().all(|l| l.curves.iter().all(curve_valid))
        }
        EntityKind::Insert(i) => {
            i.pos.is_finite()
                && i.rotation.is_finite()
                && i.scale.is_finite()
                && i.scale.x != 0.0
                && i.scale.y != 0.0
        }
        other => other.as_curve().is_some_and(|c| curve_valid(&c)),
    }
}

fn transformed(e: &Entity, m: &DAffine2) -> Option<Entity> {
    if !matrix_valid(m) || !kind_valid(&e.kind) {
        return None;
    }
    let e = xform::transform_entity(e, m);
    kind_valid(&e.kind).then_some(e)
}

impl TransformTool {
    fn new(operation: Operation) -> Self {
        Self {
            operation,
            ids: Vec::new(),
            step: Step::Select,
            copy: false,
        }
    }

    fn invalid(&self, cx: &mut ToolCx<'_>) -> ToolFlow {
        cx.error(strings(cx.lang()).invalid);
        ToolFlow::Continue
    }

    fn array_error(&self, cx: &mut ToolCx<'_>) -> ToolFlow {
        cx.error(fmt(strings(cx.lang()).array_limit, &[&MAX_ARRAY_ENTITIES]));
        ToolFlow::Continue
    }

    /// Build and validate the entire result before opening the one undoable transaction.
    fn commit(
        &self,
        cx: &mut ToolCx<'_>,
        matrices: &[DAffine2],
        copy: bool,
        delete: bool,
    ) -> ToolFlow {
        let ids = cx.editable(&self.ids);
        if ids.is_empty() {
            cx.message(core(cx.lang()).nothing_selected);
            cx.selection_mut().clear();
            return ToolFlow::Done;
        }
        if self.operation == Operation::Array
            && array_size(matrices.len() + 1, 1, ids.len()).is_none()
        {
            return self.array_error(cx);
        }
        let mut result = Vec::new();
        for m in matrices {
            for id in &ids {
                let Some(e) = cx.entity(*id).and_then(|e| transformed(e, m)) else {
                    return self.invalid(cx);
                };
                result.push(e);
            }
        }
        let n = result.len();
        cx.transact(self.name(), |tx| {
            if delete {
                for id in &ids {
                    tx.remove(*id);
                }
            }
            for mut e in result {
                if copy {
                    e.id = EntityId(0);
                    tx.insert(e);
                } else {
                    tx.modify(e.id, |old| *old = e);
                }
            }
        });
        cx.selection_mut().clear();
        cx.message(fmt(strings(cx.lang()).changed, &[&self.name(), &n]));
        if self.operation == Operation::Copy {
            ToolFlow::Continue
        } else {
            ToolFlow::Done
        }
    }

    fn apply(&self, cx: &mut ToolCx<'_>, matrix: Option<DAffine2>, delete: bool) -> ToolFlow {
        match matrix {
            Some(m) => self.commit(
                cx,
                &[m],
                self.copy || matches!(self.operation, Operation::Copy | Operation::Mirror),
                delete,
            ),
            None => self.invalid(cx),
        }
    }

    fn array(&self, cx: &mut ToolCx<'_>, rows: usize, cols: usize, dx: f64, dy: f64) -> ToolFlow {
        match array_matrices(rows, cols, dx, dy) {
            Some(ms) => self.commit(cx, &ms, true, false),
            None => self.invalid(cx),
        }
    }

    fn point_matrix(&self, p: DVec2) -> Option<DAffine2> {
        if !p.is_finite() {
            return None;
        }
        match self.step {
            Step::CopyTo(base) => {
                let m = DAffine2::from_translation(p - base);
                matrix_valid(&m).then_some(m)
            }
            Step::Angle(base) if p != base && (p - base).is_finite() => {
                let d = p - base;
                rotation(base, d.y.atan2(d.x))
            }
            Step::NewLength { base, reference } => scaling(base, distance(base, p) / reference),
            Step::Axis(a) => reflection(a, p),
            Step::Delete { a, b } => reflection(a, b),
            _ => None,
        }
    }

    fn ghosts(&self, cx: &ToolCx<'_>, m: &DAffine2, out: &mut Preview, limit: usize) {
        for id in &self.ids {
            if out.ghosts.len() >= limit {
                break;
            }
            if let Some(e) = cx.entity(*id)
                && cx.drawing().is_layer_editable(e.layer)
                && let Some(e) = transformed(e, m)
            {
                out.ghosts.push(e);
            }
        }
    }
}

impl Tool for TransformTool {
    fn name(&self) -> &'static str {
        match self.operation {
            Operation::Copy => "COPY",
            Operation::Rotate => "ROTATE",
            Operation::Scale => "SCALE",
            Operation::Mirror => "MIRROR",
            Operation::Array => "ARRAYRECT",
        }
    }

    fn prompt(&self, lang: Lang) -> String {
        let s = strings(lang);
        match self.step {
            Step::Select => core(lang).select_objects,
            Step::Base if self.operation == Operation::Mirror => s.axis_first,
            Step::Base => s.base,
            Step::CopyTo(_) => s.destination,
            Step::Angle(_) => s.angle,
            Step::Factor(_) => s.factor,
            Step::Reference(_) => s.reference_length,
            Step::NewLength { .. } => s.new_length,
            Step::Axis(_) => s.axis_second,
            Step::Delete { .. } => s.delete_source,
            Step::Rows => s.rows,
            Step::Columns(_) => s.columns,
            Step::ArrayBase { .. } => s.spacing_base,
            Step::RowSpacing { .. } => s.row_spacing,
            Step::ColumnSpacing { .. } => s.column_spacing,
        }
        .into()
    }

    fn accepts(&self) -> Accept {
        match self.step {
            Step::Select => Accept::SELECTION,
            Step::Rows | Step::Columns(_) => Accept::VALUE,
            Step::Delete { .. } => Accept::NONE,
            Step::Angle(_)
            | Step::Factor(_)
            | Step::Reference(_)
            | Step::NewLength { .. }
            | Step::RowSpacing { .. }
            | Step::ColumnSpacing { .. } => Accept::POINT_OR_VALUE,
            _ => Accept::POINT,
        }
    }

    fn keywords(&self, lang: Lang) -> Vec<Keyword> {
        let s = strings(lang);
        let kw = |id, key, label| Keyword { id, key, label };
        let mut result = Vec::new();
        if !self.copy
            && matches!(
                self.step,
                Step::Angle(_) | Step::Factor(_) | Step::Reference(_) | Step::NewLength { .. }
            )
        {
            result.push(kw("Copy", "C", s.copy));
        }
        if matches!(self.step, Step::Factor(_)) {
            result.push(kw("Reference", "R", s.reference));
        }
        if matches!(self.step, Step::Delete { .. }) {
            result.push(kw("Yes", "Y", s.yes));
            result.push(kw("No", "N", s.no));
        }
        result
    }

    fn base_point(&self) -> Option<DVec2> {
        match self.step {
            Step::CopyTo(p)
            | Step::Angle(p)
            | Step::Factor(p)
            | Step::Reference(p)
            | Step::Axis(p) => Some(p),
            Step::NewLength { base, .. }
            | Step::RowSpacing { base, .. }
            | Step::ColumnSpacing { base, .. } => Some(base),
            _ => None,
        }
    }

    fn on_input(&mut self, input: ToolInput, cx: &mut ToolCx<'_>) -> ToolFlow {
        match (&input, self.step) {
            (ToolInput::Escape, _) => return ToolFlow::Cancel,
            (ToolInput::Point(p), _) if !p.is_finite() => return self.invalid(cx),
            (ToolInput::Value(v), _) if !v.is_finite() => return self.invalid(cx),
            _ => {}
        }
        match (self.step, input) {
            (Step::Select, ToolInput::Selection(ids)) => {
                self.ids = dedup_ids(cx.editable(&ids));
                if self.ids.is_empty() {
                    cx.message(core(cx.lang()).nothing_selected);
                    return ToolFlow::Done;
                }
                self.step = if self.operation == Operation::Array {
                    Step::Rows
                } else {
                    Step::Base
                };
                if self.operation == Operation::Array && self.ids.len() > MAX_ARRAY_ENTITIES / 2 {
                    self.array_error(cx);
                    return ToolFlow::Cancel;
                }
            }
            (Step::Base, ToolInput::Point(base)) => {
                self.step = match self.operation {
                    Operation::Copy => Step::CopyTo(base),
                    Operation::Rotate => Step::Angle(base),
                    Operation::Scale => Step::Factor(base),
                    Operation::Mirror => Step::Axis(base),
                    Operation::Array => unreachable!(),
                }
            }
            (Step::CopyTo(_) | Step::Angle(_) | Step::NewLength { .. }, ToolInput::Point(p)) => {
                return self.apply(cx, self.point_matrix(p), false);
            }
            (Step::Angle(base), ToolInput::Value(degrees)) => {
                return self.apply(cx, rotation(base, (degrees % 360.0).to_radians()), false);
            }
            (Step::Factor(base), ToolInput::Value(factor)) => {
                return self.apply(cx, scaling(base, factor), false);
            }
            (Step::Factor(base) | Step::Reference(base), ToolInput::Point(p)) => {
                let reference = distance(base, p);
                if !positive(reference) {
                    return self.invalid(cx);
                }
                self.step = Step::NewLength { base, reference };
            }
            (Step::Reference(base), ToolInput::Value(reference)) => {
                if !positive(reference) {
                    return self.invalid(cx);
                }
                self.step = Step::NewLength { base, reference };
            }
            (Step::NewLength { base, reference }, ToolInput::Value(length)) => {
                return self.apply(cx, scaling(base, length / reference), false);
            }
            (Step::Factor(base), ToolInput::Keyword("Reference")) => {
                self.step = Step::Reference(base)
            }
            (
                Step::Angle(_) | Step::Factor(_) | Step::Reference(_) | Step::NewLength { .. },
                ToolInput::Keyword("Copy"),
            ) => {
                self.copy = true;
                cx.message(strings(cx.lang()).copy_enabled);
            }
            (Step::Axis(a), ToolInput::Point(b)) => {
                if reflection(a, b).is_none() {
                    return self.invalid(cx);
                }
                self.step = Step::Delete { a, b };
            }
            (Step::Delete { a, b }, ToolInput::Keyword("Yes")) => {
                return self.apply(cx, reflection(a, b), true);
            }
            (Step::Delete { a, b }, ToolInput::Keyword("No") | ToolInput::Enter) => {
                return self.apply(cx, reflection(a, b), false);
            }
            (Step::Rows, ToolInput::Value(v)) => {
                let Some(rows) =
                    count(v).filter(|&rows| array_size(rows, 1, self.ids.len()).is_some())
                else {
                    return self.array_error(cx);
                };
                self.step = Step::Columns(rows);
            }
            (Step::Columns(rows), ToolInput::Value(v)) => {
                let Some(cols) = count(v).filter(|&cols| {
                    array_size(rows, cols, self.ids.len()).is_some() && rows * cols > 1
                }) else {
                    return self.array_error(cx);
                };
                self.step = Step::ArrayBase { rows, cols };
            }
            (Step::ArrayBase { rows, cols }, ToolInput::Point(base)) => {
                self.step = if rows > 1 {
                    Step::RowSpacing { rows, cols, base }
                } else {
                    Step::ColumnSpacing {
                        rows,
                        cols,
                        base,
                        dy: 0.0,
                    }
                }
            }
            (Step::RowSpacing { rows, cols, base }, input) => {
                let dy = match input {
                    ToolInput::Value(v) => v,
                    ToolInput::Point(p) => p.y - base.y,
                    _ => return ToolFlow::Continue,
                };
                if !positive(dy.abs()) || !(dy * (rows - 1) as f64).is_finite() {
                    return self.invalid(cx);
                }
                if cols == 1 {
                    return self.array(cx, rows, cols, 0.0, dy);
                }
                self.step = Step::ColumnSpacing {
                    rows,
                    cols,
                    base,
                    dy,
                };
            }
            (
                Step::ColumnSpacing {
                    rows,
                    cols,
                    base,
                    dy,
                },
                input,
            ) => {
                let dx = match input {
                    ToolInput::Value(v) => v,
                    ToolInput::Point(p) => p.x - base.x,
                    _ => return ToolFlow::Continue,
                };
                return self.array(cx, rows, cols, dx, dy);
            }
            (Step::CopyTo(_), ToolInput::Enter) => return ToolFlow::Done,
            (Step::Select | Step::Base | Step::Rows, ToolInput::Enter) => return ToolFlow::Cancel,
            _ => {}
        }
        ToolFlow::Continue
    }

    fn preview(&self, cx: &ToolCx<'_>, out: &mut Preview) {
        if let Step::Delete { a, b } = self.step {
            if let Some(m) = reflection(a, b) {
                self.ghosts(cx, &m, out, usize::MAX);
                out.curves.push(Curve2::Line(Line2::new(a, b)));
            }
            return;
        }
        let Some(p) = cx.cursor().filter(|p| p.is_finite()) else {
            return;
        };
        out.rubber_band = self.base_point().is_some();
        if let Some(m) = self.point_matrix(p) {
            self.ghosts(cx, &m, out, usize::MAX);
        }
        let matrices = match self.step {
            Step::RowSpacing { rows, base, .. } => array_matrices(rows, 1, 0.0, p.y - base.y),
            Step::ColumnSpacing {
                rows,
                cols,
                base,
                dy,
            } => array_matrices(rows, cols, p.x - base.x, dy),
            _ => None,
        };
        if let Some(ms) = matrices {
            for m in ms {
                self.ghosts(cx, &m, out, MAX_ARRAY_PREVIEW);
                if out.ghosts.len() >= MAX_ARRAY_PREVIEW {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Harness;
    use std::f64::consts::{FRAC_PI_2, TAU};
    use wcad_doc::{
        BlockId, Color, DimStyleId, Dimension, HAlign, Hatch, HatchLoop, HatchPatternRef, Insert,
        LineWeight, LinetypeRef, MText, Text, TextStyleId, VAlign,
    };
    use wcad_geom2d::{Arc2, Circle2, EllipseArc2, Nurbs2, PolyVertex, Polyline2};

    fn line() -> EntityKind {
        EntityKind::Line(Line2::new(DVec2::X, DVec2::X * 3.0))
    }

    fn fixture(kinds: Vec<EntityKind>) -> (Harness, Vec<EntityId>) {
        let mut h = Harness::new();
        h.ed.draft.osnap_on = false;
        let ids = h.ed.doc.transact("fixture", |tx| {
            let layer = tx.ids().layer();
            let mut source_layer = tx
                .drawing()
                .layer(tx.drawing().tables.current_layer)
                .unwrap()
                .clone();
            source_layer.name = "Source".into();
            tx.tables_mut().layers.insert(layer, source_layer);
            let linetype = LinetypeRef::Id(tx.drawing().linetype_by_name("DASHED").unwrap());
            kinds
                .into_iter()
                .map(|kind| {
                    tx.insert(Entity {
                        id: EntityId(0),
                        layer,
                        color: Color::Rgb(31, 73, 129),
                        linetype,
                        linetype_scale: 2.75,
                        lineweight: LineWeight::Mm100(35),
                        kind,
                    })
                })
                .collect()
        });
        h.ed.pump();
        h.ed.doc.clear_history();
        (h, ids)
    }

    fn snapshot(h: &Harness) -> Vec<Entity> {
        h.ed.doc.drawing.entities.values().cloned().collect()
    }

    fn near(a: DVec2, b: DVec2) {
        assert!(a.distance(b) < 1e-9, "{a:?} != {b:?}");
    }

    fn source_line(h: &Harness, id: EntityId) -> Line2 {
        let EntityKind::Line(l) = h.ed.doc.drawing.entities[&id].kind else {
            panic!("line expected");
        };
        l
    }

    fn preselect(h: &mut Harness, ids: &[EntityId], name: &str) {
        h.ed.selection.set(ids.iter().copied());
        h.cmd(name);
        assert!(!h.ed.tool_accepts().selection);
    }

    fn inputs(h: &mut Harness, lines: &[&str]) {
        for input in lines {
            h.cmd(input);
        }
    }

    fn undo_redo(h: &mut Harness, before: &[Entity], label: &str) {
        let after = snapshot(h);
        assert_eq!(h.ed.doc.undo().as_deref(), Some(label));
        assert_eq!(snapshot(h), before);
        assert!(!h.ed.doc.can_undo());
        assert_eq!(h.ed.doc.redo().as_deref(), Some(label));
        assert_eq!(snapshot(h), after);
        assert!(!h.ed.doc.can_redo());
    }

    fn cases() -> [(&'static str, Vec<&'static str>, usize); 5] {
        [
            ("COPY", vec!["0,0", "5,0", ""], 2),
            ("ROTATE", vec!["0,0", "90"], 1),
            ("SCALE", vec!["0,0", "2"], 1),
            ("MIRROR", vec!["0,0", "0,2", "N"], 2),
            ("ARRAYRECT", vec!["2", "2", "0,0", "4", "5"], 4),
        ]
    }

    fn direct(h: &mut Harness, tool: &mut TransformTool, input: ToolInput) -> ToolFlow {
        let ed = &mut h.ed;
        tool.on_input(
            input,
            &mut ToolCx {
                doc: &mut ed.doc,
                selection: &mut ed.selection,
                draft: &ed.draft,
                index: &ed.index,
                log: &mut ed.log,
                requests: &mut ed.requests,
                lang: ed.lang,
                cursor: None,
                last_point: ed.last_point,
                units_per_px: ed.units_per_px,
            },
        )
    }

    #[test]
    fn registration_aliases_labels_and_input_contracts() {
        let mut registry = CommandRegistry::new();
        register(&mut registry);
        assert_eq!(registry.commands().len(), 5);
        for (name, aliases) in [
            ("COPY", vec!["CO", "CP"]),
            ("ROTATE", vec!["RO"]),
            ("SCALE", vec!["SC"]),
            ("MIRROR", vec!["MI"]),
            ("ARRAYRECT", vec!["AR"]),
        ] {
            let spec = registry.find(name).unwrap();
            assert_eq!(spec.tab, Some(RibbonTab::Modify));
            assert!(spec.icon.is_ascii());
            assert_ne!((spec.label)(Lang::Zh), (spec.label)(Lang::En));
            let CommandKind::Tool(ctor) = spec.kind else {
                panic!("tool expected");
            };
            assert_eq!(ctor().accepts(), Accept::SELECTION);
            for alias in aliases {
                assert_eq!(registry.find(alias).unwrap().name, name);
            }
        }
        assert!(registry.find("MOVE").is_none());
        assert!(registry.find("ERASE").is_none());
    }

    #[test]
    fn every_command_supports_preselection_postselection_and_one_undo() {
        for (name, tail, expected) in cases() {
            for pre in [false, true] {
                let (mut h, _) = fixture(vec![line()]);
                let before = snapshot(&h);
                if pre {
                    h.click(2.0, 0.0);
                }
                h.cmd(name);
                if !pre {
                    assert_eq!(h.ed.tool_accepts(), Accept::SELECTION);
                    h.click(2.0, 0.0).enter();
                }
                inputs(&mut h, &tail);
                assert!(!h.ed.has_tool(), "{name}");
                assert_eq!(h.count("LINE"), expected, "{name}");
                undo_redo(&mut h, &before, name);
            }
        }
    }

    #[test]
    fn copy_multiple_destinations_keep_original_base_and_independent_undo() {
        let (mut h, ids) = fixture(vec![line(), EntityKind::Point { p: DVec2::Y }]);
        let before = snapshot(&h);
        preselect(&mut h, &ids, "CP");
        h.cmd("1,1").cmd("5,6");
        let first = snapshot(&h);
        assert_eq!(first.len(), 4);
        assert_eq!(
            first[2].kind,
            EntityKind::Line(Line2::new(DVec2::new(5.0, 5.0), DVec2::new(7.0, 5.0)))
        );
        h.cmd("10,1").enter();
        let second = snapshot(&h);
        assert_eq!(second.len(), 6);
        assert_eq!(
            second[4].kind,
            EntityKind::Line(Line2::new(DVec2::new(10.0, 0.0), DVec2::new(12.0, 0.0)))
        );
        assert_eq!(h.ed.doc.undo().as_deref(), Some("COPY"));
        assert_eq!(snapshot(&h), first);
        assert_eq!(h.ed.doc.undo().as_deref(), Some("COPY"));
        assert_eq!(snapshot(&h), before);
        assert!(!h.ed.doc.can_undo());
        h.ed.doc.redo();
        h.ed.doc.redo();
        assert_eq!(snapshot(&h), second);
    }

    #[test]
    fn copy_preview_escape_does_not_discard_completed_placements() {
        let (mut h, ids) = fixture(vec![line()]);
        preselect(&mut h, &ids, "CO");
        h.cmd("0,0").hover(5.0, 3.0);
        let preview = h.ed.preview.ghosts[0].kind.clone();
        assert_eq!(h.count("LINE"), 1);
        h.click(5.0, 3.0);
        assert_eq!(snapshot(&h)[1].kind, preview);
        let placed = snapshot(&h);
        h.hover(10.0, 5.0).esc();
        assert_eq!(snapshot(&h), placed);
        assert!(h.ed.preview.is_empty());
        assert_eq!(h.ed.doc.undo().as_deref(), Some("COPY"));
        assert_eq!(h.count("LINE"), 1);
        assert!(!h.ed.doc.can_undo());
    }

    #[test]
    fn rotate_signed_numeric_angles_copy_and_large_finite_turns() {
        for degrees in [-90.0_f64, 90.0, 450.0, 1e308] {
            for copy in [false, true] {
                let (mut h, ids) = fixture(vec![line()]);
                let before = snapshot(&h);
                preselect(&mut h, &ids, "RO");
                h.cmd("1,1");
                if copy {
                    h.cmd("C");
                }
                h.cmd(&degrees.to_string());
                assert!(!h.ed.has_tool());
                let es = snapshot(&h);
                assert_eq!(es.len(), if copy { 2 } else { 1 });
                if copy {
                    assert_eq!(es[0], before[0]);
                }
                let EntityKind::Line(l) = es.last().unwrap().kind else {
                    panic!("line");
                };
                let m = DAffine2::from_angle((degrees % 360.0).to_radians());
                near(l.a, DVec2::ONE + m.transform_vector2(DVec2::new(0.0, -1.0)));
                near(l.b, DVec2::ONE + m.transform_vector2(DVec2::new(2.0, -1.0)));
                undo_redo(&mut h, &before, "ROTATE");
            }
        }
    }

    #[test]
    fn rotate_mouse_direction_and_copy_preview_match_commit() {
        for copy in [false, true] {
            let (mut h, ids) = fixture(vec![line()]);
            preselect(&mut h, &ids, "RO");
            h.cmd("1,1");
            if copy {
                h.cmd("Copy");
            }
            let prompt = h.ed.prompt();
            h.cmd("1,1");
            assert_eq!(h.ed.prompt(), prompt);
            assert!(!h.ed.doc.can_undo());
            h.hover(-4.0, 1.0);
            let expected = h.ed.preview.ghosts[0].kind.clone();
            h.click(-4.0, 1.0);
            assert_eq!(snapshot(&h).last().unwrap().kind, expected);
            let EntityKind::Line(l) = expected else {
                panic!("line");
            };
            near(l.a, DVec2::new(1.0, 2.0));
            near(l.b, DVec2::new(-1.0, 2.0));
        }
    }

    #[test]
    fn scale_positive_numeric_factors_with_copy_and_nonzero_base() {
        for factor in [0.5, 2.0, 1e-10, 1e10] {
            for copy in [false, true] {
                let (mut h, ids) = fixture(vec![line()]);
                let before = snapshot(&h);
                preselect(&mut h, &ids, "SC");
                h.cmd("1,1");
                if copy {
                    h.cmd("C");
                }
                h.cmd(&factor.to_string());
                assert!(!h.ed.has_tool());
                let es = snapshot(&h);
                assert_eq!(es.len(), if copy { 2 } else { 1 });
                if copy {
                    assert_eq!(es[0], before[0]);
                }
                let EntityKind::Line(l) = es.last().unwrap().kind else {
                    panic!("line");
                };
                near(l.a, DVec2::ONE + DVec2::new(0.0, -1.0) * factor);
                near(l.b, DVec2::ONE + DVec2::new(2.0, -1.0) * factor);
                undo_redo(&mut h, &before, "SCALE");
            }
        }
    }

    #[test]
    fn scale_mouse_distance_ratio_reference_and_copy_preview() {
        for explicit_reference in [false, true] {
            for copy in [false, true] {
                let (mut h, ids) = fixture(vec![line()]);
                let before = snapshot(&h);
                preselect(&mut h, &ids, "SC");
                h.cmd("1,2");
                if explicit_reference {
                    h.cmd("R");
                }
                h.click(4.0, 6.0);
                if copy {
                    h.cmd("C");
                }
                h.hover(1.0, 12.0);
                let expected = h.ed.preview.ghosts[0].kind.clone();
                assert_eq!(snapshot(&h), before);
                h.click(1.0, 12.0);
                assert!(!h.ed.has_tool());
                assert_eq!(snapshot(&h).last().unwrap().kind, expected);
                let EntityKind::Line(l) = expected else {
                    panic!("line");
                };
                near(l.a, DVec2::new(1.0, -2.0));
                near(l.b, DVec2::new(5.0, -2.0));
                undo_redo(&mut h, &before, "SCALE");
            }
        }
    }

    #[test]
    fn scale_reference_numeric_and_mixed_inputs() {
        for tail in [
            vec!["R", "5", "10"],
            vec!["R", "5", "0,10"],
            vec!["R", "3,4", "10"],
            vec!["3,4", "10"],
        ] {
            let (mut h, ids) = fixture(vec![line()]);
            preselect(&mut h, &ids, "SCALE");
            h.cmd("0,0");
            inputs(&mut h, &tail);
            near(source_line(&h, ids[0]).a, DVec2::X * 2.0);
            near(source_line(&h, ids[0]).b, DVec2::X * 6.0);
            assert!(!h.ed.has_tool());
        }
    }

    #[test]
    fn mirror_axes_delete_keep_default_deferred_commit_and_preview() {
        for (a, b) in [
            (DVec2::new(0.0, 2.0), DVec2::new(3.0, 2.0)),
            (DVec2::X, DVec2::ONE),
            (DVec2::ZERO, DVec2::ONE),
            (DVec2::new(2.0, -3.0), DVec2::new(5.0, 1.0)),
        ] {
            for answer in ["Y", "N", ""] {
                let (mut h, ids) = fixture(vec![line()]);
                let before = snapshot(&h);
                preselect(&mut h, &ids, "MI");
                h.click(a.x, a.y).hover(b.x, b.y);
                let expected = h.ed.preview.ghosts[0].kind.clone();
                h.click(b.x, b.y);
                assert_eq!(h.ed.tool_accepts(), Accept::NONE);
                assert_eq!(snapshot(&h), before);
                assert!(!h.ed.doc.can_undo());
                h.hover(91.0, 72.0);
                assert_eq!(h.ed.preview.ghosts[0].kind, expected);
                h.cmd(answer);
                let es = snapshot(&h);
                assert!(!h.ed.has_tool());
                assert_eq!(es.last().unwrap().kind, expected);
                assert_ne!(es.last().unwrap().id, ids[0]);
                assert_eq!(es.len(), if answer == "Y" { 1 } else { 2 });
                if answer != "Y" {
                    assert_eq!(es[0], before[0]);
                }
                undo_redo(&mut h, &before, "MIRROR");
            }
        }
    }

    #[test]
    fn mirror_repeated_axis_point_is_rejected_and_retryable() {
        let (mut h, ids) = fixture(vec![line()]);
        preselect(&mut h, &ids, "MI");
        h.cmd("1,1");
        let prompt = h.ed.prompt();
        h.cmd("1,1").hover(1.0, 1.0);
        assert_eq!(h.ed.prompt(), prompt);
        assert!(h.ed.preview.ghosts.is_empty());
        assert!(!h.ed.doc.can_undo());
        h.cmd("2,2");
        h.ed.feed(ToolInput::Point(DVec2::new(100.0, 200.0)));
        assert_eq!(h.ed.tool_accepts(), Accept::NONE);
        assert!(!h.ed.doc.can_undo());
        h.cmd("No");
        assert_eq!(h.count("LINE"), 2);
    }

    #[test]
    fn array_numeric_signed_spacing_and_single_axis_arrays() {
        for (rows, cols, dx, dy) in [(2, 3, -5.0, 7.0), (1, 3, 5.0, 0.0), (3, 1, 0.0, -7.0)] {
            let (mut h, ids) = fixture(vec![line()]);
            let before = snapshot(&h);
            preselect(&mut h, &ids, "AR");
            h.cmd(&rows.to_string()).cmd(&cols.to_string()).cmd("10,20");
            if rows > 1 {
                h.cmd(&dy.to_string());
            }
            if cols > 1 {
                h.cmd(&dx.to_string());
            }
            assert!(!h.ed.has_tool());
            let es = snapshot(&h);
            assert_eq!(es.len(), rows * cols);
            for row in 0..rows {
                for col in 0..cols {
                    let EntityKind::Line(l) = es[row * cols + col].kind else {
                        panic!("line");
                    };
                    let offset = DVec2::new(col as f64 * dx, row as f64 * dy);
                    near(l.a, DVec2::X + offset);
                    near(l.b, DVec2::X * 3.0 + offset);
                }
            }
            undo_redo(&mut h, &before, "ARRAYRECT");
        }
    }

    #[test]
    fn array_mouse_spacing_is_axis_projected_and_preview_matches() {
        let (mut h, ids) = fixture(vec![line()]);
        preselect(&mut h, &ids, "AR");
        h.cmd("2").cmd("3").cmd("10,20").hover(50.0, 16.0);
        assert_eq!(h.ed.preview.ghosts.len(), 1);
        assert!(!h.ed.doc.can_undo());
        h.click(50.0, 16.0).hover(7.0, 99.0);
        let preview: Vec<_> = h.ed.preview.ghosts.iter().map(|e| e.kind.clone()).collect();
        assert_eq!(preview.len(), 5);
        h.click(7.0, 99.0);
        assert!(!h.ed.has_tool());
        let es = snapshot(&h);
        assert_eq!(
            es.iter()
                .skip(1)
                .map(|e| e.kind.clone())
                .collect::<Vec<_>>(),
            preview
        );
        let EntityKind::Line(l) = es.last().unwrap().kind else {
            panic!("line");
        };
        near(l.a, DVec2::new(-5.0, -4.0));
    }

    #[test]
    fn array_counts_reject_fractions_zero_overflow_and_over_budget() {
        let (mut h, ids) = fixture(vec![line(), EntityKind::Point { p: DVec2::Y }]);
        preselect(&mut h, &ids, "AR");
        let rows_prompt = h.ed.prompt();
        for v in [0.0, -1.0, 1.5, 10_001.0, 1e300] {
            h.cmd(&v.to_string());
            assert_eq!(h.ed.prompt(), rows_prompt);
            assert!(!h.ed.doc.can_undo());
        }
        h.cmd("100");
        let columns_prompt = h.ed.prompt();
        for v in [0.0, -1.0, 1.5, 51.0, 1e300] {
            h.cmd(&v.to_string());
            assert_eq!(h.ed.prompt(), columns_prompt);
        }
        h.cmd("50");
        assert_eq!(h.ed.tool_accepts(), Accept::POINT);
        h.esc();
        preselect(&mut h, &ids, "AR");
        h.cmd("1");
        let prompt = h.ed.prompt();
        h.cmd("1");
        assert_eq!(h.ed.prompt(), prompt);
        h.cmd("2").cmd("0,0").cmd("5");
        assert!(!h.ed.has_tool());
        assert_eq!(snapshot(&h).len(), 4);
    }

    #[test]
    fn array_exact_total_limit_preview_budget_and_single_undo() {
        let (mut h, ids) = fixture(vec![
            EntityKind::Point { p: DVec2::ZERO },
            EntityKind::Point { p: DVec2::ONE },
        ]);
        let before = snapshot(&h);
        preselect(&mut h, &ids, "AR");
        h.cmd("50").cmd("100").cmd("0,0").cmd("3").hover(4.0, 0.0);
        assert_eq!(h.ed.preview.ghosts.len(), MAX_ARRAY_PREVIEW);
        assert_eq!(snapshot(&h), before);
        h.click(4.0, 0.0);
        assert_eq!(snapshot(&h).len(), MAX_ARRAY_ENTITIES);
        assert!(!h.ed.has_tool());
        undo_redo(&mut h, &before, "ARRAYRECT");
    }

    #[test]
    fn array_spacing_rejects_zero_nonfinite_and_overflow_without_advancing() {
        let (mut h, ids) = fixture(vec![line()]);
        preselect(&mut h, &ids, "AR");
        h.cmd("3").cmd("2").cmd("0,0");
        let prompt = h.ed.prompt();
        for v in [0.0, f64::NAN, f64::INFINITY, f64::MAX] {
            h.ed.feed(ToolInput::Value(v));
            assert_eq!(h.ed.prompt(), prompt);
            assert!(!h.ed.doc.can_undo());
        }
        h.cmd("2");
        let prompt = h.ed.prompt();
        for v in [0.0, f64::NAN, f64::NEG_INFINITY, f64::MAX] {
            h.ed.feed(ToolInput::Value(v));
            assert_eq!(h.ed.prompt(), prompt);
            assert!(!h.ed.doc.can_undo());
        }
        h.cmd("3");
        assert!(!h.ed.has_tool());
        assert_eq!(h.count("LINE"), 6);
    }

    fn all_kinds() -> Vec<EntityKind> {
        vec![
            EntityKind::Point { p: DVec2::ONE },
            line(),
            EntityKind::Circle(Circle2::new(DVec2::new(2.0, 3.0), 2.0)),
            EntityKind::Arc(Arc2::new(DVec2::ONE, 2.0, 0.3, 1.7)),
            EntityKind::Ellipse(EllipseArc2 {
                c: DVec2::ZERO,
                major: DVec2::new(4.0, 1.0),
                ratio: 0.5,
                start: 0.0,
                end: TAU,
            }),
            EntityKind::Polyline(Polyline2 {
                verts: vec![
                    PolyVertex::with_bulge(DVec2::ZERO, 0.5),
                    PolyVertex::new(DVec2::new(4.0, 0.0)),
                ],
                closed: false,
            }),
            EntityKind::Spline(
                Nurbs2::from_fit_points(&[DVec2::ZERO, DVec2::ONE, DVec2::new(3.0, 0.0)], 2)
                    .unwrap(),
            ),
            EntityKind::Text(Text {
                pos: DVec2::ONE,
                height: 2.0,
                rotation: 0.4,
                width_factor: 0.8,
                oblique: 0.1,
                style: TextStyleId(1),
                halign: HAlign::Center,
                valign: VAlign::Top,
                text: "Keep all text properties".into(),
            }),
            EntityKind::MText(MText {
                pos: DVec2::ONE,
                height: 2.0,
                width: 20.0,
                rotation: -0.3,
                line_spacing: 1.2,
                attachment: 5,
                style: TextStyleId(1),
                text: "First\\PSecond".into(),
            }),
            EntityKind::Dimension(Dimension {
                kind: DimKind::Linear {
                    p1: DVec2::ZERO,
                    p2: DVec2::new(10.0, 0.0),
                    line_point: DVec2::new(0.0, 4.0),
                    rotation: 0.3,
                },
                style: DimStyleId(1),
                text_override: Some("L=<>".into()),
                text_pos: Some(DVec2::new(5.0, 4.0)),
            }),
            EntityKind::Hatch(Hatch {
                loops: vec![HatchLoop {
                    curves: vec![Curve2::Circle(Circle2::new(DVec2::ZERO, 3.0))],
                }],
                pattern: HatchPatternRef {
                    name: "ANSI31".into(),
                    angle: 0.7,
                    scale: 1.3,
                },
            }),
            EntityKind::Insert(Insert {
                block: BlockId(1),
                pos: DVec2::ONE,
                scale: DVec2::new(2.0, 3.0),
                rotation: 0.6,
            }),
        ]
    }

    #[test]
    fn new_entities_preserve_every_source_kind_and_property_without_id_overwrite() {
        for (name, tail, m) in [
            (
                "COPY",
                vec!["0,0", "5,2", ""],
                DAffine2::from_translation(DVec2::new(5.0, 2.0)),
            ),
            (
                "ROTATE",
                vec!["0,0", "C", "90"],
                DAffine2::from_angle(FRAC_PI_2),
            ),
            (
                "SCALE",
                vec!["0,0", "C", "2"],
                DAffine2::from_scale(DVec2::splat(2.0)),
            ),
            (
                "MIRROR",
                vec!["0,0", "0,2", "N"],
                DAffine2::from_scale(DVec2::new(-1.0, 1.0)),
            ),
            (
                "ARRAYRECT",
                vec!["1", "2", "0,0", "5"],
                DAffine2::from_translation(DVec2::new(5.0, 0.0)),
            ),
        ] {
            let (mut h, ids) = fixture(all_kinds());
            let before = snapshot(&h);
            preselect(&mut h, &ids, name);
            inputs(&mut h, &tail);
            assert!(!h.ed.has_tool(), "{name}");
            let es = snapshot(&h);
            assert_eq!(es.len(), before.len() * 2, "{name}");
            assert_eq!(&es[..before.len()], before);
            for (original, actual) in before.iter().zip(es.iter().skip(before.len())) {
                let mut expected = xform::transform_entity(original, &m);
                expected.id = actual.id;
                assert_ne!(actual.id, EntityId(0));
                assert!(!ids.contains(&actual.id));
                assert_eq!(*actual, expected, "{name}: {}", original.kind.type_name());
                assert_ne!(actual.layer, h.ed.doc.drawing.tables.current_layer);
            }
            undo_redo(&mut h, &before, name);
        }
    }

    #[test]
    fn locked_hidden_and_frozen_layers_are_filtered_for_every_command() {
        for (name, tail, expected) in cases() {
            for flag in ["locked", "hidden", "frozen"] {
                let (mut h, mut ids) = fixture(vec![line()]);
                let protected = snapshot(&h)[0].clone();
                let active = h.ed.doc.transact("protect", |tx| {
                    let layer = tx.tables_mut().layers.get_mut(&protected.layer).unwrap();
                    match flag {
                        "locked" => layer.locked = true,
                        "hidden" => layer.visible = false,
                        _ => layer.frozen = true,
                    }
                    tx.add(line())
                });
                ids.push(active);
                h.ed.pump();
                h.ed.doc.clear_history();
                let before = snapshot(&h);
                preselect(&mut h, &ids, name);
                inputs(&mut h, &tail);
                assert_eq!(h.ed.doc.drawing.entities[&protected.id], protected);
                assert_eq!(h.count("LINE"), expected + 1, "{name} {flag}");
                undo_redo(&mut h, &before, name);
            }
        }
    }

    #[test]
    fn layer_editability_is_rechecked_at_final_commit() {
        for (name, tail, _) in cases() {
            let (mut h, ids) = fixture(vec![line()]);
            let before = snapshot(&h);
            preselect(&mut h, &ids, name);
            // COPY's last entry is only Enter; lock before its destination, not after it.
            let commit_index = if name == "COPY" {
                tail.len() - 2
            } else {
                tail.len() - 1
            };
            inputs(&mut h, &tail[..commit_index]);
            h.ed.doc.transact("lock", |tx| {
                tx.tables_mut()
                    .layers
                    .get_mut(&before[0].layer)
                    .unwrap()
                    .locked = true;
            });
            h.ed.doc.clear_history();
            h.cmd(tail[commit_index]);
            assert!(!h.ed.has_tool());
            assert_eq!(snapshot(&h), before, "{name}");
            assert!(!h.ed.doc.can_undo());
        }
    }

    #[test]
    fn duplicate_and_stale_ids_never_duplicate_or_overwrite_a_source() {
        for (name, tail, expected) in cases() {
            let (mut h, ids) = fixture(vec![line()]);
            h.cmd(name);
            h.ed.feed(ToolInput::Selection(vec![
                ids[0],
                ids[0],
                EntityId(u64::MAX),
            ]));
            inputs(&mut h, &tail);
            assert!(!h.ed.has_tool());
            assert_eq!(h.count("LINE"), expected, "{name}");
            assert!(!h.ed.doc.drawing.entities.contains_key(&EntityId(0)));
        }
    }

    #[test]
    fn escape_at_every_uncommitted_step_is_side_effect_free() {
        for (name, prefix) in [
            ("COPY", vec![]),
            ("COPY", vec!["0,0"]),
            ("ROTATE", vec![]),
            ("ROTATE", vec!["0,0"]),
            ("ROTATE", vec!["0,0", "C"]),
            ("SCALE", vec![]),
            ("SCALE", vec!["0,0"]),
            ("SCALE", vec!["0,0", "C"]),
            ("SCALE", vec!["0,0", "R"]),
            ("SCALE", vec!["0,0", "R", "5"]),
            ("SCALE", vec!["0,0", "3,4"]),
            ("MIRROR", vec![]),
            ("MIRROR", vec!["0,0"]),
            ("MIRROR", vec!["0,0", "1,1"]),
            ("ARRAYRECT", vec![]),
            ("ARRAYRECT", vec!["2"]),
            ("ARRAYRECT", vec!["2", "3"]),
            ("ARRAYRECT", vec!["2", "3", "0,0"]),
            ("ARRAYRECT", vec!["2", "3", "0,0", "5"]),
            ("ARRAYRECT", vec!["1", "3", "0,0"]),
        ] {
            let (mut h, ids) = fixture(vec![line()]);
            let before = snapshot(&h);
            preselect(&mut h, &ids, name);
            inputs(&mut h, &prefix);
            h.hover(6.0, 8.0).esc();
            assert!(!h.ed.has_tool());
            assert!(h.ed.preview.is_empty());
            assert_eq!(snapshot(&h), before, "{name} {prefix:?}");
            assert!(!h.ed.doc.can_undo());
        }
        for (name, _, _) in cases() {
            let (mut h, _) = fixture(vec![line()]);
            h.cmd(name).esc();
            assert!(!h.ed.has_tool());
            assert!(!h.ed.doc.can_undo());
        }
    }

    #[test]
    fn direct_tool_nonfinite_inputs_and_unadvertised_keywords_are_ignored() {
        let steps = [
            Step::Select,
            Step::Base,
            Step::CopyTo(DVec2::ZERO),
            Step::Angle(DVec2::ZERO),
            Step::Factor(DVec2::ZERO),
            Step::Reference(DVec2::ZERO),
            Step::NewLength {
                base: DVec2::ZERO,
                reference: 2.0,
            },
            Step::Axis(DVec2::ZERO),
            Step::Delete {
                a: DVec2::ZERO,
                b: DVec2::X,
            },
            Step::Rows,
            Step::Columns(2),
            Step::ArrayBase { rows: 2, cols: 2 },
            Step::RowSpacing {
                rows: 2,
                cols: 2,
                base: DVec2::ZERO,
            },
            Step::ColumnSpacing {
                rows: 2,
                cols: 2,
                base: DVec2::ZERO,
                dy: 3.0,
            },
        ];
        let (mut h, ids) = fixture(vec![line()]);
        let before = snapshot(&h);
        for step in steps {
            let mut tool = TransformTool {
                operation: Operation::Copy,
                ids: ids.clone(),
                step,
                copy: false,
            };
            for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                for input in [
                    ToolInput::Point(DVec2::new(v, 1.0)),
                    ToolInput::Value(v),
                    ToolInput::Hover(DVec2::new(v, 1.0)),
                ] {
                    assert_eq!(direct(&mut h, &mut tool, input), ToolFlow::Continue);
                    assert_eq!(tool.step, step);
                    assert_eq!(snapshot(&h), before);
                }
            }
            direct(&mut h, &mut tool, ToolInput::Keyword("NotAnOption"));
            assert_eq!(tool.step, step);
            assert!(!h.ed.doc.can_undo());
        }
    }

    #[test]
    fn invalid_scale_factors_and_references_can_be_retried() {
        let (mut h, ids) = fixture(vec![line()]);
        preselect(&mut h, &ids, "SC");
        h.cmd("1,1");
        let prompt = h.ed.prompt();
        for v in [0.0, -1.0, 1e-200, 1e200, f64::MAX] {
            h.cmd(&v.to_string());
            assert_eq!(h.ed.prompt(), prompt);
            assert!(!h.ed.doc.can_undo());
        }
        h.cmd("R");
        let prompt = h.ed.prompt();
        h.cmd("0").cmd("-5").cmd("1,1");
        assert_eq!(h.ed.prompt(), prompt);
        h.cmd("5");
        let prompt = h.ed.prompt();
        h.cmd("0").cmd("-1").cmd("1,1");
        assert_eq!(h.ed.prompt(), prompt);
        h.cmd("10");
        assert!(!h.ed.has_tool());
        near(source_line(&h, ids[0]).b, DVec2::new(5.0, -1.0));
    }

    #[test]
    fn overflow_of_any_result_prevents_the_entire_transaction() {
        let (mut h, ids) = fixture(vec![
            EntityKind::Point { p: DVec2::ZERO },
            EntityKind::Point { p: DVec2::X },
        ]);
        h.ed.doc.transact("large source", |tx| {
            tx.modify(ids[1], |e| {
                e.kind = EntityKind::Point {
                    p: DVec2::new(f64::MAX, 0.0),
                }
            });
        });
        h.ed.doc.clear_history();
        let before = snapshot(&h);
        let mut tool = TransformTool {
            operation: Operation::Copy,
            ids,
            step: Step::CopyTo(DVec2::ZERO),
            copy: false,
        };
        assert_eq!(
            direct(
                &mut h,
                &mut tool,
                ToolInput::Point(DVec2::new(f64::MAX, 0.0))
            ),
            ToolFlow::Continue
        );
        assert_eq!(snapshot(&h), before);
        assert!(!h.ed.doc.can_undo());
        tool.step = Step::CopyTo(DVec2::new(-f64::MAX, 0.0));
        direct(
            &mut h,
            &mut tool,
            ToolInput::Point(DVec2::new(f64::MAX, 0.0)),
        );
        assert_eq!(snapshot(&h), before);
        assert!(!h.ed.doc.can_undo());
    }

    #[test]
    fn numerical_helpers_reject_extremes_and_reflect_tiny_or_large_axes() {
        for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(reflection(DVec2::ZERO, DVec2::new(v, 1.0)).is_none());
            assert!(rotation(DVec2::ZERO, v).is_none());
            assert!(scaling(DVec2::ZERO, v).is_none());
            assert!(count(v).is_none());
        }
        assert!(reflection(DVec2::ONE, DVec2::ONE).is_none());
        assert!(reflection(DVec2::splat(-f64::MAX), DVec2::splat(f64::MAX)).is_none());
        for magnitude in [1e-320, 1e-100, 1.0, 1e100, 1e308] {
            let m = reflection(DVec2::ZERO, DVec2::splat(magnitude)).unwrap();
            near(
                m.transform_point2(DVec2::new(2.0, 3.0)),
                DVec2::new(3.0, 2.0),
            );
        }
        assert!(scaling(DVec2::ZERO, 1e-200).is_none());
        assert!(scaling(DVec2::ZERO, 1e200).is_none());
        assert!(array_size(usize::MAX, 2, 2).is_none());
        assert!(array_size(100, 100, 2).is_none());
        assert_eq!(array_size(50, 100, 2), Some(10_000));
        assert!(array_matrices(3, 2, 1.0, f64::MAX).is_none());
        assert!(array_matrices(1, 1, 0.0, 0.0).is_none());
        assert!(array_matrices(0, 5, 2.0, 3.0).is_none());
        assert!(array_matrices(2, 2, 0.0, 3.0).is_none());
    }

    #[test]
    fn empty_stale_or_entirely_locked_selection_finishes_without_history() {
        for (name, _, _) in cases() {
            for mode in ["empty", "stale", "locked"] {
                let (mut h, ids) = fixture(vec![line()]);
                let before = snapshot(&h);
                if mode == "locked" {
                    h.ed.doc.transact("lock", |tx| {
                        tx.tables_mut()
                            .layers
                            .get_mut(&before[0].layer)
                            .unwrap()
                            .locked = true;
                    });
                    h.ed.doc.clear_history();
                    h.ed.selection.set(ids);
                }
                h.cmd(name);
                match mode {
                    "empty" => {
                        h.enter();
                    }
                    "stale" => h.ed.feed(ToolInput::Selection(vec![EntityId(u64::MAX)])),
                    _ => {}
                }
                assert!(!h.ed.has_tool(), "{name} {mode}");
                assert_eq!(snapshot(&h), before);
                assert!(!h.ed.doc.can_undo());
            }
        }
    }

    #[test]
    fn malformed_curve_sources_are_not_silently_downgraded_or_partially_copied() {
        let invalid = [
            EntityKind::Polyline(Polyline2 {
                verts: vec![
                    PolyVertex::with_bulge(DVec2::ZERO, f64::MAX),
                    PolyVertex::new(DVec2::X),
                ],
                closed: false,
            }),
            EntityKind::Spline(Nurbs2::default()),
            EntityKind::Circle(Circle2::new(DVec2::ZERO, f64::INFINITY)),
        ];
        for kind in invalid {
            assert!(!kind_valid(&kind));
            let (mut h, ids) = fixture(vec![line(), EntityKind::Point { p: DVec2::ONE }]);
            h.ed.doc.transact("malformed", |tx| {
                tx.modify(ids[1], |e| e.kind = kind);
            });
            h.ed.doc.clear_history();
            let before = snapshot(&h);
            let mut tool = TransformTool {
                operation: Operation::Copy,
                ids,
                step: Step::CopyTo(DVec2::ZERO),
                copy: false,
            };
            direct(&mut h, &mut tool, ToolInput::Point(DVec2::ONE));
            assert_eq!(snapshot(&h), before);
            assert!(!h.ed.doc.can_undo());
        }
    }
}
