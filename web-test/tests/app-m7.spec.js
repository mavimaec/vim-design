// VimDesignWebTest — the authoring app, milestone 7: Alt + middle drag
// orbits in every mode; the Model tree at the far left with the dock to
// its right; Add level in the tree; row actions inside the panel, shown
// on hover (desktop) or on the selected / active row (touch); the Level
// sheet; moving a workplane to another level at the same world
// elevation. Both Playwright projects where they apply.

import { test, expect } from "@playwright/test";
import {
  mobile, shot, openApp, stats, elements, editState, camera, tapWorld, drawPlate,
} from "./lib/app-helpers.js";

const levels = (page) => page.evaluate(() => JSON.parse(window.__author.app.levels_json()));
const workplanes = (page) => page.evaluate(() => JSON.parse(window.__author.app.workplanes_json()));
const box = (page, sel) => page.locator(sel).first().boundingBox();

/** The Model tree: the desktop panel, or the phone's sheet. */
async function openTree(page) {
  if (mobile()) {
    if (!(await page.locator("#sheet").isVisible())) await page.locator("#tree-btn").click();
    return page.locator("#sheet-body");
  }
  if (!(await page.locator("#tree-panel").isVisible())) await page.locator("#tree-btn").click();
  return page.locator("#tree-body");
}

/** Reveal a row's actions: hover (desktop); on touch the row must be
 *  the active / selected one. */
async function revealRow(page, row) {
  if (!mobile()) await row.hover();
}

test("Alt + middle drag orbits the 3D view in Edit Mode (no marquee); a plain middle drag pans", async ({ page }) => {
  test.skip(mobile(), "mouse buttons");
  const errors = await openApp(page);
  await drawPlate(page, [-3, -2], [3, 2]);
  await tapWorld(page, 0, 0);
  await page.locator("#prop-edit").click();
  await page.locator('#edit-view-toggle button[data-view="3d"]').click();
  expect((await stats(page)).view).toBe("3d");
  const c0 = await camera(page);
  const sel0 = (await editState(page)).selection;
  const r = await box(page, "#view");
  const [x, y] = [r.x + r.width * 0.6, r.y + r.height * 0.5];
  await page.mouse.move(x, y);
  await page.keyboard.down("Alt");
  await page.mouse.down({ button: "middle" });
  await page.mouse.move(x + 120, y + 30, { steps: 8 });
  await page.mouse.up({ button: "middle" });
  await page.keyboard.up("Alt");
  const c1 = await camera(page);
  expect(Math.abs(c1.yaw - c0.yaw), "the orbit turned the view").toBeGreaterThan(0.2);
  expect((await editState(page)).selection, "no marquee").toBe(sel0);
  expect(await page.evaluate(() => window.__author.gesture().mode)).toBe("none");
  // Shift + middle is the same (window managers that take Alt + drag).
  await page.keyboard.down("Shift");
  await page.mouse.down({ button: "middle" });
  await page.mouse.move(x - 60, y + 30, { steps: 6 });
  await page.mouse.up({ button: "middle" });
  await page.keyboard.up("Shift");
  const c2 = await camera(page);
  expect(Math.abs(c2.yaw - c1.yaw)).toBeGreaterThan(0.1);
  // A plain middle drag pans: the target moves, the angle stays.
  await page.mouse.down({ button: "middle" });
  await page.mouse.move(x + 80, y + 80, { steps: 6 });
  await page.mouse.up({ button: "middle" });
  const c3 = await camera(page);
  expect(c3.yaw).toBeCloseTo(c2.yaw, 6);
  expect(Math.hypot(c3.tx - c2.tx, c3.ty - c2.ty)).toBeGreaterThan(0.05);
  await page.locator("#edit-confirm").click();
  // Outside Edit Mode too.
  await page.mouse.move(x, y);
  await page.keyboard.down("Alt");
  await page.mouse.down({ button: "middle" });
  await page.mouse.move(x + 40, y, { steps: 4 });
  await page.mouse.up({ button: "middle" });
  await page.keyboard.up("Alt");
  expect(Math.abs((await camera(page)).yaw - c3.yaw)).toBeGreaterThan(0.05);
  expect(errors).toEqual([]);
});

