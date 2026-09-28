//! Copy / Paste (the buttons and Ctrl/Cmd+C / V). The copy is session
//! state (`authoring::clipboard::Clip`); a paste ARMS a placement in the
//! mode the copy belongs to: the copy follows the pointer as a preview
//! and each tap places one (one undo step per paste). It stays armed
//! until Esc, another tool, or leaving the mode.

use std::collections::BTreeSet;

use glam::Vec3;
use wasm_bindgen::prelude::*;

use super::edit::EditTarget;
use super::{AuthorApp, MIN_WALL_HEIGHT_M, Tool};
use crate::authoring::clipboard::{self, Clip, moved, moved_run, moved_sketch, paste_delta};
use crate::authoring::edit::{Edit, ProfileModel};
use crate::authoring::edit::interact::SelectMode;
use crate::authoring::geom::P2;
use crate::authoring::model::ElementModel;
use crate::authoring::ops;

fn none(reason: &str) -> String {
    serde_json::json!({ "result": "none", "reason": reason }).to_string()
}

fn rejected(reason: &str) -> String {
    serde_json::json!({ "result": "rejected", "reason": reason }).to_string()
}

#[wasm_bindgen]
impl AuthorApp {
    /// Copy the selection of the current mode: faces (floor Edit Mode),
    /// the selected opening (Openings mode), or the selected floor plate
    /// or wall run (Select). JSON `{"result": "copied", "label"}` or
    /// `{"result": "none", "reason"}`.
    pub fn copy(&mut self) -> String {
        let clip = match self.paste_context() {
            Some("faces") => self.copy_faces(),
            Some("openings") => self.copy_openings(),
            Some("element") => self.copy_element(),
            _ => Err("Nothing to copy here".to_owned()),
        };
        match clip {
            Ok(clip) => {
                let label = clip.label();
                self.clipboard = Some(clip);
                self.paste_armed = false;
                serde_json::json!({ "result": "copied", "label": label }).to_string()
            }
            Err(reason) => none(&reason),
        }
    }

    /// Arm a paste: the copy follows the pointer; a tap places it.
    pub fn paste(&mut self) -> String {
        let Some(clip) = &self.clipboard else { return none("Copy something first") };
        if self.paste_context() != Some(clip.context()) {
            let what = match clip.context() {
                "faces" => "Faces paste in a floor's Edit Mode",
                "openings" => "Openings paste in Openings mode",
                _ => "Elements paste with the Select tool",
            };
            return none(what);
        }
        self.paste_armed = true;
        serde_json::json!({ "result": "armed", "label": clip.label() }).to_string()
    }

    pub fn paste_cancel(&mut self) {
        self.paste_armed = false;
    }

    /// A paste is armed in the current mode.
    pub fn paste_armed(&self) -> bool {
        self.paste_armed && self.clipboard.as_ref().is_some_and(|c| self.paste_context() == Some(c.context()))
    }

    /// Copy / paste availability for the page: `canCopy`, `canPaste`,
    /// `armed`, the copy's `label` and `context`.
    pub fn clipboard_json(&self) -> String {
        let context = self.paste_context();
        let can_copy = match context {
            Some("faces") => self.edit.as_ref().is_some_and(|s| s.mode == SelectMode::Faces && !s.selection.faces.is_empty()),
            Some("openings") => self.openings.as_ref().is_some_and(|o| o.selected.is_some()),
            Some("element") => self.selection.is_some_and(|id| {
                self.model
                    .iter()
                    .any(|e| e.element() == id && matches!(e, ElementModel::SketchPlate(_) | ElementModel::Run(_)))
            }),
            _ => false,
        };
        serde_json::json!({
            "context": context,
            "canCopy": can_copy,
            "canPaste": self.clipboard.as_ref().is_some_and(|c| context == Some(c.context())),
            "armed": self.paste_armed(),
            "label": self.clipboard.as_ref().map(Clip::label),
            "clip": self.clipboard.as_ref().map(Clip::context),
        })
        .to_string()
    }

