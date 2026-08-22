# Parking lot — Cap-rim chamfer with corners + optional curvature

Status: **parked 2026-08-22** after a full research pass (hands-on probes + survey).
This document preserves the findings and the recommended plan so the milestone can be
picked up without re-research. Companion file: [`chamfer-band-prototype.rs`](chamfer-band-prototype.rs)
— the working ~380-line prototype (see §4), preserved verbatim because the scratchpad it
was developed in is ephemeral.

## 1. What works today (shipped)

- Single straight edges between planar faces chamfer **exactly** via `monstertruck-fillet
  =0.4.0` (`FilletProfile::Chamfer`, `RadiusSpec::Constant`). Golden test: cube edge
  d=0.1 → volume 1−d²/2 to fp precision, watertight.
- Sets of **non-vertex-sharing** edges work — one chamfer of `VerticalEdges{OuterOnly}`
  on a square column yields an octagonal prism, volume exact to 1e-6.
- The demo's cube chamfer uses the two *opposite* top rim edges — the largest set the
  kernel supports on a cap rim.

## 2. The gap, precisely characterized

Chamfering **all edges enclosing a cap** (`RimEdges{cap}` — 4 corner-sharing edges on a
box cap) fails in `monstertruck-fillet` with `NotConnected`, for every profile. Root
causes, confirmed by reading the fillet source (0.4.0):

1. **Topology bug**: `fillet_along_wire_closed` (`src/ops.rs:545+`) creates seam
   vertices/edges as *fresh* topology (`Vertex::new`/`Edge::new`) — geometrically
   coincident, topologically distinct → `Solid::try_new` rejects. Upstream's own
   `fillet_edges_cuboid_top_4` test passes only because it never validates the solid.
2. **Corner geometry is a control-point-averaging kludge**, not a corner construction:
   measured corner inset lands at **d/2 instead of the miter point d**, and the cap
   boundary becomes a wandering bezier (d/2 at corners → d mid-edge). Even with fixed
   topology the output would be warped NURBS, not building-grade planes.
3. **Silent no-op by design**: `fillet_edges` (`src/edge_select.rs:489–560`) skips
   failed/unresolvable chains and rolls back per-chain failures, returning `Ok(())`
   (diagnostics only behind `MT_FILLET_DEBUG`). This is why chaining a chamfer onto an
   edge adjacent to an existing blend evaluates clean but removes no material.

Upstream status (checked 2026-08-22): no issue/PR/roadmap entry anywhere (monstertruck
or truck) touches corner/vertex blending; truck's fillet request (ricosjp/truck#53) has
been open without response since 2023. **Nobody is working on this.** Decision: no
upstream issue filed (user call, 2026-08-22); the repro above is complete if that ever
changes.

## 3. Curvature ("rounded chamfer")

`monstertruck-fillet` already ships `FilletProfile::{Round (default), Chamfer, Ridge,
Custom(BSpline)}` and `RadiusSpec::{Constant, Variable(fn), PerEdge}`. **Round and
Variable verified working on single straight edges** (volumes match analytic within
tessellation tolerance). The corner limitation applies identically to all profiles — so
per-edge curvature is available today; cap-rim curvature is gated on the same corner gap
as straight chamfers.

## 4. The recommended fix: profile-offset-band (prototype proven)

For a planar cap on a prismatic solid, chamfering the entire rim reduces to **2D polygon
offsetting**: the band is a ruled surface between the cap boundary at depth d and the
boundary offset inward by d; convex corners are naturally sharp miters (**no corner face
needed — nothing new for provenance to name**); reflex corners become valley seams;
optional curvature = sample the profile into N offset rings and loft (straight = 1 ring,
quarter-arc = rounded fillet, ogee = free).

Prototype results (`chamfer-band-prototype.rs`, builds monstertruck Faces over shared
vertex/edge tables, all solids pass `Solid::try_new` — the gate the kernel's fillet
fails; sub-ms build times):