test("desktop: the Model tree is left of the dock (the dock returns when it closes); its actions stay inside and show on hover", async ({ page }) => {
  test.skip(mobile(), "desktop layout");
  await page.setViewportSize({ width: 760, height: 720 });
  const errors = await openApp(page);
  await expect(page.locator("#tree-panel")).toBeVisible();
  let tree = await box(page, "#tree-panel");
  let dock = await box(page, "#toolbar");
  expect(tree.x).toBeLessThan(30);
  expect(tree.x + tree.width, "the dock is right of the tree").toBeLessThanOrEqual(dock.x);
  // A long active level name: the badge and the buttons stay inside.
  const ground = (await levels(page)).levels.find((l) => l.name === "Ground");
  await page.evaluate((id) => { window.__author.app.update_level_name(id, "Ground floor of the main building"); window.__author.refresh(); }, ground.id);
  const row = page.locator(`#tree-body [data-tree-level="${ground.id}"]`);
  await expect(row.locator(".tree-tag")).toHaveText("Active");
  const add = row.locator("[data-tree-add]");
  const pencil = row.locator("[data-tree-level-edit]");
  // Hidden until the row is hovered.
  expect(await add.evaluate((n) => getComputedStyle(n).opacity)).toBe("0");
  await page.mouse.move(500, 400);
  await row.hover();
  await expect.poll(() => add.evaluate((n) => getComputedStyle(n).opacity)).toBe("1");
  const addBox = await add.boundingBox();
  expect(addBox.x + addBox.width, "+ inside the panel").toBeLessThanOrEqual(tree.x + tree.width);
  expect((await pencil.boundingBox()).x).toBeGreaterThan((await row.locator(".tree-tag").boundingBox()).x);
  await shot(page, "tree-left");
  // Closed: the dock goes back to the left edge.
  await page.locator("#tree-close").click();
  dock = await box(page, "#toolbar");
  expect(dock.x).toBeLessThan(30);
  await page.locator("#tree-btn").click();
  tree = await box(page, "#tree-panel");
  expect(tree.x + tree.width).toBeLessThanOrEqual((await box(page, "#toolbar")).x);
  expect(errors).toEqual([]);
});

test("Add level from the tree; the Level sheet edits name, elevation, story, color with undo; row actions on touch", async ({ page }) => {
  const errors = await openApp(page);
  const before = await levels(page);
  let body = await openTree(page);
  await (mobile() ? page.locator("#sheet-add-level") : page.locator("#tree-add-level")).click();
  let ls = await levels(page);
  expect(ls.levels).toHaveLength(3);
  const added = ls.levels.find((l) => !before.levels.some((b) => b.id === l.id));
  expect(added).toMatchObject({ name: "Level 3", elevation: 6 });
  expect(ls.activeId, "the active level stays").toBe(before.activeId);
  body = await openTree(page);
  const newRow = body.locator(`[data-tree-level="${added.id}"]`);
  await expect(newRow).toHaveClass(/new/);

  // Touch: a row's pencil shows once it is the active row.
  if (mobile()) {
    await expect(newRow.locator("[data-tree-level-edit]")).toBeHidden();
    await newRow.locator(".tree-name").click();
    body = await openTree(page);
    await expect(body.locator(`[data-tree-level="${added.id}"] [data-tree-level-edit]`)).toBeVisible();
  } else {
    await revealRow(page, newRow);
  }
  await body.locator(`[data-tree-level="${added.id}"] [data-tree-level-edit]`).click();
  await expect(page.locator("#sheet-title")).toHaveText("Level 3");
  await page.locator("#lvl-name").fill("Roof");
  await page.locator("#lvl-name").press("Enter");
  await page.locator("#lvl-elevation").fill("6.5");
  await page.locator("#lvl-elevation").press("Enter");
  ls = await levels(page);
  expect(ls.levels.find((l) => l.id === added.id)).toMatchObject({ name: "Roof", elevation: 6.5 });
  await page.locator("#lvl-story").click();
  expect((await levels(page)).levels.find((l) => l.id === added.id).isStory).toBe(false);
  await shot(page, "level-sheet");
  await page.locator("#undo").click();
  expect((await levels(page)).levels.find((l) => l.id === added.id).isStory).toBe(true);
  await page.locator("#undo").click();
  expect((await levels(page)).levels.find((l) => l.id === added.id).elevation).toBeCloseTo(6, 9);
  await expect(page.locator("#lvl-elevation")).toHaveValue("6.00");
  // Delete from the sheet (empty: no prompt).
  await page.locator("#lvl-delete").click();
  expect((await levels(page)).levels).toHaveLength(2);
  expect(errors).toEqual([]);
});

