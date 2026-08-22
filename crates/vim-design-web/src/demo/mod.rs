//! The interactive demo app (wasm only): owns the `Document` + `Engine`
//! pair per the facade contract (eval::mod.rs), the wgpu renderer, the
//! orbit camera, and the slider->entity registry. JS drives it: DOM
//! events call the `set_*` methods; a requestAnimationFrame loop calls
//! `render()`.

mod renderer;
mod scene;

use std::collections::BTreeMap;

use glam::{Mat4, Vec3};
use vim_design_lib::eval::Engine;
use vim_design_lib::{Command, Document, EntityId, Params};
use wasm_bindgen::prelude::*;

use renderer::{Renderer, DEFAULT_COLOR};
use scene::SceneIds;

/// Orbit camera: right-handed, Z-up (docs/ARCHITECTURE.md §7).
struct Camera {
    target: Vec3,
    yaw: f32,
    pitch: f32,
    distance: f32,
}

impl Camera {
    fn view_proj(&self, aspect: f32) -> Mat4 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        let eye = self.target + self.distance * Vec3::new(cp * cy, cp * sy, sp);
        // wgpu clip space: right-handed view, depth 0..1 ("directx" in
        // glam's naming).
        let view = glam::camera::rh::view::look_at_mat4(eye, self.target, Vec3::Z);
        let proj =
            glam::camera::rh::proj::directx::perspective(45f32.to_radians(), aspect, 0.05, 200.0);
        proj * view
    }
}

/// App-level slider-gesture grouping for undo/redo. The document already
/// coalesces consecutive updates per entity; multi-entity sliders (the
/// cube touches 5 control points per event) still produce many undo
/// steps per drag, so the app records the undo depth at each gesture
/// start and undoes/redoes whole gestures.
#[derive(Default)]
struct Gestures {
    /// Undo depths recorded at the start of each gesture.
    marks: Vec<usize>,
    /// Step counts popped by `undo`, consumed by `redo`.
    redo_counts: Vec<usize>,
    current: Option<&'static str>,
}

impl Gestures {
    fn begin(&mut self, doc: &Document, name: &'static str) {
        if self.current != Some(name) {
            self.marks.push(doc.undo_depth());
            self.current = Some(name);
        }
        // Any new command invalidates the document's redo stack.
        self.redo_counts.clear();
    }

    fn undo(&mut self, doc: &mut Document) -> bool {
        let depth = doc.undo_depth();
        while self.marks.last().is_some_and(|m| *m >= depth) {
            self.marks.pop();
        }
        let Some(mark) = self.marks.pop() else {
            return false;
        };
        let steps = depth - mark;
        for _ in 0..steps {
            if doc.undo().is_err() {
                break;
            }
        }
        self.redo_counts.push(steps);
        self.current = None;
        true
    }

    fn redo(&mut self, doc: &mut Document) -> bool {
        let Some(steps) = self.redo_counts.pop() else {
            return false;
        };
        let mark = doc.undo_depth();
        for _ in 0..steps {
            if doc.redo().is_err() {
                break;
            }
        }
        self.marks.push(mark);
        self.current = None;
        true
    }

    fn can_undo(&self) -> bool {
        !self.marks.is_empty()
    }

    fn can_redo(&self) -> bool {
        !self.redo_counts.is_empty()
    }
}

#[wasm_bindgen]
pub struct DemoApp {
    doc: Document,
    engine: Engine,
    ids: SceneIds,
    renderer: Renderer,
    camera: Camera,
    gestures: Gestures,
    errors: BTreeMap<EntityId, String>,
    last_op: String,
    /// Commit -> mesh-ready latency of the last operation, milliseconds
    /// (evaluate_pending + poll_updates + GPU upload).
    last_latency_ms: f64,
    committed: u64,
    evaluated: u64,
    pending: usize,
}

fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|w| w.performance())
        .map(|p| p.now())
        .unwrap_or(0.0)
}

#[wasm_bindgen]
impl DemoApp {
    /// Build the scene, initialize wgpu on the given canvas, and run the
    /// initial evaluation.
    pub async fn create(canvas_id: String) -> Result<DemoApp, JsValue> {
        let document = web_sys::window()
            .and_then(|w| w.document())
            .ok_or_else(|| JsValue::from_str("no DOM document"))?;
        let canvas = document
            .get_element_by_id(&canvas_id)
            .ok_or_else(|| JsValue::from_str("canvas not found"))?
            .dyn_into::<web_sys::HtmlCanvasElement>()?;

        let renderer = Renderer::new(canvas).await.map_err(|e| JsValue::from_str(&e))?;

        let mut doc = Document::new();
        let ids = scene::build_scene(&mut doc).map_err(|e| JsValue::from_str(&e))?;

        let mut app = DemoApp {
            doc,
            engine: Engine::new(),
            ids,
            renderer,
            camera: Camera {
                target: Vec3::new(0.0, 0.0, 0.6),
                yaw: -125f32.to_radians(),
                pitch: 27f32.to_radians(),
                distance: 9.0,
            },
            gestures: Gestures::default(),
            errors: BTreeMap::new(),
            last_op: "initial scene".to_owned(),
            last_latency_ms: 0.0,
            committed: 0,
            evaluated: 0,
            pending: 0,
        };
        app.sync("initial scene");
        Ok(app)
    }

