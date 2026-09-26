//! Native file format: JSON `{ "format": "webcad", "version": 1, ... }`, optionally zlib-compressed
//! (`.wcadz`). [`load`] detects compression automatically.

use serde::{Deserialize, Serialize};

use crate::document::{DocMeta, Document};
use crate::drawing::Drawing;
use crate::ids::IdAllocator;
use crate::part::Part;
use crate::{Error, Result};

pub const FORMAT: &str = "webcad";
pub const VERSION: u32 = 1;
pub const EXTENSION: &str = "wcad";
pub const EXTENSION_COMPRESSED: &str = "wcadz";

#[derive(Serialize)]
struct FileOut<'a> {
    format: &'a str,
    version: u32,
    meta: &'a DocMeta,
    ids: &'a IdAllocator,
    drawing: &'a Drawing,
    part: &'a Part,
}

#[derive(Deserialize)]
struct FileHeader {
    format: String,
    version: u32,
}

#[derive(Deserialize)]
struct FileIn {
    meta: DocMeta,
    ids: IdAllocator,
    drawing: Drawing,
    #[serde(default)]
    part: Part,
}

pub fn to_json(doc: &Document, pretty: bool) -> Result<String> {
    let out = FileOut {
        format: FORMAT,
        version: VERSION,
        meta: &doc.meta,
        ids: &doc.ids,
        drawing: &doc.drawing,
        part: &doc.part,
    };
    Ok(if pretty {
        serde_json::to_string_pretty(&out)?
    } else {
        serde_json::to_string(&out)?
    })
}

/// Serialize; `compressed` selects zlib (`.wcadz`).
pub fn save(doc: &Document, compressed: bool) -> Result<Vec<u8>> {
    let json = to_json(doc, !compressed)?;
    Ok(if compressed {
        miniz_oxide::deflate::compress_to_vec_zlib(json.as_bytes(), 6)
    } else {
        json.into_bytes()
    })
}

