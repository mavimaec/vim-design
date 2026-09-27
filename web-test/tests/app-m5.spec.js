// VimDesignWebTest — the authoring app, milestone 5 (Phase A): ONE undo
// history (Edit Modes are spans of it), Fit and camera bounds, render
// modes (the triangle wireframe), two-finger classify-then-lock touch
// navigation, Openings mode (windows and doors on any wall), and the
// wall's plan Edit Mode (its run's points and segments).
// Runs in BOTH Playwright projects (desktop mouse, mobile touch).

import { test, expect } from "@playwright/test";
import {
  mobile, shot, openApp, stats, elements, walls, editState, camera, openings, openingsState, savedBytes,
  worldToClient, tapClient, tapWorld, tool, shape, drawPlate, drawRoom, cdpTouch, dragClient, longPressClient,
} from "./lib/app-helpers.js";

const plates = async (page) => (await elements(page)).filter((e) => e.kind === "floor_plate");
const profile = (page) => page.evaluate(() => window.__author.editProfile());
const hasPoint = (prof, [u, v]) => prof.points.some((p) => Math.abs(p.uv[0] - u) < 1e-6 && Math.abs(p.uv[1] - v) < 1e-6);

test("one undo history: Edit Mode steps are ordinary steps; undo stops at the entry; ✗ drops them; one control, no toast Undo", async ({ page }) => {
  const errors = await openApp(page);
  const normalBox = await page.locator("#undo").boundingBox();

  // A new plate in Edit Mode: two faces, each its own step.
  await page.locator('.tool[data-tool="plate"]').click();
  expect((await editState(page)).active).toBe(true);
  const editBox = await page.locator("#undo").boundingBox();
  expect(Math.abs(editBox.x - normalBox.x) + Math.abs(editBox.y - normalBox.y), "Undo sits in the same place").toBeLessThan(1);
  await expect(page.locator("#undo"), "nothing to undo at the session's entry").toBeDisabled();
  await shape(page, "rect");
  await tapWorld(page, -3, -2);
  await tapWorld(page, 0, 0);
  await tapWorld(page, 1, 1);
  await tapWorld(page, 3, 2);
  expect((await profile(page)).faces).toHaveLength(2);
  await page.locator("#undo").click();
  expect((await profile(page)).faces).toHaveLength(1);
  await page.locator("#redo").click();
  expect((await profile(page)).faces).toHaveLength(2);
  await page.locator("#edit-confirm").click();
  let [plate] = await plates(page);
  expect(plate.faceCount).toBe(2);

  // After ✓ the same steps, one by one (no collapse).
  await page.locator("#undo").click();
  expect((await plates(page))[0].faceCount).toBe(1);
  await page.locator("#undo").click();
  expect(await plates(page)).toHaveLength(0);
  await page.locator("#redo").click();
  await page.locator("#redo").click();
  expect((await plates(page))[0].faceCount).toBe(2);

  // ✗ undoes back to the entry and drops the session's steps.
  const before = await savedBytes(page);
  await tapWorld(page, -1.5, -1);
  await page.locator("#prop-edit").click();
  await page.locator('[data-edit-tool="void"]').click();
  await shape(page, "rect");
  await tapWorld(page, -2.5, -1.5);
  await tapWorld(page, -2, -1);
  expect((await editState(page)).canUndo).toBe(true);
  await page.locator("#edit-cancel").click();
  await page.locator("#dialog-ok").click();
  expect(await savedBytes(page)).toBe(before);
  await expect(page.locator("#redo"), "the cancelled steps cannot be redone").toBeDisabled();

  // Toasts never carry an Undo button: the one control teaches the habit.
  await tapWorld(page, -1.5, -1);
  await page.locator("#prop-delete").click();
  expect(await page.locator("#toasts button").count()).toBe(0);
  await page.locator("#undo").click();
  expect(await plates(page)).toHaveLength(1);
  expect(errors).toEqual([]);
});

