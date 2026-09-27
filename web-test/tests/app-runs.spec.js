// VimDesignWebTest — the authoring app, milestone 5 (Phase B): walls are
// the library's WallRun — one run per drawn polyline, mitered joins at
// any angle; openings are the run's structured openings; walls of the
// earlier tools (fixtures v1 / v2 legacy extrusions, v3 `Wall` chains)
// convert to one run when worked on. Both Playwright projects.

import { test, expect } from "@playwright/test";
import path from "node:path";
import { fileURLToPath } from "node:url";
import {
  mobile, shot, openApp, stats, walls, editState, openings, savedBytes,
  worldToClient, tapWorld, tool, shape, drawPlate,
} from "./lib/app-helpers.js";

const here = path.dirname(fileURLToPath(import.meta.url));
const fixture = (name) => path.join(here, "..", "..", "crates", "vim-design-test", "fixtures", name);
const SQRT3 = Math.sqrt(3);

async function importFixture(page, file) {
  await page.locator("#menu-btn").click();
  const [chooser] = await Promise.all([
    page.waitForEvent("filechooser"),
    page.locator('[data-testid="menu-import"]').click(),
  ]);
  await chooser.setFiles(file);
  await page.locator("#dialog-ok").click();
  await page.waitForFunction(() => window.__author.stats().walls > 0);
  await page.evaluate(() => {
    window.__author.app.set_camera_json('{"tx":4,"ty":3,"halfH":10}');
    window.__author.refresh();
  });
}

/** A clear view for drawing: the Model panel closed (desktop), the
 *  camera over x -4 .. 8. */
const clearView = async (page) => {
  if (!mobile() && await page.locator("#tree-panel").isVisible()) await page.locator("#tree-close").click();
  await page.evaluate((m) => {
    window.__author.app.set_camera_json(m ? '{"tx":2,"ty":0.5,"halfH":12}' : '{"tx":2,"ty":-1.5,"halfH":7}');
    window.__author.refresh();
  }, mobile());
};

/** Snapping off: angled points land where tapped. */
const snapOff = async (page) => {
  if ((await stats(page)).snap.enabled) await page.locator("#snap-toggle").click();
  expect((await stats(page)).snap.enabled).toBe(false);
};

const polyline = async (page, pts, close = false) => {
  await tool(page, "wall");
  await shape(page, "polygon");
  for (const [x, y] of pts) await tapWorld(page, x, y);
  if (close) await tapWorld(page, ...pts[0]);
  else await page.locator("#finish-draw").click();
};

test("runs at any angle: 60° and 135° corners, a closed pentagon — one element each, volume = footprint × H", async ({ page }) => {
  const errors = await openApp(page);
  await clearView(page);
  await snapOff(page);
  // Open: east 4 m, then 60° up to the left, then 135° back.
  await polyline(page, [[-3, -2], [1, -2], [2, -2 + SQRT3], [0.5, -2 + SQRT3 + 1.5]]);
  // A closed pentagon (snapped grid points near a regular one).
  await polyline(page, [[4, -2], [7, -2], [7.5, 1], [5.5, 3], [3.5, 1]], true);
  const ws = await walls(page);
  expect(ws.map((w) => [w.name, w.segments, w.closed])).toEqual([["Wall 1", 3, false], ["Wall 2", 5, true]]);
  for (const w of ws) {
    expect(w.run).toBe(true);
    expect(w.volume, `${w.name}: the mesh volume is the mitered footprint × H`).toBeCloseTo(w.footprintArea * 2.7, 3);
  }
  // The pentagon grows inward: its footprint is inside the drawn outline.
  const pent = ws[1];
  expect(pent.footprintArea).toBeLessThan(0.2 * 16);
  expect((await stats(page)).errors).toEqual([]);
  // A floor and a view of the angled house.
  await drawPlate(page, [-3, -2], [2, 0]);
  await page.locator('#view-toggle button[data-view="3d"]').click();
  await page.locator("#fit-btn").click();
  await shot(page, "house-angled-3d");
  await page.locator("#render-btn").click();
  await page.locator('#render-menu button[data-render="shaded-wire"]').click();
  await shot(page, "run-wireframe");
  await page.locator("#render-btn").click();
  await page.locator('#render-menu button[data-render="shaded"]').click();
  expect(errors).toEqual([]);
});

