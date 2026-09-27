// Minimal static dev server for VimDesignWeb.
//
// Sends the COOP/COEP headers required for SharedArrayBuffer (wasm
// threads):  Cross-Origin-Opener-Policy: same-origin
//            Cross-Origin-Embedder-Policy: require-corp
//
// Usage: node dev-server.mjs [rootDir] [port] [--no-isolation]
//   --no-isolation  omit COOP/COEP (mimics GitHub Pages: no SharedArrayBuffer)

import http from "node:http";
import { promises as fs } from "node:fs";
import path from "node:path";

const positional = process.argv.slice(2).filter((a) => !a.startsWith("--"));
const isolate = !process.argv.includes("--no-isolation");
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
    if (rel.endsWith("/")) rel += "index.html";
    const file = path.normalize(path.join(root, rel));
    if (!file.startsWith(root)) {
      res.writeHead(403).end("forbidden");
      return;
    }
    const data = await fs.readFile(file);
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