test("Fit frames the selection (or the model), in every mode; the camera stays near the model", async ({ page }) => {
  const errors = await openApp(page);
  await drawPlate(page, [-3, -2], [3, 2]);
  // Wandering off: the view centre and the zoom are bounded.
  await page.evaluate(() => {
    window.__author.app.set_camera_json('{"tx":500,"ty":-400,"halfH":300}');
    window.__author.refresh();
  });
  let cam = await camera(page);
  const radius = Math.hypot(6, 4, 0.3) / 2;
  expect(Math.hypot(cam.tx, cam.ty)).toBeLessThanOrEqual(radius * 1.5 + 8 + 1e-3);
  expect(cam.halfH).toBeLessThanOrEqual(radius * 3 + 12 + 1e-3);
  for (let i = 0; i < 10; i++) await page.evaluate(() => window.__author.app.zoom_at(5, 10, 10));
  expect((await camera(page)).halfH).toBeLessThanOrEqual(radius * 3 + 12 + 1e-3);

  // Fit: the whole model.
  await expect(page.locator("#fit-btn")).toBeVisible();
  await page.locator("#fit-btn").click();
  const onScreen = async (x, y) => {
    const p = await page.evaluate(([x, y]) => window.__author.worldToClient(x, y, 0), [x, y]);
    const vp = page.viewportSize();
    return p && p[0] > 0 && p[0] < vp.width && p[1] > 0 && p[1] < vp.height;
  };
  for (const [x, y] of [[-3, -2], [3, 2]]) expect(await onScreen(x, y)).toBe(true);
  await shot(page, "fit");

  // With a selection it frames that; in Edit Mode, the element edited.
  await drawRoom(page, [10, 10], [14, 13]);
  await tapWorld(page, 0, 0);
  expect((await stats(page)).selection).toBe((await plates(page))[0].id);
  await page.evaluate(() => {
    window.__author.app.set_camera_json('{"tx":12,"ty":11,"halfH":2}');
    window.__author.refresh();
  });
  await page.locator("#fit-btn").click();
  for (const [x, y] of [[-3, -2], [3, 2]]) expect(await onScreen(x, y)).toBe(true);
  expect(await onScreen(14, 13), "the room is not the focus").toBe(false);
  await page.locator("#prop-edit").click();
  await expect(page.locator("#fit-btn"), "Fit stays in Edit Mode").toBeVisible();
  await page.evaluate(() => {
    window.__author.app.set_camera_json('{"tx":12,"ty":11,"halfH":2}');
    window.__author.refresh();
  });
  await page.locator("#fit-btn").click();
  for (const [x, y] of [[-3, -2], [3, 2]]) expect(await onScreen(x, y)).toBe(true);
  await page.locator("#edit-confirm").click();
  expect(errors).toEqual([]);
});

test("render modes: shaded, shaded + wireframe, wireframe (triangle edges) with the triangle count; persisted", async ({ page }) => {
  const errors = await openApp(page);
  await drawPlate(page, [-3, -2], [3, 2]);
  await drawRoom(page, [-3, -2], [3, 2]);
  await page.locator('#view-toggle button[data-view="3d"]').click();
  await page.locator("#fit-btn").click();
  await expect(page.locator("#tri-badge")).toBeHidden();
  await page.locator("#render-btn").click();
  await expect(page.locator("#render-menu")).toBeVisible();
  await page.locator('#render-menu button[data-render="wire"]').click();
  let s = await stats(page);
  expect(s.renderMode).toBe("wire");
  await expect(page.locator("#tri-badge")).toBeVisible();
  await expect(page.locator("#tri-badge")).toHaveText(`Model · ${s.triangles.toLocaleString("en-US")} triangles`);
  await shot(page, "render-wire");
  // The selection's own count.
  await page.locator('#view-toggle button[data-view="plan"]').click();
  await tapWorld(page, 0, 0);
  s = await stats(page);
  expect(s.focusTriangles).toBeGreaterThan(0);
  await expect(page.locator("#tri-badge")).toContainText(`Floor plate 1 · ${s.focusTriangles}`);
  if (!mobile()) await page.locator("#sheet-close").click();
  else await page.locator("#sheet-close").click();
  await page.locator('#view-toggle button[data-view="3d"]').click();
  await page.locator("#render-btn").click();
  await page.locator('#render-menu button[data-render="shaded-wire"]').click();
  expect((await stats(page)).renderMode).toBe("shaded-wire");
  await shot(page, "render-shaded-wire");
  // Session state: survives a reload.
  await page.evaluate(() => window.__author.saveNow());
  await page.waitForTimeout(700);
  await page.reload();
  await page.waitForFunction(() => window.__author?.ready === true, null, { timeout: 90_000 });
  expect((await stats(page)).renderMode).toBe("shaded-wire");
  await page.locator("#render-btn").click();
  await page.locator('#render-menu button[data-render="shaded"]').click();
  await expect(page.locator("#tri-badge")).toBeHidden();
  expect(errors).toEqual([]);
});