    /// Place the armed copy at a canvas point (device px). Stays armed.
    /// JSON `{"result": "placed", "name"}` or `{"result": "rejected",
    /// "reason"}`.
    pub fn paste_place(&mut self, px: f32, py: f32, wall_tol: f32) -> String {
        if !self.paste_armed() {
            return none("No paste is armed");
        }
        let Some(clip) = self.clipboard.clone() else { return none("Copy something first") };
        match &clip {
            Clip::Faces(faces) => self.paste_faces(faces, px, py),
            Clip::Openings(copies) => match self.paste_openings(copies, px, py, wall_tol) {
                Ok(_) => serde_json::json!({ "result": "placed", "name": clip.label() }).to_string(),
                Err(e) => rejected(&e),
            },
            Clip::Plate { .. } | Clip::Run { .. } => self.paste_element(&clip, px, py),
        }
    }

    /// The paste preview at a canvas point: outlines in device pixels
    /// (`items: [{pts, closed}]`) and whether a tap there would place it
    /// (`ok`).
    pub fn paste_ghost_json(&self, px: f32, py: f32, wall_tol: f32) -> String {
        let empty = r#"{"items":[],"ok":false}"#.to_owned();
        if !self.paste_armed() {
            return empty;
        }
        let Some(clip) = &self.clipboard else { return empty };
        let items: Vec<(Vec<[f32; 2]>, bool)> = match clip {
            Clip::Openings(copies) => match self.openings_preview(copies, px, py, wall_tol) {
                Some(polys) => polys.into_iter().map(|p| (p, true)).collect(),
                None => return empty,
            },
            _ => {
                let Some((z, d)) = self.paste_move(clip, px, py) else { return empty };
                let (w, h) = self.size_f();
                clip.outlines()
                    .iter()
                    .map(|(outline, closed)| {
                        let pts = moved(outline, d)
                            .iter()
                            .filter_map(|p| self.camera.project(Vec3::new(p[0] as f32, p[1] as f32, z), w, h))
                            .map(|(x, y)| [x, y])
                            .collect();
                        (pts, *closed)
                    })
                    .collect()
            }
        };
        let items: Vec<serde_json::Value> =
            items.into_iter().map(|(pts, closed)| serde_json::json!({ "pts": pts, "closed": closed })).collect();
        serde_json::json!({ "ok": !items.is_empty(), "items": items }).to_string()
    }
}

