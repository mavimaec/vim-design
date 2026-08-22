// VIM Design — interactive demo driver.
//
// Loads the wasm module (threaded bundle in ./pkg, which degrades to
// sequential evaluation when the pool is not initialized), creates the
// DemoApp (document + engine + wgpu renderer), and wires the DOM: six
// parameter sliders, wireframe toggle, undo/redo, orbit/zoom camera.
//
// Test hooks:
//   window.__vimReady    true once the initial scene is settled + rendered
//   window.__vimStats    latest stats_json() payload (parsed)
//   window.__latencyLog  { op: [commit->mesh-ready ms, ...] }
//   window.__vimError    set if initialization failed

const $ = (id) => document.getElementById(id);
const statusbar = $("statusbar");

window.__vimReady = false;
window.__latencyLog = {};

function showStatus(stats) {
  window.__vimStats = stats;
  const err = stats.errors.length
    ? ` | <span class="err">errors: ${stats.errors.join("; ")}</span>`
    : "";
  statusbar.innerHTML =
    `${stats.backend}` +
    ` | gen ${stats.evaluated}/${stats.committed}${stats.settled ? "" : " (pending)"}` +
    ` | ${stats.triangles.toLocaleString()} tris` +
    ` | ${stats.lastOp}: ${stats.lastLatencyMs.toFixed(1)} ms commit→mesh` +
    err;
  $("undo").disabled = !stats.canUndo;
  $("redo").disabled = !stats.canRedo;
}

function refresh(app, op) {
  const stats = JSON.parse(app.stats_json());
  if (op) {
    (window.__latencyLog[op] ??= []).push(stats.lastLatencyMs);
  }
  showStatus(stats);
  return stats;
}

async function main() {
  statusbar.textContent = "loading wasm module…";
  const mod = await import("./pkg/vim_design_web.js");
  await mod.default();

  // Keep the rayon pool alive for parity with the probe page (evaluation
  // itself is sequential on wasm in this milestone).
  if (mod.threads_supported?.() && globalThis.crossOriginIsolated === true && mod.initThreadPool) {
    try {
      await mod.initThreadPool(navigator.hardwareConcurrency ?? 1);
    } catch (e) {
      console.warn("initThreadPool failed (continuing single-threaded):", e);
    }
  }

  const canvas = $("view");
  const fitCanvas = () => {
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    canvas.width = Math.max(1, Math.round(canvas.clientWidth * dpr));
    canvas.height = Math.max(1, Math.round(canvas.clientHeight * dpr));
  };
  fitCanvas();

  statusbar.textContent = "initializing renderer…";
  const app = await mod.DemoApp.create("view");

  window.addEventListener("resize", () => {
    fitCanvas();
    app.resize(canvas.width, canvas.height);
  });

  // -- Camera: drag to orbit, wheel to zoom ---------------------------
  let dragging = false, lastX = 0, lastY = 0;
  canvas.addEventListener("pointerdown", (e) => {
    dragging = true;
    lastX = e.clientX;
    lastY = e.clientY;
    canvas.setPointerCapture(e.pointerId);
  });
  canvas.addEventListener("pointermove", (e) => {
    if (!dragging) return;
    app.orbit(e.clientX - lastX, e.clientY - lastY);
    lastX = e.clientX;
    lastY = e.clientY;
  });
  canvas.addEventListener("pointerup", () => { dragging = false; });
  canvas.addEventListener("wheel", (e) => {
    e.preventDefault();
    app.zoom(e.deltaY);
  }, { passive: false });

  // -- Sliders ---------------------------------------------------------
  const sliders = [
    ["cube-size",       "cube size",       "cubeSize",       (v) => app.set_cube_size(v)],
    ["plate-thickness", "plate height",    "plateThickness", (v) => app.set_plate_thickness(v)],
    ["cyl-radius",      "cylinder radius", "cylRadius",      (v) => app.set_cylinder_radius(v)],
    ["cyl-height",      "cylinder height", "cylHeight",      (v) => app.set_cylinder_height(v)],
    ["cone-radius",     "cone radius",     "coneRadius",     (v) => app.set_cone_radius(v)],
    ["cone-height",     "cone height",     "coneHeight",     (v) => app.set_cone_height(v)],
  ];
  for (const [id, op, , apply] of sliders) {
    const input = $(id);
    const label = $(`${id}-val`);
    input.addEventListener("input", () => {
      const v = parseFloat(input.value);
      label.textContent = `${v.toFixed(2)} m`;
      apply(v);
      refresh(app, op);
    });
  }

  // Resynchronize every slider position + label from the document (the
  // single source of truth) — at startup and after undo/redo, when the
  // model changes without the sliders being touched.
  const syncSlidersFromDocument = () => {
    const params = JSON.parse(app.params_json());
    for (const [id, , key] of sliders) {
      const v = params[key];
      $(id).value = String(v);
      $(`${id}-val`).textContent = `${v.toFixed(2)} m`;
    }
  };

  // -- Display options ---------------------------------------------------
  $("wireframe").addEventListener("change", (e) => {
    app.set_wireframe(e.target.checked);
    refresh(app);
  });

  // -- Undo / redo -------------------------------------------------------
  $("undo").addEventListener("click", () => {
    if (app.undo()) {
      refresh(app, "undo");
      syncSlidersFromDocument();
    }
  });
  $("redo").addEventListener("click", () => {
    if (app.redo()) {
      refresh(app, "redo");
      syncSlidersFromDocument();
    }
  });

  // -- Frame loop --------------------------------------------------------
  let frames = 0;
  const frame = () => {
    try {
      app.render();
      frames++;
      if (!window.__vimReady && frames >= 2 && window.__vimStats?.settled) {
        window.__vimReady = true;
      }
    } catch (e) {
      console.error("render failed:", e);
      window.__vimError = String(e);
      statusbar.textContent = `render error: ${e}`;
      return; // stop the loop
    }
    requestAnimationFrame(frame);
  };

  refresh(app, "initial scene");
  syncSlidersFromDocument();
  requestAnimationFrame(frame);
}

main().catch((e) => {
  console.error(e);
  window.__vimError = String(e);
  statusbar.textContent = `error: ${e}`;
});
