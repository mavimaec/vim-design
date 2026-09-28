// VimDesignWebTest — the authoring app, milestone 6 (Phase B): rooms are
// the library's Room entities; a plane's RoomLayout (owned by a "Room
// walls" element) generates ONE wall network from them. Both Playwright
// projects.

import { test, expect } from "@playwright/test";
import {
  mobile, shot, openApp, stats, elements, editState, openingsState, tapWorld, tool, shape, dragClient, worldToClient,
  PARTITION_M,
} from "./lib/app-helpers.js";

const rooms = (page) => page.evaluate(() => JSON.parse(window.__author.app.rooms_json()));
const roomsHud = (page) => page.evaluate(() => JSON.parse(window.__author.app.rooms_hud_json()));
const roomWalls = async (page) => (await elements(page)).filter((e) => e.kind === "room_walls");
const levels = (page) => page.evaluate(() => JSON.parse(window.__author.app.levels_json()));
const byName = (rs, name) => rs.find((r) => r.name === name);

/** Rooms tool: rectangles by two opposite corners. */
async function drawRooms(page, rects) {
  await tool(page, "room");
  await shape(page, "rect");
  for (const [a, b] of rects) {
    await tapWorld(page, ...a);
    await tapWorld(page, ...b);
  }
  await tool(page, "select");
}

/** The walls' volume is their united footprint × the wall height. */
function expectPrisms(w) {
  expect(w.volume, `${w.name}: volume = footprint × H`).toBeCloseTo(w.footprintArea * w.effectiveHeight, 3);
}

test("two rooms share one wall; a room on top cuts into them; restack with undo; a hidden wall leaves two labels", async ({ page }) => {
  const errors = await openApp(page);
  await drawRooms(page, [[[-3, -2], [0, 1]], [[0, -2], [3, 1]]]);
  let rs = await rooms(page);
  expect(rs.map((r) => r.name)).toEqual(["Room 001", "Room 002"]);
  for (const r of rs) expect(r).toMatchObject({ area: 9, status: "whole" });
  let [w] = await roomWalls(page);
  expect(w).toMatchObject({ name: "Room walls", rooms: 2, mode: "fixed", effectiveHeight: 2.7 });
  expect(w.thickness).toBeCloseTo(PARTITION_M, 9);
  // Centered walls on 7 segments (the shared edge once), united.
  const T = PARTITION_M;
  expect(w.footprintArea).toBeCloseTo((6 + T) * (3 + T) - 2 * (3 - T) * (3 - T), 6);
  expectPrisms(w);
  expect((await stats(page)).elements).toBe(1);
  await shot(page, "rooms-plan");
  await page.evaluate(() => window.__author.app.room_select(-1));
  await page.locator('#view-toggle button[data-view="3d"]').click();
  await page.locator("#fit-btn").click();
  await shot(page, "rooms-3d");
  await page.locator('#view-toggle button[data-view="plan"]').click();
  await page.evaluate(() => { window.__author.app.set_camera_json('{"tx":0,"ty":-0.5,"halfH":6}'); window.__author.refresh(); });

  // A room over both, drawn last: on top, it cuts into them.
  await drawRooms(page, [[[-1, -1], [1, 0]]]);
  rs = await rooms(page);
  expect(rs.map((r) => [r.name, r.rank, r.area])).toEqual([["Room 001", 3, 8], ["Room 002", 2, 8], ["Room 003", 1, 2]]);

  // Room 001's page: forward twice, it takes its part back.
  await tapWorld(page, -2.5, 0.5);
  await expect(page.locator("#sheet-title")).toHaveText("Room 001");
  await page.locator("#room-forward").click();
  await page.locator("#room-forward").click();
  rs = await rooms(page);
  expect(byName(rs, "Room 001")).toMatchObject({ rank: 1, area: 9 });
  expect(byName(rs, "Room 003").area).toBeCloseTo(1, 6);
  await page.locator("#undo").click();
  expect(byName(await rooms(page), "Room 001").rank).toBe(2);
  await page.locator("#redo").click();
  expect(byName(await rooms(page), "Room 001").rank).toBe(1);
  // Rename (one step).
  await page.locator("#room-name").fill("Kitchen");
  await page.locator("#room-name").press("Enter");
  expect(byName(await rooms(page), "Kitchen")).toBeTruthy();

  // Room Edit Mode: hide the kitchen's east wall (the shared one).
  const before = (await roomWalls(page))[0];
  await page.locator("#room-edit").click();
  expect(await editState(page)).toMatchObject({ active: true, target: "room", mode: "edges" });
  await tapWorld(page, 0, 0.6);
  expect((await editState(page)).room.selectedEdges).toBe(1);
  await page.locator("#room-hidden").click();
  expect((await editState(page)).room).toMatchObject({ hidden: 1, selectedHidden: true });
  await shot(page, "room-edit");
  await page.locator("#edit-confirm").click();
  const hud = await roomsHud(page);
  expect(hud.hidden.length, "the separator is dashed").toBeGreaterThan(0);
  expect(hud.labels).toHaveLength(3);
  w = (await roomWalls(page))[0];
  expect(w.footprintArea).toBeLessThan(before.footprintArea);
  expectPrisms(w);
  // One undo brings the wall back.
  await page.locator("#undo").click();
  expect((await roomWalls(page))[0].footprintArea).toBeCloseTo(before.footprintArea, 6);
  expect((await stats(page)).errors).toEqual([]);
  expect(errors).toEqual([]);
});

