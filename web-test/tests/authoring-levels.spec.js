// VimDesignWebTest — Phase B authoring UI: project settings (Site),
// level manager (sorted list, add/edit/color/story/delete), level
// overlays, active-level session state, and the acceptance proof that
// the scene is authored ON levels: dragging Ground's elevation moves
// every mesh, dragging an empty level moves nothing.
//
// The cascade-delete flow follows docs/AUTHORING.md §2: DeleteLevel
// without cascade is rejected while dependents exist; the app shows an
// honest confirm dialog and submits the cascade form on acceptance —
// ONE undo step, fully restorable.

import { test, expect } from "@playwright/test";
import path from "node:path";
import crypto from "node:crypto";
import { fileURLToPath } from "node:url";
import { mkdirSync } from "node:fs";

const here = path.dirname(fileURLToPath(import.meta.url));
const screenshotDir = path.join(here, "..", "screenshots");

// 3D-content region between the two UI panels (sliders left, authoring
// right) and above the status bar.
const SCENE_CLIP = { x: 340, y: 0, width: 590, height: 600 };

async function canvasHash(page) {
  const buf = await page.screenshot({ clip: SCENE_CLIP });
  return crypto.createHash("sha256").update(buf).digest("hex");
}

async function waitSettled(page) {
  await page.waitForFunction(
    () => window.__vimStats?.settled === true && !window.__vimError,
    { timeout: 30_000 },
  );
  await page.evaluate(
    () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))),
  );
}

/** Set an input's value and fire input + change events. */
async function setField(page, locator, value) {
  await locator.evaluate((el, v) => {
    el.value = String(v);
    el.dispatchEvent(new Event("input", { bubbles: true }));
    el.dispatchEvent(new Event("change", { bubbles: true }));
    el.blur();
  }, value);
}

const levels = (page) => page.evaluate(() => window.__vim.levels());
const bbox = (page) => page.evaluate(() => window.__vim.bbox());

function levelIdByName(state, name) {
  const lvl = state.levels.find((l) => l.name === name);
  expect(lvl, `level "${name}" exists`).toBeTruthy();
  return lvl.id;
}

const rowFor = (page, id) => page.locator(`.level-row[data-id="${id}"]`);

