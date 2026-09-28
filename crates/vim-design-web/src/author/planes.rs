//! Workplanes in the authoring app: construction planes nested under a
//! level or another workplane (a ceiling plane, a sill plane). Create,
//! rename, offset, recolor, and delete — each one undo step (a drag of
//! the offset coalesces) — plus their rows in the Model tree.
//!
//! Making a workplane the active plane is session state (see
//! [`AuthorApp::set_active_plane`]): drawing then happens on it, and new
//! elements are associated with its root story level.

use vim_design_lib::entity::slot;
use vim_design_lib::{Command, Document, EntityId, EntityKind, Params, VimStatus};
use wasm_bindgen::prelude::*;

use super::{AuthorApp, eid};
use crate::authoring::ops::{self, WorkplaneInfo};

/// Offset of a new workplane above a level (a ceiling plane) and above
/// another workplane (meters).
const DEFAULT_WORKPLANE_OFFSET_M: f64 = 2.4;
const DEFAULT_NESTED_WORKPLANE_OFFSET_M: f64 = 0.3;
/// Offset limits of a workplane edit (meters).
const MAX_WORKPLANE_OFFSET_M: f64 = 100.0;
/// Opacity of a new workplane's color (its outline and grid tint).
const WORKPLANE_ALPHA: f32 = 0.3;

/// A workplane's parent (a level or a workplane); `None` for a level.
pub(super) fn parent(doc: &Document, plane: EntityId) -> Option<EntityId> {
    let record = doc.entity(plane)?;
    if record.kind() != EntityKind::Workplane {
        return None;
    }
    record.inputs.get(slot::WORKPLANE_PARENT)?.referenced().next()
}

#[wasm_bindgen]
impl AuthorApp {
    /// Add a workplane under `parent` (a level or a workplane) — one undo
    /// step. It takes the parent's color and extent. Returns its id, or
    /// -1 when refused.
    pub fn add_workplane(&mut self, parent: f64) -> f64 {
        let parent = eid(parent);
        let (color, extent, is_level) = match self.doc.entity(parent).map(|e| &e.params) {
            Some(Params::Level { color, extent_m, .. }) => (*color, *extent_m, true),
            Some(Params::Workplane { color, extent_m, .. }) => (*color, *extent_m, false),
            _ => return -1.0,
        };
        let offset = if is_level { DEFAULT_WORKPLANE_OFFSET_M } else { DEFAULT_NESTED_WORKPLANE_OFFSET_M };
        let name = next_workplane_name(&self.doc);
        let depth = self.doc.undo_depth();
        match self.doc.submit(Command::CreateWorkplane {
            parent,
            name,
            offset_m: offset,
            color: [color[0], color[1], color[2], WORKPLANE_ALPHA],
            extent_m: extent,
        }) {
            Ok(created) => {
                self.gestures.one_shot(depth);
                self.sync("add workplane");
                created.created_ids.first().map_or(-1.0, |id| id.0 as f64)
            }
            Err(status) => {
                web_sys::console::error_1(&JsValue::from_str(&format!("CreateWorkplane rejected: {status:?}")));
                -1.0
            }
        }
    }

    /// Every workplane (flat, by offset), with its parent, path, and
    /// elevation.
    pub fn workplanes_json(&self) -> String {
        let rows: Vec<serde_json::Value> = ops::workplanes(&self.doc)
            .iter()
            .map(|w| self.workplane_value(w))
            .collect();
        serde_json::Value::Array(rows).to_string()
    }

    /// One workplane with what deleting it would take along (`null` when
    /// it does not exist).
    pub fn workplane_json(&self, id: f64) -> String {
        let id = eid(id);
        let Some(w) = ops::workplanes(&self.doc).into_iter().find(|w| w.id == id) else {
            return "null".to_owned();
        };
        let contents = ops::workplane_contents(&self.doc, id);
        let mut v = self.workplane_value(&w);
        if let Some(obj) = v.as_object_mut() {
            obj.insert(
                "contents".into(),
                serde_json::json!({
                    "workplanes": contents.workplanes.len().saturating_sub(1),
                    "elements": contents.elements.len(),
                    "toppedWalls": contents.topped_walls.len(),
                }),
            );
            // Where it can move: every level and every workplane but its own.
            let parents: Vec<serde_json::Value> =
                ops::workplane_parent_candidates(&self.doc, id).into_iter().map(|p| self.plane_json(p)).collect();
            obj.insert("parents".into(), parents.into());
        }
        v.to_string()
    }

    pub fn update_workplane_name(&mut self, id: f64, name: String) {
        self.submit_workplane_update(id, "workplane name", Some(name), None, None);
    }

    /// Offset from the parent plane (meters; coalesced within a gesture —
    /// the page calls [`AuthorApp::end_gesture`] on release).
    pub fn update_workplane_offset(&mut self, id: f64, offset: f64) {
        if offset.is_finite() {
            let offset = offset.clamp(-MAX_WORKPLANE_OFFSET_M, MAX_WORKPLANE_OFFSET_M);
            self.submit_workplane_update(id, "workplane offset", None, Some(offset), None);
        }
    }

