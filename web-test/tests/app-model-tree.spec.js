// VimDesignWebTest — the authoring app, milestone 4 (Phase A): the Model
// tree (grouped by level), the active construction plane, and — behind
// the ?m4preview flag — the wall height modes and wall Edit Mode (an
// in-memory preview: nothing is written to the document yet).
// Runs in BOTH Playwright projects (desktop mouse, mobile touch). Tap
// targets come from WORLD coordinates (window.__author.worldToClient).

import { test, expect } from "@playwright/test";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { mkdirSync } from "node:fs";

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

test("model tree: levels top first, grouped elements; a row selects and frames; a level row activates it", async ({ page }) => {
  const errors = await openApp(page);
  // Desktop: the panel is open by default. Phone: it is a sheet.
  await expect(page.locator("#tree-panel")).toBeVisible({ visible: !mobile() });
  // Without the preview flag, walls have no height-mode row.
  await tool(page, "wall");
  await expect(page.locator("#wall-height-mode")).toBeHidden();
  await tool(page, "select");

  await drawPlate(page, [-3, -2], [3, 2]);
  await drawRoom(page, [-3, -2], [3, 2]);
  await tool(page, "select");
  const t = await tree(page);
  expect(t.levels.map((l) => l.name)).toEqual(["Level 2", "Ground"]);
  const ground = t.levels[1];
  expect(ground.active).toBe(true);
  expect(ground.groups.map((g) => [g.key, g.items.length])).toEqual([["floors", 1], ["walls", 4]]);
  expect(ground.groups[0].items[0]).toMatchObject({ name: "Floor plate 1", kind: "floor_plate", editable: true });
  expect(t.levels[0].groups.every((g) => g.items.length === 0)).toBe(true);

  let body = await openTree(page);
  await expect(body.locator(".tree-row.level").first()).toContainText("Level 2");
  await expect(body.locator(".tree-row.level").nth(1)).toContainText("Ground");
  await expect(body.locator(`[data-tree-item]`)).toHaveCount(5);
  // Floors carry a pencil; walls only in the preview.
  const plate = ground.groups[0].items[0];
  await expect(body.locator(`[data-tree-item="${plate.id}"] .tree-edit`)).toHaveCount(1);
  const wall = ground.groups[1].items[2];
  await expect(body.locator(`[data-tree-item="${wall.id}"] .tree-edit`)).toHaveCount(0);
  await shot(page, "tree");

  // Zoom out, then tap a wall row: selected, framed, properties shown.
  await page.evaluate(() => {
    window.__author.app.set_camera_json('{"tx":40,"ty":40,"halfH":4}');
    window.__author.refresh();
  });
  await body.locator(`[data-tree-item="${wall.id}"]`).click();
  expect((await stats(page)).selection).toBe(wall.id);
  await expect(page.locator("#sheet-title")).toHaveText(wall.name);
  const w = (await walls(page)).find((x) => x.id === wall.id);
  const mid = await worldToClient(page, (w.start[0] + w.end[0]) / 2, (w.start[1] + w.end[1]) / 2);
  const vp = page.viewportSize();
  expect(mid[0] > 0 && mid[0] < vp.width && mid[1] > 0 && mid[1] < vp.height, "the wall is framed").toBe(true);
  if (!mobile()) await expect(page.locator(`#tree-body [data-tree-item="${wall.id}"]`)).toHaveClass(/selected/);

  // A level row makes it the active plane (session only: no undo step).
  const undoBefore = (await stats(page)).canUndo;
  body = await openTree(page);
  const level2 = t.levels[0];
  await body.locator(`[data-tree-level="${level2.id}"]`).click();
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
  await expect(body.locator("[data-tree-item]")).toHaveCount(5);

  if (mobile()) {
    // Selecting a row closes the sheet and reveals the element.
    await body.locator(`[data-tree-item="${plate.id}"]`).click();
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

test("preview: wall height Up to a plane — new walls and the wall properties", async ({ page }) => {
  const errors = await openApp(page, "?m4preview");
  await tool(page, "wall");
  await expect(page.locator("#wall-height-mode")).toBeVisible();
  await expect(page.locator("#wall-top-plane")).toBeHidden();
  await page.locator('#wall-mode-toggle button[data-wall-mode="upto"]').click();
  let ws = await page.evaluate(() => window.__author.wallSettings());
  const l = await levels(page);
  const level2 = l.levels.find((x) => x.name === "Level 2");
  expect(ws).toMatchObject({ mode: "upto", topPlane: level2.id });
  expect(ws.effectiveHeight).toBeCloseTo(3, 9);
  await expect(page.locator("#wall-height-stepper")).toBeHidden();
  await expect(page.locator("#wall-top-plane")).toBeVisible();
  await page.locator("#wall-top-offset").fill("-0.5");
  await page.locator("#wall-top-offset").press("Enter");
  ws = await page.evaluate(() => window.__author.wallSettings());
  expect(ws.topOffset).toBeCloseTo(-0.5, 9);
  expect(ws.effectiveHeight).toBeCloseTo(2.5, 9);
  await page.locator('#wall-top-offset-stepper button[data-top-offset="1"]').click();
  ws = await page.evaluate(() => window.__author.wallSettings());
  expect(ws.effectiveHeight).toBeCloseTo(2.55, 9);
  await page.locator('#wall-top-offset-stepper button[data-top-offset="-1"]').click();
  await expect(page.locator("#hint")).toContainText("2.50 m high");

  // New walls take the Up-to height.
  await shape(page, "rect");
  await tapWorld(page, -3, -2);
  await tapWorld(page, 3, 2);
  const made = await walls(page);
  expect(made).toHaveLength(4);
  for (const w of made) expect(w.height).toBeCloseTo(2.5, 9);
  await shot(page, "wall-height-mode");

  // A top plane below the base is refused (Ground from Ground).
  const ground = l.levels.find((x) => x.name === "Ground");
  await page.locator("#wall-top-plane").selectOption(String(ground.id));
  ws = await page.evaluate(() => window.__author.wallSettings());
  expect(ws.effectiveHeight).toBeNull();
  await page.locator("#wall-top-plane").selectOption(String(level2.id));

  // Wall properties: Up to Level 2 + 0.2 -> 3.2 m; one undo restores it.
  await tool(page, "select");
  const bottom = made.find((w) => w.start[1] === -2 && w.end[1] === -2);
  await tapWorld(page, 0, -1.92);
  expect((await stats(page)).selection).toBe(bottom.id);
  await expect(page.locator("#prop-wall-height-mode")).toBeVisible();
  await page.locator('#prop-wall-mode button[data-wall-mode="upto"]').click();
  await expect(page.locator("#prop-height")).toBeHidden();
  await expect(page.locator("#prop-wall-top")).toHaveValue(String(level2.id));
  let h = (await walls(page)).find((w) => w.id === bottom.id).height;
  expect(h).toBeCloseTo(3, 9);
  await page.locator("#prop-wall-offset").fill("0.2");
  await page.locator("#prop-wall-offset").press("Enter");
  h = (await walls(page)).find((w) => w.id === bottom.id).height;
  expect(h).toBeCloseTo(3.2, 9);
  await expect(page.locator("#prop-wall-effective")).toHaveText("3.20 m");
  await shot(page, "wall-height-props");
  await page.locator("#undo").click();
  h = (await walls(page)).find((w) => w.id === bottom.id).height;
  expect(h).toBeCloseTo(3, 9);
  expect((await stats(page)).errors).toEqual([]);
  expect(errors).toEqual([]);
});

test("preview: wall Edit Mode — elevation, window and door presets, anchors, memory ✓ leaves the document", async ({ page }) => {
  const errors = await openApp(page, "?m4preview");
  await drawPlate(page, [-3, -2], [3, 2]);
  await drawRoom(page, [-3, -2], [3, 2]);
  await tool(page, "select");
  const bottom = (await walls(page)).find((w) => w.start[1] === -2 && w.end[1] === -2);
  await tapWorld(page, 0, -1.92);
  expect((await stats(page)).selection).toBe(bottom.id);
  const before = await savedBytes(page);

  // The pencil: the elevation facing the wall, the same edit dock.
  await page.locator("#prop-edit").click();
  let es = await editState(page);
  expect(es).toMatchObject({ active: true, target: "wall", memory: true, name: bottom.name });
  expect((await stats(page)).view).toBe("elevation");
  await expect(page.locator('#edit-view-toggle button[data-view="elevation"]')).toHaveText("Wall");
  await expect(page.locator('[data-edit-tool="window"]')).toBeVisible();
  await expect(page.locator('[data-edit-tool="door"]')).toBeVisible();
  let prof = await profile(page);
  expect(prof.faces).toHaveLength(1);

  // Window preset: 1.2 x 1.2 m at a 0.9 m sill, centred where tapped.
  await page.locator('[data-edit-tool="window"]').click();
  expect((await editState(page)).tool).toBe("window");
  await tapWall(page, bottom.id, 1.5, 1.5);
  prof = await profile(page);
  const voids = () => prof.faces.filter((f) => f.kind.void !== undefined);
  expect(voids()).toHaveLength(1);
  expect(sortedOutline(voids()[0].outline)).toEqual(sortedOutline([[0.9, 0.9], [2.1, 0.9], [2.1, 2.1], [0.9, 2.1]]));

  // Door preset: 0.9 x 2.1 m, reaching below the wall bottom.
  await page.locator('[data-edit-tool="door"]').click();
  await tapWall(page, bottom.id, 4, 1);
  prof = await profile(page);
  expect(voids()).toHaveLength(2);
  const door = voids()[1].outline;
  expect(Math.min(...door.map((p) => p[1]))).toBeLessThan(0);
  expect(Math.max(...door.map((p) => p[1]))).toBeCloseTo(2.1, 9);
  expect(Math.max(...door.map((p) => p[0])) - Math.min(...door.map((p) => p[0]))).toBeCloseTo(0.9, 9);
  es = await editState(page);
  expect(es.canUndo).toBe(true);

  // Anchors: the top corners follow the height (square handles).
  await page.locator('[data-edit-tool="door"]').click(); // back to select
  await page.locator('[data-edit-mode="points"]').click();
  let hud = await page.evaluate(() => window.__author.editHud());
  expect(hud.points.filter((p) => p.top)).toHaveLength(2);
  await expect(page.locator("#edit-anchor-row")).toBeHidden();
  await tapWall(page, bottom.id, 0, bottom.height);
  es = await editState(page);
  expect(es.anchor).toEqual({ selected: 1, top: 1 });
  await expect(page.locator("#edit-anchor-row")).toBeVisible();
  await expect(page.locator('#edit-anchor button[data-anchor="top"]')).toHaveClass(/on/);
  await page.locator('#edit-anchor button[data-anchor="bottom"]').click();
  expect((await editState(page)).anchor).toEqual({ selected: 1, top: 0 });
  await page.locator('#edit-anchor button[data-anchor="top"]').click();
  expect((await editState(page)).anchor).toEqual({ selected: 1, top: 1 });
  await shot(page, "wall-edit");

  // Undo inside the session (in memory) removes the door.
  await page.locator("#edit-undo").click();
  prof = await profile(page);
  expect(voids()).toHaveLength(1);

  // ✓ in the preview writes nothing: the document is byte-identical.
  await page.locator("#edit-confirm").click();
  expect((await editState(page)).active).toBe(false);
  expect(await page.evaluate(() => window.__author.toasts.map((t) => t.msg ?? t))).toContainEqual(
    expect.stringContaining("Preview: wall edits are not saved yet"),
  );
  expect(await savedBytes(page)).toBe(before);
  expect((await stats(page)).view).toBe("plan");
  expect(errors).toEqual([]);
});
