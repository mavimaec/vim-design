// VimDesignWebTest — the interactive rendered demo.
//
// Loads the demo page, waits for settledness (window.__vimReady, which
// requires evaluated == committed and rendered frames), asserts no page
// errors and a nonzero triangle count, screenshots the initial scene,
// then drives all six sliders (asserting canvas pixels change after each
// settled update), toggles the wireframe checkbox both ways, exercises
// undo once, and captures a final screenshot with visibly different
// proportions.

import { test, expect } from "@playwright/test";
import path from "node:path";
import crypto from "node:crypto";
import { fileURLToPath } from "node:url";
import { mkdirSync } from "node:fs";

const here = path.dirname(fileURLToPath(import.meta.url));
const screenshotDir = path.join(here, "..", "screenshots");

// Region of the page occupied by rendered 3D content only — excludes the
// slider panel (top-left) and the status bar (bottom), whose DOM text
// changes must not count as "canvas pixels changed".
const SCENE_CLIP = { x: 340, y: 0, width: 900, height: 600 };

/** Hash of the rendered scene region (cheap pixel-change detector). */
async function canvasHash(page) {
  const buf = await page.screenshot({ clip: SCENE_CLIP });
  return crypto.createHash("sha256").update(buf).digest("hex");
}

/** Wait until the app reports settledness and a couple frames rendered. */
async function waitSettled(page) {
  await page.waitForFunction(
    () => window.__vimStats?.settled === true && !window.__vimError,
    { timeout: 30_000 },
  );
  // Let the swapchain present the updated mesh before pixel comparison.
  await page.evaluate(
    () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))),
  );
}

/** Set a range input's value and fire an input event. */
async function setSlider(page, id, value) {
  await page.locator(`#${id}`).evaluate((el, v) => {
    el.value = String(v);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  }, value);
}