test("level-based authoring: site, level manager, ground drag, cascade delete, undo", async ({ page }) => {
  const pageErrors = [];
  page.on("pageerror", (e) => pageErrors.push(String(e)));
  let dialogCount = 0;

  await page.goto("/index.html");
  await page.waitForFunction(() => window.__vimReady === true || window.__vimError, {
    timeout: 90_000,
  });
  expect(await page.evaluate(() => window.__vimError)).toBeFalsy();

  // --- Project settings panel: Montreal defaults, seeded by the app ---
  await expect(page.locator("#site-lat")).toHaveValue("45.5019");
  await expect(page.locator("#site-lon")).toHaveValue("-73.5674");
  await expect(page.locator("#site-elev")).toHaveValue("36");

  // Edit a field and read it back through the pump.
  await setField(page, page.locator("#site-elev"), 52);
  await waitSettled(page);
  expect((await page.evaluate(() => window.__vim.site())).elevation).toBe(52);

  // --- Level manager: sorted by elevation (top story first) -----------
  let state = await levels(page);
  expect(state.levels.map((l) => l.name)).toEqual(["Ground", "Level 2"]); // ascending in data
  const ground = levelIdByName(state, "Ground");
  const level2 = levelIdByName(state, "Level 2");
  expect(state.activeId).toBe(ground);
  // DOM display order is descending: Level 2 row above Ground row.
  const rowIds = await page.$$eval(".level-row", (rows) => rows.map((r) => r.dataset.id));
  expect(rowIds).toEqual([String(level2), String(ground)]);
  await expect(rowFor(page, ground)).toHaveClass(/active/);

  // --- The payoff: dragging Ground's elevation moves the whole scene --
  const b0 = await bbox(page);
  await setField(page, rowFor(page, ground).locator(".lvl-elev"), 1.0);
  await waitSettled(page);
  const b1 = await bbox(page);
  expect(b1.min[2]).toBeCloseTo(b0.min[2] + 1.0, 5);
  expect(b1.max[2]).toBeCloseTo(b0.max[2] + 1.0, 5);
  expect(b1.min[0]).toBeCloseTo(b0.min[0], 6); // x/y untouched
  expect(b1.max[1]).toBeCloseTo(b0.max[1], 6);
  // Slider DOMs stay consistent — the objects' own parameters did not change.
  await expect(page.locator("#cube-size")).toHaveValue("1"); // range inputs canonicalize "1.0"
  await expect(page.locator("#cyl-height")).toHaveValue("1.2");
  let stats = await page.evaluate(() => window.__vimStats);
  expect(stats.errors).toEqual([]);
  console.log(`ground drag: commit→mesh ${stats.lastLatencyMs.toFixed(1)} ms (whole scene)`);

  // --- Dragging an empty level moves nothing ---------------------------
  await setField(page, rowFor(page, level2).locator(".lvl-elev"), 4.0);
  await waitSettled(page);
  const b2 = await bbox(page);
  expect(b2).toEqual(b1); // meshes untouched (the overlay moved, geometry did not)

  // --- Add level: above the current top, next palette color -----------
  await page.locator("#add-level").click();
  await waitSettled(page);
  state = await levels(page);
  expect(state.levels).toHaveLength(3);
  const level3 = levelIdByName(state, "Level 3");
  expect(state.levels.find((l) => l.id === level3).elevation).toBeCloseTo(7.0, 9); // top(4) + 3
  const rowIdsAfterAdd = await page.$$eval(".level-row", (rows) => rows.map((r) => r.dataset.id));
  expect(rowIdsAfterAdd[0]).toBe(String(level3)); // sorted to the top

  // --- Color change reflects in the overlay pixels — and is COSMETIC:
  // the change cutoff means zero re-tessellation (triangle count
  // identical, trivial latency), yet the overlay repaints.
  const beforeColor = await canvasHash(page);
  const trisBeforeColor = (await page.evaluate(() => window.__vimStats)).triangles;
  await setField(page, rowFor(page, ground).locator(".lvl-color"), "#ff2020");
  await waitSettled(page);
  expect(await canvasHash(page), "overlay color change repaints").not.toBe(beforeColor);
  state = await levels(page);
  expect(state.levels.find((l) => l.id === ground).color[0]).toBeCloseTo(1.0, 2);
  stats = await page.evaluate(() => window.__vimStats);
  expect(stats.triangles, "cosmetic edit does not re-tessellate").toBe(trisBeforeColor);
  expect(stats.lastLatencyMs, "cosmetic edit is trivial").toBeLessThan(15);
  console.log(`level color (cosmetic): commit→mesh ${stats.lastLatencyMs.toFixed(2)} ms`);

  // --- Active-level switching (session state) --------------------------
  const beforeActive = await canvasHash(page);
  await rowFor(page, level2).locator(".lvl-active").check();
  await expect(rowFor(page, level2)).toHaveClass(/active/);
  await expect(rowFor(page, ground)).not.toHaveClass(/active/);
  expect((await levels(page)).activeId).toBe(level2);
  await page.evaluate(
    () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))),
  );
  expect(await canvasHash(page), "active emphasis changes overlay alpha").not.toBe(beforeActive);
  // Back to Ground for the fallback test below.
  await rowFor(page, ground).locator(".lvl-active").check();
  expect((await levels(page)).activeId).toBe(ground);

  // --- Delete an empty level: immediate, no dialog ----------------------
  page.on("dialog", (d) => {
    dialogCount++;
    // Orphan-sweep semantics: the cascade deletes associated elements
    // and their geometry completely.
    expect(d.message()).toContain("All elements associated with this level");
    expect(d.message()).toContain("and their geometry");
    expect(d.message()).toContain("One undo restores everything");
    d.accept();
  });
  await rowFor(page, level3).locator(".lvl-delete").click();
  await waitSettled(page);
  expect(dialogCount, "empty level deletes without a dialog").toBe(0);
  expect((await levels(page)).levels).toHaveLength(2);

  // --- Cascade delete Ground: dialog → confirm → scene empties ---------
  const bboxBeforeCascade = await bbox(page);
  const trisBeforeCascade = (await page.evaluate(() => window.__vimStats)).triangles;
  await rowFor(page, ground).locator(".lvl-delete").click();
  await waitSettled(page);
  expect(dialogCount, "cascade delete asked for confirmation").toBe(1);
  stats = await page.evaluate(() => window.__vimStats);
  expect(stats.triangles, "attached geometry cascaded away").toBe(0);
  expect(await bbox(page)).toBeNull();
  state = await levels(page);
  expect(state.levels.map((l) => l.name)).toEqual(["Level 2"]);
  expect(state.activeId, "active falls back to the nearest remaining level").toBe(level2);

  // --- ONE undo restores everything --------------------------------------
  await expect(page.locator("#undo")).toBeEnabled();
  await page.locator("#undo").click();
  await waitSettled(page);
  stats = await page.evaluate(() => window.__vimStats);
  expect(stats.errors).toEqual([]);
  expect(stats.triangles).toBe(trisBeforeCascade);
  expect(await bbox(page)).toEqual(bboxBeforeCascade);
  state = await levels(page);
  expect(state.levels.map((l) => l.name)).toEqual(["Ground", "Level 2"]);
  // Session state is NOT undoable: the fallback to Level 2 persists.
  expect(state.activeId).toBe(level2);
  console.log(`cascade undo: commit→mesh ${stats.lastLatencyMs.toFixed(1)} ms (full scene rebuild)`);

  mkdirSync(screenshotDir, { recursive: true });
  await page.screenshot({ path: path.join(screenshotDir, "authoring-levels.png") });

  // --- No-levels guard: deleting ALL levels (only reachable by
  // cascading the last one) must not crash; the active level falls back
  // to None, authoring is disabled, and the hint row shows.
  expect(await page.evaluate(() => window.__vim.canAuthor())).toBe(true);
  await expect(page.locator("#no-levels-hint")).toBeHidden();
  await rowFor(page, level2).locator(".lvl-delete").click(); // empty: immediate
  await waitSettled(page);
  expect(dialogCount).toBe(1);
  await rowFor(page, ground).locator(".lvl-delete").click(); // cascade: dialog #2
  await waitSettled(page);
  expect(dialogCount).toBe(2);
  state = await levels(page);
  expect(state.levels).toEqual([]);
  expect(state.activeId).toBeNull();
  expect(await page.evaluate(() => window.__vim.canAuthor())).toBe(false);
  await expect(page.locator("#no-levels-hint")).toBeVisible();
  stats = await page.evaluate(() => window.__vimStats);
  expect(stats.settled).toBe(true);
  expect(stats.errors).toEqual([]);

  expect(pageErrors, `page errors: ${pageErrors.join("; ")}`).toEqual([]);
});
