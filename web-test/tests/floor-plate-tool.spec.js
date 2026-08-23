// VimDesignWebTest — Phase C: the interactive floor-plate drawing tool
// (docs/AUTHORING.md §3/§5).
//
// Click targets are computed from WORLD coordinates via the app's
// window.__vim.worldToScreen helper (documented choice: the spec
// survives camera changes; hardcoded pixels would not). The in-progress
// outline is view-only — nothing enters the document until the loop
// closes, so Escape leaves the committed generation untouched.
//
// Error-path reality check (probed 2026-08-23, noted honestly): the
// kernel TRIANGULATES self-intersecting (bowtie) outlines and
// boundary-crossing holes without complaint — no typed error fires for
// those; they produce valid-but-wrong-looking meshes. The typed
// per-entity error path is exercised with a duplicate vertex instead
// (zero-length line -> Degenerate + upstream error chain), which the
// status line surfaces and one undo fully clears.

import { test, expect } from "@playwright/test";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { mkdirSync } from "node:fs";

const here = path.dirname(fileURLToPath(import.meta.url));
const screenshotDir = path.join(here, "..", "screenshots");

async function waitSettled(page) {
  await page.waitForFunction(
    () => window.__vimStats?.settled === true && !window.__vimError,
    { timeout: 30_000 },
  );
  await page.evaluate(
    () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))),
  );
}

const stats = (page) => page.evaluate(() => window.__vimStats);

/** Click the canvas at the screen position of a world point on z = 0. */
async function clickWorld(page, x, y) {
  const p = await page.evaluate(([wx, wy]) => window.__vim.worldToScreen(wx, wy, 0), [x, y]);
  expect(p, `world (${x}, ${y}) projects on screen`).toBeTruthy();
  // Must land on unobstructed canvas (between the two UI panels).
  expect(p[0]).toBeGreaterThan(335);
  expect(p[0]).toBeLessThan(945);
  expect(p[1]).toBeLessThan(660);
  await page.mouse.click(p[0], p[1]);
}

const PLATE = [[-2.6, 0.4], [-0.9, 0.4], [-0.9, 1.7], [-2.6, 1.7]];
const HOLE1 = [[-2.3, 0.7], [-1.9, 0.7], [-1.9, 1.1], [-2.3, 1.1]];
const HOLE2 = [[-1.6, 1.0], [-1.2, 1.0], [-1.2, 1.4], [-1.6, 1.4]];