test("demo scene renders and all six sliders re-mesh the scene", async ({ page }) => {
  const pageErrors = [];
  page.on("pageerror", (e) => pageErrors.push(String(e)));

  await page.goto("/index.html");
  await expect(page).toHaveTitle(/VIM Design/);

  // Initial settledness (wasm + wgpu init + initial evaluation).
  await page.waitForFunction(() => window.__vimReady === true || window.__vimError, {
    timeout: 90_000,
  });
  const initError = await page.evaluate(() => window.__vimError);
  expect(initError, `demo failed to initialize: ${initError}`).toBeFalsy();

  let stats = await page.evaluate(() => window.__vimStats);
  console.log("");
  console.log("================ DEMO SCENE ================");
  console.log(`renderer backend: ${stats.backend}`);
  console.log(`initial: gen ${stats.evaluated}/${stats.committed}, ` +
    `${stats.triangles} triangles, wireframe=${stats.wireframe}, ` +
    `initial evaluate+upload ${stats.lastLatencyMs.toFixed(1)} ms`);

  expect(stats.settled).toBe(true);
  expect(stats.triangles).toBeGreaterThan(0);
  expect(stats.errors).toEqual([]);
  expect(stats.wireframe).toBe(true); // wireframe overlay on by default

  mkdirSync(screenshotDir, { recursive: true });
  await page.screenshot({ path: path.join(screenshotDir, "demo-scene.png") });

  // --- Drive all six sliders; each must change the rendered pixels ----
  // Final values are deliberately extreme: tall skinny cylinder, wide
  // flat cone, thick plate, big cube.
  const sliderMoves = [
    ["cube-size", 1.8],
    ["plate-thickness", 0.8],
    ["cyl-radius", 0.12],
    ["cyl-height", 2.8],
    ["cone-radius", 1.0],
    ["cone-height", 0.4],
  ];

  let prevHash = await canvasHash(page);
  for (const [id, value] of sliderMoves) {
    await setSlider(page, id, value);
    await waitSettled(page);
    const hash = await canvasHash(page);
    expect(hash, `canvas pixels should change after moving #${id}`).not.toBe(prevHash);
    prevHash = hash;

    stats = await page.evaluate(() => window.__vimStats);
    expect(stats.errors, `eval errors after #${id}`).toEqual([]);
    console.log(
      `${id} -> ${value}: commit→mesh ${stats.lastLatencyMs.toFixed(1)} ms, ` +
      `${stats.triangles} triangles (gen ${stats.evaluated}/${stats.committed})`,
    );
  }

  // Feedback-loop guard: a normal slider drag must leave the slider at
  // the dragged value — the dirty-pump-triggered resync (which fires on
  // the drag's own params_changed) skips the active slider and never
  // rewrites equal values.
  await expect(page.locator("#cyl-height")).toHaveValue("2.8");
  await expect(page.locator("#cyl-height-val")).toHaveText("2.80 m");

  // --- Wireframe toggle: off changes pixels, back on changes again ----
  const wireOnHash = prevHash;
  await page.locator("#wireframe").uncheck();
  await waitSettled(page);
  const wireOffHash = await canvasHash(page);
  expect(wireOffHash, "unchecking wireframe should change the canvas").not.toBe(wireOnHash);
  await page.locator("#wireframe").check();
  await waitSettled(page);
  const wireBackHash = await canvasHash(page);
  expect(wireBackHash, "re-checking wireframe should change the canvas back").not.toBe(wireOffHash);
  prevHash = wireBackHash;

  // --- Undo: revert the last slider gesture (cone height) --------------
  // The sliders were driven by the test, so the DOM must show the last
  // set value before undo, and resynchronize from the document after.
  await expect(page.locator("#cone-height")).toHaveValue("0.4");
  await expect(page.locator("#undo")).toBeEnabled();
  await expect(page.locator("#redo")).toBeDisabled();
  await page.locator("#undo").click();
  await waitSettled(page);
  const afterUndo = await canvasHash(page);
  expect(afterUndo, "undo should change the scene back").not.toBe(prevHash);
  // Slider DOM position + value label resynchronized from the document.
  await expect(page.locator("#cone-height")).toHaveValue("1.4");
  await expect(page.locator("#cone-height-val")).toHaveText("1.40 m");
  stats = await page.evaluate(() => window.__vimStats);
  expect(stats.errors).toEqual([]);
  console.log(`undo: commit→mesh ${stats.lastLatencyMs.toFixed(1)} ms, ${stats.triangles} triangles`);

  // Redo it so the final screenshot shows the extreme proportions.
  await expect(page.locator("#redo")).toBeEnabled();
  await page.locator("#redo").click();
  await waitSettled(page);
  const afterRedo = await canvasHash(page);
  expect(afterRedo, "redo should restore the flattened cone").not.toBe(afterUndo);
  await expect(page.locator("#cone-height")).toHaveValue("0.4");
  await expect(page.locator("#cone-height-val")).toHaveText("0.40 m");
  await expect(page.locator("#redo")).toBeDisabled();
  prevHash = await canvasHash(page);

  // --- Cube chamfer: create / update / delete via one slider -----------
  // The chamfer entity replaces the cube extrusion as mesh owner, so
  // both directions exercise the tombstone/upsert handoff.
  stats = await page.evaluate(() => window.__vimStats);
  const trisUnchamfered = stats.triangles;

  await setSlider(page, "cube-chamfer", 0.15);
  await waitSettled(page);
  const chamferHash = await canvasHash(page);
  expect(chamferHash, "chamfer should change the canvas").not.toBe(prevHash);
  stats = await page.evaluate(() => window.__vimStats);
  expect(stats.errors, "chamfer eval errors").toEqual([]);
  expect(stats.triangles, "chamfer adds blend faces").not.toBe(trisUnchamfered);
  const trisChamfered = stats.triangles;
  console.log(
    `cube-chamfer -> 0.15: commit→mesh ${stats.lastLatencyMs.toFixed(1)} ms, ` +
    `${trisUnchamfered} -> ${trisChamfered} triangles`,
  );

  // Back to 0: DeleteChamfer hands the mesh back to the extrusion.
  await setSlider(page, "cube-chamfer", 0);
  await waitSettled(page);
  const unchamferedHash = await canvasHash(page);
  expect(unchamferedHash, "removing the chamfer should change the canvas").not.toBe(chamferHash);
  stats = await page.evaluate(() => window.__vimStats);
  expect(stats.errors).toEqual([]);
  expect(stats.triangles, "triangle count reverts with the chamfer gone").toBe(trisUnchamfered);

  // Re-apply for the undo/redo slider-sync assertions + final screenshot.
  await setSlider(page, "cube-chamfer", 0.15);
  await waitSettled(page);
  await expect(page.locator("#cube-chamfer")).toHaveValue("0.15");

  // Undo reverts the whole consecutive chamfer gesture group (0.15 -> 0
  // -> 0.15) back to "no chamfer"; the slider resyncs from the document.
  await page.locator("#undo").click();
  await waitSettled(page);
  await expect(page.locator("#cube-chamfer")).toHaveValue("0");
  await expect(page.locator("#cube-chamfer-val")).toHaveText("0.00 m");
  stats = await page.evaluate(() => window.__vimStats);
  expect(stats.triangles).toBe(trisUnchamfered);

  await page.locator("#redo").click();
  await waitSettled(page);
  await expect(page.locator("#cube-chamfer")).toHaveValue("0.15");
  await expect(page.locator("#cube-chamfer-val")).toHaveText("0.15 m");
  stats = await page.evaluate(() => window.__vimStats);
  expect(stats.triangles).toBe(trisChamfered);
  expect(stats.errors).toEqual([]);

  await page.screenshot({
    path: path.join(screenshotDir, "demo-scene-after-sliders.png"),
  });

  const latencyLog = await page.evaluate(() => window.__latencyLog);
  console.log("commit→mesh latency log (ms):", JSON.stringify(latencyLog));
  console.log("============================================");

  expect(pageErrors, `page errors: ${pageErrors.join("; ")}`).toEqual([]);
});