test("a workplane moves to another level at the same world elevation; its floor keeps its place and follows the level; one undo", async ({ page }) => {
  const errors = await openApp(page);
  const ls = await levels(page);
  const ground = ls.levels.find((l) => l.name === "Ground");
  const level2 = ls.levels.find((l) => l.name === "Level 2");
  // A workplane 2.4 m above Ground, a floor plate drawn on it.
  const wp = await page.evaluate((g) => { const id = window.__author.app.add_workplane(g); window.__author.app.set_active_plane(id); window.__author.refresh(); return id; }, ground.id);
  await drawPlate(page, [-2, -1], [2, 1]);
  let [plate] = (await elements(page)).filter((e) => e.kind === "floor_plate");
  expect(plate.levelId).toBe(ground.id);
  const bbox0 = plate.bbox;
  expect(bbox0[1][2]).toBeCloseTo(2.4, 4);
  // Its sheet: move it under Level 2.
  await page.evaluate((id) => window.__author.openWorkplane(id), wp);
  await expect(page.locator("#sheet-title")).toHaveText("Workplane 1");
  await page.locator("#wp-parent-picker").selectOption(String(level2.id));
  let w = (await workplanes(page)).find((x) => x.id === wp);
  expect(w.parent).toBe(level2.id);
  expect(w.offset).toBeCloseTo(-0.6, 9);
  expect(w.elevation, "same world elevation").toBeCloseTo(2.4, 9);
  [plate] = (await elements(page)).filter((e) => e.kind === "floor_plate");
  expect(plate.levelId, "the floor follows the new level").toBe(level2.id);
  for (let k = 0; k < 3; k++) {
    expect(plate.bbox[0][k]).toBeCloseTo(bbox0[0][k], 4);
    expect(plate.bbox[1][k]).toBeCloseTo(bbox0[1][k], 4);
  }
  await expect(page.locator("#wp-parent")).toHaveText("Level 2 › Workplane 1");
  await shot(page, "workplane-move");
  // The tree re-nests it under Level 2.
  const tree = await page.evaluate(() => JSON.parse(window.__author.app.tree_json()));
  expect(tree.levels.find((l) => l.id === level2.id).planes.map((p) => p.id)).toEqual([wp]);
  // One undo restores both.
  await page.locator("#undo").click();
  w = (await workplanes(page)).find((x) => x.id === wp);
  expect(w).toMatchObject({ parent: ground.id });
  expect(w.offset).toBeCloseTo(2.4, 9);
  expect((await elements(page)).find((e) => e.kind === "floor_plate").levelId).toBe(ground.id);
  expect(errors).toEqual([]);
});

// -- Plan span (item 8) ----------------------------------------------------------------------

const spanOf = (page, level) => page.evaluate((l) => JSON.parse(window.__author.app.plan_span_json(l)), level);
/** What a Select tap at a world point picks (device px through the app). */
const pickAt = (page, x, y, z) => page.evaluate(([x, y, z]) => {
  const p = JSON.parse(window.__author.app.world_to_screen(x, y, z));
  return p ? window.__author.app.pick(p[0], p[1]) : -2;
}, [x, y, z]);

