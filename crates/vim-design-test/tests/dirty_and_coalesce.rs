//! Test plan (b): dirty-closure exactness, rewiring, and coalescing.

use std::collections::BTreeSet;

use vim_design_lib::{Command, Document, EntityId, Params};
use vim_design_test::{assert_save_load_roundtrip, build_chain, ok};

fn set(ids: impl IntoIterator<Item = EntityId>) -> BTreeSet<EntityId> {
    ids.into_iter().collect()
}

#[test]
fn update_at_chain_bottom_dirties_exactly_the_downstream_closure() {
    let mut doc = Document::new();
    let chain = build_chain(&mut doc);
    doc.take_dirty(); // clear creation dirt

    ok(
        &mut doc,
        Command::UpdateControlPoint {
            id: chain.cps[0],
            position: [0.5, 0.5, 0.0],
            coalesce: false,
        },
    );

    // Exactly: the control point plus its downstream transitive closure.
    let expected = set([
        chain.cps[0],
        chain.spline,
        chain.edge,
        chain.wire,
        chain.face,
        chain.extrusion,
        chain.element,
        chain.instances[0],
        chain.instances[1],
    ]);
    assert_eq!(doc.take_dirty(), expected);
    assert!(doc.dirty_set().is_empty(), "take_dirty drains");

    // A material edit dirties the material and everything consuming it.
    ok(
        &mut doc,
        Command::UpdateMaterial {
            id: chain.material,
            name: None,
            color: Some([1.0, 0.0, 0.0]),
            roughness: None,
            coalesce: false,
        },
    );
    let expected = set([
        chain.material,
        chain.face,
        chain.extrusion,
        chain.element,
        chain.instances[0],
        chain.instances[1],
    ]);
    assert_eq!(doc.take_dirty(), expected);

    assert_save_load_roundtrip(&doc);
}

#[test]
fn rewiring_extrusion_path_updates_both_downstream_lists_and_dirty() {
    let mut doc = Document::new();
    let chain = build_chain(&mut doc);
    doc.take_dirty();

    // Before: the line feeds the extrusion; the spline feeds only the edge.
    assert_eq!(doc.dependents(chain.line), Ok(vec![chain.extrusion]));
    assert_eq!(doc.dependents(chain.spline), Ok(vec![chain.edge]));

    // Rewire the extrusion's path from the line to the spline.
    ok(
        &mut doc,
        Command::UpdateExtrusion {
            id: chain.extrusion,
            profile: None,
            path: Some(chain.spline),
            coalesce: false,
        },
    );

    // Both downstream lists updated...
    assert_eq!(doc.dependents(chain.line), Ok(vec![]), "line lost its dependent");
    assert_eq!(
        doc.dependents(chain.spline),
        Ok(vec![chain.edge, chain.extrusion]),
        "spline gained the extrusion"
    );
    doc.debug_validate()
        .expect("index matches from-scratch rebuild after rewire");

    // ...and the dirty closure is exactly the rewired entity + downstream.
    let expected = set([
        chain.extrusion,
        chain.element,
        chain.instances[0],
        chain.instances[1],
    ]);
    assert_eq!(doc.take_dirty(), expected);

    // Undo restores the original wiring on both sides.
    doc.undo().expect("undo rewire");
    assert_eq!(doc.dependents(chain.line), Ok(vec![chain.extrusion]));
    assert_eq!(doc.dependents(chain.spline), Ok(vec![chain.edge]));

    assert_save_load_roundtrip(&doc);
}

#[test]
fn hundred_coalesced_updates_collapse_to_one_undo_step() {
    let mut doc = Document::new();
    let chain = build_chain(&mut doc);
    let depth_before = doc.undo_depth();
    let original = doc.entity(chain.cps[0]).expect("cp exists").params.clone();

    for i in 1..=100 {
        ok(
            &mut doc,
            Command::UpdateControlPoint {
                id: chain.cps[0],
                position: [f64::from(i) * 0.01, 0.0, 0.0],
                coalesce: true,
            },
        );
    }
    assert_eq!(
        doc.undo_depth(),
        depth_before + 1,
        "100 coalesced updates are one undo step"
    );

    // Last new-value wins...
    assert_eq!(
        doc.entity(chain.cps[0]).map(|r| r.params.clone()),
        Some(Params::ControlPoint {
            position: [1.0, 0.0, 0.0]
        })
    );
    // ...and one undo restores the first old-value.
    doc.undo().expect("undo the whole drag");
    assert_eq!(
        doc.entity(chain.cps[0]).map(|r| r.params.clone()),
        Some(original)
    );
    assert_eq!(doc.undo_depth(), depth_before);

    assert_save_load_roundtrip(&doc);
}

#[test]
fn non_coalesced_updates_do_not_merge() {
    let mut doc = Document::new();
    let chain = build_chain(&mut doc);
    let depth_before = doc.undo_depth();

    for i in 1..=3 {
        ok(
            &mut doc,
            Command::UpdateControlPoint {
                id: chain.cps[0],
                position: [f64::from(i), 0.0, 0.0],
                coalesce: false,
            },
        );
    }
    assert_eq!(doc.undo_depth(), depth_before + 3);

    // Coalesced runs on *different* entities stay separate steps too.
    ok(
        &mut doc,
        Command::UpdateControlPoint {
            id: chain.cps[1],
            position: [9.0, 0.0, 0.0],
            coalesce: true,
        },
    );
    ok(
        &mut doc,
        Command::UpdateControlPoint {
            id: chain.cps[2],
            position: [9.0, 0.0, 0.0],
            coalesce: true,
        },
    );
    assert_eq!(doc.undo_depth(), depth_before + 5);
}
