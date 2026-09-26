//! Native annotation entities: no document mutation before the final confirmation, and no
//! exploded dimension graphics. Ghosts use the same display path as committed entities.

mod dimension;
mod hatch;
#[cfg(test)]
mod tests;
mod text;

#[path = "../i18n/annotate.rs"]
pub mod strings;

use wcad_doc::{Entity, EntityId, EntityKind};
use wcad_math::DVec2;

use super::{Keyword, Preview, Tool, ToolCx, ToolFlow, ToolInput};
use crate::commands::{CommandKind, CommandRegistry, CommandSpec, RibbonTab};
use crate::i18n::Lang;
use dimension::{DimMode, DimensionTool};
use hatch::HatchTool;
use strings::annotate;
use text::TextTool;

const EPS: f64 = 1e-9;
// Fill tessellation rejects extents of 1e30 and above. Reject out-of-range input before
// storing a step, so a finite-but-unrenderable size cannot poison the remaining command.
const MAX_SIZE: f64 = 1e30;

pub fn register(r: &mut CommandRegistry) {
    let tool = |name,
                aliases,
                label: fn(Lang) -> &'static str,
                icon,
                group,
                ctor: fn() -> Box<dyn Tool>| CommandSpec {
        name,
        aliases,
        label,
        icon,
        tab: Some(RibbonTab::Annotate),
        group,
        kind: CommandKind::Tool(ctor),
    };
    r.add(tool(
        "TEXT",
        &["DT"],
        |l| annotate(l).text,
        "Ab",
        "text",
        || Box::new(TextTool::new(false)),
    ));
    r.add(tool(
        "MTEXT",
        &["MT", "T"],
        |l| annotate(l).mtext,
        "MT",
        "text",
        || Box::new(TextTool::new(true)),
    ));
    r.add(tool(
        "DIMLINEAR",
        &["DLI"],
        |l| annotate(l).linear,
        "|-|",
        "dimension",
        || Box::new(DimensionTool::new(DimMode::Linear)),
    ));
    r.add(tool(
        "DIMALIGNED",
        &["DAL"],
        |l| annotate(l).aligned,
        "/-/",
        "dimension",
        || Box::new(DimensionTool::new(DimMode::Aligned)),
    ));
    r.add(tool(
        "DIMRADIUS",
        &["DRA"],
        |l| annotate(l).radius,
        "R",
        "dimension",
        || Box::new(DimensionTool::new(DimMode::Radius)),
    ));
    r.add(tool(
        "DIMDIAMETER",
        &["DDI"],
        |l| annotate(l).diameter,
        "D",
        "dimension",
        || Box::new(DimensionTool::new(DimMode::Diameter)),
    ));
    r.add(tool(
        "DIMANGULAR",
        &["DAN"],
        |l| annotate(l).angular,
        "<)",
        "dimension",
        || Box::new(DimensionTool::new(DimMode::Angular)),
    ));
    r.add(tool(
        "DIMORDINATE",
        &["DOR"],
        |l| annotate(l).ordinate,
        "X,Y",
        "dimension",
        || Box::new(DimensionTool::new(DimMode::Ordinate)),
    ));
    r.add(tool(
        "HATCH",
        &["H"],
        |l| annotate(l).hatch,
        "///",
        "hatch",
        || Box::new(HatchTool::default()),
    ));
}

fn kw(id: &'static str, key: &'static str, label: &'static str) -> Keyword {
    Keyword { id, key, label }
}

fn positive(v: f64) -> bool {
    v.is_finite() && v > EPS && v < MAX_SIZE
}

fn valid_point(p: DVec2) -> bool {
    p.is_finite() && p.abs().max_element() < MAX_SIZE
}

fn separated(a: DVec2, b: DVec2) -> bool {
    valid_point(a) && valid_point(b) && positive(a.distance(b))
}

fn finite_input(input: &ToolInput) -> bool {
    match input {
        ToolInput::Point(p) | ToolInput::Hover(p) => valid_point(*p),
        ToolInput::Value(v) => v.is_finite(),
        _ => true,
    }
}

fn error(cx: &mut ToolCx<'_>, message: &'static str) -> ToolFlow {
    cx.error(message);
    ToolFlow::Continue
}

fn commit(cx: &mut ToolCx<'_>, name: &'static str, kind: EntityKind) -> ToolFlow {
    if !cx
        .drawing()
        .is_layer_editable(cx.drawing().tables.current_layer)
    {
        let message = annotate(cx.lang()).layer_locked;
        return error(cx, message);
    }
    cx.transact(name, |tx| tx.add(kind));
    ToolFlow::Done
}

fn ghost(cx: &ToolCx<'_>, out: &mut Preview, kind: EntityKind) {
    if cx
        .drawing()
        .is_layer_editable(cx.drawing().tables.current_layer)
    {
        out.ghosts.push(Entity {
            id: EntityId(0),
            layer: cx.drawing().tables.current_layer,
            color: Default::default(),
            linetype: Default::default(),
            linetype_scale: 1.0,
            lineweight: Default::default(),
            kind,
        });
    }
}
