// Shared helpers for the authoring app specs (www/app.html served in the
// Pages layout on :8791). Tap targets come from WORLD coordinates
// (window.__author.worldToClient); model state from the debug hooks.

import { test, expect } from "@playwright/test";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { mkdirSync } from "node:fs";

export const APP_URL = "http://localhost:8791/";
const here = path.dirname(fileURLToPath(import.meta.url));
const screenshotDir = path.join(here, "..", "..", "screenshots");
mkdirSync(screenshotDir, { recursive: true });

export const mobile = () => test.info().project.name === "mobile";

export async function shot(page, name) {
  await page.waitForTimeout(350); // let sheet/toast entrance animations finish
  await page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
  await page.screenshot({ path: path.join(screenshotDir, `app-${test.info().project.name}-${name}.png`) });
}

export async function openApp(page, query = "") {
  const errors = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.goto(APP_URL + query);
  await page.waitForFunction(() => window.__author?.ready === true || window.__author?.error, null, { timeout: 90_000 });
  expect(await page.evaluate(() => window.__author.error)).toBeFalsy();
  if (mobile()) {
    await page.evaluate(() => {
      window.__author.app.set_camera_json('{"tx":0,"ty":0.5,"halfH":10}');
      window.__author.refresh();
    });
  }
  return errors;
}

export const stats = (page) => page.evaluate(() => window.__author.stats());
export const elements = (page) => page.evaluate(() => window.__author.elements());
export const walls = async (page) => (await elements(page)).filter((e) => e.kind === "wall");
export const editState = (page) => page.evaluate(() => window.__author.editState());
export const camera = (page) => page.evaluate(() => window.__author.camera());
export const openings = (page) => page.evaluate(() => JSON.parse(window.__author.app.openings_json()));
export const openingsState = (page) => page.evaluate(() => JSON.parse(window.__author.app.openings_state_json()));
export const savedBytes = (page) => page.evaluate(() => Array.from(window.__author.app.save_document()).join(","));

export async function worldToClient(page, x, y, z = 0) {
  const p = await page.evaluate(([x, y, z]) => window.__author.worldToClient(x, y, z), [x, y, z]);
  expect(p, `world (${x}, ${y}, ${z}) is on screen`).toBeTruthy();
  return p;
}

export async function tapClient(page, [cx, cy]) {
  if (mobile()) await page.touchscreen.tap(cx, cy);
  else await page.mouse.click(cx, cy);
}

export async function tapWorld(page, x, y, z = 0) {
  await tapClient(page, await worldToClient(page, x, y, z));
}

export async function tool(page, name) {
  await page.locator(`.tool[data-tool="${name}"]`).click();
  expect((await stats(page)).tool).toBe(name);
}

export async function shape(page, name) {
  await page.locator(`#shape-toggle button[data-shape="${name}"]`).click();
  expect((await stats(page)).shape).toBe(name);
}

/** Floor tool: a new plate in Edit Mode; a rectangle; ✓. */
export async function drawPlate(page, a, b) {
  await page.locator('.tool[data-tool="plate"]').click();
  expect((await editState(page)).active).toBe(true);
  await shape(page, "rect");
  await tapWorld(page, ...a);
  await tapWorld(page, ...b);
  await page.locator("#edit-confirm").click();
  expect((await editState(page)).active).toBe(false);
}

/** Wall tool: a rectangular room. */
export async function drawRoom(page, a, b) {
  await tool(page, "wall");
  await shape(page, "rect");
  await tapWorld(page, ...a);
  await tapWorld(page, ...b);
  await tool(page, "select");
}

export async function cdpTouch(page) {
  const cdp = await page.context().newCDPSession(page);
  return (type, points) => cdp.send("Input.dispatchTouchEvent", {
    type, touchPoints: points.map(([x, y], id) => ({ x, y, id })),
  });
}

/** Press at client point `a`, drag to `b`, release (mouse or touch). */
export async function dragClient(page, pa, pb, steps = 12) {
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
export async function longPressClient(page, p, ms = 800) {
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
