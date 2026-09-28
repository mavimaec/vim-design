// VimDesignWebTest — the authoring app (www/app.html, the GitHub Pages
// site root), served in the Pages layout (no COOP/COEP, single-threaded
// wasm bundle as ./pkg/). Runs in BOTH Playwright projects:
//   desktop — mouse input, 1280x720;
//   mobile  — Pixel 7 emulation (touch, DPR 2.6): canvas input is real
//             touch (touchscreen taps / CDP touch sequences).
//
// Tap targets are computed from WORLD coordinates via
// window.__author.worldToClient, never hardcoded pixels. Model state is
// read through the debug hooks (window.__author.*), which expose the
// element list DERIVED from the document. Floor plates are authored in
// Edit Mode: the Floor tool opens a new plate's profile, ✓ keeps it.

import { test, expect } from "@playwright/test";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { mkdirSync, readFileSync } from "node:fs";

const APP_URL = "http://localhost:8791/";
const DOC_KEY = "vim-design/doc/v1";
/** New walls' default thickness: a stud partition (see app-helpers.js). */
const PARTITION_M = 0.114;
const here = path.dirname(fileURLToPath(import.meta.url));
const screenshotDir = path.join(here, "..", "screenshots");
mkdirSync(screenshotDir, { recursive: true });

const mobile = () => test.info().project.name === "mobile";
const shot = async (page, name) => {
  await page.waitForTimeout(350); // let sheet/toast entrance animations finish
  await settleFrames(page);
  await page.screenshot({ path: path.join(screenshotDir, `app-${test.info().project.name}-${name}.png`) });
};

async function settleFrames(page) {
  await page.evaluate(
    () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))),
  );
}

async function openApp(page, query = "") {
  const errors = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.goto(APP_URL + query);
  await page.waitForFunction(() => window.__author?.ready === true || window.__author?.error, null, {
    timeout: 90_000,
  });
  expect(await page.evaluate(() => window.__author.error)).toBeFalsy();
  if (test.info().project.name === "mobile") {
    // Zoom in a little (as a user would) so the geometry below spans the
    // phone screen: ~42 px/m, touch capture radius ~0.57 m.
    await page.evaluate(() => {
      window.__author.app.set_camera_json('{"tx":0,"ty":0.5,"halfH":10}');
      window.__author.refresh();
    });
  }
  return errors;
}

const stats = (page) => page.evaluate(() => window.__author.stats());
const elements = (page) => page.evaluate(() => window.__author.elements());
const plates = async (page) => (await elements(page)).filter((e) => e.kind === "floor_plate");

async function worldToClient(page, x, y, z = 0) {
  const p = await page.evaluate(([x, y, z]) => window.__author.worldToClient(x, y, z), [x, y, z]);
  expect(p, `world (${x}, ${y}, ${z}) is on screen`).toBeTruthy();
  return p;
}

/** Tap (touch) or click (mouse) the canvas at a world point. */
async function tapWorld(page, x, y, z = 0) {
  const [cx, cy] = await worldToClient(page, x, y, z);
  if (mobile()) await page.touchscreen.tap(cx, cy);
  else await page.mouse.click(cx, cy);
}

async function tool(page, name) {
  await page.locator(`.tool[data-tool="${name}"]`).click();
  expect((await stats(page)).tool).toBe(name);
}

async function shape(page, name) {
  await page.locator(`#shape-toggle button[data-shape="${name}"]`).click();
  expect((await stats(page)).shape).toBe(name);
}

const snapStep = async (page) => (await stats(page)).snap.step;
const snapTo = (v, step) => Math.round(v / step) * step;

/** Tap a sequence of world points, then Finish. */
async function drawPolygon(page, pts) {
  for (const [x, y] of pts) await tapWorld(page, x, y);
  await page.locator("#finish-draw").click();
}

const sortedOutline = (o) => [...o].map(([x, y]) => [+x.toFixed(6), +y.toFixed(6)]).sort((a, b) => a[0] - b[0] || a[1] - b[1]);

const FIXTURE = path.join(here, "..", "..", "crates", "vim-design-test", "fixtures", "authoring_project.vimd");

async function cdpTouch(page) {
  const cdp = await page.context().newCDPSession(page);
  return (type, points) => cdp.send("Input.dispatchTouchEvent", {
    type, touchPoints: points.map(([x, y], id) => ({ x, y, id })),
  });
}

/** Press at world `a`, drag to world `b`, release (mouse or touch). */
async function dragWorld(page, a, b, steps = 10) {
  const pa = await worldToClient(page, ...a);
  const pb = await worldToClient(page, ...b);
  if (mobile()) {
    const touch = await cdpTouch(page);
    await touch("touchStart", [pa]);
    for (let i = 1; i <= steps; i++) {
      await touch("touchMove", [[pa[0] + ((pb[0] - pa[0]) * i) / steps, pa[1] + ((pb[1] - pa[1]) * i) / steps]]);
    }
    await touch("touchEnd", []);
  } else {
    await page.mouse.move(pa[0], pa[1]);
    await page.mouse.down();
    await page.mouse.move(pb[0], pb[1], { steps });
    await page.mouse.up();
  }
}

