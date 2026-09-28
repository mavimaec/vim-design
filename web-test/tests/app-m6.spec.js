// VimDesignWebTest — the authoring app, milestone 6 (Phase A): the item
// just created is the selection (its panel values apply to it), the last
// choices are remembered (also across a reload), copy / paste in each
// mode, the partition default for new walls, the dock (Openings; Snap in
// the view pill). Rooms: app-rooms.spec.js. Both Playwright projects.

import { test, expect } from "@playwright/test";
import {
  mobile, shot, openApp, stats, elements, walls, editState, openings, openingsState,
  tapWorld, tool, shape, drawPlate, drawRoom, PARTITION_M,
} from "./lib/app-helpers.js";

const profile = (page) => page.evaluate(() => JSON.parse(window.__author.app.edit_profile_json()));
const plates = async (page) => (await elements(page)).filter((e) => e.kind === "floor_plate");
const clip = (page) => page.evaluate(() => JSON.parse(window.__author.app.clipboard_json()));
const defaults = (page) => page.evaluate(() => JSON.parse(window.__author.app.session_defaults_json()));

/** Set the value of the face just drawn: the edit panel (desktop) or
 *  the drawing bar's stepper (phones). */
async function setFresh(page, field, value) {
  const input = mobile() ? page.locator("#fresh-input") : page.locator(field === "thickness" ? "#edit-thickness" : "#edit-depth");
  await input.fill(String(value));
  await input.press("Enter");
}

test("dock: Openings; Snap in the view pill; new walls are a 0.114 m partition; the wall just drawn is the selection and the drawing bar changes it", async ({ page }) => {
  const errors = await openApp(page);
  await expect(page.locator('.tool[data-tool="window"] span')).toHaveText("Openings");
  await expect(page.locator("#view-cluster #snap-toggle")).toBeVisible();
  await expect(page.locator("#toolbar #snap-toggle")).toHaveCount(0);
  expect((await stats(page)).wall.thickness).toBeCloseTo(PARTITION_M, 9);

  await tool(page, "wall");
  await expect(page.locator("#wall-thickness-input")).toHaveValue("0.114");
  await shape(page, "rect");
  await tapWorld(page, -3, -2);
  await tapWorld(page, 3, 2);
  const [room] = await walls(page);
  expect(room.thickness).toBeCloseTo(PARTITION_M, 9);
  let s = await stats(page);
  expect(s.selection).toBe(room.id);
  expect(s.wall.fresh).toMatchObject({ id: room.id, name: "Wall 1" });
  await expect(page.locator("#draw-target")).toHaveText("Changes apply to Wall 1 and the next walls");
  await shot(page, "m6-fresh-wall");

  // + is 5 mm, on the wall just drawn too: one undo step.
  const thicker = page.locator('#wall-settings button[data-wall="thickness"][data-step="1"]');
  await thicker.click();
  expect((await walls(page))[0].thickness).toBeCloseTo(0.119, 9);
  expect((await stats(page)).wall.thickness).toBeCloseTo(0.119, 9);
  await expect(page.locator("#wall-thickness-input")).toHaveValue("0.119");
  await page.locator("#undo").click();
  expect((await walls(page))[0].thickness).toBeCloseTo(PARTITION_M, 9);
  await page.locator("#redo").click();
  expect((await walls(page))[0].thickness).toBeCloseTo(0.119, 9);

  // The next wall's first point ends it: the bar is for new walls again.
  await tapWorld(page, -3, 3.5);
  expect((await stats(page)).wall.fresh).toBeNull();
  await expect(page.locator("#draw-target")).toBeHidden();
  await thicker.click();
  expect((await walls(page))[0].thickness).toBeCloseTo(0.119, 9);
  expect((await stats(page)).wall.thickness).toBeCloseTo(0.124, 9);
  await tool(page, "select");
  expect((await stats(page)).selection).toBeNull();
  expect(errors).toEqual([]);
});

test("the face just drawn is the selection: its thickness / depth right away; the choices are remembered, also after a reload", async ({ page }) => {
  const errors = await openApp(page);
  await page.locator('.tool[data-tool="plate"]').click();
  await shape(page, "rect");
  await tapWorld(page, -3, -2);
  await tapWorld(page, 3, 2);
  let es = await editState(page);
  expect(es).toMatchObject({ selection: 1, tool: "solid", mode: "faces" });
  expect(es.panel.target).toBe("fresh");
  if (mobile()) await expect(page.locator("#fresh-label")).toHaveText("New face");
  else await expect(page.locator("#edit-panel-title")).toHaveText("New face · thickness");
  await setFresh(page, "thickness", 0.5);
  expect((await profile(page)).faces[0].kind).toEqual({ solid: 0.5 });

  // A void rectangle: its depth right away (a depth turns Through off).
  await page.locator('[data-edit-tool="void"]').click();
  await tapWorld(page, -2, -1);
  await tapWorld(page, 0, 1);
  es = await editState(page);
  expect(es.panel.target).toBe("fresh");
  expect(es.panel.void).toMatchObject({ through: true });
  if (mobile()) await expect(page.locator("#fresh-label")).toHaveText("New void");
  else await expect(page.locator("#edit-panel-title")).toHaveText("New void · through");
  await setFresh(page, "depth", 0.15);
  let prof = await profile(page);
  expect(prof.faces.filter((f) => f.kind.void === 0.15)).toHaveLength(1);
  if (!mobile()) await expect(page.locator("#edit-panel-title")).toHaveText("New void · depth");
  await shot(page, "m6-fresh-void");

  // The next void starts with the depth just chosen.
  await tapWorld(page, 1, -1);
  await tapWorld(page, 2, 1);
  prof = await profile(page);
  expect(prof.faces.filter((f) => f.kind.void === 0.15)).toHaveLength(2);
  await page.locator("#edit-confirm").click();
  expect((await editState(page)).active).toBe(false);

  // The next floor plate starts with the remembered thickness.
  expect(await defaults(page)).toMatchObject({ floorThickness: 0.5, voidDepth: 0.15, voidThrough: false, wallThickness: PARTITION_M });
  await page.waitForTimeout(700); // the session is saved (debounced)
  await page.reload();
  await page.waitForFunction(() => window.__author?.ready === true);
  expect(await defaults(page)).toMatchObject({ floorThickness: 0.5, voidDepth: 0.15, voidThrough: false });
  expect(errors).toEqual([]);
});