test("openings on a run: an angled segment, a door, a niche; placed inside the clear span; too short a segment refuses", async ({ page }) => {
  const errors = await openApp(page);
  await clearView(page);
  await snapOff(page);
  // A run with an angled 4 m segment and a short 0.9 m one.
  const b = [3 + 4 * 0.5, -2 + 4 * SQRT3 / 2];
  await polyline(page, [[-3, -2], [3, -2], b, [b[0], b[1] + 0.9]]);
  const [run] = await walls(page);
  expect(run.segments).toBe(3);
  await page.locator('.tool[data-tool="window"]').click();

  // A window on the angled segment, centred on the tap.
  const mid = [(3 + b[0]) / 2, (-2 + b[1]) / 2];
  const inward = [-SQRT3 / 2 * 0.1, 0.5 * 0.1];
  await tapWorld(page, mid[0] + inward[0], mid[1] + inward[1]);
  let os = await openings(page);
  expect(os).toHaveLength(1);
  expect(os[0]).toMatchObject({ kind: "window", segment: 1 });
  expect(os[0].offset + os[0].width / 2).toBeCloseTo(2, 1);
  // Near the corner: kept inside the clear span (clear of the join).
  await page.locator('[data-opening-preset="door"]').click();
  await tapWorld(page, -2.95, -1.9);
  os = await openings(page);
  const door = os.find((o) => o.kind === "door");
  expect(door.segment).toBe(0);
  expect(door.offset).toBeGreaterThanOrEqual(door.span[0] - 1e-9);
  expect(door.sill).toBe(0);
  // A niche: the window made 0.1 m deep.
  await page.locator('[data-opening-preset="window"]').click();
  await tapWorld(page, mid[0] + inward[0], mid[1] + inward[1]); // selects it
  await page.locator("#opening-through").click();
  expect((await openings(page)).find((o) => o.segment === 1).depth).toBeCloseTo(0.1, 9);
  await shot(page, "openings-angled");
  // The short segment has no room for a 1.2 m window.
  const before = (await openings(page)).length;
  await tapWorld(page, b[0] - 0.1, b[1] + 0.45);
  expect(await page.evaluate(() => window.__author.toasts.map((t) => t.msg))).toContainEqual(expect.stringContaining("too short"));
  expect(await openings(page)).toHaveLength(before);
  await page.locator("#edit-confirm").click();
  // The mesh: no evaluation errors, the openings cut the volume.
  const [after] = await walls(page);
  expect(after.volume).toBeLessThan(after.footprintArea * 2.7 - 0.2 * 1.2 * 1.2 * 0.5);
  expect((await stats(page)).errors).toEqual([]);
  expect(errors).toEqual([]);
});

test("conversion: the v3 wall chain (mixed tops, a door, windows) becomes one run; ✗ restores it; fixture v1's legacy walls convert too", async ({ page }) => {
  const errors = await openApp(page);
  await importFixture(page, fixture("authoring_project_v3.vimd"));
  let ws = await walls(page);
  expect(ws).toHaveLength(4);
  expect(ws.every((w) => !w.run && !w.legacy)).toBe(true);
  const bytes = await savedBytes(page);
  await tool(page, "select");
  await tapWorld(page, 4, 0.1);
  await page.locator("#prop-edit").click();
  const es = await editState(page);
  expect(es.run).toMatchObject({ segments: 4, closed: true, openings: 3 });
  ws = await walls(page);
  expect(ws).toHaveLength(1);
  expect((await openings(page)).map((o) => o.kind).sort()).toEqual(["door", "window", "window"]);
  expect((await stats(page)).errors).toEqual([]);
  await page.locator("#edit-cancel").click();
  expect(await savedBytes(page)).toBe(bytes);

  // v1: legacy extrusion walls with windows.
  await importFixture(page, fixture("authoring_project.vimd"));
  ws = await walls(page);
  expect(ws.every((w) => w.legacy)).toBe(true);
  await tool(page, "select");
  await tapWorld(page, 4, 0.1);
  await page.locator("#prop-edit").click();
  expect((await editState(page)).run).toMatchObject({ segments: 4, closed: true, openings: 2 });
  await page.locator("#edit-confirm").click();
  expect(await walls(page)).toHaveLength(1);
  // Reload: the converted run persists.
  await page.evaluate(() => window.__author.saveNow());
  await page.reload();
  await page.waitForFunction(() => window.__author?.ready === true, null, { timeout: 90_000 });
  ws = await walls(page);
  expect(ws.map((w) => [w.run, w.segments])).toEqual([[true, 4]]);
  expect(await openings(page)).toHaveLength(2);
  expect((await stats(page)).errors).toEqual([]);
  expect(errors).toEqual([]);
});