/** Press and hold at a world point. */
async function longPressWorld(page, a, ms = 800) {
  const p = await worldToClient(page, ...a);
  if (mobile()) {
    const touch = await cdpTouch(page);
    await touch("touchStart", [p]);
    await page.waitForTimeout(ms);
    await touch("touchEnd", []);
  } else {
    await page.mouse.move(p[0], p[1]);
    await page.mouse.down();
    await page.waitForTimeout(ms);
    await page.mouse.up();
  }
}

const editState = (page) => page.evaluate(() => window.__author.editState());
const profile = (page) => page.evaluate(() => window.__author.editProfile());
const hasPoint = (prof, [u, v]) => prof.points.some((p) => Math.abs(p.uv[0] - u) < 1e-6 && Math.abs(p.uv[1] - v) < 1e-6);
const solidOutline = (plate) => sortedOutline(plate.faces.find((f) => f.kind === "solid").outline);
/** The document's saved bytes (for byte-identity checks). */
const savedBytes = (page) => page.evaluate(() => Array.from(window.__author.app.save_document()).join(","));

async function editTool(page, name) {
  await page.locator(`[data-edit-tool="${name}"]`).click();
  expect((await editState(page)).tool).toBe(name);
}

async function editMode(page, name) {
  await page.locator(`[data-edit-mode="${name}"]`).click();
  expect((await editState(page)).mode).toBe(name);
}

/** Floor tool: a new plate in Edit Mode; a rectangle; ✓. */
async function drawPlate(page, a, b) {
  await page.locator('.tool[data-tool="plate"]').click();
  expect((await editState(page)).active).toBe(true);
  await shape(page, "rect");
  await tapWorld(page, ...a);
  await tapWorld(page, ...b);
  await page.locator("#edit-confirm").click();
  expect((await editState(page)).active).toBe(false);
}

async function selectAt(page, x, y) {
  await tool(page, "select");
  await tapWorld(page, x, y);
}

test("loads settled: Montreal site, two levels, plan view, clean chrome", async ({ page }) => {
  const errors = await openApp(page);
  const s = await stats(page);
  expect(s.settled).toBe(true);
  expect(s.backend).toBe("WebGL2"); // headless chromium: WebGPU never composites
  expect(s.view).toBe("plan");
  expect(s.tool).toBe("select");
  expect(s.elements).toBe(0);
  expect(s.canUndo).toBe(false);
  expect(s.snap.step).toBe(mobile() ? 0.5 : 0.25); // coarse pointer default
  const site = await page.evaluate(() => window.__author.site());
  expect(site).toMatchObject({ latitude: 45.5019, longitude: -73.5674, elevation: 36, trueNorth: 0 });
  const levels = await page.evaluate(() => window.__author.levels());
  expect(levels.levels.map((l) => [l.name, l.elevation, l.isStory])).toEqual([
    ["Ground", 0, true],
    ["Level 2", 3, true],
  ]);
  expect(levels.levels[0].color).not.toEqual(levels.levels[1].color);
  expect(levels.activeId).toBe(levels.levels[0].id);
  await expect(page.locator("#level-chip .chip-name")).toHaveText("Ground");
  await expect(page.locator("#view-toggle button.on")).toHaveText("Plan");
  const s2 = await stats(page);
  console.log(`[${test.info().project.name}] load ${Math.round(await page.evaluate(() => window.__author.loadMs))} ms ` +
    `(wasm ${Math.round(await page.evaluate(() => window.__author.wasmMs))} ms), backend ${s2.backend}, msaa ${s2.msaa}`);
  await shot(page, "empty");
  expect(errors).toEqual([]);
});


