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
// element list DERIVED from the document.

import { test, expect } from "@playwright/test";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { mkdirSync, readFileSync } from "node:fs";

const APP_URL = "http://localhost:8791/";
const DOC_KEY = "vim-design/doc/v1";
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

async function openApp(page) {
  const errors = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.goto(APP_URL);
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

test("draw plates with snapping, auto-targeted hole, validation, undo/redo", async ({ page }) => {
  const errors = await openApp(page);
  const step = await snapStep(page);

  // --- Rectangle plate: two opposite corners, slightly off-grid ---------
  await tool(page, "plate");
  await shape(page, "rect");
  const gen0 = (await stats(page)).committed;
  await tapWorld(page, -3.93, -1.92);
  await tapWorld(page, -1.08, 2.07);
  let ps = await plates(page);
  expect(ps).toHaveLength(1);
  expect(ps[0].name).toBe("Floor plate 1");
  expect(sortedOutline(ps[0].outline)).toEqual(sortedOutline([
    [snapTo(-3.93, step), snapTo(-1.92, step)], [snapTo(-1.08, step), snapTo(-1.92, step)],
    [snapTo(-1.08, step), snapTo(2.07, step)], [snapTo(-3.93, step), snapTo(2.07, step)],
  ]));
  expect(ps[0].thickness).toBeCloseTo(0.3, 9);
  expect((await stats(page)).committed).toBeGreaterThan(gen0);

  // --- Polygon plate (L-shape), closed by tapping the first vertex -------
  await shape(page, "polygon");
  const L = [[1.04, -1.97], [4.46, -2.03], [4.52, 0.98], [3.03, 1.04], [2.97, 3.02], [0.98, 2.96]];
  for (const [x, y] of L) await tapWorld(page, x, y);
  // Mid-sketch screenshot: rubber band + snapped cursor.
  const hover = await worldToClient(page, 1.1, -1.2);
  if (!mobile()) await page.mouse.move(hover[0], hover[1]);
  await shot(page, "drawing");
  await tapWorld(page, 1.02, -2.01); // first vertex: closes the loop
  ps = await plates(page);
  expect(ps).toHaveLength(2);
  const lPlate = ps.find((p) => p.name === "Floor plate 2");
  expect(sortedOutline(lPlate.outline)).toEqual(sortedOutline(L.map(([x, y]) => [snapTo(x, 0.5), snapTo(y, 0.5)])));
  expect(lPlate.area).toBeCloseTo(3.5 * 3 + 2 * 2, 6);

  // --- Hole: auto-targets the plate that contains it ---------------------
  await tool(page, "hole");
  await shape(page, "rect");
  await tapWorld(page, 3.52, -1.46);
  await tapWorld(page, 3.98, 0.47);
  ps = await plates(page);
  expect(ps.find((p) => p.name === "Floor plate 1").holes).toHaveLength(0);
  const holed = ps.find((p) => p.name === "Floor plate 2");
  expect(holed.holes).toHaveLength(1);
  expect(holed.area).toBeCloseTo(14.5 - 0.5 * 2, 6);
  await shot(page, "plates");

  // --- Invalid hole (outside every plate): rejected, nothing changes ------
  let gen = (await stats(page)).committed;
  await tapWorld(page, -0.4, -0.5);
  await tapWorld(page, 0.6, 0.5);
  expect(await page.evaluate(() => window.__author.toasts.map((t) => t.msg))).toContain(
    "A hole must lie inside a floor plate",
  );
  expect((await stats(page)).committed, "rejected hole never touched the document").toBe(gen);
  // Hole overlapping the existing hole: rejected too.
  await page.locator("#cancel-draw").click();
  await tapWorld(page, 3.02, -1.02);
  await tapWorld(page, 4.02, 0.02);
  expect(await page.evaluate(() => window.__author.toasts.map((t) => t.msg))).toContain(
    "Holes must not touch or overlap",
  );
  expect((await stats(page)).committed).toBe(gen);

  // --- Self-intersecting outline: red preview, Finish disabled ------------
  await page.locator("#cancel-draw").click(); // clear the pending corner
  await page.locator("#cancel-draw").click(); // no points: back to Select
  expect((await stats(page)).tool).toBe("select");
  await tool(page, "plate");
  await shape(page, "polygon");
  for (const [x, y] of [[-4, 3], [-1, 5], [-1, 3], [-4, 5]]) await tapWorld(page, x, y);
  const hud = await page.evaluate(() => window.__author.hud());
  expect(hud.canFinish).toBe(false);
  expect(hud.reason).toBe("The outline crosses itself");
  await expect(page.locator("#finish-draw")).toBeDisabled();
  await expect(page.locator("#draw-reason")).toHaveText("The outline crosses itself");
  await tapWorld(page, -4, 3); // tapping the first vertex tries to close
  expect((await stats(page)).committed, "bowtie rejected").toBe(gen);
  expect(await page.evaluate(() => window.__author.sketch().points.length)).toBe(4);
  await page.locator("#undo-point").click();
  expect(await page.evaluate(() => window.__author.sketch().points.length)).toBe(3);
  await page.locator("#cancel-draw").click();
  await page.locator("#cancel-draw").click();

  // --- Undo/redo: one step per gesture ---------------------------------
  await page.locator("#undo").click(); // the hole
  expect((await plates(page)).find((p) => p.name === "Floor plate 2").holes).toHaveLength(0);
  await page.locator("#undo").click(); // the L plate
  expect(await plates(page)).toHaveLength(1);
  await page.locator("#redo").click();
  expect(await plates(page)).toHaveLength(2);
  await page.locator("#redo").click();
  expect((await plates(page)).find((p) => p.name === "Floor plate 2").holes).toHaveLength(1);
  await expect(page.locator("#redo")).toBeDisabled();
  expect((await stats(page)).errors).toEqual([]);
  expect(errors).toEqual([]);
});

test("select a plate: properties, thickness, delete hole, delete plate", async ({ page }) => {
  const errors = await openApp(page);
  await tool(page, "plate");
  await shape(page, "rect");
  await tapWorld(page, -3, -2);
  await tapWorld(page, 3, 2);
  await tool(page, "hole");
  await tapWorld(page, -2, -1);
  await tapWorld(page, -1, 1);
  await tapWorld(page, 1, -1);
  await tapWorld(page, 2, 1);
  let [plate] = await plates(page);
  expect(plate.holes).toHaveLength(2);
  const entitiesBefore = (await page.evaluate(() => window.__author.debugInfo())).document.entities;

  await tool(page, "select");
  await tapWorld(page, 0, 1.5);
  expect((await stats(page)).selection).toBe(plate.id);
  await expect(page.locator("#sheet")).toBeVisible();
  await expect(page.locator("#sheet-title")).toHaveText("Floor plate 1");
  await expect(page.locator("#prop-area")).toHaveText("20.00 m²");
  // The tap that opened the sheet must not focus a field (no keyboard).
  expect(await page.evaluate(() => document.activeElement?.tagName)).not.toBe("INPUT");
  await shot(page, "properties");

  // Thickness edit: one undo step for the whole typed edit.
  const input = page.locator("#prop-thickness");
  await input.fill("0.45");
  await input.press("Enter");
  [plate] = await plates(page);
  expect(plate.thickness).toBeCloseTo(0.45, 9);
  await page.locator("#undo").click();
  expect((await plates(page))[0].thickness).toBeCloseTo(0.3, 9);
  await expect(input).toHaveValue("0.30"); // the panel follows the document (dirty pump)
  await page.locator("#redo").click();
  expect((await plates(page))[0].thickness).toBeCloseTo(0.45, 9);

  // Delete a hole: the face loses the wire AND its geometry is swept.
  await page.locator('[data-testid="delete-hole"]').first().click();
  [plate] = await plates(page);
  expect(plate.holes).toHaveLength(1);
  const entitiesAfter = (await page.evaluate(() => window.__author.debugInfo())).document.entities;
  expect(entitiesBefore - entitiesAfter, "wire + 4 edges + 4 lines + 4 points").toBe(13);
  await expect(page.locator("#prop-hole-count")).toHaveText("1");

  // Delete the plate (orphan sweep): nothing left but site + levels.
  await page.locator("#prop-delete").click();
  expect(await elements(page)).toHaveLength(0);
  await expect(page.locator("#sheet")).toBeHidden();
  const doc = (await page.evaluate(() => window.__author.debugInfo())).document;
  expect(doc.entities, "site + 2 levels remain").toBe(3);
  // Undo restores the plate in one step.
  await page.locator("#undo").click();
  expect(await plates(page)).toHaveLength(1);
  expect(errors).toEqual([]);
});

test("persistence: reload restores the model; corrupt storage starts fresh with a backup", async ({ page }) => {
  await openApp(page);
  await tool(page, "plate");
  await shape(page, "rect");
  await tapWorld(page, -2, -2);
  await tapWorld(page, 2, 1);
  const before = await plates(page);
  expect(before).toHaveLength(1);
  // The debounced save lands ~300 ms after the change.
  await page.waitForFunction((k) => !!localStorage.getItem(k), DOC_KEY, { timeout: 5000 });

  await page.reload();
  await page.waitForFunction(() => window.__author?.ready === true, null, { timeout: 90_000 });
  const after = await plates(page);
  expect(after.map((p) => [p.name, sortedOutline(p.outline), p.thickness]))
    .toEqual(before.map((p) => [p.name, sortedOutline(p.outline), p.thickness]));
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
  await tool(page, "plate");
  await shape(page, "rect");
  await tapWorld(page, -2, -1);
  await tapWorld(page, 3, 2);
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
  expect(after.map((p) => sortedOutline(p.outline))).toEqual(before.map((p) => sortedOutline(p.outline)));
});

test("Plan/3D toggle; drawing works in 3D; level elevation edit is transform-only", async ({ page }) => {
  const errors = await openApp(page);
  await tool(page, "plate");
  await shape(page, "rect");
  await tapWorld(page, -3, -2);
  await tapWorld(page, 2, 2);
  await page.locator('#view-toggle button[data-view="3d"]').click();
  expect((await stats(page)).view).toBe("3d");
  // Draw on Level 2 in the 3D view.
  await page.locator("#level-chip").click();
  await page.locator('#level-popover [data-level]').first().click(); // top of the list = Level 2
  const levels = await page.evaluate(() => window.__author.levels());
  expect(levels.levels.find((l) => l.id === levels.activeId).name).toBe("Level 2");
  await tapWorld(page, -1, -1, 3);
  await tapWorld(page, 1, 1, 3);
  expect(await plates(page)).toHaveLength(2);
  const upper = (await plates(page)).find((p) => p.levelName === "Level 2");
  expect(sortedOutline(upper.outline)).toEqual(sortedOutline([[-1, -1], [1, -1], [1, 1], [-1, 1]]));
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
  const cdp = await page.context().newCDPSession(page);
  const touch = (type, points) => cdp.send("Input.dispatchTouchEvent", {
    type, touchPoints: points.map(([x, y], id) => ({ x, y, id })),
  });
  const drag = async (from, to, steps = 8) => {
    const a = await worldToClient(page, ...from);
    const b = await worldToClient(page, ...to);
    await touch("touchStart", [a]);
    for (let i = 1; i <= steps; i++) {
      await touch("touchMove", [[a[0] + ((b[0] - a[0]) * i) / steps, a[1] + ((b[1] - a[1]) * i) / steps]]);
    }
    await touch("touchEnd", []);
  };

  // Rectangle: press at one corner, drag, release at the other.
  await tool(page, "plate");
  await shape(page, "rect");
  await drag([-2.1, -1.1], [1.9, 2.1]);
  let ps = await plates(page);
  expect(ps).toHaveLength(1);
  expect(sortedOutline(ps[0].outline)).toEqual(sortedOutline([[-2, -1], [2, -1], [2, 2], [-2, 2]]));

  // Polygon: the vertex lands where the finger is RELEASED (snapped).
  await shape(page, "polygon");
  await drag([4, 4], [3.1, 3.9]);
  let sk = await page.evaluate(() => window.__author.sketch());
  expect(sk.points).toEqual([[3, 4]]);

  // A second finger cancels the pending placement and navigates.
  const a = await worldToClient(page, 3.5, 5.5);
  const b = await worldToClient(page, 1, 6.5);
  await touch("touchStart", [a]);
  await touch("touchStart", [a, b]);
  await touch("touchMove", [[a[0] - 20, a[1] - 20], [b[0] + 20, b[1] + 20]]);
  await touch("touchEnd", []);
  sk = await page.evaluate(() => window.__author.sketch());
  expect(sk.points, "pinch placed nothing").toEqual([[3, 4]]);
  expect((await stats(page)).committed).toBeGreaterThan(0);
});
