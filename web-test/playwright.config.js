// Playwright configuration for VimDesignWebTest.
import { defineConfig } from "@playwright/test";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, "..");
const wwwDir = path.join(repoRoot, "crates", "vim-design-web", "www");
const serverScript = path.join(repoRoot, "devops", "lib", "dev-server.mjs");

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
  webServer: {
    command: `node "${serverScript}" "${wwwDir}" 8787`,
    url: "http://localhost:8787/index.html",
    reuseExistingServer: true,
    timeout: 30_000,
  },
});