test("floor plates in Edit Mode: rectangle + polygon with snapping, validation, ✓ one undo, ✗ leaves no trace", async ({ page }) => {
  const errors = await openApp(page);
  const step = await snapStep(page);
  const bytes0 = await savedBytes(page);

  // The Floor tool opens a NEW plate's profile with the Solid tool armed.
  await page.locator('.tool[data-tool="plate"]').click();
  let st = await editState(page);
  expect([st.active, st.isNew, st.tool, st.name]).toEqual([true, true, "solid", "Floor plate 1"]);
  await expect(page.locator("#edit-bar")).toBeVisible();
  await expect(page.locator("#toolbar")).toBeHidden();
  await shape(page, "rect");
  await tapWorld(page, -3.93, -1.92);
  await tapWorld(page, -1.08, 2.07);
  let prof = await profile(page);
  expect(prof.faces).toHaveLength(1);
  expect(sortedOutline(prof.faces[0].outline)).toEqual(sortedOutline([
    [snapTo(-3.93, step), snapTo(-1.92, step)], [snapTo(-1.08, step), snapTo(-1.92, step)],
    [snapTo(-1.08, step), snapTo(2.07, step)], [snapTo(-3.93, step), snapTo(2.07, step)],
  ]));
  expect((await stats(page)).elements, "the first face creates the plate").toBe(1);

  // A second, disjoint face in the same element: an L-shaped polygon,
  // closed by tapping its first vertex.
  await shape(page, "polygon");
  const L = [[1.04, -1.97], [4.46, -2.03], [4.52, 0.98], [3.03, 1.04], [2.97, 3.02], [0.98, 2.96]];
  for (const [x, y] of L) await tapWorld(page, x, y);
  const hover = await worldToClient(page, 1.1, -1.2);
  if (!mobile()) await page.mouse.move(hover[0], hover[1]);
  await shot(page, "drawing");
  await tapWorld(page, 1.02, -2.01);
  prof = await profile(page);
  expect(prof.faces).toHaveLength(2);
  expect(sortedOutline(prof.faces[1].outline)).toEqual(sortedOutline(L.map(([x, y]) => [snapTo(x, 0.5), snapTo(y, 0.5)])));

  // A self-crossing outline is refused live: red preview, no Finish.
  for (const [x, y] of [[-4, 3], [-1, 5], [-1, 3], [-4, 5]]) await tapWorld(page, x, y);
  const hud = await page.evaluate(() => window.__author.hud());
  expect(hud.canFinish).toBe(false);
  expect(hud.reason).toBe("The outline crosses itself");
  await expect(page.locator("#finish-draw")).toBeDisabled();
  await page.locator("#cancel-draw").click();
  expect((await profile(page)).faces).toHaveLength(2);

  // ✓ keeps the session's steps: each face is one step of the history.
  await page.locator("#edit-confirm").click();
  let [plate] = await plates(page);
  expect([plate.name, plate.sketch, plate.faceCount]).toEqual(["Floor plate 1", true, 2]);
  expect(plate.area).toBeCloseTo(3 * 4 + (3.5 * 3 + 2 * 2), 4);
  expect(plate.volume).toBeCloseTo(0.3 * (12 + 14.5), 4);
  await page.locator("#undo").click();
  expect((await plates(page))[0].faceCount).toBe(1);
  await page.locator("#undo").click();
  expect(await elements(page)).toHaveLength(0);
  expect(await savedBytes(page), "undo restores the document exactly").toBe(bytes0);
  await page.locator("#redo").click();
  await page.locator("#redo").click();
  expect((await plates(page))[0].faceCount).toBe(2);

  // ✗ on a new plate leaves no trace, byte for byte.
  const bytes1 = await savedBytes(page);
  await page.locator('.tool[data-tool="plate"]').click();
  await shape(page, "rect");
  await tapWorld(page, -4, 3.5);
  await tapWorld(page, -2, 4.5);
  expect((await stats(page)).elements).toBe(2);
  await page.locator("#edit-cancel").click();
  await page.locator("#dialog-ok").click();
  expect((await editState(page)).active).toBe(false);
  expect(await savedBytes(page)).toBe(bytes1);
  // ✓ with no face leaves nothing either.
  await page.locator('.tool[data-tool="plate"]').click();
  await page.locator("#edit-confirm").click();
  expect(await savedBytes(page)).toBe(bytes1);
  expect((await stats(page)).errors).toEqual([]);
  expect(errors).toEqual([]);
});

test("Edit Mode: voids (outside, pocket), per-face thickness, split — the mesh follows live", async ({ page }) => {
  const errors = await openApp(page);
  await drawPlate(page, [-3, -2], [3, 2]);
  let [plate] = await plates(page);
  expect(plate.area).toBeCloseTo(24, 4);
  expect(plate.volume).toBeCloseTo(7.2, 4);
  const bbox0 = plate.bbox;

  // Properties outside Edit Mode: faces, area, the pencil.
  await selectAt(page, 0, 0);
  await expect(page.locator("#prop-face-count")).toHaveText("1");
  await expect(page.locator("#prop-area")).toHaveText("24.00 m²");
  await shot(page, "edit-properties");
  await page.locator("#prop-edit").click();
  expect((await editState(page)).active).toBe(true);
  if (mobile()) {
    // Back to the test's framing (the properties sheet moved the view).
    await page.evaluate(() => {
      window.__author.app.set_camera_json('{"tx":0,"ty":0.5,"halfH":10}');
      window.__author.refresh();
    });
  }

  // A through void reaching OUTSIDE the plate removes only what it covers.
  await editTool(page, "void");
  await shape(page, "rect");
  await tapWorld(page, 2, -1);
  await tapWorld(page, 4.5, 1);
  [plate] = await plates(page);
  expect(plate.area).toBeCloseTo(24 - 2, 4);
  expect(plate.volume).toBeCloseTo(7.2 - 0.6, 4);

  // A pocket: a void shallower than the plate.
  await tapWorld(page, -2, -1);
  await tapWorld(page, -1, 1);
  await editMode(page, "faces");
  await tapWorld(page, -1.5, 0);
  let st = await editState(page);
  expect(st.panel.void.through).toBe(true);
  await page.locator("#edit-depth").fill("0.1");
  [plate] = await plates(page);
  expect(plate.faces.find((f) => f.kind === "void" && f.outline.some(([x]) => x === -2)).depth).toBeCloseTo(0.1, 9);
  expect(plate.volume).toBeCloseTo(6.6 - 2 * 0.1, 4); // a 2 m² pocket, 0.1 deep
  expect(plate.bbox).toEqual(bbox0);

  // Split the solid, then give one half its own thickness.
  await editTool(page, "split");
  await tapWorld(page, 0, -3);
  await tapWorld(page, 0, 3);
  let prof = await profile(page);
  expect(prof.faces.filter((f) => f.kind.solid !== undefined)).toHaveLength(2);
  await editMode(page, "faces");
  await tapWorld(page, 1, 1.5);
  await page.locator("#edit-thickness").fill("0.5");
  await page.locator("#edit-thickness").press("Enter");
  [plate] = await plates(page);
  expect(plate.bbox[0][2]).toBeCloseTo(-0.5, 4);
  await shot(page, "edit-faces");
  await page.locator('#edit-view-toggle button[data-view="3d"]').click();
  await page.evaluate(() => window.__author.app.zoom_fit());
  await shot(page, "edit-3d");
  await page.locator('#edit-view-toggle button[data-view="plan"]').click();

  // Undo inside Edit Mode steps back one edit (the thickness).
  await page.locator("#undo").click();
  expect((await plates(page))[0].bbox[0][2]).toBeCloseTo(-0.3, 4);
  await page.locator("#redo").click();
  await page.locator("#edit-confirm").click();

  // Reload: the sketch plate comes back with all its faces.
  await page.evaluate(() => window.__author.saveNow());
  const before = (await plates(page))[0];
  await page.reload();
  await page.waitForFunction(() => window.__author?.ready === true, null, { timeout: 90_000 });
  const after = (await plates(page))[0];
  expect(after.faces.map((f) => [f.kind, f.thickness ?? f.depth, sortedOutline(f.outline)]))
    .toEqual(before.faces.map((f) => [f.kind, f.thickness ?? f.depth, sortedOutline(f.outline)]));
  expect(after.volume).toBeCloseTo(before.volume, 6);
  expect((await stats(page)).errors).toEqual([]);
  expect(errors).toEqual([]);
});

