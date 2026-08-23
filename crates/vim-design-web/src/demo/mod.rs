//! The interactive demo app (wasm only): owns the `Document` + `Engine`
//! pair per the facade contract (eval::mod.rs), the wgpu renderer, the
//! orbit camera, and the slider->entity registry. JS drives it: DOM
//! events call the `set_*` methods; a requestAnimationFrame loop calls
//! `render()`.

mod renderer;
mod scene;

use std::collections::{BTreeMap, BTreeSet};

use glam::{Mat4, Vec3};
use vim_design_lib::eval::Engine;
use vim_design_lib::{Command, Document, EntityId, Params, VimStatus};
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
    current: Option<String>,
}

impl Gestures {
    fn begin(&mut self, doc: &Document, name: &str) {
        if self.current.as_deref() != Some(name) {
            self.marks.push(doc.undo_depth());
            self.current = Some(name.to_owned());
        }
        // Any new command invalidates the document's redo stack.
        self.redo_counts.clear();
    }

    /// Record a one-shot operation (add/delete level, ...) that was
    /// already submitted successfully: `depth` is the undo depth
    /// captured BEFORE the submit. Used instead of `begin` when the
    /// command may be rejected — a rejected command must not leave a
    /// stray gesture mark or clear the redo counts.
    fn one_shot(&mut self, depth: usize) {
        self.marks.push(depth);
        self.redo_counts.clear();
        self.current = None;
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
    /// The dirty pump's interest filter (docs/ARCHITECTURE.md §6.3):
    /// exactly the entities the sliders derive their values from.
    ///
    /// Watch-set decision: *grow-only dynamic set*. The static slider
    /// sources are registered at startup; the chamfer entity — created
    /// and deleted at runtime — is added the moment it is created and
    /// never removed. A watched-but-dead id costs nothing (ids are not
    /// reused), and keeping it covers undo/redo replays, which
    /// re-create the same entity id behind the app's back.
    watch: BTreeSet<EntityId>,
    /// Set when a poll reported a non-empty `params_changed`; drained by
    /// [`DemoApp::take_params_dirty`] — the page's trigger to resync the
    /// slider DOM from the document.
    params_dirty: bool,
    /// The active level (docs/AUTHORING.md §5): SESSION state, never in
    /// the document, never undoable. `None` only when no levels exist.
    active_level: Option<EntityId>,
    /// Last known elevation of the active level — the fallback metric
    /// when the active level disappears (delete, undo of its creation):
    /// the nearest remaining level by elevation becomes active.
    active_elevation: f64,
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

        // Interest filter for the parametric dirty pump: the entities the
        // sliders read their values from (see the `watch` field docs).
        // Registered BEFORE the first poll so the scene-creation dirt for
        // these ids is reported, not discarded at drain time — the
        // initial slider sync then flows through the same pump path as
        // every later change.
        let mut engine = Engine::new();
        let watch: BTreeSet<EntityId> = ids
            .cube_base_cps
            .iter()
            .copied()
            .chain([
                ids.cube_top_cp,
                ids.plate_bottom_cp,
                ids.cyl_circle,
                ids.cyl_top_cp,
                ids.cone_rim_cp,
                ids.cone_apex_cp,
                // Authoring panels: the Site singleton and every level
                // (later-created levels are added dynamically, grow-only).
                ids.site,
            ])
            .chain(scene::levels_sorted(&doc).iter().map(|l| l.id))
            .collect();
        engine.set_params_watch(Some(watch.clone()));

        // Session state: the scene is authored on Ground, so it starts
        // as the active level.
        let ground = ids.ground;

        let mut app = DemoApp {
            doc,
            engine,
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
            watch,
            params_dirty: false,
            active_level: Some(ground),
            active_elevation: 0.0,
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
                            // Grow the pump's interest filter before the
                            // drain in sync() so this creation (and every
                            // later update/delete/undo replay of the same
                            // id) reports through params_changed.
                            if self.watch.insert(*chamfer) {
                                self.engine.set_params_watch(Some(self.watch.clone()));
                            }
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

    // -- Authoring: Site + Levels (docs/AUTHORING.md) ---------------------
    // Ids cross the JS boundary as f64 (they are small; f64 is exact far
    // beyond any id this app will allocate).

    /// The Site singleton's editable fields.
    pub fn site_json(&self) -> String {
        match scene::site_params(&self.doc) {
            Some((id, lat, lon, elev, north)) => serde_json::json!({
                "id": id.0 as f64,
                "latitude": lat,
                "longitude": lon,
                "elevation": elev,
                "trueNorth": north,
            })
            .to_string(),
            None => "null".to_owned(),
        }
    }

    pub fn set_site(&mut self, latitude: f64, longitude: f64, elevation: f64) {
        let Some((id, ..)) = scene::site_params(&self.doc) else {
            return;
        };
        self.gestures.begin(&self.doc, "site");
        self.submit(Command::UpdateSite {
            id,
            latitude_deg: Some(latitude),
            longitude_deg: Some(longitude),
            elevation_m: Some(elevation),
            true_north_deg: None,
            coalesce: true,
        });
        self.sync("site");
    }

    /// Level list (ascending elevation — the panel displays it top story
    /// first) plus the session's active level id.
    pub fn levels_json(&self) -> String {
        let levels: Vec<serde_json::Value> = scene::levels_sorted(&self.doc)
            .iter()
            .map(|l| {
                serde_json::json!({
                    "id": l.id.0 as f64,
                    "name": l.name,
                    "elevation": l.elevation_m,
                    "isStory": l.is_building_story,
                    "color": l.color,
                    "extent": l.extent_m,
                })
            })
            .collect();
        serde_json::json!({
            "activeId": self.active_level.map(|id| id.0 as f64),
            "levels": levels,
        })
        .to_string()
    }

    /// Select the active level — SESSION state only: no document
    /// mutation, no undo step, no pump traffic. The overlay emphasis
    /// updates immediately; the page re-renders its panel by hand (this
    /// is the one legitimate hand-placed refresh, because there is no
    /// document change to pump).
    pub fn set_active_level(&mut self, id: f64) -> bool {
        let id = EntityId(id as u64);
        if self.doc.entity(id).is_none() {
            return false;
        }
        self.active_level = Some(id);
        self.refresh_session_and_overlays();
        true
    }

    /// Create a level above the current top (+3 m), cycling the palette.
    pub fn add_level(&mut self) {
        let levels = scene::levels_sorted(&self.doc);
        let top = levels.last().map_or(0.0, |l| l.elevation_m);
        let elevation = if levels.is_empty() { 0.0 } else { top + 3.0 };
        let name = format!("Level {}", levels.len() + 1);
        let color = scene::LEVEL_COLORS[levels.len() % scene::LEVEL_COLORS.len()];
        let depth = self.doc.undo_depth();
        match self.doc.submit(Command::CreateLevel {
            name,
            elevation_m: elevation,
            is_building_story: true,
            color,
            extent_m: scene::LEVEL_EXTENT_M,
        }) {
            Ok(out) => {
                self.gestures.one_shot(depth);
                for id in out.created_ids {
                    if self.watch.insert(id) {
                        self.engine.set_params_watch(Some(self.watch.clone()));
                    }
                }
            }
            Err(status) => web_sys::console::error_1(&JsValue::from_str(&format!(
                "CreateLevel rejected: {status:?}"
            ))),
        }
        self.sync("add level");
    }

    pub fn update_level_name(&mut self, id: f64, name: String) {
        self.submit_level_update(id, "level name", Some(name), None, None, None);
    }

    pub fn update_level_elevation(&mut self, id: f64, elevation: f64) {
        self.submit_level_update(id, "level elevation", None, Some(elevation), None, None);
    }

    pub fn update_level_story(&mut self, id: f64, is_story: bool) {
        self.submit_level_update(id, "level story", None, None, Some(is_story), None);
    }

    /// Update a level's overlay color, preserving its stored alpha.
    pub fn update_level_color(&mut self, id: f64, r: f32, g: f32, b: f32) {
        let eid = EntityId(id as u64);
        let alpha = scene::levels_sorted(&self.doc)
            .iter()
            .find(|l| l.id == eid)
            .map_or(0.28, |l| l.color[3]);
        self.submit_level_update(id, "level color", None, None, None, Some([r, g, b, alpha]));
    }

    /// Non-cascade delete attempt. Returns `"deleted"`,
    /// `"has_dependents"` (the page shows the cascade confirmation), or
    /// an error string. A rejection leaves the document, the pump, and
    /// the gesture stack completely untouched.
    pub fn delete_level(&mut self, id: f64) -> String {
        let eid = EntityId(id as u64);
        let depth = self.doc.undo_depth();
        match self.doc.submit(Command::DeleteLevel { id: eid, cascade: false }) {
            Ok(_) => {
                self.gestures.one_shot(depth);
                self.sync("delete level");
                "deleted".to_owned()
            }
            Err(VimStatus::HasDependents) => "has_dependents".to_owned(),
            Err(status) => format!("error: {status:?}"),
        }
    }

    /// Confirmed cascade delete: the level plus its transitive dependent
    /// closure, ONE undo step (docs/AUTHORING.md §2).
    pub fn delete_level_cascade(&mut self, id: f64) -> bool {
        let eid = EntityId(id as u64);
        let depth = self.doc.undo_depth();
        match self.doc.submit(Command::DeleteLevel { id: eid, cascade: true }) {
            Ok(_) => {
                self.gestures.one_shot(depth);
                self.sync("delete level (cascade)");
                true
            }
            Err(status) => {
                web_sys::console::error_1(&JsValue::from_str(&format!(
                    "DeleteLevel cascade rejected: {status:?}"
                )));
                false
            }
        }
    }

    /// World AABB of the drawn scene (meshes x instances; overlays
    /// excluded) — the Playwright proof that dragging Ground moves the
    /// geometry while dragging an empty level does not.
    pub fn scene_bbox_json(&self) -> String {
        match self.renderer.scene_bbox() {
            Some((min, max)) => {
                serde_json::json!({ "min": min, "max": max }).to_string()
            }
            None => "null".to_owned(),
        }
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

    /// True (drained on read) when a poll reported watched parametric
    /// changes since the last call — the page's one trigger to resync
    /// slider DOM from the document. Fired by every mutation path alike:
    /// slider submits, undo, redo (the dirty pump is the single gate).
    pub fn take_params_dirty(&mut self) -> bool {
        std::mem::take(&mut self.params_dirty)
    }

    /// The slider parameters as currently stored in the document — the
    /// single source of truth the page resynchronizes its sliders from
    /// whenever [`DemoApp::take_params_dirty`] fires.
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
    /// Shared body of the level-field updates: one gesture per
    /// (field, level), coalesced document-side per entity. Note the
    /// document's coalesce key is (label, id), so consecutive edits to
    /// DIFFERENT fields of the same level merge into one undo step —
    /// acceptable "per-level fiddling burst" granularity for a testbed.
    fn submit_level_update(
        &mut self,
        id: f64,
        op: &str,
        name: Option<String>,
        elevation_m: Option<f64>,
        is_building_story: Option<bool>,
        color: Option<[f32; 4]>,
    ) {
        let eid = EntityId(id as u64);
        if self.doc.entity(eid).is_none() {
            return;
        }
        self.gestures.begin(&self.doc, &format!("{op}_{}", eid.0));
        self.submit(Command::UpdateLevel {
            id: eid,
            name,
            elevation_m,
            is_building_story,
            color,
            extent_m: None,
            coalesce: true,
        });
        self.sync(op);
    }

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

        // Parametric dirty pump: any watched target changed (by submit,
        // undo, or redo — one gate) flags the page to resync its slider
        // DOM from the document.
        if !updates.params_changed.is_empty() {
            self.params_dirty = true;
        }

        self.committed = updates.committed_generation;
        self.evaluated = updates.evaluated_generation;
        self.pending = updates.pending_count;

        // Session state + overlays react to any document change (levels
        // can appear/disappear via undo/redo as well as via the panel).
        self.refresh_session_and_overlays();

        self.last_latency_ms = now_ms() - t0;
        self.last_op = op.to_owned();
    }

    /// Validate the active level against the document (fallback: nearest
    /// remaining level by elevation — docs/AUTHORING.md §5) and rebuild
    /// the renderer's level-overlay quads from current Level params.
    fn refresh_session_and_overlays(&mut self) {
        let levels = scene::levels_sorted(&self.doc);

        let alive = self
            .active_level
            .filter(|id| self.doc.entity(*id).is_some());
        self.active_level = alive.or_else(|| {
            levels
                .iter()
                .min_by(|a, b| {
                    let da = (a.elevation_m - self.active_elevation).abs();
                    let db = (b.elevation_m - self.active_elevation).abs();
                    da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|l| l.id)
        });
        if let Some(active) = self.active_level {
            if let Some(info) = levels.iter().find(|l| l.id == active) {
                self.active_elevation = info.elevation_m;
            }
        }

        // Overlay quads, ascending elevation (back-to-front from the
        // usual above-the-scene camera). The active level is emphasized
        // with a brighter alpha.
        let quads: Vec<renderer::OverlayQuad> = levels
            .iter()
            .map(|l| {
                let active = Some(l.id) == self.active_level;
                // Seen nearly edge-on the squares cover much of the
                // viewport, so the resting alpha is modest; the active
                // level is emphasized with a brighter one.
                let alpha = if active {
                    (l.color[3] * 1.2).min(0.6)
                } else {
                    l.color[3] * 0.45
                };
                renderer::OverlayQuad {
                    elevation: l.elevation_m as f32,
                    extent: l.extent_m as f32,
                    color: [l.color[0], l.color[1], l.color[2], alpha],
                }
            })
            .collect();
        self.renderer.set_overlays(&quads);
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
