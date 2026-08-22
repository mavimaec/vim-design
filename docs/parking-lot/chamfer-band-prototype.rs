//! Profile-offset-band prototype: chamfer/round the ENTIRE rim of a planar
//! cap on a prismatic solid by lofting inward polygon offsets, built as a
//! watertight monstertruck Shell with shared vertices/edges.

use monstertruck_meshing::prelude::*;
use monstertruck_modeling::{builder, Edge, Face, Point3, Shell, Solid, Vertex, Wire};
use std::collections::HashMap;

type P2 = [f64; 2];

/// Loop stored with material on the LEFT of travel:
/// outer loops CCW, hole loops CW (viewed from +z).
#[derive(Clone)]
struct Loop(Vec<P2>);

/// Inward (into-material) miter offset by t: shift each edge along its left
/// normal, new vertices at consecutive line intersections.
fn miter_offset(l: &Loop, t: f64) -> Option<Loop> {
    let n = l.0.len();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        // vertex i is the meet of edge (i-1 -> i) and edge (i -> i+1)
        let p_prev = l.0[(i + n - 1) % n];
        let p = l.0[i];
        let p_next = l.0[(i + 1) % n];
        let d0 = norm2([p[0] - p_prev[0], p[1] - p_prev[1]])?;
        let d1 = norm2([p_next[0] - p[0], p_next[1] - p[1]])?;
        let n0 = [-d0[1], d0[0]]; // left normal
        let n1 = [-d1[1], d1[0]];
        // intersect: p_prev + t*n0 + a*d0 = p + t*n1 + b*d1  (solve on lines)
        // line0: point q0 = p + t*n0, dir d0 ; line1: q1 = p + t*n1, dir d1
        let q0 = [p[0] + t * n0[0], p[1] + t * n0[1]];
        let q1 = [p[0] + t * n1[0], p[1] + t * n1[1]];
        let det = d0[0] * (-d1[1]) - d0[1] * (-d1[0]);
        if det.abs() < 1e-12 {
            // collinear edges: just shift
            out.push(q0);
        } else {
            let rhs = [q1[0] - q0[0], q1[1] - q0[1]];
            let a = (rhs[0] * (-d1[1]) - rhs[1] * (-d1[0])) / det;
            out.push([q0[0] + a * d0[0], q0[1] + a * d0[1]]);
        }
    }
    Some(Loop(out))
}

fn norm2(v: P2) -> Option<P2> {
    let l = (v[0] * v[0] + v[1] * v[1]).sqrt();
    (l > 1e-12).then(|| [v[0] / l, v[1] / l])
}

fn shoelace(l: &Loop) -> f64 {
    let n = l.0.len();
    let mut s = 0.0;
    for i in 0..n {
        let a = l.0[i];
        let b = l.0[(i + 1) % n];
        s += a[0] * b[1] - b[0] * a[1];
    }
    s / 2.0
}

/// Profile sample: (inset u, height z). Ring k of a loop = miter_offset(u_k) at z_k.
struct ProfileSamples(Vec<(f64, f64)>);

impl ProfileSamples {
    fn chamfer(d: f64, z_top: f64) -> Self {
        ProfileSamples(vec![(0.0, z_top - d), (d, z_top)])
    }
    /// Quarter-arc fillet-like profile, N segments.
    fn quarter_arc(r: f64, z_top: f64, n: usize) -> Self {
        ProfileSamples(
            (0..=n)
                .map(|k| {
                    let phi = std::f64::consts::FRAC_PI_2 * k as f64 / n as f64;
                    (r * (1.0 - phi.cos()), z_top - r + phi.sin() * r)
                })
                .collect(),
        )
    }
}

struct ShellBuilder {
    verts: Vec<Vertex>,
    edges: HashMap<(usize, usize), Edge>,
    faces: Vec<Face>,
}

impl ShellBuilder {
    fn new() -> Self {
        Self { verts: Vec::new(), edges: HashMap::new(), faces: Vec::new() }
    }
    fn vert(&mut self, p: [f64; 3]) -> usize {
        self.verts.push(builder::vertex(Point3::new(p[0], p[1], p[2])));
        self.verts.len() - 1
    }
    fn edge(&mut self, a: usize, b: usize) -> Edge {
        if let Some(e) = self.edges.get(&(a, b)) {
            return e.clone();
        }
        if let Some(e) = self.edges.get(&(b, a)) {
            return e.inverse();
        }
        let e = builder::line(&self.verts[a], &self.verts[b]);
        self.edges.insert((a, b), e.clone());
        e
    }
    fn face(&mut self, rings: &[&[usize]]) -> Result<(), String> {
        let wires: Vec<Wire> = rings
            .iter()
            .map(|ring| {
                (0..ring.len())
                    .map(|i| self.edge(ring[i], ring[(i + 1) % ring.len()]))
                    .collect::<Vec<Edge>>()
                    .into()
            })
            .collect();
        let f = builder::try_attach_plane(wires).map_err(|e| format!("{e:?}"))?;
        self.faces.push(f);
        Ok(())
    }
}

