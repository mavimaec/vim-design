// VimDesignWebTest — headed WebGPU verification (OPT-IN, not a CI gate).
//
// Headless chromium never composites WebGPU canvases in this environment
// (see playwright.config.js), so the real WebGPU render path can only be
// verified in a headed browser on a desktop session. Run it with:
//
//   pwsh devops/vactions.ps1 -TestWebGpu        (sets VIM_WEBGPU_HEADED=1)
//
// Asserts the status feed reports the WebGPU backend (not the WebGL2
// fallback), settledness with nonzero triangles and no eval errors, that
// the composited pixels actually change when a slider moves (the exact
// failure mode of headless WebGPU was "renders fine, presents nothing"),
// and captures screenshots/demo-scene-webgpu.png.

import { test, expect } from "@playwright/test";
import path from "node:path";
import crypto from "node:crypto";
import { fileURLToPath } from "node:url";
import { mkdirSync } from "node:fs";

const here = path.dirname(fileURLToPath(import.meta.url));
const screenshotDir = path.join(here, "..", "screenshots");

const enabled = !!process.env.VIM_WEBGPU_HEADED;

test.skip(
  !enabled,
  "headed WebGPU verification is opt-in: set VIM_WEBGPU_HEADED=1 " +
    "(pwsh devops/vactions.ps1 -TestWebGpu); needs a desktop session",
);

test.use({
  headless: false,
  launchOptions: {
    // Linux chromium still gates WebGPU behind these.
    args: ["--enable-unsafe-webgpu", "--enable-features=Vulkan"],
  },
});

// Same scene-only region as demo-scene.spec.js (excludes panel + status bar).
const SCENE_CLIP = { x: 340, y: 0, width: 900, height: 600 };

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

test("demo scene renders end-to-end on real WebGPU (headed)", async ({ page }) => {
  const pageErrors = [];
  page.on("pageerror", (e) => pageErrors.push(String(e)));

  await page.goto("/index.html");
  await page.waitForFunction(() => window.__vimReady === true || window.__vimError, {
    timeout: 90_000,
  });
  const initError = await page.evaluate(() => window.__vimError);
  expect(initError, `demo failed to initialize: ${initError}`).toBeFalsy();

  let stats = await page.evaluate(() => window.__vimStats);
  console.log("");
  console.log("=============== HEADED WEBGPU ===============");
  console.log(`backend: ${stats.backend}`);
  console.log(`initial: gen ${stats.evaluated}/${stats.committed}, ` +
    `${stats.triangles} triangles, initial evaluate+upload ${stats.lastLatencyMs.toFixed(1)} ms`);

  expect(stats.backend, "expected the real WebGPU backend, not the fallback").toBe("WebGPU");
  expect(stats.settled).toBe(true);
  expect(stats.triangles).toBeGreaterThan(0);
  expect(stats.errors).toEqual([]);

  // Presentation proof: WebGPU's headless failure mode is "renders
  // without errors but composites nothing" — so assert pixels actually
  // change when a parameter changes.
  const before = await canvasHash(page);
  for (const [id, value] of [
    ["cube-size", 1.8],
    ["plate-thickness", 0.8],
    ["cyl-radius", 0.12],
    ["cyl-height", 2.8],
    ["cone-radius", 1.0],
    ["cone-height", 0.4],
  ]) {
    await page.locator(`#${id}`).evaluate((el, v) => {
      el.value = String(v);
      el.dispatchEvent(new Event("input", { bubbles: true }));
    }, value);
    await waitSettled(page);
  }
  const after = await canvasHash(page);
  expect(after, "WebGPU canvas must present live changes").not.toBe(before);

  stats = await page.evaluate(() => window.__vimStats);
  expect(stats.errors).toEqual([]);
  const latencyLog = await page.evaluate(() => window.__latencyLog);
  console.log("WebGPU commit→mesh latency log (ms):", JSON.stringify(latencyLog));
  console.log("=============================================");

  mkdirSync(screenshotDir, { recursive: true });
  await page.screenshot({ path: path.join(screenshotDir, "demo-scene-webgpu.png") });

  expect(pageErrors, `page errors: ${pageErrors.join("; ")}`).toEqual([]);
});
