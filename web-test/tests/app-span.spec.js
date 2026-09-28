// VimDesignWebTest — the plan span section diagram (Level sheet), driven
// ONLY through the page's real controls: fields typed, buttons pressed,
// sliders, handles, and regions dragged (mouse on desktop, touch on the
// phone). Also: the Level sheet follows the active level, and the sheet
// header's color chip is the level's color picker. Both projects.

import { test, expect } from "@playwright/test";
import { mobile, shot, openApp, drawPlate, dragClient } from "./lib/app-helpers.js";

const levels = (page) => page.evaluate(() => JSON.parse(window.__author.app.levels_json()));
const levelId = async (page, name) => (await levels(page)).levels.find((l) => l.name === name).id;
const span = async (page, name) => {
  const id = await levelId(page, name);
  return page.evaluate((l) => JSON.parse(window.__author.app.plan_span_json(l)), id);
};
const center = async (loc) => {
  const b = await loc.boundingBox();
  return [b.x + b.width / 2, b.y + b.height / 2];
};
const toasts = (page) => page.evaluate(() => window.__author.toasts.map((t) => t.msg));

/** Open a level's sheet with its tree pencil (hover on desktop; on the
 *  phone the active row shows its actions). */
async function openLevelSheet(page, name) {
  if (mobile()) {
    await page.locator("#tree-btn").click();
    const row = page.locator("#sheet-body .tree-row.level", { hasText: name });
    if (!(await row.locator("[data-tree-level-edit]").isVisible())) await row.locator(".tree-name").click();
    await row.locator("[data-tree-level-edit]").click();
  } else {
    const row = page.locator("#tree-body .tree-row.level", { hasText: name });
    await row.hover();
    await row.locator("[data-tree-level-edit]").click();
  }
  await expect(page.locator("#sheet-title")).toHaveText(name);
  await page.locator("#span-diagram").scrollIntoViewIfNeeded();
}

async function typeField(page, sel, value) {
  await page.locator(sel).fill(String(value));
  await page.locator(sel).press("Enter");
}

test("fields, Top mode, and Reset work by real input; a refusal toasts and snaps back", async ({ page }) => {
  const errors = await openApp(page);
  await drawPlate(page, [-3, -2], [3, 2]);
  await openLevelSheet(page, "Ground");
  await expect(page.locator("#span-reset")).toBeDisabled();

  // Cut: typed; the handle follows.
  await typeField(page, "#span-cut", 1.5);
  expect((await span(page, "Ground")).cut).toBeCloseTo(1.5, 9);
  await expect(page.locator(".sd-h-cut .sd-val")).toHaveText("Cut +1.50");
  await expect(page.locator("#span-cut")).toHaveValue("1.50");
  await expect(page.locator("#span-reset")).toBeEnabled();

  // Top: Offset, then a typed top, then back to Next story.
  await page.locator('[data-span-top="offset"]').click();
  expect((await span(page, "Ground")).top.mode).toBe("offset");
  await expect(page.locator('[data-span-top="offset"]')).toHaveClass(/on/);
  await typeField(page, "#span-top", 2.6);
  expect((await span(page, "Ground")).top).toMatchObject({ mode: "offset", offset: 2.6 });
  await expect(page.locator(".sd-h-top .sd-val")).toHaveText("Top +2.60");
  await page.locator('[data-span-top="next"]').click();
  expect((await span(page, "Ground")).top.mode).toBe("next");
  await expect(page.locator(".sd-h-top")).toHaveClass(/locked/);
  await expect(page.locator("#span-top")).toHaveValue("3.00");

  // A bottom above the cut is refused: a toast says why, the field snaps back.
  await typeField(page, "#span-bottom", 2);
  expect((await span(page, "Ground")).bottom).toBe(0);
  await expect(page.locator("#span-bottom")).toHaveValue("0.00");
  expect((await toasts(page)).some((m) => /bottom must be below the cut/i.test(m))).toBe(true);

  // Opacity: slider (clicked) and field; the region's alpha follows.
  const sl = await page.locator("#span-above").boundingBox();
  await page.mouse.click(sl.x + sl.width * 0.8, sl.y + sl.height / 2);
  let sp = await span(page, "Ground");
  expect(sp.above).toBeGreaterThan(0.7);
  await expect(page.locator("#span-above-pct")).toHaveValue(String(Math.round(sp.above * 100)));
  await expect(page.locator(".sd-above .sd-region-label")).toHaveText(`Above · ${Math.round(sp.above * 100)} %`);
  expect(Number(await page.locator(".sd-above").evaluate((n) => n.style.getPropertyValue("--a")))).toBeCloseTo(sp.above, 6);
  await typeField(page, "#span-below-pct", 10);
  expect((await span(page, "Ground")).below).toBeCloseTo(0.1, 6);

  // Reset after all that: the defaults, visibly.
  await page.locator("#span-reset").click();
  sp = await span(page, "Ground");
  expect(sp).toMatchObject({ custom: false, cut: 1.2, bottom: 0, above: 0.25 });
  await expect(page.locator("#span-cut")).toHaveValue("1.20");
  await expect(page.locator("#span-above-pct")).toHaveValue("25");
  await expect(page.locator(".sd-h-cut .sd-val")).toHaveText("Cut +1.20");
  await expect(page.locator("#span-reset")).toBeDisabled();
  expect(errors).toEqual([]);
});

