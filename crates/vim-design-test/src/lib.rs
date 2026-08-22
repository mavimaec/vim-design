//! VimDesignTest: shared fixtures for the integration & regression tests
//! in `tests/` (docs/ARCHITECTURE.md §12).

use vim_design_lib::{Command, CommandOutput, Document, EntityId};

/// Identity rigid transform (row-major 4x3).
pub const IDENTITY_XFORM: [f64; 12] = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0,
];

/// Rigid translation transform (row-major 4x3).
pub fn translation(x: f64, y: f64, z: f64) -> [f64; 12] {
    [
        1.0, 0.0, 0.0, x, //
        0.0, 1.0, 0.0, y, //
        0.0, 0.0, 1.0, z,
    ]
}

/// Submit a command that must succeed.
pub fn ok(doc: &mut Document, cmd: Command) -> CommandOutput {
    let label = cmd.label();
    match doc.submit(cmd) {
        Ok(output) => output,
        Err(status) => panic!("{label} unexpectedly rejected: {status:?}"),
    }
}

/// Submit a command that must succeed and create exactly one entity.
pub fn one(doc: &mut Document, cmd: Command) -> EntityId {
    let output = ok(doc, cmd);
    assert_eq!(output.created_ids.len(), 1, "expected exactly one created id");
    output.created_ids[0]
}

/// Serialize, asserting success.
pub fn save(doc: &Document) -> Vec<u8> {
    doc.save().expect("save should succeed")
}

/// Scenario postcondition (test plan item g): save -> load -> save must be
/// byte-identical, and the reloaded graph must pass full validation.
pub fn assert_save_load_roundtrip(doc: &Document) {
    let first = save(doc);
    let reloaded = Document::load(&first).expect("load of fresh save should succeed");
    reloaded
        .debug_validate()
        .expect("reloaded graph must satisfy all invariants");
    assert_eq!(reloaded.entity_count(), doc.entity_count());
    assert!(!reloaded.can_undo(), "undo stacks are not persisted");
    let second = save(&reloaded);
    assert_eq!(first, second, "save -> load -> save must be byte-identical");
}

/// The bottom-up chain from test plan item (a):
/// 4 control points -> spline -> edge -> wire -> face; 2 control points
/// -> line; extrusion(face, line); material; element with the extrusion;
/// 2 instances.
pub struct Chain {
    pub cps: [EntityId; 4],
    pub spline: EntityId,
    pub edge: EntityId,
    pub wire: EntityId,
    pub face: EntityId,
    pub line_cps: [EntityId; 2],
    pub line: EntityId,
    pub extrusion: EntityId,
    pub material: EntityId,
    pub element: EntityId,
    pub instances: [EntityId; 2],
}

/// Build the standard chain (validating nothing beyond command success;
/// the chain_bottom_up test asserts the invariants step by step).
pub fn build_chain(doc: &mut Document) -> Chain {
    let cps = [
        one(doc, Command::CreateControlPoint { position: [0.0, 0.0, 0.0] }),
        one(doc, Command::CreateControlPoint { position: [1.0, 0.0, 0.0] }),
        one(doc, Command::CreateControlPoint { position: [1.0, 1.0, 0.0] }),
        one(doc, Command::CreateControlPoint { position: [0.0, 1.0, 0.0] }),
    ];
    let spline = one(
        doc,
        Command::CreateSpline {
            control_points: cps.to_vec(),
            degree: Some(3),
            knots: None,
        },
    );
    let edge = one(doc, Command::CreateEdge { curve: spline });
    let wire = one(doc, Command::CreateWire { edges: vec![edge] });
    let face = one(
        doc,
        Command::CreateFace {
            outer: wire,
            holes: vec![],
            plane: None,
        },
    );
    let line_cps = [
        one(doc, Command::CreateControlPoint { position: [0.0, 0.0, 0.0] }),
        one(doc, Command::CreateControlPoint { position: [0.0, 0.0, 3.0] }),
    ];
    let line = one(
        doc,
        Command::CreateLine {
            start: line_cps[0],
            end: line_cps[1],
        },
    );
    let extrusion = one(
        doc,
        Command::CreateExtrusion {
            profile: face,
            path: line,
        },
    );
    let material = one(
        doc,
        Command::CreateMaterial {
            name: "concrete".to_owned(),
            color: [0.7, 0.7, 0.65],
            roughness: 0.9,
        },
    );
    ok(
        doc,
        Command::UpdateFaceMaterial {
            face,
            material: Some(material),
        },
    );
    let element = one(
        doc,
        Command::CreateElement {
            name: "wall".to_owned(),
            members: vec![extrusion],
        },
    );
    let instances = [
        one(
            doc,
            Command::CreateInstance {
                element,
                transform: IDENTITY_XFORM,
            },
        ),
        one(
            doc,
            Command::CreateInstance {
                element,
                transform: translation(5.0, 0.0, 0.0),
            },
        ),
    ];
    Chain {
        cps,
        spline,
        edge,
        wire,
        face,
        line_cps,
        line,
        extrusion,
        material,
        element,
        instances,
    }
}