    // -- Frame loop -----------------------------------------------------

    pub fn render(&mut self) -> Result<(), JsValue> {
        let view_proj = self.camera.view_proj(self.renderer.aspect());
        self.renderer.render(view_proj).map_err(|e| JsValue::from_str(&e))
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.renderer.resize(width, height);
    }

    // -- Camera ---------------------------------------------------------

    pub fn orbit(&mut self, dx: f32, dy: f32) {
        self.camera.yaw -= dx * 0.008;
        self.camera.pitch = (self.camera.pitch + dy * 0.008)
            .clamp(-1.45, 1.45);
    }

    pub fn zoom(&mut self, delta: f32) {
        self.camera.distance = (self.camera.distance * (1.0 + delta * 0.001)).clamp(1.5, 60.0);
    }

    // -- Display options --------------------------------------------------

    pub fn set_wireframe(&mut self, enabled: bool) {
        self.renderer.wireframe = enabled;
    }

    pub fn wireframe(&self) -> bool {
        self.renderer.wireframe
    }

    // -- Sliders ----------------------------------------------------------

    pub fn set_cube_size(&mut self, size: f64) {
        self.gestures.begin(&self.doc, "cube_size");
        for cmd in scene::cube_size_commands(&self.ids, size) {
            self.submit(cmd);
        }
        self.sync("cube size");
    }

    /// Cube top-rim chamfer distance. 0 means "no chamfer": the entity
    /// is created on the first movement above zero, updated (coalesced)
    /// while dragging, and deleted when the slider returns to zero.
    ///
    /// Ownership: a chamfer replaces its target as render shape. Because
    /// the cube's extrusion is consumed by the cube *element*, the
    /// element's members slot is swapped to the chamfer on create (and
    /// back on delete) so the instance draws the chamfered solid —
    /// otherwise the chamfer would surface as a standalone mesh at
    /// identity while the element kept the raw extrusion. An excessive
    /// distance (>= half the cube size) is a typed per-entity eval
    /// error: the previous good mesh is retained and the error surfaces
    /// in the status line until the parameters are fixed.
    pub fn set_cube_chamfer(&mut self, distance: f64) {
        let existing = scene::find_cube_chamfer(&self.doc, &self.ids);
        if existing.is_none() && distance <= 0.0 {
            return; // nothing to create, nothing to delete
        }
        self.gestures.begin(&self.doc, "cube_chamfer");
        match (existing, distance > 0.0) {
            (Some((id, _)), true) => self.submit(Command::UpdateChamfer {
                id,
                distance: Some(distance),
                target: None,
                edges: None,
                sub_edges: None,
                coalesce: true,
            }),
            (Some((id, _)), false) => {
                // Hand the element back its extrusion first — deleting a
                // chamfer that is still a member would be rejected
                // (reject-if-dependents).
                self.submit(Command::UpdateElement {
                    id: self.ids.cube_element,
                    name: None,
                    members: Some(vec![self.ids.cube_extrusion]),
                    coalesce: false,
                });
                self.submit(Command::DeleteChamfer { id });
            }
            (None, true) => {
                let output = self.doc.submit(Command::CreateChamfer {
                    target: self.ids.cube_extrusion,
                    distance,
                    edges: vec![],
                    sub_edges: scene::cube_chamfer_sub_edges(&self.ids),
                });
                match output {
                    Ok(out) => {
                        if let [chamfer] = out.created_ids.as_slice() {
                            self.submit(Command::UpdateElement {
                                id: self.ids.cube_element,
                                name: None,
                                members: Some(vec![*chamfer]),
                                coalesce: false,
                            });
                        }
                    }
                    Err(status) => web_sys::console::error_1(&JsValue::from_str(
                        &format!("CreateChamfer rejected: {status:?}"),
                    )),
                }
            }
            (None, false) => {} // early-returned above; keep never-crash

        }
        self.sync("cube chamfer");
    }

    pub fn set_plate_thickness(&mut self, thickness: f64) {
        self.gestures.begin(&self.doc, "plate_thickness");
        let cmd = scene::plate_thickness_command(&self.ids, thickness);
        self.submit(cmd);
        self.sync("plate thickness");
    }

    pub fn set_cylinder_radius(&mut self, radius: f64) {
        self.gestures.begin(&self.doc, "cyl_radius");
        let cmd = scene::cylinder_radius_command(&self.ids, radius);
        self.submit(cmd);
        self.sync("cylinder radius");
    }