test("openings: the one just placed is selected (its size is remembered per kind); copy one and paste it on another wall", async ({ page }) => {
  const errors = await openApp(page);
  await drawRoom(page, [-3, -2], [3, 2]);
  await page.locator('.tool[data-tool="window"]').click();
  await tapWorld(page, -1.5, -1.95);
  let st = await openingsState(page);
  expect(st.fresh).toBe(true);
  await expect(page.locator("#edit-panel-title")).toHaveText("New window · Wall 1");
  await page.locator("#opening-width").fill("1.5");
  await page.locator("#opening-width").press("Enter");
  st = await openingsState(page);
  expect(st.presetSize.window.width).toBeCloseTo(1.5, 9);
  // Another window: the remembered width.
  await tapWorld(page, 1.5, 1.95);
  let os = await openings(page);
  expect(os).toHaveLength(2);
  expect(os[1].width).toBeCloseTo(1.5, 9);

  // Copy it, paste on the left wall (the paste stays armed until turned off).
  await expect(page.locator("#clip-cluster")).toBeVisible();
  await page.locator("#copy-btn").click();
  await page.locator("#paste-btn").click();
  expect((await clip(page)).armed).toBe(true);
  await expect(page.locator("#paste-btn")).toHaveClass(/armed/);
  await tapWorld(page, -2.95, 0);
  os = await openings(page);
  expect(os).toHaveLength(3);
  expect(os[2]).toMatchObject({ kind: "window" });
  expect(os[2].width).toBeCloseTo(1.5, 9);
  expect(os[2].segment).not.toBe(os[1].segment);
  await shot(page, "m6-paste-openings");
  // One undo step per paste.
  await page.locator("#undo").click();
  expect(await openings(page)).toHaveLength(2);
  await page.locator("#redo").click();
  expect(await openings(page)).toHaveLength(3);
  await page.locator("#paste-btn").click();
  expect((await clip(page)).armed).toBe(false);
  await page.locator("#edit-confirm").click();
  expect(errors).toEqual([]);
});

test("copy / paste: a whole floor plate in Select (named by the naming rule), and faces in the floor's Edit Mode; one step each", async ({ page }) => {
  const errors = await openApp(page);
  await drawPlate(page, [-3, -2], [0, 1]);
  await tool(page, "select");
  await tapWorld(page, -1.5, -0.5);
  const [plate] = await plates(page);
  expect((await stats(page)).selection).toBe(plate.id);
  if (mobile()) await page.locator("#copy-btn").click();
  else await page.keyboard.press("Control+c");
  expect(await clip(page)).toMatchObject({ clip: "element", label: "Floor plate 1", canPaste: true });
  if (mobile()) await page.locator("#paste-btn").click();
  else await page.keyboard.press("Control+v");
  expect((await clip(page)).armed).toBe(true);
  await expect(page.locator("#hint")).toContainText("where the copy goes");
  await shot(page, "m6-paste-armed");
  await tapWorld(page, 2, -4); // clear of the pills (top right) and the sheet (phones)
  let ps = await plates(page);
  expect(ps.map((p) => p.name)).toEqual(["Floor plate 1", "Floor plate 2"]);
  // Moved by whole grid steps: its centre lands near the tap.
  const bb = ps[1].bbox;
  expect(Math.abs((bb[0][0] + bb[1][0]) / 2 - 2)).toBeLessThanOrEqual(0.25 + 1e-9);
  expect((await stats(page)).selection).toBe(ps[1].id);
  await page.locator("#undo").click();
  expect(await plates(page)).toHaveLength(1);
  await page.locator("#redo").click();
  expect(await plates(page)).toHaveLength(2);
  if (mobile()) await page.locator("#paste-btn").click();
  else await page.keyboard.press("Escape");
  expect((await clip(page)).armed).toBe(false);

  // Faces: in the plate's Edit Mode.
  await tapWorld(page, -1.5, -0.5);
  await page.locator("#prop-edit").click();
  expect((await editState(page)).active).toBe(true);
  await page.locator('[data-edit-mode="faces"]').click();
  await tapWorld(page, -1.5, -0.5);
  expect((await editState(page)).selection).toBe(1);
  await page.locator("#copy-btn").click();
  expect((await clip(page)).clip).toBe("faces");
  await page.locator("#paste-btn").click();
  await tapWorld(page, -1.5, 3);
  const prof = await profile(page);
  expect(prof.faces).toHaveLength(2);
  expect((await editState(page)).panel.target).toBe("fresh");
  await shot(page, "m6-paste-faces");
  await page.locator("#paste-btn").click();
  await page.locator("#edit-confirm").click();
  expect(errors).toEqual([]);
});