impl AuthorApp {
    /// Which kind of copy the current mode works with.
    pub(super) fn paste_context(&self) -> Option<&'static str> {
        if self.edit.is_some() {
            return (self.edit_target == EditTarget::Plane).then_some("faces");
        }
        if self.openings.is_some() {
            return Some("openings");
        }
        (self.tool == Tool::Select).then_some("element")
    }

    fn copy_faces(&self) -> Result<Clip, String> {
        let s = self.edit.as_ref().ok_or("Nothing to copy here")?;
        let view = s.model.view();
        let faces: Vec<_> = view
            .faces
            .iter()
            .filter(|f| s.selection.faces.contains(&f.id))
            .map(|f| (view.polygon(f), f.kind))
            .collect();
        if faces.is_empty() {
            return Err("Select faces to copy".to_owned());
        }
        Ok(Clip::Faces(faces))
    }

    fn copy_openings(&self) -> Result<Clip, String> {
        let (element, id) = self.openings.as_ref().and_then(|o| o.selected).ok_or("Select an opening to copy")?;
        let r = self.run_model(element).ok_or("Select an opening to copy")?;
        let o = r.data.openings.iter().find(|o| o.id == id).ok_or("Select an opening to copy")?;
        Ok(Clip::Openings(vec![*o]))
    }

    fn copy_element(&self) -> Result<Clip, String> {
        let id = self.selection.ok_or("Select a floor plate or a wall to copy")?;
        match self.model.iter().find(|e| e.element() == id) {
            Some(ElementModel::SketchPlate(p)) => Ok(Clip::Plate { sketch: p.sketch.clone(), name: p.name.clone() }),
            Some(ElementModel::Run(r)) => Ok(Clip::Run {
                data: r.data.clone(),
                top: r.top,
                base: r.base,
                top_height: r.top_height,
                name: r.name.clone(),
            }),
            _ => Err("Copy works on floor plates and walls (edit an older wall once to copy it)".to_owned()),
        }
    }

    /// Where a copy lands: the elevation of its plane and the move that
    /// puts its anchor under the pointer (snapped to whole grid steps).
    fn paste_move(&self, clip: &Clip, px: f32, py: f32) -> Option<(f32, P2)> {
        let anchor = clip.anchor()?;
        let (w, h) = self.size_f();
        let (z, uv) = match clip {
            Clip::Faces(_) => {
                let frame = self.edit_frame();
                let hit = self.camera.unproject_to(px, py, w, h, frame.origin, frame.normal())?;
                (frame.origin.z, frame.local(hit))
            }
            _ => {
                let z = self.plane_elevation(self.plane()?) as f32;
                let hit = self.camera.unproject_to_plane(px, py, w, h, z)?;
                (z, [f64::from(hit.x), f64::from(hit.y)])
            }
        };
        let step = self.snap_enabled.then_some(self.snap_step);
        Some((z, paste_delta(anchor, uv, step)))
    }

    fn paste_faces(&mut self, faces: &[(Vec<P2>, crate::authoring::edit::FaceKind)], px: f32, py: f32) -> String {
        let clip = Clip::Faces(faces.to_vec());
        let Some((_, d)) = self.paste_move(&clip, px, py) else { return none("Point at the plane to paste") };
        let Some(s) = self.edit.as_ref() else { return none("Nothing to paste into") };
        let before: BTreeSet<u32> = s.model.view().faces.iter().map(|f| f.id).collect();
        let edit = Edit::AddFaces(faces.iter().map(|(o, k)| (moved(o, d), *k)).collect());
        let out = self.edit_apply(&edit, None, "placed");
        if out.contains(r#""result":"placed""#) {
            self.select_new_faces(&before);
        }
        out
    }

    fn paste_element(&mut self, clip: &Clip, px: f32, py: f32) -> String {
        let Some(plane) = self.plane().filter(|_| self.can_author()) else { return none("Nothing to paste into") };
        let Some((_, d)) = self.paste_move(clip, px, py) else { return none("Point at the plane to paste") };
        let level = self.root_level(plane).unwrap_or(plane);
        let names: Vec<String> = ops::element_names(&self.doc);
        let depth = self.doc.undo_depth();
        let result = match clip {
            Clip::Plate { sketch, name } => {
                let name = clipboard::copy_name(name, names.iter().map(String::as_str));
                ops::create_sketch_element(&mut self.doc, plane, level, &moved_sketch(sketch, d), &name).map(|(e, _)| (e, name))
            }
            Clip::Run { data, top, base, top_height, name } => {
                let name = clipboard::copy_name(name, names.iter().map(String::as_str));
                let mut data = moved_run(data, d);
                // Keep the height mode when the top is still above the new
                // base; otherwise a fixed height, as tall as the original.
                let reach = top.map(|t| self.plane_elevation(t) + data.top_offset_m - self.plane_elevation(plane));
                let top = if *base == plane || reach.is_some_and(|r| r >= MIN_WALL_HEIGHT_M) { *top } else { None };
                if top.is_none() {
                    data.height_m = *top_height;
                }
                ops::create_run_element(&mut self.doc, plane, level, top, data, &name).map(|e| (e, name))
            }
            _ => Err("not an element".to_owned()),
        };
        match result {
            Ok((element, name)) => {
                self.gestures.one_shot(depth);
                self.sync("paste");
                self.selection = Some(element);
                self.refresh_styles();
                self.notice = Some(format!("{name} pasted"));
                serde_json::json!({ "result": "placed", "name": name, "element": element.0 as f64 }).to_string()
            }
            Err(e) => {
                ops::rollback_to(&mut self.doc, depth);
                self.gestures.invalidate_redo();
                self.sync("paste failed");
                rejected(&e)
            }
        }
    }
}
