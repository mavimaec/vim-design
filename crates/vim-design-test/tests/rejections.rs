//! Test plan (c): rejected commands leave the document byte-identical.
//!
//! Covers delete-with-dependents (+ dependents query), cycle rewire,
//! kind-mismatch wiring, unknown ids, and double delete. After EVERY
//! rejection the serialized bytes must equal the bytes from before the
//! attempt (speculative apply + rollback, docs/ARCHITECTURE.md §4.1).

use vim_design_lib::{Command, Document, EntityId, VimStatus};
use vim_design_test::{assert_save_load_roundtrip, build_chain, one, save};

/// Assert `cmd` is rejected with `expected` and that the attempt left no
/// trace: identical bytes, identical undo/redo depths, no dirty entries.
fn assert_rejected(doc: &mut Document, cmd: Command, expected: VimStatus) {
    let bytes_before = save(doc);
    let undo_before = doc.undo_depth();
    let redo_before = doc.redo_depth();
    let dirty_before = doc.dirty_set().clone();
    let label = cmd.label();

    assert_eq!(doc.submit(cmd).err(), Some(expected), "{label}");

    assert_eq!(save(doc), bytes_before, "{label}: bytes changed after rejection");
    assert_eq!(doc.undo_depth(), undo_before, "{label}: undo stack changed");
    assert_eq!(doc.redo_depth(), redo_before, "{label}: redo stack changed");
    assert_eq!(doc.dirty_set(), &dirty_before, "{label}: dirty set changed");
    doc.debug_validate().expect("invariants after rejection");
}

#[test]
fn delete_with_dependents_is_rejected_and_dependents_are_queryable() {
    let mut doc = Document::new();
    let chain = build_chain(&mut doc);

    assert_rejected(
        &mut doc,
        Command::DeleteControlPoint { id: chain.cps[0] },
        VimStatus::HasDependents,
    );
    // The UI-facing "why not" query (docs/ARCHITECTURE.md §3.2).
    assert_eq!(doc.dependents(chain.cps[0]), Ok(vec![chain.spline]));

    assert_rejected(
        &mut doc,
        Command::DeleteFace { id: chain.face },
        VimStatus::HasDependents,
    );
    assert_eq!(doc.dependents(chain.face), Ok(vec![chain.extrusion]));

    assert_rejected(
        &mut doc,
        Command::DeleteElement { id: chain.element },
        VimStatus::HasDependents,
    );
    assert_eq!(
        doc.dependents(chain.element),
        Ok(chain.instances.to_vec())
    );

    assert_save_load_roundtrip(&doc);
}

#[test]
fn cycle_creating_rewires_are_rejected() {
    let mut doc = Document::new();
    let chain = build_chain(&mut doc);

    // Wire the extrusion's downstream element back into the extrusion.
    // The cycle check deliberately fires before the kind check, so any
    // self/transitive loop reports as WouldCreateCycle regardless of the
    // kinds involved. (With today's slot tables no kind-correct cycle is
    // even constructible — the kind-level "consumes" relation is acyclic —
    // but the graph machinery does not rely on that staying true.)
    assert_rejected(
        &mut doc,
        Command::UpdateExtrusion {
            id: chain.extrusion,
            profile: None,
            path: Some(chain.element),
            coalesce: false,
        },
        VimStatus::WouldCreateCycle,
    );

    // Transitive cycle through several hops: edge -> face -> extrusion;
    // rewiring the edge's curve to anything downstream of the edge loops.
    assert_rejected(
        &mut doc,
        Command::UpdateEdge {
            id: chain.edge,
            curve: chain.extrusion,
            coalesce: false,
        },
        VimStatus::WouldCreateCycle,
    );

    // Direct self-reference.
    assert_rejected(
        &mut doc,
        Command::UpdateEdge {
            id: chain.edge,
            curve: chain.edge,
            coalesce: false,
        },
        VimStatus::WouldCreateCycle,
    );

    assert_save_load_roundtrip(&doc);
}

#[test]
fn kind_mismatch_wiring_is_rejected() {
    let mut doc = Document::new();
    let chain = build_chain(&mut doc);

    assert_rejected(
        &mut doc,
        Command::CreateEdge { curve: chain.cps[0] },
        VimStatus::SlotKindMismatch,
    );
    assert_rejected(
        &mut doc,
        Command::CreateChamfer {
            distance: 0.01,
            edges: vec![chain.face], // faces are not edges/selections
        },
        VimStatus::SlotKindMismatch,
    );
    assert_rejected(
        &mut doc,
        Command::UpdateFaceMaterial {
            face: chain.face,
            material: Some(chain.line), // not a material
        },
        VimStatus::SlotKindMismatch,
    );
    // Wrong kind behind an otherwise valid id, at the command level.
    assert_rejected(
        &mut doc,
        Command::UpdateControlPoint {
            id: chain.line,
            position: [0.0; 3],
            coalesce: false,
        },
        VimStatus::WrongEntityKind,
    );

    assert_save_load_roundtrip(&doc);
}

#[test]
fn unknown_ids_and_double_delete_are_rejected() {
    let mut doc = Document::new();
    let chain = build_chain(&mut doc);
    let ghost = EntityId(999_999);

    assert_rejected(
        &mut doc,
        Command::UpdateControlPoint {
            id: ghost,
            position: [0.0; 3],
            coalesce: false,
        },
        VimStatus::EntityNotFound,
    );
    assert_rejected(
        &mut doc,
        Command::CreateLine {
            start: chain.cps[0],
            end: ghost,
        },
        VimStatus::EntityNotFound,
    );
    assert_eq!(doc.dependents(ghost).err(), Some(VimStatus::EntityNotFound));

    // Double delete: first succeeds, second is EntityNotFound.
    let lonely = one(&mut doc, Command::CreateControlPoint { position: [7.0; 3] });
    assert!(doc.submit(Command::DeleteControlPoint { id: lonely }).is_ok());
    assert_rejected(
        &mut doc,
        Command::DeleteControlPoint { id: lonely },
        VimStatus::EntityNotFound,
    );

    assert_save_load_roundtrip(&doc);
}