test("Edit Mode: select, drag point/edge/face, long-press insert, marquee, the three deletes, ✓/✗", async ({ page }) => {
  const errors = await openApp(page);
  await drawPlate(page, [-3, -2], [3, 2]);
  await selectAt(page, 0, 0);
  await page.locator("#prop-edit").click();

  // Points: tap selects; drag moves (snapped); a crossing move snaps back.
  await editMode(page, "points");
  await tapWorld(page, 3, -2);
  expect((await editState(page)).selection).toBe(1);
  await dragWorld(page, [3, 2], [4.07, 2.94]);
  let prof = await profile(page);
  expect(hasPoint(prof, [4, 3])).toBe(true);
  await dragWorld(page, [3, -2], [-4, 0.1]);
  prof = await profile(page);
  expect(hasPoint(prof, [3, -2]), "the crossing move was rejected").toBe(true);
  expect(await page.evaluate(() => window.__author.toasts.map((t) => t.msg).join("|"))).toContain("the move was undone");

  // Edges: dragging the bottom edge moves both its points.
  await editMode(page, "edges");
  await dragWorld(page, [0, -2], [0.03, -2.46]);
  prof = await profile(page);
  expect(hasPoint(prof, [-3, -2.5]) && hasPoint(prof, [3, -2.5])).toBe(true);

  // Faces: a void, dragged by its face.
  await editTool(page, "void");
  await shape(page, "rect");
  await tapWorld(page, -1, -0.5);
  await tapWorld(page, 1, 0.5);
  await editMode(page, "faces");
  await dragWorld(page, [0, 0], [0.5, 0.02]);
  prof = await profile(page);
  const voidFace = () => prof.faces.find((f) => f.kind.void !== undefined);
  expect(sortedOutline(voidFace().outline)).toEqual(sortedOutline([[-0.5, -0.5], [1.5, -0.5], [1.5, 0.5], [-0.5, 0.5]]));

  // Long press on an edge (Points mode) inserts a point there.
  await editMode(page, "points");
  const solidPoints = () => prof.faces.find((f) => f.kind.solid !== undefined).points.length;
  await longPressWorld(page, [-3, 0.4]);
  prof = await profile(page);
  expect(solidPoints()).toBe(5);
  expect(hasPoint(prof, [-3, 0.4])).toBe(true);
  // Marquee selects it; Delete removes it and reconnects its neighbours.
  await dragWorld(page, [-3.9, -0.4], [-2.4, 1.4]);
  expect((await editState(page)).selection).toBe(1);
  await page.locator("#edit-delete").click();
  prof = await profile(page);
  expect(solidPoints()).toBe(4);
  // Delete an edge: its two points merge (the void becomes a triangle).
  await editMode(page, "edges");
  await tapWorld(page, 0.5, 0.5);
  await page.locator("#edit-delete").click();
  prof = await profile(page);
  expect(voidFace().points).toHaveLength(3);
  // Delete a face: its points go with it.
  await editMode(page, "faces");
  await tapWorld(page, 1.1, -0.2);
  await page.locator("#edit-delete").click();
  prof = await profile(page);
  expect(prof.faces).toHaveLength(1);
  expect(prof.points).toHaveLength(4);
  await shot(page, "edit-mode");

  // ✓ keeps the session's steps: undo walks back through them, one by
  // one, to the plate as it was.
  await page.locator("#edit-confirm").click();
  let [plate] = await plates(page);
  expect(sortedOutline(plate.faces[0].outline)).toContainEqual([4, 3]);
  let undos = 0;
  while (undos < 30 && !(await plates(page)).some((p) => sortedOutline(p.faces[0].outline).length === 4
      && JSON.stringify(solidOutline(p)) === JSON.stringify(sortedOutline([[-3, -2], [3, -2], [3, 2], [-3, 2]])) && p.faces.length === 1)) {
    await page.locator("#undo").click();
    undos++;
  }
  expect(undos, "several steps, not one").toBeGreaterThan(1);
  [plate] = await plates(page);
  expect(solidOutline(plate)).toEqual(sortedOutline([[-3, -2], [3, -2], [3, 2], [-3, 2]]));

  // ✗ after edits: the document is byte-identical to before the session.
  const bytes = await savedBytes(page);
  await selectAt(page, 0, 0);
  await page.locator("#prop-edit").click();
  await editMode(page, "points");
  await dragWorld(page, [3, 2], [3.5, 2.5]);
  await page.locator("#edit-cancel").click();
  await page.locator("#dialog-ok").click();
  expect(await savedBytes(page)).toBe(bytes);
  expect(errors).toEqual([]);
});