| Case | Volume | Reference |
|---|---|---|
| Unit cube, whole rim, straight d=0.15 | 0.959500 | analytic 1−2d²+(4/3)d³ — **exact** |
| Cap with 0.4² hole, both rims d=0.1 | 0.812000 | analytic 1−a²−2d²(1+a) — **exact** (convex/reflex corner corrections cancel) |
| Rounded quarter-arc r=0.15, N=2/4/8/16 rings | 0.97573→0.98188 | converges to continuous 0.981980 |
| L-shaped cap (reflex), both profiles | — | matches 2D Simpson integral to ~1e-9 |
| d past straight-skeleton event | rejected | detector: offset edge direction-reversal + loop orientation flip |

2D layer: use **our own miter offset** (1:1 vertex correspondence per source edge —
required for provenance and ring lofting). `cavalier_contours` 0.9 was probed: exact arc
offsets and hole/island culling, but reflex corners force arc joins and change vertex
counts between rings — useful as a validity oracle / max-distance detector / arc-rim
engine, not as the band generator.

### Productionization plan (~1–2 weeks when unparked)

1. In `kernel.rs::chamfer_solid`: detect "selected edges = entire boundary loop(s) of a
   planar cap" → route to the band construction; otherwise keep monstertruck-fillet.
2. Splice band + shrunken cap + trimmed side faces into the *existing* shell (reuse
   untouched faces/edges; the prototype rebuilds the whole prism — this is the main delta).
3. Provenance: band faces keep the existing rule — named by the `SharedEdge` path of the
   edge they replace; for `Rounded{N}` loft the N rings into ONE BREP face per source
   edge so naming stays 1:1. Reflex-corner arc patches (if ever adopted) are named by
   the vertex's `SharedEdge{Side{e1},Side{e2}}` pair.
4. Typed errors: distance past a straight-skeleton event; miter-limit fallback to a
   two-plane bevel corner for acute reflex notches.
5. Params extension (serde-default, document-compatible):
   ```rust
   #[derive(Default, ...)]
   pub enum BlendProfile {
       #[default] Straight,
       Rounded { segments: Option<u32> },   // quarter-arc; radius = distance
       // future: Asymmetric { d_xy, d_z }, Custom { polyline: Vec<[f64;2]> }
   }
   // Params::Chamfer { distance, #[serde(default)] profile: BlendProfile, sub_edges }
   ```
6. The `RimEdges` tests in `provenance_query.rs`/`chamfer_eval.rs` are written to flip
   from asserting the typed corner-gap error to asserting analytic volumes — no API
   change needed when this lands.
7. Later (+~1 week): arc-edged rims (cylinder/revolve caps) via exact r±d arc offsets;
   band faces become cone frusta.

## 5. Survey summary (what else exists)

| Source | Corner handling | Verdict |
|---|---|---|
| OpenCascade `ChFi3d` | Complete. Planar subset is closed-form: 2 chamfers at a vertex = plane∩plane trim; 3 = planar triangular facet; rounds = cylinder∩cylinder, ⅛-sphere | Never port the code (~93k LOC, LGPL entanglement); the planar-subset *algorithm* is the blueprint if mid-solid corners are ever needed (~3–6 weeks) |
| BrepRs 0.6.1-alpha | None — skeleton/generated code, unstitched faces, dead repo link | Non-starter |
| CGAL Straight Skeleton 2 (`extrude_skeleton`) | Exactly the offset-band machinery, robust, weighted/per-edge angles | **GPL** — idea only |
| BOSL2 `offset_sweep`/`rounded_prism` (BSD-2) | Stacked 2D offsets + end profiles + join rules — closest semantic match to §4 | Spec/reference, OpenSCAD script |
| Clipper2 (Boost-1.0) | Offset joins Miter/Square/Bevel/Round | Reference 2D engine |
| Kelly & Wonka 2011 "Procedural Extrusions" (+ campskeleton, Apache-2.0) | Arbitrary per-edge profiles via weighted straight skeleton | The generalization if profiles ever vary per edge |
| Fornjot / SolveSpace / FreeCAD | none / none / OCCT wrapper | — |
