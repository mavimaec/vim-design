// Playwright configuration for VimDesignWebTest.
import { defineConfig, devices } from "@playwright/test";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, "..");
const wwwDir = path.join(repoRoot, "crates", "vim-design-web", "www");
const serverScript = path.join(repoRoot, "devops", "lib", "dev-server.mjs");

// The authoring app specs (app-*.spec.js) run against the www/ checkout
// served in the GitHub Pages layout (dev-server --pages-layout: no
// COOP/COEP, single-threaded bundle as ./pkg/, app.html as the root) —
// the same conditions as https://mavimaec.github.io/vim-design/.
export const APP_URL = "http://localhost:8791/";
const APP_SPECS = /app-.*\.spec\.js/;

export default defineConfig({
  testDir: "./tests",
  timeout: 120_000,
  retries: 0,
  reporter: [["list"]],
  use: {
    baseURL: "http://localhost:8787",
    // wasm threads need cross-origin isolation; the dev server provides
    // the COOP/COEP headers.
    browserName: "chromium",
    // NOTE on WebGPU in headless chromium (investigated 2026-08-22): with
    // "--enable-unsafe-webgpu" (+/- "--use-webgpu-adapter=swiftshader" /
    // "--enable-features=Vulkan" / "--use-angle=vulkan") an adapter comes
    // up and wgpu renders without validation errors, but the presented
    // canvas never reaches the headless compositor — screenshots stay
    // fully transparent (raw-JS WebGPU clears reproduce this too). CI
    // therefore runs WITHOUT WebGPU flags: navigator.gpu is absent and
    // the demo takes its WebGL2 (SwiftShader) fallback path, which
    // composites correctly. Real desktop browsers get WebGPU.
  },
  projects: [
    {
      // Every spec (demo, probe, authoring app) at the default 1280x720.
      name: "desktop",
      use: { browserName: "chromium" },
    },
    {
      // The authoring app on an emulated phone: touch input, DPR 2.6,
      // 412x839 viewport.
      name: "mobile",
      testMatch: APP_SPECS,
      use: { ...devices["Pixel 7"], browserName: "chromium" },
    },
  ],
  webServer: [
    {
      command: `node "${serverScript}" "${wwwDir}" 8787`,
      url: "http://localhost:8787/index.html",
      reuseExistingServer: true,
      timeout: 30_000,
    },
    {
      command: `node "${serverScript}" "${wwwDir}" 8791 --pages-layout`,
      url: APP_URL,
      reuseExistingServer: true,
      timeout: 30_000,
    },
  ],
});