    /// Recolor a workplane, preserving its stored alpha.
    pub fn update_workplane_color(&mut self, id: f64, r: f32, g: f32, b: f32) {
        let alpha = ops::workplanes(&self.doc)
            .iter()
            .find(|w| w.id == eid(id))
            .map_or(WORKPLANE_ALPHA, |w| w.color[3]);
        self.submit_workplane_update(id, "workplane color", None, None, Some([r, g, b, alpha]));
    }

    /// Move a workplane under another level or workplane at the same
    /// world elevation (its offset becomes the difference); the elements
    /// drawn on it follow the new root level. One undo step. Returns ""
    /// or why it was refused.
    pub fn move_workplane(&mut self, id: f64, parent: f64) -> String {
        let (id, parent) = (eid(id), eid(parent));
        if self::parent(&self.doc, id) == Some(parent) {
            return String::new();
        }
        let depth = self.doc.undo_depth();
        match ops::move_workplane(&mut self.doc, id, parent) {
            Ok(_) => {
                self.gestures.one_shot(depth);
                self.sync("move workplane");
                String::new()
            }
            Err(e) => {
                ops::rollback_to(&mut self.doc, depth);
                self.gestures.invalidate_redo();
                self.sync("move workplane (refused)");
                e
            }
        }
    }

    /// Plain delete: "deleted", "has_dependents" (the page shows what the
    /// cascade would take and asks), or an error string.
    pub fn delete_workplane(&mut self, id: f64) -> String {
        let depth = self.doc.undo_depth();
        match self.doc.submit(Command::DeleteWorkplane { id: eid(id) }) {
            Ok(_) => {
                self.gestures.one_shot(depth);
                self.sync("delete workplane");
                "deleted".to_owned()
            }
            Err(VimStatus::HasDependents) => "has_dependents".to_owned(),
            Err(status) => format!("error: {status:?}"),
        }
    }

    /// Confirmed cascade delete (the library's `DeleteWorkplaneCascade`):
    /// the workplane, its nested workplanes, and what stands on them;
    /// walls that only go up to them are disconnected and keep their
    /// current height — ONE undo step.
    pub fn delete_workplane_cascade(&mut self, id: f64) -> bool {
        let depth = self.doc.undo_depth();
        match self.doc.submit(Command::DeleteWorkplaneCascade { id: eid(id) }) {
            Ok(_) => {
                self.gestures.one_shot(depth);
                self.sync("delete workplane (cascade)");
                true
            }
            Err(status) => {
                web_sys::console::error_1(&JsValue::from_str(&format!("DeleteWorkplaneCascade rejected: {status:?}")));
                false
            }
        }
    }
}

impl AuthorApp {
    /// Tree rows of the workplanes under `parent`, nested.
    pub(super) fn plane_rows(&self, all: &[WorkplaneInfo], parent: EntityId) -> Vec<serde_json::Value> {
        all.iter()
            .filter(|w| w.parent == parent)
            .map(|w| {
                let mut v = self.workplane_value(w);
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("planes".into(), self.plane_rows(all, w.id).into());
                }
                v
            })
            .collect()
    }

    fn workplane_value(&self, w: &WorkplaneInfo) -> serde_json::Value {
        let mut v = self.plane_json(w.id);
        if let Some(obj) = v.as_object_mut() {
            obj.insert("offset".into(), w.offset_m.into());
            obj.insert("color".into(), serde_json::json!(w.color));
            obj.insert("extent".into(), w.extent_m.into());
            obj.insert("parent".into(), (w.parent.0 as f64).into());
            obj.insert("root".into(), self.root_level(w.id).map(|r| r.0 as f64).into());
            obj.insert("parentName".into(), self.plane_name(w.parent).into());
            obj.insert("active".into(), (self.plane() == Some(w.id)).into());
        }
        v
    }

    fn submit_workplane_update(
        &mut self,
        id: f64,
        op: &str,
        name: Option<String>,
        offset_m: Option<f64>,
        color: Option<[f32; 4]>,
    ) {
        let id = eid(id);
        if self.doc.entity(id).map(|e| e.kind()) != Some(EntityKind::Workplane) {
            return;
        }
        let coalesce = self.gestures.begin_continuing(&self.doc, &format!("{op}_{}", id.0));
        self.submit(Command::UpdateWorkplane { id, parent: None, name, offset_m, color, extent_m: None, coalesce });
        self.sync(op);
    }
}

/// Next free "Workplane N" name, derived from the document.
fn next_workplane_name(doc: &Document) -> String {
    let max = doc
        .entities()
        .filter_map(|(_, r)| match &r.params {
            Params::Workplane { name, .. } => name.strip_prefix("Workplane").and_then(|n| n.trim().parse::<u64>().ok()),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    format!("Workplane {}", max + 1)
}