test("touch: two fingers pan OR zoom (classified once, locked); a new pinch re-classifies", async ({ page }) => {
  test.skip(!mobile(), "multi-touch runs on the phone project");
  const errors = await openApp(page);
  await drawPlate(page, [-3, -2], [3, 2]);
  const touch = await cdpTouch(page);
  const vp = page.viewportSize();
  const [cx, cy] = [vp.width / 2, vp.height / 2];
  const gesture = async (a0, b0, a1, b1, steps = 12, lift = true) => {
    await touch("touchStart", [a0, b0]);
    for (let i = 1; i <= steps; i++) {
      const t = i / steps;
      const lerp = (p, q) => [p[0] + (q[0] - p[0]) * t, p[1] + (q[1] - p[1]) * t];
      await touch("touchMove", [lerp(a0, a1), lerp(b0, b1)]);
    }
    if (lift) await touch("touchEnd", []);
  };
  // Parallel slide: a pan (the zoom does not change).
  let c0 = await camera(page);
  await gesture([cx - 60, cy], [cx + 60, cy], [cx - 60, cy + 90], [cx + 60, cy + 90]);
  let c1 = await camera(page);
  expect(await page.evaluate(() => window.__author.touchClass())).toBe("pan");
  expect(c1.halfH).toBeCloseTo(c0.halfH, 6);
  expect(Math.hypot(c1.tx - c0.tx, c1.ty - c0.ty)).toBeGreaterThan(0.5);
  // Opposing slide about the view centre: a zoom (the centre stays).
  c0 = c1;
  await gesture([cx - 40, cy], [cx + 40, cy], [cx - 120, cy], [cx + 120, cy]);
  c1 = await camera(page);
  expect(await page.evaluate(() => window.__author.touchClass())).toBe("zoom");
  expect(c1.halfH).toBeLessThan(c0.halfH * 0.6);
  expect(Math.hypot(c1.tx - c0.tx, c1.ty - c0.ty)).toBeLessThan(0.15);
  // Starts as a pan, then spreads: stays a pan.
  c0 = c1;
  await touch("touchStart", [[cx - 50, cy], [cx + 50, cy]]);
  for (let i = 1; i <= 6; i++) await touch("touchMove", [[cx - 50, cy - 8 * i], [cx + 50, cy - 8 * i]]);
  for (let i = 1; i <= 6; i++) await touch("touchMove", [[cx - 50 - 15 * i, cy - 48], [cx + 50 + 15 * i, cy - 48]]);
  c1 = await camera(page);
  expect(await page.evaluate(() => window.__author.touchClass())).toBe("pan");
  expect(c1.halfH, "a locked pan never zooms").toBeCloseTo(c0.halfH, 6);
  // Lift the fingers, one finger down again, then the second: the new
  // two-finger gesture is classified afresh — a zoom.
  await touch("touchEnd", []);
  await touch("touchStart", [[cx - 60, cy]]);
  await gesture([cx - 60, cy], [cx + 20, cy], [cx - 100, cy], [cx + 120, cy]);
  expect(await page.evaluate(() => window.__author.touchClass())).toBe("zoom");
  expect(errors).toEqual([]);
});