test("legacy plates: import the fixture, convert with the pencil; ✗ undoes the conversion, ✓ keeps it", async ({ page }) => {
  const errors = await openApp(page);
  await page.locator("#menu-btn").click();
  const [chooser] = await Promise.all([
    page.waitForEvent("filechooser"),
    page.locator('[data-testid="menu-import"]').click(),
  ]);
  await chooser.setFiles(FIXTURE);
  await page.locator("#dialog-ok").click();
  await page.waitForFunction(() => window.__author.stats().walls === 4);
  if (mobile()) {
    await page.evaluate(() => {
      window.__author.app.set_camera_json('{"tx":4,"ty":3,"halfH":10}');
      window.__author.refresh();
    });
  }
  let [plate] = await plates(page);
  expect([plate.name, plate.sketch ?? false, plate.holes.length]).toEqual(["Floor plate 1", false, 2]);
  const bytes = await savedBytes(page);

  // The pencil converts it (inside the session) and opens Edit Mode.
  await selectAt(page, 4, 3);
  await expect(page.locator("#prop-edit")).toBeVisible();
  await page.locator("#prop-edit").click();
  let st = await editState(page);
  expect(st.active).toBe(true);
  [plate] = await plates(page);
  expect([plate.name, plate.sketch, plate.faceCount]).toEqual(["Floor plate 1", true, 3]);
  expect(plate.area).toBeCloseTo(48 - 1 - 1.5, 4);
  expect(st.canUndo, "the conversion is not an edit to step back over").toBe(false);
  // ✗: back to the legacy plate, byte for byte.
  await page.locator("#edit-cancel").click();
  expect(await savedBytes(page)).toBe(bytes);
  expect((await plates(page))[0].sketch ?? false).toBe(false);

  // ✓: the converted plate stays (one main undo step reverts it).
  await selectAt(page, 4, 3);
  await page.locator("#prop-edit").click();
  await page.locator("#edit-confirm").click();
  [plate] = await plates(page);
  expect([plate.sketch, plate.faceCount]).toEqual([true, 3]);
  expect((await stats(page)).walls).toBe(4);
  await page.locator("#undo").click();
  expect(await savedBytes(page)).toBe(bytes);

  // The Hole tool on a plate opens Edit Mode with the Void tool armed.
  await tool(page, "hole");
  await tapWorld(page, 4, 3);
  st = await editState(page);
  expect([st.active, st.tool]).toEqual([true, "void"]);
  await page.locator("#edit-cancel").click();
  expect((await stats(page)).errors).toEqual([]);
  expect(errors).toEqual([]);
});

test("persistence: reload restores the model; corrupt storage starts fresh with a backup", async ({ page }) => {
  await openApp(page);
  await drawPlate(page, [-2, -2], [2, 1]);
  const before = await plates(page);
  expect(before).toHaveLength(1);
  // The debounced save lands ~300 ms after the change.
  await page.waitForFunction((k) => !!localStorage.getItem(k), DOC_KEY, { timeout: 5000 });

  await page.reload();
  await page.waitForFunction(() => window.__author?.ready === true, null, { timeout: 90_000 });
  const after = await plates(page);
  expect(after.map((p) => [p.name, solidOutline(p), p.faces[0].thickness]))
    .toEqual(before.map((p) => [p.name, solidOutline(p), p.faces[0].thickness]));
  // Undo history is not persisted (by design).
  expect((await stats(page)).canUndo).toBe(false);

  // Corrupt the stored document: the app must start fresh, keep a backup.
  await page.evaluate((k) => localStorage.setItem(k, "this is not a VIM Design document!"), DOC_KEY);
  await page.reload();
  await page.waitForFunction(() => window.__author?.ready === true, null, { timeout: 90_000 });
  expect(await elements(page)).toHaveLength(0);
  expect((await page.evaluate(() => window.__author.levels())).levels).toHaveLength(2);
  const keys = await page.evaluate(() => Object.keys(localStorage));
  const backup = keys.find((k) => k.startsWith(`${DOC_KEY}.corrupt-`));
  expect(backup, "backup key exists").toBeTruthy();
  expect(await page.evaluate((k) => localStorage.getItem(k), backup)).toBe("this is not a VIM Design document!");
  expect(keys).not.toContain(DOC_KEY);
  await page.waitForFunction(
    () => window.__author.toasts.some((t) => t.msg.includes("could not be opened")),
    null, { timeout: 5000 },
  );
});

