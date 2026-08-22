// VimDesignWebTest — loads the wasm threading probe page, waits for its
// verdict, records it, and takes a screenshot.
//
// NOTE: this test passes whether or not true parallelism was achieved —
// its job is to capture and report the verdict, not to gate on it.

import { test, expect } from "@playwright/test";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { mkdirSync } from "node:fs";

const here = path.dirname(fileURLToPath(import.meta.url));
const screenshotDir = path.join(here, "..", "screenshots");

test("threading probe page loads and reports a verdict", async ({ page }) => {
  const consoleErrors = [];
  page.on("pageerror", (e) => consoleErrors.push(String(e)));

  await page.goto("/probe.html");
  await expect(page).toHaveTitle(/VIM Design/);

  // The probe runs a multi-second wasm workload; wait for the verdict.
  const verdictEl = page.locator("#verdict");
  await expect(verdictEl).toHaveAttribute("data-verdict", /^(PASS|FAIL)$/, {
    timeout: 90_000,
  });

  const verdict = await verdictEl.getAttribute("data-verdict");
  const verdictText = await verdictEl.textContent();
  const result = await page.evaluate(() => window.__probeResult);

  console.log("");
  console.log("================ WASM THREADING PROBE ================");
  console.log(`VERDICT: ${verdict}`);
  console.log(verdictText);
  console.log(JSON.stringify(result, null, 2));
  console.log("======================================================");

  test.info().annotations.push({
    type: "wasm-threading-verdict",
    description: `${verdict}: ${verdictText}`,
  });

  mkdirSync(screenshotDir, { recursive: true });
  await page.screenshot({
    path: path.join(screenshotDir, "threading-probe.png"),
    fullPage: true,
  });

  // The page must have loaded and produced a definite verdict.
  expect(["PASS", "FAIL"]).toContain(verdict);
  expect(result).toBeTruthy();
  expect(consoleErrors, `page errors: ${consoleErrors.join("; ")}`).toEqual([]);
});