    pub fn set_cylinder_height(&mut self, height: f64) {
        self.gestures.begin(&self.doc, "cyl_height");
        let cmd = scene::cylinder_height_command(&self.ids, height);
        self.submit(cmd);
        self.sync("cylinder height");
    }

    pub fn set_cone_radius(&mut self, radius: f64) {
        self.gestures.begin(&self.doc, "cone_radius");
        let cmd = scene::cone_radius_command(&self.ids, radius);
        self.submit(cmd);
        self.sync("cone radius");
    }

    pub fn set_cone_height(&mut self, height: f64) {
        self.gestures.begin(&self.doc, "cone_height");
        let cmd = scene::cone_height_command(&self.ids, height);
        self.submit(cmd);
        self.sync("cone height");
    }

    // -- Undo / redo ------------------------------------------------------

    pub fn undo(&mut self) -> bool {
        if self.gestures.undo(&mut self.doc) {
            self.sync("undo");
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self) -> bool {
        if self.gestures.redo(&mut self.doc) {
            self.sync("redo");
            true
        } else {
            false
        }
    }

    pub fn can_undo(&self) -> bool {
        self.gestures.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.gestures.can_redo()
    }

    /// The six slider parameters as currently stored in the document —
    /// the single source of truth the page resynchronizes its sliders
    /// from (at startup and after undo/redo).
    pub fn params_json(&self) -> String {
        let p = scene::current_params(&self.doc, &self.ids);
        serde_json::json!({
            "cubeSize": p.cube_size,
            "cubeChamfer": p.cube_chamfer,
            "plateThickness": p.plate_thickness,
            "cylRadius": p.cyl_radius,
            "cylHeight": p.cyl_height,
            "coneRadius": p.cone_radius,
            "coneHeight": p.cone_height,
        })
        .to_string()
    }

    // -- Status -----------------------------------------------------------

    /// JSON status blob for the page's status line and the Playwright
    /// settledness checks.
    pub fn stats_json(&self) -> String {
        let errors: Vec<String> = self
            .errors
            .iter()
            .map(|(id, msg)| format!("#{}: {}", id.0, msg))
            .collect();
        serde_json::json!({
            "backend": self.renderer.backend_name(),
            "committed": self.committed,
            "evaluated": self.evaluated,
            "pending": self.pending,
            "settled": self.committed == self.evaluated && self.pending == 0,
            "triangles": self.renderer.drawn_triangle_count(),
            "lastOp": self.last_op,
            "lastLatencyMs": self.last_latency_ms,
            "wireframe": self.renderer.wireframe,
            "canUndo": self.can_undo(),
            "canRedo": self.can_redo(),
            "errors": errors,
        })
        .to_string()
    }
}

impl DemoApp {
    fn submit(&mut self, cmd: Command) {
        let label = cmd.label();
        if let Err(status) = self.doc.submit(cmd) {
            web_sys::console::error_1(&JsValue::from_str(&format!(
                "{label} rejected: {status:?}"
            )));
        }
    }

    /// The facade drive cycle: evaluate the dirty closure, poll the
    /// changed-set, and apply it to the GPU (removals before upserts).
    /// Timed as the commit -> mesh-ready latency shown in the status line.
    fn sync(&mut self, op: &str) {
        let t0 = now_ms();
        self.engine.evaluate_pending(&mut self.doc);
        let updates = self.engine.poll_updates(&self.doc);

        for id in &updates.meshes_removed {
            self.renderer.remove_mesh(*id);
        }
        for id in &updates.instances_removed {
            self.renderer.remove_instance(*id);
        }
        for mu in &updates.meshes {
            let colors: Vec<[f32; 3]> = mu
                .mesh
                .submeshes
                .iter()
                .map(|sub| self.material_color(sub.material))
                .collect();
            self.renderer.upsert_mesh(mu.id, &mu.mesh, &colors);
        }
        for iu in &updates.instances {
            self.renderer.upsert_instance(iu.id, iu.element_id, &iu.transform);
        }
        for (id, diag) in &updates.errors {
            self.errors.insert(*id, diag.to_string());
        }
        for id in &updates.errors_cleared {
            self.errors.remove(id);
        }

        self.committed = updates.committed_generation;
        self.evaluated = updates.evaluated_generation;
        self.pending = updates.pending_count;
        self.last_latency_ms = now_ms() - t0;
        self.last_op = op.to_owned();
    }

    fn material_color(&self, material: Option<EntityId>) -> [f32; 3] {
        let Some(id) = material else {
            return DEFAULT_COLOR;
        };
        match self.doc.entity(id).map(|e| &e.params) {
            Some(Params::Material { color, .. }) => {
                [color[0] as f32, color[1] as f32, color[2] as f32]
            }
            _ => DEFAULT_COLOR,
        }
    }
}