test("export / import round trip via the menu", async ({ page }) => {
  await openApp(page);
  await drawPlate(page, [-2, -1], [3, 2]);
  const before = await plates(page);

  await page.locator("#menu-btn").click();
  await expect(page.locator("#sheet-title")).toHaveText("Menu");
  await shot(page, "menu");
  const [download] = await Promise.all([
    page.waitForEvent("download"),
    page.locator('[data-testid="menu-export"]').click(),
  ]);
  expect(download.suggestedFilename()).toMatch(/^vim-design-\d{4}-\d{2}-\d{2}\.vimd$/);
  const file = test.info().outputPath("export.vimd");
  await download.saveAs(file);
  expect(readFileSync(file).subarray(0, 4).toString()).toBe("VIMD");

  // New project (confirmed) empties the model.
  await page.locator('[data-testid="menu-new"]').click();
  await page.locator("#dialog-ok").click();
  expect(await elements(page)).toHaveLength(0);

  // Import the export back (file chooser + confirm).
  await page.locator("#menu-btn").click();
  const [chooser] = await Promise.all([
    page.waitForEvent("filechooser"),
    page.locator('[data-testid="menu-import"]').click(),
  ]);
  await chooser.setFiles(file);
  await expect(page.locator("#dialog-backdrop")).toBeVisible();
  await page.locator("#dialog-ok").click();
  await page.waitForFunction(() => window.__author.stats().elements === 1);
  const after = await plates(page);
  expect(after.map(solidOutline)).toEqual(before.map(solidOutline));
});

test("Plan/3D toggle; drawing works in 3D; level elevation edit is transform-only", async ({ page }) => {
  const errors = await openApp(page);
  await drawPlate(page, [-3, -2], [2, 2]);
  await page.locator('#view-toggle button[data-view="3d"]').click();
  expect((await stats(page)).view).toBe("3d");
  // Draw on Level 2 in the 3D view.
  await page.locator("#level-chip").click();
  await page.locator('#level-popover [data-level]').first().click(); // top of the list = Level 2
  const levels = await page.evaluate(() => window.__author.levels());
  expect(levels.levels.find((l) => l.id === levels.activeId).name).toBe("Level 2");
  await page.locator('.tool[data-tool="plate"]').click();
  await shape(page, "rect");
  await tapWorld(page, -1, -1, 3);
  await tapWorld(page, 1, 1, 3);
  await page.locator("#edit-confirm").click();
  expect(await plates(page)).toHaveLength(2);
  const upper = (await plates(page)).find((p) => p.levelName === "Level 2");
  expect(solidOutline(upper)).toEqual(sortedOutline([[-1, -1], [1, -1], [1, 1], [-1, 1]]));
  await tool(page, "select");
  await page.locator("#fit-btn").click();
  await shot(page, "3d");

  // Elevation edit through the Levels panel: transform-only poll.
  await page.locator("#menu-btn").click();
  await page.locator('[data-testid="menu-levels"]').click();
  await expect(page.locator("#sheet-title")).toHaveText("Levels");
  await shot(page, "levels");
  const ground = levels.levels.find((l) => l.name === "Ground");
  const bbox0 = await page.evaluate(() => JSON.parse(window.__author.app.scene_bbox_json()));
  await page.locator(`.level-row[data-id="${ground.id}"] .lvl-elev`).fill("0.5");
  const s = await stats(page);
  expect(s.lastMeshUpserts, "elevation edit re-uploads no meshes").toBe(0);
  expect(s.lastBaseTransforms, "one re-placement for the Ground plate").toBe(1);
  const bbox1 = await page.evaluate(() => JSON.parse(window.__author.app.scene_bbox_json()));
  expect(bbox1.min[2] - bbox0.min[2]).toBeCloseTo(0.5, 5);
  await page.locator("#sheet-close").click();
  await page.locator('#view-toggle button[data-view="plan"]').click();
  expect((await stats(page)).view).toBe("plan");
  expect(errors).toEqual([]);
});