test("Openings mode: place windows and doors on any wall, select, drag along it, size, delete; ✗ reverts, ✓ keeps (one step each)", async ({ page }) => {
  const errors = await openApp(page);
  await drawRoom(page, [-3, -2], [3, 2]);
  const ws = await walls(page);
  const bottom = ws.find((w) => Math.abs(w.start[1] + 2) < 1e-9 && Math.abs(w.end[1] + 2) < 1e-9);
  const before = await savedBytes(page);

  // The Window tool opens the mode.
  await page.locator('.tool[data-tool="window"]').click();
  let st = await openingsState(page);
  expect(st).toMatchObject({ active: true, preset: "window", selected: null });
  await expect(page.locator("#edit-name")).toHaveText("Windows and doors");
  await expect(page.locator("#fit-btn")).toBeVisible();
  await expect(page.locator("#undo")).toBeDisabled();

  // Tap the bottom wall: a 1.2 × 1.2 m window at a 0.9 m sill, centred
  // on the tap along the wall.
  await tapWorld(page, 0.5, -1.9);
  let os = await openings(page);
  expect(os).toHaveLength(1);
  expect(os[0]).toMatchObject({ wall: bottom.id, kind: "window", width: 1.2, height: 1.2, sill: 0.9 });
  const u = 0.5 - Math.min(bottom.start[0], bottom.end[0]);
  const along = bottom.start[0] < bottom.end[0] ? os[0].offset + 0.6 : bottom.length - os[0].offset - 0.6;
  expect(Math.abs(along - u)).toBeLessThan(0.06);
  st = await openingsState(page);
  expect(st.selected).toMatchObject({ kind: "window", width: 1.2 });

  // Drag it 1 m along the wall (0.1 m grid).
  const x0 = 0.5;
  await dragClient(page, await worldToClient(page, x0, -1.9), await worldToClient(page, x0 + 1, -1.9));
  const moved = (await openings(page))[0];
  expect(Math.abs(Math.abs(moved.offset - os[0].offset) - 1)).toBeLessThan(0.06);
  expect(moved.sill, "a drag in plan keeps the sill").toBeCloseTo(0.9, 9);

  // Size it in the panel.
  await page.locator("#opening-width").fill("1.5");
  await page.locator("#opening-width").press("Enter");
  expect((await openings(page))[0].width).toBeCloseTo(1.5, 9);
  await page.locator('#opening-through').click();
  expect((await openings(page))[0].depth, "a niche").toBeCloseTo(0.1, 9);

  // Door preset on the right wall.
  await page.locator('[data-opening-preset="door"]').click();
  await tapWorld(page, 2.9, 0);
  os = await openings(page);
  expect(os).toHaveLength(2);
  const door = os.find((o) => o.kind === "door");
  expect(door).toMatchObject({ sill: 0, width: 0.9, height: 2.1 });
  // Head-on to the wall ("Wall" view).
  await page.locator('#edit-view-toggle button[data-view="elevation"]').click();
  expect((await stats(page)).view).toBe("elevation");
  await shot(page, "openings");
  await page.locator('#edit-view-toggle button[data-view="plan"]').click();
  // Every change is one step inside the session.
  await page.locator("#undo").click();
  expect(await openings(page)).toHaveLength(1);
  await page.locator("#redo").click();
  // Delete the selected door.
  await page.locator("#edit-delete").click();
  expect(await openings(page)).toHaveLength(1);

  // ✗: back to the walls without openings, byte for byte.
  await page.locator("#edit-cancel").click();
  await page.locator("#dialog-ok").click();
  expect((await openingsState(page)).active).toBe(false);
  expect(await savedBytes(page)).toBe(before);

  // ✓ keeps them; one undo removes the last placement only.
  await page.locator('.tool[data-tool="window"]').click();
  await page.locator('[data-opening-preset="window"]').click();
  await tapWorld(page, -1, -1.9);
  await tapWorld(page, 1.5, -1.9);
  await page.locator("#edit-confirm").click();
  expect(await openings(page)).toHaveLength(2);
  await page.locator("#undo").click();
  expect(await openings(page)).toHaveLength(1);
  expect((await stats(page)).errors).toEqual([]);
  expect(errors).toEqual([]);
});