test("room edit: a shared corner moves both rooms and their wall; delete a room (its openings go)", async ({ page }) => {
  const errors = await openApp(page);
  await drawRooms(page, [[[-3, -2], [0, 1]], [[0, -2], [3, 1]]]);
  await tapWorld(page, 1.5, 0);
  await expect(page.locator("#sheet-title")).toHaveText("Room 002");
  await page.locator("#room-edit").click();
  await page.locator('[data-edit-mode="points"]').click();
  // Drag the shared top corner (0, 1) up to (0, 2).
  await dragClient(page, await worldToClient(page, 0, 1), await worldToClient(page, 0, 2));
  await page.locator("#edit-confirm").click();
  const rs = await rooms(page);
  for (const name of ["Room 001", "Room 002"]) {
    expect(byName(rs, name).boundary.some(([x, y]) => Math.abs(x) < 1e-9 && Math.abs(y - 2) < 1e-9), `${name} follows`).toBe(true);
    expect(byName(rs, name).area).toBeCloseTo(9 + 1.5, 6);
  }
  const [w] = await roomWalls(page);
  expect(w.bbox[1][1]).toBeGreaterThan(2);
  expectPrisms(w);
  // One step for both rooms.
  await page.locator("#undo").click();
  expect(byName(await rooms(page), "Room 001").area).toBeCloseTo(9, 6);
  await page.locator("#redo").click();

  // A door in Room 002's south wall; deleting the room takes it.
  await page.locator('.tool[data-tool="window"]').click();
  await page.locator('[data-opening-preset="door"]').click();
  await tapWorld(page, 1.5, -2);
  expect((await roomWalls(page))[0].openings).toBe(1);
  await page.locator("#edit-confirm").click();
  await tapWorld(page, 1.5, 0);
  await page.locator("#room-delete").click();
  expect((await rooms(page)).map((r) => r.name)).toEqual(["Room 001"]);
  expect((await roomWalls(page))[0].openings).toBe(0);
  await page.locator("#undo").click();
  expect(await rooms(page)).toHaveLength(2);
  expect((await roomWalls(page))[0].openings).toBe(1);
  // The last room takes the room walls with it.
  await tapWorld(page, 1.5, 0);
  await page.locator("#room-delete").click();
  await tapWorld(page, -1.5, 0);
  await page.locator("#room-delete").click();
  expect(await rooms(page)).toHaveLength(0);
  expect(await roomWalls(page)).toHaveLength(0);
  expect(errors).toEqual([]);
});

test("openings on room walls: a window in the shared wall, a door, copy / paste; drag along the wall", async ({ page }) => {
  const errors = await openApp(page);
  await drawRooms(page, [[[-3, -2], [0, 1]], [[0, -2], [3, 1]]]);
  await page.locator('.tool[data-tool="window"]').click();
  // The shared wall: a window, anchored to the room edge that covers it.
  await tapWorld(page, 0.02, -0.5);
  let st = await openingsState(page);
  expect(st.selected).toMatchObject({ kind: "window", roomWalls: true });
  expect(st.selected.wallName).toMatch(/^Room 00[12] wall$/);
  expect(st.fresh).toBe(true);
  // A door in Room 001's north wall.
  await page.locator('[data-opening-preset="door"]').click();
  await tapWorld(page, -1.5, 1);
  st = await openingsState(page);
  expect(st.selected).toMatchObject({ kind: "door", wallName: "Room 001 wall" });
  let [w] = await roomWalls(page);
  expect(w.openingList.map((o) => o.kind)).toEqual(["window", "door"]);
  expect(w.volume, "the openings cut the walls").toBeLessThan(w.footprintArea * w.effectiveHeight - 0.3);
  // Copy the door, paste it on Room 002's north wall.
  await page.locator("#copy-btn").click();
  await page.locator("#paste-btn").click();
  await tapWorld(page, 1.5, 1);
  w = (await roomWalls(page))[0];
  expect(w.openingList.map((o) => o.kind)).toEqual(["window", "door", "door"]);
  expect(w.openingList[2].roomName).toBe("Room 002");
  await page.locator("#undo").click();
  expect((await roomWalls(page))[0].openings).toBe(2);
  await page.locator("#redo").click();
  await page.locator("#paste-btn").click();
  // Drag the first door 1 m along its wall (one step).
  const door = (await roomWalls(page))[0].openingList[1];
  await dragClient(page, await worldToClient(page, -1.5, 1), await worldToClient(page, -2.5, 1));
  const moved = (await roomWalls(page))[0].openingList.find((o) => o.id === door.id);
  expect(moved.offset).not.toBeCloseTo(door.offset, 3);
  await page.locator('#edit-view-toggle button[data-view="3d"]').click();
  await page.locator("#fit-btn").click();
  await shot(page, "rooms-openings");
  await page.locator("#edit-confirm").click();
  expect((await stats(page)).errors).toEqual([]);
  expect(errors).toEqual([]);
});