test("touch: press-drag-release placement, two fingers never place", async ({ page }) => {
  test.skip(!mobile(), "touch gestures are exercised in the mobile project");
  await openApp(page);
  const touch = await cdpTouch(page);

  // Rectangle face: press at one corner, drag, release at the other.
  await page.locator('.tool[data-tool="plate"]').click();
  await shape(page, "rect");
  await dragWorld(page, [-2.1, -1.1], [1.9, 2.1], 8);
  const prof = await profile(page);
  expect(prof.faces).toHaveLength(1);
  expect(sortedOutline(prof.faces[0].outline)).toEqual(sortedOutline([[-2, -1], [2, -1], [2, 2], [-2, 2]]));

  // Polygon: the vertex lands where the finger is RELEASED (snapped).
  await shape(page, "polygon");
  await dragWorld(page, [4, 4], [3.1, 3.9], 8);
  let sk = await page.evaluate(() => window.__author.sketch());
  expect(sk.points).toEqual([[3, 4]]);

  // A second finger cancels the pending placement and navigates.
  // (Clear of the Fit / View buttons at the right edge.)
  const a = await worldToClient(page, 2, 4.5);
  const b = await worldToClient(page, 0, 6.5);
  await touch("touchStart", [a]);
  await touch("touchStart", [a, b]);
  await touch("touchMove", [[a[0] - 20, a[1] - 20], [b[0] + 20, b[1] + 20]]);
  await touch("touchEnd", []);
  sk = await page.evaluate(() => window.__author.sketch());
  expect(sk.points, "pinch placed nothing").toEqual([[3, 4]]);
  expect((await stats(page)).committed).toBeGreaterThan(0);
});

// ---------------------------------------------------------------------------
// Walls and windows
// ---------------------------------------------------------------------------

const walls = async (page) => (await elements(page)).filter((e) => e.kind === "wall");

test("walls: a closed loop traced on a plate is ONE wall run growing inward (mitered corners); an open run; one undo per run", async ({ page }) => {
  const errors = await openApp(page);
  await drawPlate(page, [-3, -2], [3, 2]);

  // Trace the plate outline (corner snapping) and close on the first point.
  await tool(page, "wall");
  await shape(page, "polygon");
  for (const [x, y] of [[-2.97, -1.96], [2.96, -2.03], [3.02, 1.97], [-3.03, 2.02]]) {
    await tapWorld(page, x, y);
  }
  const hud = await page.evaluate(() => window.__author.hud());
  expect(hud.bands.length, "the preview shows the wall footprint").toBeGreaterThan(0);
  const near = await worldToClient(page, -2.2, -1.4);
  if (!mobile()) await page.mouse.move(near[0], near[1]);
  await shot(page, "wall-drawing");
  await tapWorld(page, -2.98, -2.02); // first point: closes the loop
  let ws = await walls(page);
  expect(ws).toHaveLength(1);
  const [room] = ws;
  expect(room).toMatchObject({ name: "Wall 1", run: true, closed: true, segments: 4 });
  expect(room.height).toBeCloseTo(2.7, 9);
  // New walls default to a stud partition.
  const T = PARTITION_M;
  expect(room.thickness).toBeCloseTo(T, 9);
  // Inward on the plate edge, mitered: the band inside the outline.
  expect(room.footprintArea).toBeCloseTo(6 * 4 - (6 - 2 * T) * (4 - 2 * T), 6);
  expect(room.volume).toBeCloseTo(room.footprintArea * 2.7, 3);
  expect(room.points.every(([x, y]) => Math.abs(Math.abs(x) - 3) < 1e-9 && Math.abs(Math.abs(y) - 2) < 1e-9)).toBe(true);

  // One undo removes the whole run; redo restores it.
  await page.locator("#undo").click();
  expect(await walls(page)).toHaveLength(0);
  expect(await plates(page)).toHaveLength(1);
  await page.locator("#redo").click();
  expect(await walls(page)).toHaveLength(1);

  // An open run (Finish): its points as drawn, the corner mitered.
  for (const [x, y] of [[-4, 3], [0, 3], [0, 4.5]]) await tapWorld(page, x, y);
  await page.locator("#finish-draw").click();
  ws = await walls(page);
  expect(ws).toHaveLength(2);
  expect(ws[1]).toMatchObject({ name: "Wall 2", closed: false, segments: 2, points: [[-4, 3], [0, 3], [0, 4.5]] });
  expect(ws[1].footprintArea).toBeCloseTo(4 * T + (1.5 - T) * T, 6);
  await tool(page, "select");
  await shot(page, "walls");
  expect((await stats(page)).errors).toEqual([]);
  expect(errors).toEqual([]);
});