test("plan span: above the active level is see-through (picked through), opacity edits undo and persist, the toggle, the other level's view", async ({ page }) => {
  const errors = await openApp(page);
  const ls = await levels(page);
  const ground = ls.levels.find((l) => l.name === "Ground");
  const level2 = ls.levels.find((l) => l.name === "Level 2");
  // Walls on Ground and on Level 2 (3.0 .. 5.7 m: above Ground's span).
  const wallOn = async (level, [a, b]) => {
    await page.evaluate((id) => { window.__author.app.set_active_level(id); window.__author.refresh(); }, level);
    await page.locator('.tool[data-tool="wall"]').click();
    await page.locator('#shape-toggle button[data-shape="rect"]').click();
    await tapWorld(page, ...a, ls.levels.find((l) => l.id === level).elevation);
    await tapWorld(page, ...b, ls.levels.find((l) => l.id === level).elevation);
    await page.locator('.tool[data-tool="select"]').click();
  };
  await wallOn(ground.id, [[-3, -2], [3, 2]]);
  await wallOn(level2.id, [[-3, -2], [3, 2]]);
  const walls = (await elements(page)).filter((e) => e.kind === "wall");
  const upper = walls.find((w) => w.levelId === level2.id);
  const lower = walls.find((w) => w.levelId === ground.id);
  await page.evaluate((id) => { window.__author.app.set_active_level(id); window.__author.refresh(); }, ground.id);
  await page.locator('#view-toggle button[data-view="3d"]').click();
  await page.locator("#fit-btn").click();
  let s = await stats(page);
  expect(s.span).toMatchObject({ topZ: 3, bottomZ: 0, above: 0.25 });
  expect(s.span.below).toBeCloseTo(0.35, 6);
  // The Level 2 wall face (x = 0, y = -2, 1.5 m up it) is see-through:
  // a pick passes through it.
  expect(await pickAt(page, 0, -2, 4.5)).not.toBe(upper.id);
  await shot(page, "plan-span-3d");

  // The toggle: everything drawn normally (and picked).
  await page.locator("#render-btn").click();
  await page.locator("#plan-span-toggle").click();
  expect((await stats(page)).span).toBeNull();
  expect(await pickAt(page, 0, -2, 4.5)).toBe(upper.id);
  await page.locator("#render-btn").click();
  await page.locator("#plan-span-toggle").click();
  expect((await stats(page)).span).not.toBeNull();

  // Ground's Level sheet: above opacity 1 (drawn normally), one undo.
  await page.evaluate((id) => window.__author.openLevel(id), ground.id);
  await expect(page.locator("#span-above")).toBeVisible();
  await shot(page, "level-span");
  const setSlider = (sel, v) => page.locator(sel).evaluate((n, v) => {
    n.value = String(v);
    n.dispatchEvent(new Event("input", { bubbles: true }));
    n.dispatchEvent(new Event("change", { bubbles: true }));
  }, v);
  await setSlider("#span-above", 1);
  expect((await spanOf(page, ground.id))).toMatchObject({ custom: true, above: 1 });
  expect(await pickAt(page, 0, -2, 4.5)).toBe(upper.id);
  await page.locator("#undo").click();
  expect((await spanOf(page, ground.id))).toMatchObject({ custom: false, above: 0.25 });
  await page.locator("#redo").click();
  await setSlider("#span-below", 0);
  expect((await spanOf(page, ground.id)).below).toBe(0);
  // Persisted with the document.
  await page.evaluate(() => window.__author.saveNow());
  await page.reload();
  await page.waitForFunction(() => window.__author?.ready === true);
  expect(await spanOf(page, ground.id)).toMatchObject({ custom: true, above: 1, below: 0 });

  // Level 2 active: Ground's elements are its below band.
  await page.evaluate((id) => { window.__author.app.set_active_level(id); window.__author.app.set_view_mode("3d"); window.__author.refresh(); }, level2.id);
  s = await stats(page);
  expect(s.span.bottomZ).toBe(3);
  expect(s.span.below).toBeCloseTo(0.35, 6);
  expect(lower.bbox[1][2]).toBeLessThanOrEqual(3 + 1e-6);
  expect(errors).toEqual([]);
});

test("deleting an empty level with a plan span needs no prompt; one undo restores both", async ({ page }) => {
  const errors = await openApp(page);
  const level2 = (await levels(page)).levels.find((l) => l.name === "Level 2");
  expect(await page.evaluate((id) => window.__author.app.set_plan_span(id, "", NaN, 1.5, NaN, NaN, NaN), level2.id)).toBe("");
  await page.evaluate(() => { window.__author.app.end_gesture(); window.__author.refresh(); });
  expect((await spanOf(page, level2.id)).custom).toBe(true);
  await page.evaluate((id) => window.__author.openLevel(id), level2.id);
  await page.locator("#lvl-delete").click();
  await expect(page.locator("#dialog-backdrop")).toBeHidden();
  expect((await levels(page)).levels.map((l) => l.name)).toEqual(["Ground"]);
  await page.locator("#undo").click();
  expect((await levels(page)).levels).toHaveLength(2);
  expect((await spanOf(page, level2.id))).toMatchObject({ custom: true, cut: 1.5 });
  expect(errors).toEqual([]);
});
