//! CPU picking: ray-cast against CPU copies of the polled meshes,
//! composed EXACTLY like the renderer draws them (`world = instance ∘
//! base`, base applied first; owners without instances at base alone).

use std::collections::HashMap;

use glam::{Mat4, Vec3};
use vim_design_lib::EntityId;
use vim_design_lib::eval::Mesh;

use crate::render::mat4_from_row_major_4x3;

struct PickMesh {
    positions: Vec<Vec3>,
    indices: Vec<u32>,
    base: Mat4,
    /// Local AABB for a cheap reject.
    min: Vec3,
    max: Vec3,
}

#[derive(Default)]
pub struct PickScene {
    meshes: HashMap<EntityId, PickMesh>,
    instances: HashMap<EntityId, (EntityId, Mat4)>,
}

impl PickScene {
    pub fn clear(&mut self) {
        self.meshes.clear();
        self.instances.clear();
    }

    pub fn upsert_mesh(&mut self, id: EntityId, mesh: &Mesh, base: &[f64; 12]) {
        let positions: Vec<Vec3> = mesh.positions.iter().map(|p| Vec3::from_array(*p)).collect();
        let (mut min, mut max) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
        for p in &positions {
            min = min.min(*p);
            max = max.max(*p);
        }
        self.meshes.insert(
            id,
            PickMesh {
                positions,
                indices: mesh.indices.clone(),
                base: mat4_from_row_major_4x3(base),
                min,
                max,
            },
        );
    }

    pub fn remove_mesh(&mut self, id: EntityId) {
        self.meshes.remove(&id);
    }

    pub fn set_base(&mut self, id: EntityId, base: &[f64; 12]) {
        if let Some(m) = self.meshes.get_mut(&id) {
            m.base = mat4_from_row_major_4x3(base);
        }
    }

    pub fn upsert_instance(&mut self, id: EntityId, element: EntityId, t: &[f64; 12]) {
        self.instances.insert(id, (element, mat4_from_row_major_4x3(t)));
    }

    pub fn remove_instance(&mut self, id: EntityId) {
        self.instances.remove(&id);
    }

    pub fn owner_ids(&self) -> impl Iterator<Item = EntityId> + '_ {
        self.meshes.keys().copied()
    }

    /// World placements of one owner (same rule as the renderer).
    fn placements(&self, id: EntityId, base: Mat4) -> Vec<Mat4> {
        let placed: Vec<Mat4> = self
            .instances
            .values()
            .filter(|(element, _)| *element == id)
            .map(|(_, m)| *m * base)
            .collect();
        if placed.is_empty() { vec![base] } else { placed }
    }

    /// Every owner the ray hits, nearest hit per owner, sorted by world
    /// distance (the caller breaks ties, e.g. wall over plate).
    pub fn pick_all(&self, origin: Vec3, dir: Vec3) -> Vec<(EntityId, f32)> {
        let mut hits: Vec<(EntityId, f32)> = Vec::new();
        for (id, mesh) in &self.meshes {
            let mut best: Option<f32> = None;
            for world in self.placements(*id, mesh.base) {
                let inv = world.inverse();
                let o = inv.transform_point3(origin);
                let d = inv.transform_vector3(dir);
                if !ray_hits_aabb(o, d, mesh.min, mesh.max) {
                    continue;
                }
                for tri in mesh.indices.chunks_exact(3) {
                    let (Some(a), Some(b), Some(c)) = (
                        mesh.positions.get(tri[0] as usize),
                        mesh.positions.get(tri[1] as usize),
                        mesh.positions.get(tri[2] as usize),
                    ) else {
                        continue;
                    };
                    if let Some(t) = ray_triangle(o, d, *a, *b, *c) {
                        // `t` is in local units; compare in world units.
                        let hit_world = world.transform_point3(o + d * t);
                        let dist = (hit_world - origin).length();
                        if best.is_none_or(|bd| dist < bd) {
                            best = Some(dist);
                        }
                    }
                }
            }
            if let Some(d) = best {
                hits.push((*id, d));
            }
        }
        hits.sort_by(|a, b| a.1.total_cmp(&b.1));
        hits
    }
}

fn ray_hits_aabb(o: Vec3, d: Vec3, min: Vec3, max: Vec3) -> bool {
    let mut t0 = f32::NEG_INFINITY;
    let mut t1 = f32::INFINITY;
    for axis in 0..3 {
        let (oa, da, lo, hi) = (o[axis], d[axis], min[axis] - 1e-4, max[axis] + 1e-4);
        if da.abs() < 1e-12 {
            if oa < lo || oa > hi {
                return false;
            }
        } else {
            let (a, b) = ((lo - oa) / da, (hi - oa) / da);
            t0 = t0.max(a.min(b));
            t1 = t1.min(a.max(b));
        }
    }
    t1 >= t0.max(0.0)
}

/// Möller–Trumbore; returns the ray parameter of the hit.
fn ray_triangle(o: Vec3, d: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Option<f32> {
    let e1 = b - a;
    let e2 = c - a;
    let p = d.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    let s = o - a;
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = d.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = e2.dot(q) * inv;
    (t > 0.0).then_some(t)
}