test("wall properties: height and thickness edits with undo, flip side, delete", async ({ page }) => {
  const errors = await openApp(page);
  await tool(page, "wall");
  // Flip: a room drawn with flip grows outward.
  await page.locator("#flip-toggle").click();
  expect((await stats(page)).wall.flip).toBe(true);
  await shape(page, "rect");
  await tapWorld(page, -2, -1.5);
  await tapWorld(page, 2, 1.5);
  const T = PARTITION_M;
  let [room] = await walls(page);
  expect(room.footprintArea).toBeCloseTo((4 + 2 * T) * (3 + 2 * T) - 12, 6);
  await page.locator("#undo").click();
  await page.locator("#flip-toggle").click();
  expect((await stats(page)).wall.flip).toBe(false);
  await tapWorld(page, -2, -1.5);
  await tapWorld(page, 2, 1.5);
  [room] = await walls(page);
  expect(room.footprintArea).toBeCloseTo(12 - (4 - 2 * T) * (3 - 2 * T), 6);

  // Select the run by tapping its thin band in plan.
  await tool(page, "select");
  await tapWorld(page, 0, -1.42);
  expect((await stats(page)).selection).toBe(room.id);
  await expect(page.locator("#sheet-title")).toHaveText(room.name);
  await expect(page.locator("#prop-height")).toHaveValue("2.70");
  await expect(page.locator("#prop-length")).toHaveText("14.00 m");
  await shot(page, "wall-properties");

  const byId = async () => (await walls(page)).find((w) => w.id === room.id);
  await page.locator("#prop-height").fill("3.1");
  await page.locator("#prop-height").press("Enter");
  expect((await byId()).height).toBeCloseTo(3.1, 9);
  await page.locator("#undo").click();
  expect((await byId()).height).toBeCloseTo(2.7, 9);
  await expect(page.locator("#prop-height")).toHaveValue("2.70");
  await page.locator("#redo").click();
  expect((await byId()).height).toBeCloseTo(3.1, 9);

  // Thickness: the whole run, the line stays.
  await page.locator("#prop-wall-thickness").fill("0.3");
  await page.locator("#prop-wall-thickness").press("Enter");
  const thick = await byId();
  expect(thick.thickness).toBeCloseTo(0.3, 9);
  expect(thick.points).toEqual(room.points);
  expect(thick.footprintArea).toBeCloseTo(12 - 3.4 * 2.4, 6);
  await page.locator("#undo").click();
  expect((await byId()).thickness).toBeCloseTo(T, 9);

  await page.locator("#prop-delete").click();
  expect(await walls(page)).toHaveLength(0);
  await page.locator("#undo").click();
  expect(await walls(page)).toHaveLength(1);
  expect(errors).toEqual([]);
});

test("openings: placed from the Window tool (Openings mode), kept by ✓, listed in the properties, reloaded; a level drag stays transform-only", async ({ page }) => {
  const errors = await openApp(page);
  await drawPlate(page, [-3, -2], [3, 2]);
  await tool(page, "wall");
  await shape(page, "rect");
  await tapWorld(page, -3, -2);
  await tapWorld(page, 3, 2);
  let [room] = await walls(page);
  const runOpenings = () => page.evaluate(() => JSON.parse(window.__author.app.openings_json()));

  // The Window tool: Openings mode; a window, then a door, on the wall.
  await page.locator('.tool[data-tool="window"]').click();
  await tapWorld(page, -1, -1.9);
  await page.locator('[data-opening-preset="door"]').click();
  await tapWorld(page, 1.5, -1.9);
  await shot(page, "window-elevation");
  await page.locator("#edit-confirm").click();
  let os = await runOpenings();
  expect(os.map((o) => o.kind)).toEqual(["window", "door"]);
  expect(os[0].segment, "both on the bottom segment").toBe(os[1].segment);

  // Height stays above the highest opening (the door, 2.1 m + 0.05 m).
  await tool(page, "select");
  await tapWorld(page, 0, -1.9);
  expect((await stats(page)).selection).toBe(room.id);
  await expect(page.locator("#prop-window-count")).toHaveText("2");
  await page.locator("#prop-height").fill("1.0");
  await page.locator("#prop-height").press("Enter");
  const lowered = (await walls(page))[0];
  expect(lowered.minHeight).toBeCloseTo(2.15, 9);
  expect(lowered.height).toBeCloseTo(2.15, 9);
  await page.locator("#undo").click();

  // Delete an opening from the properties; undo restores it.
  await page.locator('[data-testid="delete-opening"]').first().click();
  expect(await runOpenings()).toHaveLength(1);
  await page.locator("#undo").click();
  expect(await runOpenings()).toHaveLength(2);
  await page.locator("#sheet-close").click();

  // Walls and openings survive a reload.
  await page.waitForFunction((k) => !!localStorage.getItem(k), DOC_KEY, { timeout: 5000 });
  await page.evaluate(() => window.__author.saveNow());
  await page.reload();
  await page.waitForFunction(() => window.__author?.ready === true, null, { timeout: 90_000 });
  expect(await walls(page)).toHaveLength(1);
  expect(await runOpenings()).toHaveLength(2);

  // Level elevation edit (a run without a top constraint): transform-only.
  await page.locator("#menu-btn").click();
  await page.locator('[data-testid="menu-levels"]').click();
  const levels = await page.evaluate(() => window.__author.levels());
  const ground = levels.levels.find((l) => l.name === "Ground");
  await page.locator(`.level-row[data-id="${ground.id}"] .lvl-elev`).fill("0.5");
  let s = await stats(page);
  expect(s.lastMeshUpserts, "no mesh re-upload").toBe(0);
  expect(s.lastBaseTransforms, "the plate and the run re-placed").toBe(2);
  await page.locator(`.level-row[data-id="${ground.id}"] .lvl-elev`).fill("0");
  await page.locator("#sheet-close").click();
  s = await stats(page);
  expect(s.errors).toEqual([]);
  expect(errors).toEqual([]);
});