/// Load from bytes (plain JSON or zlib-compressed JSON).
pub fn load(bytes: &[u8]) -> Result<Document> {
    let first = bytes.iter().copied().find(|b| !b.is_ascii_whitespace());
    let json: std::borrow::Cow<[u8]> = if first == Some(b'{') {
        bytes.into()
    } else {
        miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(bytes, 1 << 30)
            .map_err(|_| Error::Decompress)?
            .into()
    };
    let header: FileHeader = serde_json::from_slice(&json).map_err(|_| Error::NotWebcad)?;
    if header.format != FORMAT {
        return Err(Error::NotWebcad);
    }
    if header.version > VERSION {
        return Err(Error::UnsupportedVersion(header.version));
    }
    let f: FileIn = serde_json::from_slice(&json)?;
    let mut doc = Document::from_parts(f.meta, f.drawing, f.part, f.ids);
    doc.mark_saved();
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::EntityKind;
    use crate::{AxisRef, BodyOp, Extent, Feature, FeatureKind, PlaneRef, ProfileRef};
    use wcad_geom2d::{Arc2, Line2};
    use wcad_math::DVec2;
    use wcad_sketch::{ConstraintKind, DimValue, SkConstraintId, SkEntityId, Sketch};

    #[test]
    fn round_trip_plain_and_compressed() {
        let mut doc = Document::new();
        doc.transact("draw", |tx| {
            tx.add(EntityKind::Line(Line2::new(
                DVec2::ZERO,
                DVec2::new(10.0, 5.0),
            )));
            tx.add(EntityKind::Arc(Arc2::new(DVec2::ONE, 3.0, 0.0, 1.5)));
        });
        for compressed in [false, true] {
            let bytes = save(&doc, compressed).unwrap();
            let back = load(&bytes).unwrap();
            assert_eq!(back.drawing, doc.drawing);
            assert_eq!(back.part, doc.part);
            assert!(!back.is_dirty());
        }
        assert!(matches!(
            load(b"{\"format\":\"other\",\"version\":1}"),
            Err(Error::NotWebcad)
        ));
    }

    fn sketch_document() -> Document {
        let mut sketch = Sketch::new();
        let rectangle = sketch.add_rectangle(DVec2::new(1.0, 0.0), DVec2::new(5.0, 2.0));
        sketch.add_constraint(ConstraintKind::Fix {
            p: rectangle.corners[0],
        });
        let distance = sketch.add_constraint(ConstraintKind::Distance {
            p1: rectangle.corners[0],
            p2: rectangle.corners[1],
            value: DimValue {
                value: 4.0,
                expr: Some("2*2".into()),
            },
        });
        sketch.set_enabled(distance, false).unwrap();
        sketch
            .add_reference_dimension(ConstraintKind::Length {
                line: rectangle.lines[1],
                value: DimValue::new(2.0),
            })
            .unwrap();
        let circle = sketch.add_circle_center_radius(DVec2::new(3.0, 1.0), 0.5);
        sketch.add_constraint(ConstraintKind::Radius {
            round: circle,
            value: DimValue::new(0.5),
        });
        let arc = sketch
            .add_arc_center_start_end(
                DVec2::new(8.0, 0.0),
                DVec2::new(9.0, 0.0),
                DVec2::new(8.0, 1.0),
            )
            .unwrap();
        sketch.set_construction(arc, true).unwrap();
        // Persist counters above the retained maps, not just max(retained ID) + 1.
        let removed = sketch.add_point(DVec2::new(11.0, 12.0));
        sketch.remove_entity(removed).unwrap();
        let removed = sketch.add_constraint(ConstraintKind::Fix {
            p: rectangle.corners[3],
        });
        sketch.remove_constraint(removed).unwrap();

        let mut doc = Document::new();
        doc.transact("sketch features", |tx| {
            let sketch_id = tx.ids().feature();
            let extrude_id = tx.ids().feature();
            let revolve_id = tx.ids().feature();
            tx.part_mut().features.extend([
                Feature {
                    id: sketch_id,
                    name: "Parametric sketch".into(),
                    suppressed: false,
                    kind: FeatureKind::Sketch {
                        plane: PlaneRef::Xz,
                        sketch,
                    },
                },
                Feature {
                    id: extrude_id,
                    name: "Extrude".into(),
                    suppressed: false,
                    kind: FeatureKind::Extrude {
                        profile: ProfileRef {
                            sketch: sketch_id,
                            regions: vec![DVec2::new(2.0, 1.0)],
                        },
                        extent: Extent::Symmetric { distance: 2.0 },
                        reversed: false,
                        op: BodyOp::NewBody,
                    },
                },
                Feature {
                    id: revolve_id,
                    name: "Revolve".into(),
                    suppressed: false,
                    kind: FeatureKind::Revolve {
                        profile: ProfileRef {
                            sketch: sketch_id,
                            regions: vec![],
                        },
                        axis: AxisRef::SketchLine {
                            sketch: sketch_id,
                            line: rectangle.lines[0],
                        },
                        angle: std::f64::consts::TAU,
                        op: BodyOp::NewBody,
                    },
                },
            ]);
        });
        doc
    }

    #[test]
    fn nested_sketch_features_round_trip_plain_and_compressed() {
        let doc = sketch_document();
        let json: serde_json::Value = serde_json::from_str(&to_json(&doc, false).unwrap()).unwrap();
        assert_eq!(json["version"], VERSION);
        let serialized = &json["part"]["features"][0]["kind"]["sketch"];
        assert_eq!(serialized["entities"]["4"]["geom"]["a"], 0);
        assert_eq!(serialized["entities"]["4"]["geom"]["b"], 1);
        assert_eq!(serialized["constraints"]["4"]["kind"]["p"], 0);
        assert_eq!(
            serialized["constraints"]["5"]["kind"]["value"]["expr"],
            "2*2"
        );
        let FeatureKind::Sketch { sketch, .. } = &doc.part.features[0].kind else {
            panic!("sketch")
        };
        let mut expected = sketch.clone();
        let point_id = expected.add_point(DVec2::ONE);
        let constraint_id = expected.add_constraint(ConstraintKind::Fix { p: point_id });

        for compressed in [false, true] {
            let bytes = save(&doc, compressed).unwrap();
            let loaded = load(&bytes).unwrap();
            assert_eq!(loaded.part, doc.part);
            assert_eq!(loaded.drawing, doc.drawing);
            assert_eq!(loaded.ids(), doc.ids());
            assert!(!loaded.is_dirty());
            assert!(!loaded.can_undo());
            let FeatureKind::Sketch {
                sketch: restored, ..
            } = &loaded.part.features[0].kind
            else {
                panic!("sketch")
            };
            assert_eq!(restored.entities, sketch.entities);
            assert_eq!(restored.constraints, sketch.constraints);
            for constraint in restored.constraints.values() {
                restored.validate_constraint(&constraint.kind).unwrap();
            }
            let mut restored = restored.clone();
            assert_eq!(restored.add_point(DVec2::ONE), point_id);
            assert_eq!(
                restored.add_constraint(ConstraintKind::Fix { p: point_id }),
                constraint_id
            );
        }
    }

    #[test]
    fn legacy_nested_sketch_defaults_and_id_maps_survive_native_load() {
        let mut json: serde_json::Value =
            serde_json::from_str(&to_json(&sketch_document(), false).unwrap()).unwrap();
        json["part"]["features"].as_array_mut().unwrap().truncate(1);
        // The original version-1 layout, without optional flags or allocation counters.
        json["part"]["features"][0]["kind"]["sketch"] = serde_json::json!({
            "entities": {
                "3": { "geom": { "type": "Point", "p": [1.0, 2.0] } },
                "4": { "geom": { "type": "Point", "p": [5.0, 2.0] } },
                "17": { "geom": { "type": "Line", "a": 3, "b": 4 } }
            },
            "constraints": {
                "9": { "kind": { "type": "Fix", "p": 3 } },
                "23": { "kind": { "type": "Distance", "p1": 3, "p2": 4, "value": { "value": 4.0, "expr": "2*2" } } }
            }
        });
        let bytes = serde_json::to_vec(&json).unwrap();
        for bytes in [
            bytes.clone(),
            miniz_oxide::deflate::compress_to_vec_zlib(&bytes, 6),
        ] {
            let loaded = load(&bytes).unwrap();
            let FeatureKind::Sketch { sketch, .. } = &loaded.part.features[0].kind else {
                panic!("sketch")
            };
            assert_eq!(sketch.point(SkEntityId(3)), Some(DVec2::new(1.0, 2.0)));
            assert_eq!(
                sketch.line_ends(SkEntityId(17)),
                Some((SkEntityId(3), SkEntityId(4)))
            );
            assert!(!sketch.entity(SkEntityId(17)).unwrap().construction);
            assert!(sketch.constraint(SkConstraintId(9)).unwrap().is_enforced());
            assert_eq!(
                sketch.constraints[&SkConstraintId(23)]
                    .kind
                    .dim_value()
                    .unwrap()
                    .expr
                    .as_deref(),
                Some("2*2")
            );
            let mut sketch = sketch.clone();
            let point = sketch.add_point(DVec2::ZERO);
            assert_eq!(point, SkEntityId(18));
            assert_eq!(
                sketch.add_constraint(ConstraintKind::Fix { p: point }),
                SkConstraintId(24)
            );
        }
    }

    #[test]
    fn invalid_nested_sketch_ids_are_rejected_in_both_native_encodings() {
        let original: serde_json::Value =
            serde_json::from_str(&to_json(&sketch_document(), false).unwrap()).unwrap();
        let reject = |json: &serde_json::Value| {
            let bytes = serde_json::to_vec(json).unwrap();
            for bytes in [
                bytes.clone(),
                miniz_oxide::deflate::compress_to_vec_zlib(&bytes, 6),
            ] {
                assert!(
                    matches!(load(&bytes), Err(Error::Json(_))),
                    "invalid sketch ID accepted: {json}"
                );
            }
        };
        for key in ["", "-1", "+1", "1.5", "1e2", "4294967296", " 1", "abc"] {
            for field in ["entities", "constraints"] {
                let mut json = original.clone();
                let map = json["part"]["features"][0]["kind"]["sketch"][field]
                    .as_object_mut()
                    .unwrap();
                let entry = map.remove("0").unwrap();
                map.insert(key.into(), entry);
                reject(&json);
            }
        }
        for value in [
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!(4294967296u64),
            serde_json::json!("-1"),
            serde_json::json!("4294967296"),
            serde_json::Value::Null,
        ] {
            for pointer in [
                "/part/features/0/kind/sketch/entities/4/geom/a",
                "/part/features/0/kind/sketch/constraints/4/kind/p",
            ] {
                let mut json = original.clone();
                *json.pointer_mut(pointer).unwrap() = value.clone();
                reject(&json);
            }
        }
    }
}