/// Build a prism (loops at z=0..z_top, vertical sides) with the entire top rim
/// blended by the given profile. Returns the watertight Solid.
fn build_banded_prism(loops: &[Loop], z_top: f64, profile: &ProfileSamples) -> Result<Solid, String> {
    let nk = profile.0.len(); // rings per loop (k = 0..nk-1)
    let mut b = ShellBuilder::new();

    // Ring polygons in 2D per loop per k (offset may fail -> error).
    let rings2d: Vec<Vec<Loop>> = loops
        .iter()
        .map(|l| {
            profile
                .0
                .iter()
                .map(|(u, _)| miter_offset(l, *u).ok_or_else(|| "offset failed".to_string()))
                .collect::<Result<Vec<_>, _>>()
        })
        .collect::<Result<Vec<_>, _>>()?;
    // sanity: offsets must stay simple & consistently oriented (no collapse).
    // Robust check: every offset edge must keep its original direction
    // (dot > 0); an edge that shrinks through zero length has passed a
    // straight-skeleton event for this offset distance.
    for (li, rl) in rings2d.iter().enumerate() {
        let orig = &loops[li].0;
        let n = orig.len();
        let s0 = shoelace(&rl[0]).signum();
        for r in rl {
            if shoelace(r).signum() != s0 {
                return Err(format!(
                    "loop {li}: offset inverted (straight-skeleton event)"
                ));
            }
            for j in 0..n {
                let j1 = (j + 1) % n;
                let od = [orig[j1][0] - orig[j][0], orig[j1][1] - orig[j][1]];
                let nd = [r.0[j1][0] - r.0[j][0], r.0[j1][1] - r.0[j][1]];
                if od[0] * nd[0] + od[1] * nd[1] <= 0.0 {
                    return Err(format!(
                        "loop {li} edge {j}: offset edge collapsed/reversed \
                         (straight-skeleton event before requested distance)"
                    ));
                }
            }
        }
    }

    // Vertex indices: base ring (z=0) and profile rings per loop.
    let base: Vec<Vec<usize>> = loops
        .iter()
        .map(|l| l.0.iter().map(|p| b.vert([p[0], p[1], 0.0])).collect())
        .collect();
    let rings: Vec<Vec<Vec<usize>>> = rings2d
        .iter()
        .map(|rl| {
            rl.iter()
                .zip(profile.0.iter())
                .map(|(ring, (_, z))| ring.0.iter().map(|p| b.vert([p[0], p[1], *z])).collect())
                .collect()
        })
        .collect();

    // Bottom cap: all loops reversed.
    let bot: Vec<Vec<usize>> = base
        .iter()
        .map(|ring| ring.iter().rev().copied().collect())
        .collect();
    b.face(&bot.iter().map(|r| r.as_slice()).collect::<Vec<_>>())?;

    // Side + band quads per loop edge.
    for (li, l) in loops.iter().enumerate() {
        let n = l.0.len();
        for j in 0..n {
            let j1 = (j + 1) % n;
            // side: base -> ring 0
            b.face(&[&[base[li][j], base[li][j1], rings[li][0][j1], rings[li][0][j]]])?;
            // bands: ring k -> ring k+1
            for k in 0..nk - 1 {
                b.face(&[&[
                    rings[li][k][j],
                    rings[li][k][j1],
                    rings[li][k + 1][j1],
                    rings[li][k + 1][j],
                ]])?;
            }
        }
    }

    // Top cap: all loops at last ring, stored orientation.
    let top: Vec<&[usize]> = rings.iter().map(|rl| rl[nk - 1].as_slice()).collect();
    b.face(&top)?;

    let shell: Shell = b.faces.into();
    Solid::try_new(vec![shell]).map_err(|e| format!("Solid::try_new: {e:?}"))
}

/// Reference removed-volume via 2D integration of band area (Simpson over z).
fn reference_volume(loops: &[Loop], z_top: f64, profile: &ProfileSamples, steps: usize) -> f64 {
    // area of cap region (outer CCW positive, holes CW negative)
    let cap_area = |t: f64| -> f64 {
        loops
            .iter()
            .map(|l| miter_offset(l, t).map(|o| shoelace(&o)).unwrap_or(0.0))
            .sum()
    };
    let full = cap_area(0.0);
    // integrate solid cross-section area over z: below profile start it's `full`,
    // inside the profile use linear interpolation of inset u(z) between samples.
    let z0 = profile.0.first().unwrap().1;
    let u_of_z = |z: f64| -> f64 {
        let s = &profile.0;
        for w in s.windows(2) {
            let (u0, za) = w[0];
            let (u1, zb) = w[1];
            if z >= za - 1e-12 && z <= zb + 1e-12 {
                if (zb - za).abs() < 1e-12 {
                    return u1;
                }
                return u0 + (u1 - u0) * (z - za) / (zb - za);
            }
        }
        s.last().unwrap().0
    };
    // Simpson on [z0, z_top]
    let m = steps * 2;
    let h = (z_top - z0) / m as f64;
    let mut acc = 0.0;
    for i in 0..=m {
        let z = z0 + h * i as f64;
        let w = if i == 0 || i == m { 1.0 } else if i % 2 == 1 { 4.0 } else { 2.0 };
        acc += w * cap_area(u_of_z(z));
    }
    let band_part = acc * h / 3.0;
    full * z0 + band_part
}