test("room walls up to a plane; the top plane deleted disconnects; a base-only layout drags transform-only; a level cascade; reload", async ({ page }) => {
  const errors = await openApp(page);
  await drawRooms(page, [[[-3, -2], [0, 1]], [[0, -2], [3, 1]]]);
  const ls = await levels(page);
  const ground = ls.levels.find((l) => l.name === "Ground");
  const level2 = ls.levels.find((l) => l.name === "Level 2");
  // A base-only layout: a Ground elevation drag moves it as a transform.
  let s = await page.evaluate((id) => { window.__author.app.update_level_elevation(id, 0.5); return JSON.parse(window.__author.app.stats_json()); }, ground.id);
  expect(s.lastMeshUpserts, "base-only room walls: transform only").toBe(0);
  await page.evaluate((id) => { window.__author.app.update_level_elevation(id, 0); window.__author.refresh(); }, ground.id);

  // A tap on a wall selects the Room walls element: its settings (no
  // Delete: the walls come from the rooms). Thickness: one step.
  await tapWorld(page, -3, -0.5);
  await expect(page.locator("#sheet-title")).toHaveText("Room walls");
  await expect(page.locator("#prop-delete")).toHaveCount(0);
  await page.locator("#room-wall-thickness").fill("0.2");
  await page.locator("#room-wall-thickness").press("Enter");
  expect((await roomWalls(page))[0].thickness).toBeCloseTo(0.2, 9);
  expectPrisms((await roomWalls(page))[0]);
  await page.locator("#undo").click();
  expect((await roomWalls(page))[0].thickness).toBeCloseTo(PARTITION_M, 9);
  await page.locator("#sheet-close").click();

  // Up to Level 2 from the room page's wall settings.
  await tapWorld(page, -1.5, -0.5);
  await page.locator('#room-wall-mode button[data-wall-mode="upto"]').click();
  await page.locator("#sheet-close").click();
  let [w] = await roomWalls(page);
  expect(w).toMatchObject({ mode: "upto", topPlane: level2.id });
  expect(w.effectiveHeight).toBeCloseTo(3, 9);
  expectPrisms(w);
  // Deleting Level 2 disconnects the walls at their height.
  await page.evaluate((id) => { window.__author.app.delete_level_cascade(id); window.__author.refresh(); }, level2.id);
  [w] = await roomWalls(page);
  expect(w.mode).toBe("fixed");
  expect(w.effectiveHeight).toBeCloseTo(3, 9);
  await page.locator("#undo").click();
  expect((await roomWalls(page))[0].mode).toBe("upto");

  // Rooms on Level 2; deleting it (cascade) takes them and their walls.
  await page.evaluate((id) => { window.__author.app.set_active_level(id); window.__author.refresh(); }, level2.id);
  await drawRooms(page, [[[-3, -2], [0, 1]]]);
  expect(await rooms(page)).toHaveLength(3);
  expect(await roomWalls(page)).toHaveLength(2);
  expect((await roomWalls(page)).map((x) => x.name).sort()).toEqual(["Room walls", "Room walls 2"]);
  await page.evaluate((id) => { window.__author.app.delete_level_cascade(id); window.__author.refresh(); }, level2.id);
  expect(await rooms(page)).toHaveLength(2);
  expect(await roomWalls(page)).toHaveLength(1);

  // Persisted with the document.
  await page.evaluate(() => window.__author.saveNow());
  await page.reload();
  await page.waitForFunction(() => window.__author?.ready === true);
  expect((await rooms(page)).map((r) => r.name)).toEqual(["Room 001", "Room 002"]);
  expectPrisms((await roomWalls(page))[0]);
  expect(errors).toEqual([]);
});
