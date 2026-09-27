// VimDesignWebTest — the authoring app, milestone 4: the Model tree
// (grouped by level), workplanes (construction planes nested in a
// level), walls as the library's Wall entity (height fixed or up to a
// plane), wall Edit Mode (windows, doors, niches, top anchors), and the
// in-place conversion of legacy walls.
// Runs in BOTH Playwright projects (desktop mouse, mobile touch). Tap
// targets come from WORLD coordinates (window.__author.worldToClient).

import { test, expect } from "@playwright/test";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { mkdirSync } from "node:fs";

const FIXTURE_V2 = path.join(path.dirname(fileURLToPath(import.meta.url)), "..", "..", "crates", "vim-design-test", "fixtures", "authoring_project_v2.vimd");

const APP_URL = "http://localhost:8791/";
const here = path.dirname(fileURLToPath(import.meta.url));
const screenshotDir = path.join(here, "..", "screenshots");
mkdirSync(screenshotDir, { recursive: true });

const mobile = () => test.info().project.name === "mobile";
const shot = async (page, name) => {
  await page.waitForTimeout(350); // let sheet/toast entrance animations finish
  await page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
  await page.screenshot({ path: path.join(screenshotDir, `app-${test.info().project.name}-${name}.png`) });
};

async function openApp(page, query = "") {
  const errors = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.goto(APP_URL + query);
  await page.waitForFunction(() => window.__author?.ready === true || window.__author?.error, null, {
    timeout: 90_000,
  });
  expect(await page.evaluate(() => window.__author.error)).toBeFalsy();
  if (mobile()) {
    await page.evaluate(() => {
      window.__author.app.set_camera_json('{"tx":0,"ty":0.5,"halfH":10}');
      window.__author.refresh();
    });
  }
  return errors;
}

const stats = (page) => page.evaluate(() => window.__author.stats());
const elements = (page) => page.evaluate(() => window.__author.elements());
const walls = async (page) => (await elements(page)).filter((e) => e.kind === "wall");
const tree = (page) => page.evaluate(() => window.__author.tree());
const levels = (page) => page.evaluate(() => window.__author.levels());
const editState = (page) => page.evaluate(() => window.__author.editState());
const profile = (page) => page.evaluate(() => window.__author.editProfile());
const savedBytes = (page) => page.evaluate(() => Array.from(window.__author.app.save_document()).join(","));
const sortedOutline = (o) => [...o].map(([x, y]) => [+x.toFixed(6), +y.toFixed(6)]).sort((a, b) => a[0] - b[0] || a[1] - b[1]);

async function worldToClient(page, x, y, z = 0) {
  const p = await page.evaluate(([x, y, z]) => window.__author.worldToClient(x, y, z), [x, y, z]);
  expect(p, `world (${x}, ${y}, ${z}) is on screen`).toBeTruthy();
  return p;
}

async function tapWorld(page, x, y, z = 0) {
  const [cx, cy] = await worldToClient(page, x, y, z);
  if (mobile()) await page.touchscreen.tap(cx, cy);
  else await page.mouse.click(cx, cy);
}

/** Tap a wall-local point (u along the wall, v up) in the elevation view. */
async function tapWall(page, wallId, u, v) {
  const w = await page.evaluate(([id, u, v]) => window.__author.wallToWorld(id, u, v), [wallId, u, v]);
  expect(w, `wall ${wallId} exists`).toBeTruthy();
  await tapWorld(page, w[0], w[1], w[2]);
}

async function tool(page, name) {
  await page.locator(`.tool[data-tool="${name}"]`).click();
  expect((await stats(page)).tool).toBe(name);
}

async function shape(page, name) {
  await page.locator(`#shape-toggle button[data-shape="${name}"]`).click();
  expect((await stats(page)).shape).toBe(name);
}

async function drawPlate(page, a, b) {
  await page.locator('.tool[data-tool="plate"]').click();
  expect((await editState(page)).active).toBe(true);
  await shape(page, "rect");
  await tapWorld(page, ...a);
  await tapWorld(page, ...b);
  await page.locator("#edit-confirm").click();
  expect((await editState(page)).active).toBe(false);
}