test("the diagram: drag top / cut / bottom and an opacity region; keys; an invalid drag snaps back; one undo per drag", async ({ page }) => {
  const errors = await openApp(page);
  await drawPlate(page, [-3, -2], [3, 2]);
  await openLevelSheet(page, "Ground");
  const px = await page.evaluate(() => {
    const s = document.querySelector(".sd-strip").getBoundingClientRect();
    return s.height;
  });
  expect(px).toBeGreaterThan(200);

  // Cut: dragged up ~0.5 m (one step).
  const [cx, cy] = await center(page.locator(".sd-h-cut"));
  const pxPerM = await page.evaluate(() => {
    const cut = document.querySelector(".sd-h-cut").getBoundingClientRect();
    const bottom = document.querySelector(".sd-h-bottom").getBoundingClientRect();
    return (bottom.top - cut.top) / 1.2; // cut +1.20 over bottom 0
  });
  await dragClient(page, [cx, cy], [cx, cy - pxPerM * 0.5]);
  let sp = await span(page, "Ground");
  expect(sp.cut).toBeGreaterThan(1.55);
  expect(sp.cut).toBeLessThan(1.85);
  await expect(page.locator("#span-cut")).toHaveValue(sp.cut.toFixed(2));
  await page.locator("#undo").click();
  expect((await span(page, "Ground")).cut).toBeCloseTo(1.2, 9);
  await expect(page.locator("#span-cut")).toHaveValue("1.20");

  // Bottom dragged above the cut: refused, toast, snaps back.
  const [bx, by] = await center(page.locator(".sd-h-bottom"));
  await dragClient(page, [bx, by], [bx, by - pxPerM * 2]);
  expect((await span(page, "Ground")).bottom).toBe(0);
  expect((await toasts(page)).some((m) => /bottom must be below the cut/i.test(m))).toBe(true);
  await expect(page.locator(".sd-h-bottom .sd-val")).toHaveText("Bottom 0.00");

  // Top: dragged up off Level 2 → Offset; back onto Level 2 → Next story.
  let [tx, ty] = await center(page.locator(".sd-h-top"));
  await dragClient(page, [tx, ty], [tx, ty - pxPerM * 0.6]);
  sp = await span(page, "Ground");
  expect(sp.top.mode).toBe("offset");
  expect(sp.top.offset).toBeGreaterThan(3.3);
  await expect(page.locator(".sd-h-top")).not.toHaveClass(/locked/);
  [tx, ty] = await center(page.locator(".sd-h-top"));
  const level2Y = await page.evaluate(() => {
    const t = [...document.querySelectorAll(".sd-level")].find((n) => n.textContent === "Level 2");
    return t.getBoundingClientRect().top;
  });
  await dragClient(page, [tx, ty], [tx, level2Y + 2]);
  expect((await span(page, "Ground")).top.mode).toBe("next");
  await expect(page.locator(".sd-h-top")).toHaveClass(/locked/);

  // Keys: the bottom handle steps 5 cm.
  await page.locator(".sd-h-bottom").focus();
  await page.keyboard.press("ArrowDown");
  expect((await span(page, "Ground")).bottom).toBeCloseTo(-0.05, 9);

  // A drag across the below region sets its opacity.
  const r = await page.locator(".sd-below").boundingBox();
  await dragClient(page, [r.x + r.width * 0.2, r.y + r.height - 12], [r.x + r.width * 0.9, r.y + r.height - 12]);
  sp = await span(page, "Ground");
  expect(sp.below).toBeGreaterThan(0.8);
  await expect(page.locator("#span-below-pct")).toHaveValue(String(Math.round(sp.below * 100)));
  await shot(page, "plan-span-diagram");
  expect(errors).toEqual([]);
});

test("the Level sheet follows the active level; its header chip is the color picker (one undo)", async ({ page }) => {
  const errors = await openApp(page);
  await openLevelSheet(page, "Level 2");
  // Switch the active level from the level chip.
  await page.locator("#level-chip").click();
  await page.locator(`.pop-item[data-level="${await levelId(page, "Ground")}"]`).click();
  await expect(page.locator("#sheet-title")).toHaveText("Ground");
  await expect(page.locator("#sheet-chip")).toBeVisible();
  const before = (await levels(page)).levels.find((l) => l.name === "Ground").color;
  await page.locator("#sheet-chip-input").evaluate((n) => {
    n.value = "#ff0000";
    n.dispatchEvent(new Event("input", { bubbles: true }));
    n.dispatchEvent(new Event("change", { bubbles: true }));
  });
  let c = (await levels(page)).levels.find((l) => l.name === "Ground").color;
  expect(c[0]).toBeCloseTo(1, 2);
  expect(c[1]).toBeCloseTo(0, 2);
  await page.locator("#undo").click();
  c = (await levels(page)).levels.find((l) => l.name === "Ground").color;
  expect(c[0]).toBeCloseTo(before[0], 4);
  // The separate Color row is gone.
  await expect(page.locator("#lvl-color")).toHaveCount(0);
  expect(errors).toEqual([]);
});