test("floor-plate tool: draw, two holes, cancel, degenerate error, undo/redo, gating", async ({ page }) => {
  const pageErrors = [];
  page.on("pageerror", (e) => pageErrors.push(String(e)));
  page.on("dialog", (d) => d.accept()); // cascade confirms in the teardown

  await page.goto("/index.html");
  await page.waitForFunction(() => window.__vimReady === true || window.__vimError, {
    timeout: 90_000,
  });
  expect(await page.evaluate(() => window.__vimError)).toBeFalsy();

  let s = await stats(page);
  const tris0 = s.triangles;
  expect(s.canAuthor).toBe(true);
  await expect(page.locator("#draw-plate")).toBeEnabled();
  await expect(page.locator("#draw-hole")).toBeDisabled(); // no tool-authored plate yet

  // --- Escape cancels: preview only, document untouched ----------------
  const genBefore = s.committed;
  await page.locator("#draw-plate").click();
  expect((await page.evaluate(() => window.__vim.drawState())).active).toBe(true);
  await clickWorld(page, ...PLATE[0]);
  await clickWorld(page, ...PLATE[1]);
  await page.keyboard.press("Escape");
  expect((await page.evaluate(() => window.__vim.drawState())).active).toBe(false);
  s = await stats(page);
  expect(s.committed, "cancelled sketch never touched the document").toBe(genBefore);
  expect(s.triangles).toBe(tris0);

  // --- Draw the plate: 4 vertices, close by clicking the first ---------
  await page.locator("#draw-plate").click();
  for (const [x, y] of PLATE) await clickWorld(page, x, y);
  await clickWorld(page, ...PLATE[0]); // snap-close on the first vertex
  await waitSettled(page);
  s = await stats(page);
  const trisPlate = s.triangles;
  expect(trisPlate, "plate meshed").toBeGreaterThan(tris0);
  expect(s.errors).toEqual([]);
  expect(s.canAddHole).toBe(true);
  await expect(page.locator("#draw-hole")).toBeEnabled();
  console.log(`draw plate: commit→mesh ${s.lastLatencyMs.toFixed(2)} ms, ${tris0} -> ${trisPlate} tris`);

  // --- The plate participates: Ground drag stays transform-only, now
  // with FIVE owners riding along ----------------------------------------
  const groundId = await page.evaluate(
    () => window.__vim.levels().levels.find((l) => l.name === "Ground").id,
  );
  const bboxBefore = await page.evaluate(() => window.__vim.bbox());
  await page.locator(`.level-row[data-id="${groundId}"] .lvl-elev`).evaluate((el) => {
    el.value = "0.6";
    el.dispatchEvent(new Event("input", { bubbles: true }));
    el.dispatchEvent(new Event("change", { bubbles: true }));
    el.blur();
  });
  await waitSettled(page);
  s = await stats(page);
  expect(s.lastMeshUpserts).toBe(0);
  expect(s.lastBaseTransforms, "authored plate rides the level too").toBe(5);
  const bboxAfter = await page.evaluate(() => window.__vim.bbox());
  expect(bboxAfter.min[2]).toBeCloseTo(bboxBefore.min[2] + 0.6, 5);
  console.log(`ground drag with authored plate: ${s.lastLatencyMs.toFixed(2)} ms, transform-only x5`);
  // Back to 0 for stable hole-click coordinates.
  await page.locator(`.level-row[data-id="${groundId}"] .lvl-elev`).evaluate((el) => {
    el.value = "0";
    el.dispatchEvent(new Event("input", { bubbles: true }));
    el.dispatchEvent(new Event("change", { bubbles: true }));
    el.blur();
  });
  await waitSettled(page);

  // --- Two holes (the acceptance criterion) ------------------------------
  const bboxNoHoles = await page.evaluate(() => window.__vim.bbox());
  await page.locator("#draw-hole").click();
  for (const [x, y] of HOLE1) await clickWorld(page, x, y);
  await page.keyboard.press("Enter"); // commit via Enter
  await waitSettled(page);
  s = await stats(page);
  const trisHole1 = s.triangles;
  expect(trisHole1, "hole 1 adds an opening (side walls)").toBeGreaterThan(trisPlate);
  expect(s.errors).toEqual([]);
  console.log(`hole 1: commit→mesh ${s.lastLatencyMs.toFixed(2)} ms, -> ${trisHole1} tris`);

  await page.locator("#draw-hole").click();
  for (const [x, y] of HOLE2) await clickWorld(page, x, y);
  await clickWorld(page, ...HOLE2[0]); // snap-close
  await waitSettled(page);
  s = await stats(page);
  const trisHole2 = s.triangles;
  expect(trisHole2, "hole 2 adds a second opening").toBeGreaterThan(trisHole1);
  expect(s.errors).toEqual([]);
  // Interior holes never change the world bbox.
  expect(await page.evaluate(() => window.__vim.bbox())).toEqual(bboxNoHoles);
  console.log(`hole 2: commit→mesh ${s.lastLatencyMs.toFixed(2)} ms, -> ${trisHole2} tris`);

  mkdirSync(screenshotDir, { recursive: true });
  await page.screenshot({ path: path.join(screenshotDir, "floor-plate-tool.png") });

  // --- Undo chain: hole 2, hole 1, (ground-drag gesture), plate — each
  // draw commit is ONE gesture group; the elevation drag between plate
  // and holes is its own gesture (visually a no-op: it went 0.6 -> 0).
  await page.locator("#undo").click();
  await waitSettled(page);
  expect((await stats(page)).triangles, "undo 1: hole 2 gone").toBe(trisHole1);
  await page.locator("#undo").click();
  await waitSettled(page);
  expect((await stats(page)).triangles, "undo 2: hole 1 gone").toBe(trisPlate);
  await page.locator("#undo").click(); // the ground-drag gesture
  await waitSettled(page);
  expect((await stats(page)).triangles).toBe(trisPlate);
  await page.locator("#undo").click();
  await waitSettled(page);
  s = await stats(page);
  expect(s.triangles, "ONE undo removes the whole plate").toBe(tris0);
  expect(s.canAddHole, "no live plate after undo").toBe(false);
  // Redo restores everything, byte-exact through the same pump paths.
  await page.locator("#redo").click(); // plate
  await waitSettled(page);
  expect((await stats(page)).triangles).toBe(trisPlate);
  await page.locator("#redo").click(); // ground-drag gesture
  await waitSettled(page);
  await page.locator("#redo").click(); // hole 1
  await waitSettled(page);
  expect((await stats(page)).triangles).toBe(trisHole1);
  await page.locator("#redo").click(); // hole 2
  await waitSettled(page);
  expect((await stats(page)).triangles).toBe(trisHole2);

  // --- Typed error path: duplicate vertex -> zero-length line ----------
  await page.locator("#draw-plate").click();
  await clickWorld(page, 1.0, 0.6);
  await clickWorld(page, 2.0, 1.2);
  await clickWorld(page, 2.0, 1.2); // duplicate: zero-length outline edge
  await page.keyboard.press("Enter");
  await waitSettled(page);
  s = await stats(page);
  expect(s.errors.length, "degenerate outline surfaces typed errors").toBeGreaterThan(0);
  expect(s.errors.join(" ")).toContain("Degenerate");
  await page.locator("#undo").click();
  await waitSettled(page);
  s = await stats(page);
  expect(s.errors, "undo clears the failed sketch and its errors").toEqual([]);
  expect(s.triangles).toBe(trisHole2);

  // --- can_author gating: cascade away all levels -----------------------
  const level2Id = await page.evaluate(
    () => window.__vim.levels().levels.find((l) => l.name === "Level 2").id,
  );
  await page.locator(`.level-row[data-id="${level2Id}"] .lvl-delete`).click(); // empty
  await waitSettled(page);
  await page.locator(`.level-row[data-id="${groundId}"] .lvl-delete`).click(); // cascade (auto-accepted)
  await waitSettled(page);
  s = await stats(page);
  expect(s.triangles, "cascade swept the authored plate with the seeded scene").toBe(0);
  expect(s.canAuthor).toBe(false);
  await expect(page.locator("#draw-plate")).toBeDisabled();
  await expect(page.locator("#draw-hole")).toBeDisabled();
  await expect(page.locator("#no-levels-hint")).toBeVisible();

  expect(pageErrors, `page errors: ${pageErrors.join("; ")}`).toEqual([]);
});