async function drawRoom(page, a, b) {
  await tool(page, "wall");
  await shape(page, "rect");
  await tapWorld(page, ...a);
  await tapWorld(page, ...b);
}

/** The Model tree: the desktop panel, or the phone's bottom sheet. */
async function openTree(page) {
  if (mobile()) {
    const open = (await page.locator("#sheet").isVisible()) && (await page.locator("#sheet-title").textContent()) === "Model";
    if (!open) await page.locator("#tree-btn").click();
    await expect(page.locator("#sheet-title")).toHaveText("Model");
    return page.locator("#sheet-body");
  }
  await expect(page.locator("#tree-panel")).toBeVisible();
  return page.locator("#tree-body");
}

async function cdpTouch(page) {
  const cdp = await page.context().newCDPSession(page);
  return (type, points) => cdp.send("Input.dispatchTouchEvent", {
    type, touchPoints: points.map(([x, y], id) => ({ x, y, id })),
  });
}

/** Press at client point `a`, drag to `b`, release (mouse or touch). */
async function dragClient(page, pa, pb, steps = 12) {
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

/** Press and hold at a client point. */
async function longPressClient(page, p, ms = 800) {
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

/** A wall-local point (u along, v up) -> client pixels. */
async function wallClient(page, wallId, u, v) {
  const w = await page.evaluate(([id, u, v]) => window.__author.wallToWorld(id, u, v), [wallId, u, v]);
  expect(w, `wall ${wallId} exists`).toBeTruthy();
  return worldToClient(page, w[0], w[1], w[2]);
}

async function editTool(page, name) {
  await page.locator(`[data-edit-tool="${name}"]`).click();
  expect((await editState(page)).tool).toBe(name);
}

async function editMode(page, name) {
  await page.locator(`[data-edit-mode="${name}"]`).click();
  expect((await editState(page)).mode).toBe(name);
}

const wallById = async (page, id) => (await walls(page)).find((w) => w.id === id);
const voidsOf = (w) => w.faces.filter((f) => f.kind === "void");
const toasts = (page) => page.evaluate(() => window.__author.toasts.map((t) => t.msg));

/** Menu → Levels: set a level's elevation (one committed edit). */
async function setLevelElevation(page, name, elevation) {
  await page.locator("#menu-btn").click();
  await page.locator('[data-testid="menu-levels"]').click();
  const lvl = (await levels(page)).levels.find((l) => l.name === name);
  const input = page.locator(`.level-row[data-id="${lvl.id}"] .lvl-elev`);
  await input.fill(String(elevation));
  await input.press("Enter");
  const s = await stats(page);
  await page.locator("#sheet-close").click();
  return s;
}

async function importFixture(page, file) {
  await page.locator("#menu-btn").click();
  const [chooser] = await Promise.all([
    page.waitForEvent("filechooser"),
    page.locator('[data-testid="menu-import"]').click(),
  ]);
  await chooser.setFiles(file);
  await page.locator("#dialog-ok").click();
  await page.waitForFunction(() => window.__author.stats().walls === 4);
}

test("model tree: levels top first, grouped elements; a row selects and frames; a level row activates it", async ({ page }) => {
  const errors = await openApp(page);
  // Desktop: the panel is open by default. Phone: it is a sheet.
  await expect(page.locator("#tree-panel")).toBeVisible({ visible: !mobile() });

  await drawPlate(page, [-3, -2], [3, 2]);
  await drawRoom(page, [-3, -2], [3, 2]);
  await tool(page, "select");
  const t = await tree(page);
  expect(t.levels.map((l) => l.name)).toEqual(["Level 2", "Ground"]);
  const ground = t.levels[1];
  expect(ground.active).toBe(true);
  expect(ground.groups.map((g) => [g.key, g.items.length])).toEqual([["floors", 1], ["walls", 1]]);
  expect(ground.groups[1].items[0]).toMatchObject({ name: "Wall 1", kind: "wall", meta: "4 segments · h 2.70 m" });
  expect(ground.groups[0].items[0]).toMatchObject({ name: "Floor plate 1", kind: "floor_plate", editable: true });
  expect(t.levels[0].groups.every((g) => g.items.length === 0)).toBe(true);

  let body = await openTree(page);
  await expect(body.locator(".tree-row.level").first()).toContainText("Level 2");
  await expect(body.locator(".tree-row.level").nth(1)).toContainText("Ground");
  await expect(body.locator(`[data-tree-item]`)).toHaveCount(2);
  // Floors and walls carry a pencil (Edit Mode).
  const plate = ground.groups[0].items[0];
  await expect(body.locator(`[data-tree-item="${plate.id}"] .tree-edit`)).toHaveCount(1);
  const wall = ground.groups[1].items[0];
  await expect(body.locator(`[data-tree-item="${wall.id}"] .tree-edit`)).toHaveCount(1);
  await shot(page, "tree");

  // Zoom out, then tap a wall row: selected, framed, properties shown.
  await page.evaluate(() => {
    window.__author.app.set_camera_json('{"tx":40,"ty":40,"halfH":4}');
    window.__author.refresh();
  });
  await body.locator(`[data-tree-item="${wall.id}"] .tree-name`).click();
  expect((await stats(page)).selection).toBe(wall.id);
  await expect(page.locator("#sheet-title")).toHaveText(wall.name);
  const w = (await walls(page)).find((x) => x.id === wall.id);
  const mid = await worldToClient(page, (w.points[0][0] + w.points[1][0]) / 2, (w.points[0][1] + w.points[1][1]) / 2);
  const vp = page.viewportSize();
  expect(mid[0] > 0 && mid[0] < vp.width && mid[1] > 0 && mid[1] < vp.height, "the wall is framed").toBe(true);
  if (!mobile()) await expect(page.locator(`#tree-body [data-tree-item="${wall.id}"]`)).toHaveClass(/selected/);

  // A level row makes it the active plane (session only: no undo step).
  const undoBefore = (await stats(page)).canUndo;
  body = await openTree(page);
  const level2 = t.levels[0];
  await body.locator(`[data-tree-level="${level2.id}"] .tree-name`).click();
  const l = await levels(page);
  expect(l.activeId).toBe(level2.id);
  expect(l.activePlane).toMatchObject({ id: level2.id, name: "Level 2", isLevel: true, elevation: 3 });
  await expect(page.locator("#level-chip .chip-name")).toHaveText("Level 2");
  expect((await stats(page)).canUndo).toBe(undoBefore);

  // Collapse Ground (the twist) — it stays collapsed after a reload.
  body = await openTree(page);
  await body.locator(`[data-tree-level="${ground.id}"] [data-twist]`).click();
  await expect(body.locator("[data-tree-item]")).toHaveCount(0);
  await page.evaluate(() => window.__author.saveNow());
  await page.waitForTimeout(600); // the session save is debounced
  await page.reload();
  await page.waitForFunction(() => window.__author?.ready === true, null, { timeout: 90_000 });
  body = await openTree(page);
  await expect(body.locator(`[data-tree-level="${ground.id}"]`)).toBeVisible();
  await expect(body.locator("[data-tree-item]")).toHaveCount(0);
  await body.locator(`[data-tree-level="${ground.id}"] [data-twist]`).click();
  await expect(body.locator("[data-tree-item]")).toHaveCount(2);

  if (mobile()) {
    // Selecting a row closes the sheet and reveals the element.
    await body.locator(`[data-tree-item="${plate.id}"] .tree-name`).click();
    expect((await stats(page)).selection).toBe(plate.id);
    await expect(page.locator("#sheet-title")).toHaveText("Floor plate 1");
  } else {
    // The panel closes and reopens from the top bar.
    await page.locator("#tree-close").click();
    await expect(page.locator("#tree-panel")).toBeHidden();
    await page.locator("#tree-btn").click();
    await expect(page.locator("#tree-panel")).toBeVisible();
  }
  expect(errors).toEqual([]);
});

test("workplanes: add in the tree, rename, nest, draw a ceiling on one; it follows its level; offset; cascade delete + undo", async ({ page }) => {
  const errors = await openApp(page);
  const ground = (await levels(page)).levels.find((l) => l.name === "Ground");

  // "+" on the Ground row: a workplane 2.40 m up, active, its sheet open.
  let body = await openTree(page);
  await body.locator(`[data-tree-level="${ground.id}"] [data-tree-add]`).click();
  await expect(page.locator("#sheet-title")).toHaveText("Workplane 1");
  let planes = await page.evaluate(() => JSON.parse(window.__author.app.workplanes_json()));
  expect(planes).toHaveLength(1);
  const ceiling = planes[0];
  expect(ceiling).toMatchObject({ name: "Workplane 1", parent: ground.id, offset: 2.4, elevation: 2.4, active: true });
  await page.locator("#wp-name").fill("Ceiling");
  await page.locator("#wp-name").press("Enter");
  await expect(page.locator("#level-chip .chip-name")).toHaveText(mobile() ? "Ceiling" : "Ground › Ceiling");
  await expect(page.locator("#level-chip")).toHaveAttribute("title", "Drawing on Ground › Ceiling");
  await expect(page.locator("#level-chip .chip-elev")).toHaveText("+2.40 m");

  // A workplane inside it (0.30 m further up): nested in the tree.
  await page.locator("#wp-add-nested").click();
  planes = await page.evaluate(() => JSON.parse(window.__author.app.workplanes_json()));
  const nested = planes.find((p) => p.parent === ceiling.id);
  // Names are derived: "Workplane 1" is free again after the rename.
  expect(nested).toMatchObject({ offset: 0.3, path: "Ground › Ceiling › Workplane 1" });
  expect(nested.elevation).toBeCloseTo(2.7, 9);
  await page.locator("#sheet-close").click();
  const t = await tree(page);
  const g = t.levels.find((l) => l.id === ground.id);
  expect(g.planes.map((p) => [p.name, p.planes.map((q) => q.name)])).toEqual([["Ceiling", ["Workplane 1"]]]);
  body = await openTree(page);
  await expect(body.locator(`[data-tree-plane="${nested.id}"]`)).toHaveAttribute("style", /--depth:\s*2/);

  // Draw on the ceiling: a plate whose top is the workplane, associated
  // with Ground.
  await body.locator(`[data-tree-plane="${ceiling.id}"] .tree-name`).click();
  expect((await levels(page)).activePlane).toMatchObject({ id: ceiling.id, isLevel: false });
  if (mobile()) await page.locator("#sheet-close").click();
  await drawPlate(page, [-2, -2], [2, 2]);
  let [plate] = (await elements(page)).filter((e) => e.kind === "floor_plate");
  expect(plate.levelId).toBe(ground.id);
  expect(plate.bbox[1][2]).toBeCloseTo(2.4, 4);
  await openTree(page);
  await shot(page, "workplanes-tree");
  if (mobile()) await page.locator("#sheet-close").click();

  // The ceiling follows its level: a Ground drag is transform-only.
  let s = await setLevelElevation(page, "Ground", 1);
  expect(s.lastMeshUpserts, "no re-tessellation").toBe(0);
  [plate] = (await elements(page)).filter((e) => e.kind === "floor_plate");
  expect(plate.bbox[1][2]).toBeCloseTo(3.4, 4);

  // Offset edit in the workplane sheet: one undo step.
  body = await openTree(page);
  await body.locator(`[data-tree-plane="${ceiling.id}"] [data-tree-plane-edit]`).click();
  await page.locator("#wp-offset").fill("2.6");
  await page.locator("#wp-offset").press("Enter");
  [plate] = (await elements(page)).filter((e) => e.kind === "floor_plate");
  expect(plate.bbox[1][2]).toBeCloseTo(3.6, 4);
  await page.locator("#undo").click();
  [plate] = (await elements(page)).filter((e) => e.kind === "floor_plate");
  expect(plate.bbox[1][2]).toBeCloseTo(3.4, 4);
  await page.locator("#sheet-close").click();

  // A wall on Ground up to the ceiling: 2.40 m high.
  await page.locator("#level-chip").click();
  await page.locator(`.pop-item[data-level="${ground.id}"]`).click();
  await tool(page, "wall");
  await page.locator('#wall-mode-toggle button[data-wall-mode="upto"]').click();
  await page.locator("#wall-top-plane").selectOption(String(ceiling.id));
  await shape(page, "polygon");
  await tapWorld(page, -3, 3, 1);
  await tapWorld(page, 3, 3, 1);
  await page.locator("#finish-draw").click();
  let [topped] = await walls(page);
  expect(topped).toMatchObject({ mode: "upto", topPlane: ceiling.id });
  expect(topped.height).toBeCloseTo(2.4, 9);
  await page.locator('#wall-mode-toggle button[data-wall-mode="fixed"]').click();
  await tool(page, "select");

  // Delete the ceiling: the honest cascade prompt, one undo restores all.
  body = await openTree(page);
  await body.locator(`[data-tree-plane="${ceiling.id}"] [data-tree-plane-edit]`).click();
  await page.locator("#wp-delete").click();
  const msg = await page.locator("#dialog-message").textContent();
  expect(msg).toContain("1 element drawn on it");
  expect(msg).toContain("1 nested workplane");
  expect(msg).toContain("Walls that go up to it keep their current height (1 wall)");
  await page.locator("#dialog-ok").click();
  expect(await page.evaluate(() => JSON.parse(window.__author.app.workplanes_json()))).toHaveLength(0);
  expect((await elements(page)).filter((e) => e.kind === "floor_plate")).toHaveLength(0);
  [topped] = await walls(page);
  expect(topped).toMatchObject({ mode: "fixed" });
  expect(topped.height).toBeCloseTo(2.4, 9);
  expect((await levels(page)).activePlane.isLevel, "the active plane falls back to its level").toBe(true);
  await page.locator("#undo").click();
  expect(await page.evaluate(() => JSON.parse(window.__author.app.workplanes_json()))).toHaveLength(2);
  expect((await elements(page)).filter((e) => e.kind === "floor_plate")).toHaveLength(1);
  [topped] = await walls(page);
  expect(topped).toMatchObject({ mode: "upto", topPlane: ceiling.id });
  expect((await stats(page)).errors).toEqual([]);
  expect(errors).toEqual([]);
});

test("walls up to a plane: new walls and properties; the top level drags the height, the sill stays; re-mesh vs transform-only", async ({ page }) => {
  const errors = await openApp(page);
  await tool(page, "wall");
  await expect(page.locator("#wall-height-mode")).toBeVisible();
  await expect(page.locator("#wall-top-plane")).toBeHidden();
  await page.locator('#wall-mode-toggle button[data-wall-mode="upto"]').click();
  let ws = await page.evaluate(() => window.__author.wallSettings());
  const l = await levels(page);
  const level2 = l.levels.find((x) => x.name === "Level 2");
  const ground = l.levels.find((x) => x.name === "Ground");
  expect(ws).toMatchObject({ mode: "upto", topPlane: level2.id });
  expect(ws.effectiveHeight).toBeCloseTo(3, 9);
  await expect(page.locator("#wall-height-stepper")).toBeHidden();
  await page.locator("#wall-top-offset").fill("-0.5");
  await page.locator("#wall-top-offset").press("Enter");
  ws = await page.evaluate(() => window.__author.wallSettings());
  expect(ws.effectiveHeight).toBeCloseTo(2.5, 9);
  await expect(page.locator("#hint")).toContainText("2.50 m high");

  // A new run is up to Level 2 (its height follows it).
  await shape(page, "rect");
  await tapWorld(page, -3, -2);
  await tapWorld(page, 3, 2);
  const [room] = await walls(page);
  expect(room).toMatchObject({ run: true, mode: "upto", topPlane: level2.id, topOffset: -0.5, legacy: false, segments: 4 });
  expect(room.height).toBeCloseTo(2.5, 9);
  await shot(page, "wall-height-mode");

  // A top plane below the base is refused.
  await page.locator("#wall-top-plane").selectOption(String(ground.id));
  ws = await page.evaluate(() => window.__author.wallSettings());
  expect(ws.effectiveHeight).toBeNull();
  await page.locator('#wall-mode-toggle button[data-wall-mode="fixed"]').click();

  // A window in the bottom segment (the Window tool: Openings mode).
  await page.locator('.tool[data-tool="window"]').click();
  await tapWorld(page, 0, -1.92);
  await page.locator("#edit-confirm").click();
  const runOpenings = () => page.evaluate(() => JSON.parse(window.__author.app.openings_json()));
  expect(await runOpenings()).toHaveLength(1);

  // Drag Level 2 up: the run re-meshes (its top follows), the window
  // keeps its sill.
  let s = await setLevelElevation(page, "Level 2", 3.5);
  expect(s.lastMeshUpserts, "a run with a top constraint re-meshes").toBe(1);
  let w = await wallById(page, room.id);
  expect(w.height).toBeCloseTo(3.0, 9);
  expect((await runOpenings())[0].sill).toBeCloseTo(0.9, 9);
  expect(w.bbox[1][2]).toBeCloseTo(3.0, 4);

  // Properties: Up to Level 2, offset 0.2 -> 3.7 m; one undo restores it.
  await tool(page, "select");
  await tapWorld(page, 0, -1.92);
  expect((await stats(page)).selection).toBe(room.id);
  await expect(page.locator('#prop-wall-mode button[data-wall-mode="upto"]')).toHaveClass(/on/);
  await expect(page.locator("#prop-height")).toBeHidden();
  await expect(page.locator("#prop-wall-top")).toHaveValue(String(level2.id));
  await page.locator("#prop-wall-offset").fill("0.2");
  await page.locator("#prop-wall-offset").press("Enter");
  expect((await wallById(page, room.id)).height).toBeCloseTo(3.7, 9);
  await expect(page.locator("#prop-wall-effective")).toHaveText("3.70 m");
  await shot(page, "wall-height-props");
  await page.locator("#undo").click();
  expect((await wallById(page, room.id)).height).toBeCloseTo(3.0, 9);

  // Fixed: the run keeps its height and no longer follows Level 2; a
  // drag of its base level is then transform-only.
  await page.locator('#prop-wall-mode button[data-wall-mode="fixed"]').click();
  w = await wallById(page, room.id);
  expect(w).toMatchObject({ mode: "fixed", topPlane: null });
  expect(w.height).toBeCloseTo(3.0, 9);
  await expect(page.locator("#prop-height")).toBeVisible();
  await page.locator("#sheet-close").click();
  s = await setLevelElevation(page, "Level 2", 3);
  expect((await wallById(page, room.id)).height).toBeCloseTo(3.0, 9);
  expect(s.lastMeshUpserts, "nothing follows Level 2 any more").toBe(0);
  s = await setLevelElevation(page, "Ground", 0.5);
  expect(s.lastMeshUpserts, "a base-only run moves by transform").toBe(0);
  expect(s.lastBaseTransforms).toBe(1);
  expect((await stats(page)).errors).toEqual([]);
  expect(errors).toEqual([]);
});

test("legacy walls (v2 fixture): shown and selectable; the pencil converts them (with their windows) into the run's plan Edit Mode; ✗ undoes it, ✓ keeps it; Openings mode converts too", async ({ page }) => {
  const errors = await openApp(page);
  await importFixture(page, FIXTURE_V2);
  if (mobile()) {
    await page.evaluate(() => {
      window.__author.app.set_camera_json('{"tx":4,"ty":3,"halfH":10}');
      window.__author.refresh();
    });
  }
  let ws = await walls(page);
  expect(ws.map((w) => [w.name, w.legacy])).toEqual([["Wall 1", true], ["Wall 2", true], ["Wall 3", true], ["Wall 4", true]]);
  const w1 = ws[0];
  expect(w1.windows).toHaveLength(1);
  const bytes = await savedBytes(page);

  // The pencil converts the level's legacy walls inside the session and
  // edits their run: the room becomes ONE closed wall run of four
  // segments, with its two windows in place.
  const runOpenings = () => page.evaluate(() => JSON.parse(window.__author.app.openings_json()));
  await tool(page, "select");
  await tapWorld(page, 4, 0.1);
  expect((await stats(page)).selection).toBe(w1.id);
  await expect(page.locator("#prop-legacy-note")).toBeVisible();
  await page.locator("#prop-edit").click();
  let es = await editState(page);
  expect(es).toMatchObject({ active: true, target: "run" });
  expect(es.run).toMatchObject({ segments: 4, closed: true, thickness: 0.2, openings: 2 });
  expect(es.canUndo, "the conversion is not an edit to step back over").toBe(false);
  let w = await wallById(page, w1.id);
  expect([w.run, w.legacy, w.name, w.levelId]).toEqual([true, false, "Wall 1", w1.levelId]);
  expect(await walls(page)).toHaveLength(1);
  expect(w.height).toBeCloseTo(2.7, 9);
  let os = await runOpenings();
  expect(os).toHaveLength(2);
  for (const o of os) {
    expect([o.kind, o.depth]).toEqual(["window", null]);
    for (const [k, v] of [["width", 1.2], ["height", 1.0], ["sill", 0.9]]) expect(o[k]).toBeCloseTo(v, 9);
  }
  // Wall 1's window was 2.0 m from its trimmed start (0.2, 0): 2.2 m from
  // the corner.
  const seg0 = w.points.findIndex(([x, y]) => x === 0 && y === 0);
  expect(os.find((o) => o.segment === seg0).offset).toBeCloseTo(2.2, 9);
  // ✗: back to the legacy walls, byte for byte.
  await page.locator("#edit-cancel").click();
  expect(await savedBytes(page)).toBe(bytes);
  expect((await walls(page)).every((x) => x.legacy)).toBe(true);

  // ✓: the run stays; undo steps back over the conversion (two steps:
  // legacy walls to walls, walls to one run).
  await tapWorld(page, 4, 0.1);
  await page.locator("#prop-edit").click();
  await page.locator("#edit-confirm").click();
  expect(await walls(page)).toHaveLength(1);
  expect((await stats(page)).errors).toEqual([]);
  await page.locator("#undo").click();
  await page.locator("#undo").click();
  expect(await savedBytes(page)).toBe(bytes);

  // Openings mode converts a legacy wall (its whole room) when an opening
  // goes into it.
  // (Beside Wall 2's window, which spans y 2.2 .. 3.4.)
  await page.locator('.tool[data-tool="window"]').click();
  await page.evaluate(() => {
    window.__author.app.set_camera_json('{"tx":4,"ty":3,"halfH":10}');
    window.__author.refresh();
  });
  await tapWorld(page, 7.9, 4.8);
  expect(await walls(page)).toHaveLength(1);
  expect(await runOpenings()).toHaveLength(3);
  await page.locator("#edit-cancel").click();
  await page.locator("#dialog-ok").click();
  expect(await savedBytes(page)).toBe(bytes);

  // Legacy walls keep working: delete one, undo.
  await tool(page, "select");
  await tapWorld(page, 4, 0.1);
  await page.locator("#prop-delete").click();
  expect(await walls(page)).toHaveLength(3);
  await page.locator("#undo").click();
  expect(await savedBytes(page)).toBe(bytes);
  await page.locator('#view-toggle button[data-view="3d"]').click();
  await page.locator("#fit-btn").click();
  await shot(page, "house-3d");
  expect(errors).toEqual([]);
});