test("wall Edit Mode in plan: the run's points and segments — move, insert, delete, open, extend, thickness; ✗ byte-identical; ✓ keeps each step", async ({ page }) => {
  const errors = await openApp(page);
  await drawRoom(page, [-3, -2], [3, 2]);
  const before = await savedBytes(page);
  await tapWorld(page, 0, -1.92);
  await page.locator("#prop-edit").click();
  let es = await editState(page);
  expect(es).toMatchObject({ active: true, target: "run", mode: "points" });
  expect(es.run).toMatchObject({ walls: 4, points: 4, closed: true, thickness: 0.2 });
  expect((await stats(page)).view).toBe("plan");
  expect((await page.evaluate(() => window.__author.editHud())).footprint).toHaveLength(2);

  // Drag a corner: the run follows (the walls are rewritten).
  await dragClient(page, await worldToClient(page, 3, 2), await worldToClient(page, 4, 3));
  let prof = await profile(page);
  expect(hasPoint(prof, [4, 3])).toBe(true);
  expect((await walls(page)).length).toBe(4);

  // Hold on the bottom segment: a new point; Delete merges it back.
  // (Fit first: the moved corner grew the run past the view.)
  await page.locator("#fit-btn").click();
  await shot(page, "wall-plan-edit");
  await longPressClient(page, await worldToClient(page, 0, -2));
  es = await editState(page);
  expect(es.run.points).toBe(5);
  expect((await walls(page)).length).toBe(5);
  await page.locator("#edit-delete").click();
  es = await editState(page);
  expect(es.run.points).toBe(4);
  expect((await walls(page)).length).toBe(4);

  // Edges: deleting one merges its two points.
  await page.locator('[data-edit-mode="edges"]').click();
  await tapWorld(page, -3, 0);
  await page.locator("#edit-delete").click();
  es = await editState(page);
  expect(es.run.points).toBe(3);

  // Open the loop, then extend it from its end.
  await page.locator("#run-closed").click();
  es = await editState(page);
  expect(es.run).toMatchObject({ closed: false, walls: 2 });
  await page.locator('[data-edit-tool="extend"]').click();
  prof = await profile(page);
  const last = prof.points[prof.points.length - 1].uv;
  // (Inside the view: the left of the canvas is under the edit dock.)
  await tapWorld(page, last[0] + 1, last[1] - 2);
  es = await editState(page);
  expect(es.run).toMatchObject({ points: 4, walls: 3 });

  // Thickness of the whole run.
  await page.locator("#run-thickness").fill("0.3");
  await page.locator("#run-thickness").press("Enter");
  for (const w of await walls(page)) expect(w.thickness).toBeCloseTo(0.3, 9);

  // ✗: the room as it was, byte for byte.
  await page.locator("#edit-cancel").click();
  await page.locator("#dialog-ok").click();
  expect(await savedBytes(page)).toBe(before);

  // ✓ keeps the steps: two edits, two undos.
  await tapWorld(page, 0, -1.92);
  await page.locator("#prop-edit").click();
  await dragClient(page, await worldToClient(page, 3, 2), await worldToClient(page, 4, 3));
  await page.locator("#run-thickness").fill("0.25");
  await page.locator("#run-thickness").press("Enter");
  await page.locator("#edit-confirm").click();
  expect((await walls(page)).every((w) => Math.abs(w.thickness - 0.25) < 1e-9)).toBe(true);
  await page.locator("#undo").click();
  expect((await walls(page)).every((w) => Math.abs(w.thickness - 0.2) < 1e-9)).toBe(true);
  await page.locator("#undo").click();
  expect(await savedBytes(page)).toBe(before);
  expect((await stats(page)).errors).toEqual([]);
  expect(errors).toEqual([]);
});
