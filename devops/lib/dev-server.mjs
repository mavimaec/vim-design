// Minimal static dev server for VimDesignWeb.
//
// Sends the COOP/COEP headers required for SharedArrayBuffer (wasm
// threads):  Cross-Origin-Opener-Policy: same-origin
//            Cross-Origin-Embedder-Policy: require-corp
//
// Usage: node dev-server.mjs [rootDir] [port] [--no-isolation] [--pages-layout]
//   --no-isolation  omit COOP/COEP (mimics GitHub Pages: no SharedArrayBuffer)
//   --pages-layout  serve a www/ checkout the way the GitHub Pages site is laid
//                   out (devops/vpages.ps1), without a build step: implies
//                   --no-isolation, "/" is app.html, and /pkg/* is answered
//                   from pkg-st/ (the single-threaded bundle). Used by the
//                   Playwright specs of the authoring app.

import http from "node:http";
import { promises as fs } from "node:fs";
import path from "node:path";

const positional = process.argv.slice(2).filter((a) => !a.startsWith("--"));
const pagesLayout = process.argv.includes("--pages-layout");
const isolate = !process.argv.includes("--no-isolation") && !pagesLayout;
const root = path.resolve(positional[0] ?? "crates/vim-design-web/www");
const port = Number(positional[1] ?? 8787);

const MIME = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".wasm": "application/wasm",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".map": "application/json; charset=utf-8",
  ".png": "image/png",
  ".svg": "image/svg+xml",
  ".ico": "image/x-icon",
};

const server = http.createServer(async (req, res) => {
  try {
    const url = new URL(req.url, `http://${req.headers.host}`);
    let rel = decodeURIComponent(url.pathname);
    if (pagesLayout) {
      if (rel === "/" || rel === "/index.html") rel = "/app.html";
      else if (rel === "/demo.html") rel = "/index.html";
      else if (rel.startsWith("/pkg/")) rel = "/pkg-st/" + rel.slice("/pkg/".length);
    }
    if (rel.endsWith("/")) rel += "index.html";
    const file = path.normalize(path.join(root, rel));
    if (!file.startsWith(root)) {
      res.writeHead(403).end("forbidden");
      return;
    }
    let data;
    try {
      data = await fs.readFile(file);
    } catch (e) {
      // Pages layout without a build: synthesize the build stamp that
      // devops/vpages.ps1 would write, so the app's About page works.
      if (!(pagesLayout && rel === "/version.json" && e.code === "ENOENT")) throw e;
      data = JSON.stringify({
        commit: "dev", short: "dev", dirty: true, ref: "local dev server",
        built_at: new Date().toISOString(),
      });
    }
    const headers = {
      "Content-Type": MIME[path.extname(file)] ?? "application/octet-stream",
      "Cache-Control": "no-store",
    };
    if (isolate) {
      headers["Cross-Origin-Opener-Policy"] = "same-origin";
      headers["Cross-Origin-Embedder-Policy"] = "require-corp";
    }
    res.writeHead(200, headers);
    res.end(data);
  } catch (e) {
    res.writeHead(e.code === "ENOENT" ? 404 : 500, {
      "Content-Type": "text/plain",
    });
    res.end(String(e.code ?? e));
  }
});

server.listen(port, () => {
  console.log(`vim-design dev server: http://localhost:${port}/ (root: ${root})`);
  console.log(
    isolate
      ? "COOP/COEP headers enabled — SharedArrayBuffer available."
      : "COOP/COEP headers OFF (GitHub Pages mode) — single-threaded only."
  );
});