fn check(label: &str, loops: &[Loop], z_top: f64, profile: &ProfileSamples, analytic: Option<f64>) {
    let t0 = std::time::Instant::now();
    match build_banded_prism(loops, z_top, profile) {
        Ok(solid) => {
            let build_ms = t0.elapsed().as_millis();
            let mesh = solid.triangulation(0.005).to_polygon();
            let v = mesh.volume();
            let reference = reference_volume(loops, z_top, profile, 400);
            let nfaces = solid.boundaries()[0].len();
            print!(
                "[{label}] OK faces={nfaces} tris={} vol={:.6} ref2d={:.6} dv={:+.2e} cond={:?} ({}ms)",
                mesh.faces().tri_faces().len(),
                v,
                reference,
                v - reference,
                mesh.shell_condition(),
                build_ms
            );
            if let Some(a) = analytic {
                print!(" analytic={:.6} dv={:+.2e}", a, v - a);
            }
            println!();
        }
        Err(e) => println!("[{label}] FAILED: {e}"),
    }
}

fn main() {
    let square = Loop(vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]);
    // hole: 0.4x0.4 centered, CW (material on left)
    let hole = Loop(vec![[0.3, 0.3], [0.3, 0.7], [0.7, 0.7], [0.7, 0.3]]);
    // L-shape (reflex vertex at (0.5,0.5)), CCW
    let ell = Loop(vec![
        [0.0, 0.0],
        [1.0, 0.0],
        [1.0, 0.5],
        [0.5, 0.5],
        [0.5, 1.0],
        [0.0, 1.0],
    ]);

    let d = 0.15;
    check(
        "cube chamfer d=0.15",
        &[square.clone()],
        1.0,
        &ProfileSamples::chamfer(d, 1.0),
        Some(1.0 - 2.0 * d * d + 4.0 / 3.0 * d * d * d),
    );

    for n in [2usize, 4, 8, 16] {
        check(
            &format!("cube rounded r=0.15 N={n}"),
            &[square.clone()],
            1.0,
            &ProfileSamples::quarter_arc(0.15, 1.0, n),
            None,
        );
    }
    // continuous-fillet analytic reference for the square, r=0.15:
    // removed = \int_0^r (4u - 4u^2) dw, u = r - sqrt(r^2 - w^2)  (unit square)
    let r: f64 = 0.15;
    let m = 4000;
    let mut removed = 0.0;
    for i in 0..m {
        let w = r * (i as f64 + 0.5) / m as f64;
        let u = r - (r * r - w * w).sqrt();
        removed += (4.0 * u - 4.0 * u * u) * r / m as f64;
    }
    println!("   (continuous quarter-arc analytic volume: {:.6})", 1.0 - removed);

    // cap with hole, chamfer both rims: V = 1 - a^2 - 2 d^2 (1 + a), a=0.4, d=0.1
    let dh = 0.1;
    let a = 0.4;
    check(
        "hole-prism chamfer d=0.1 (both rims)",
        &[square.clone(), hole.clone()],
        1.0,
        &ProfileSamples::chamfer(dh, 1.0),
        Some(1.0 - a * a - 2.0 * dh * dh * (1.0 + a)),
    );
    check(
        "hole-prism rounded r=0.1 N=8",
        &[square.clone(), hole.clone()],
        1.0,
        &ProfileSamples::quarter_arc(0.1, 1.0, 8),
        None,
    );

    // L-shape (reflex corner): chamfer + rounded
    check(
        "L-prism chamfer d=0.1",
        &[ell.clone()],
        1.0,
        &ProfileSamples::chamfer(0.1, 1.0),
        None,
    );
    check(
        "L-prism rounded r=0.1 N=8",
        &[ell.clone()],
        1.0,
        &ProfileSamples::quarter_arc(0.1, 1.0, 8),
        None,
    );

    // failure modes: offset past collapse
    check(
        "cube chamfer d=0.6 (over-collapse)",
        &[square.clone()],
        1.0,
        &ProfileSamples::chamfer(0.6, 1.0),
        None,
    );
    check(
        "L-prism chamfer d=0.3 (leg collapse)",
        &[ell.clone()],
        1.0,
        &ProfileSamples::chamfer(0.3, 1.0),
        None,
    );
}
