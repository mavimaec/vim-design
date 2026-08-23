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

// Reaction to the parametric dirty pump; assigned in main() once the
// sliders exist. Receives the id of the slider being actively dragged
// (if any) so the resync never fights the drag.
let onParamsDirty = null;

function refresh(app, op, activeSliderId) {
  const stats = JSON.parse(app.stats_json());
  if (op) {
    (window.__latencyLog[op] ??= []).push(stats.lastLatencyMs);
  }
  showStatus(stats);
  // The dirty pump is the ONE trigger for slider DOM synchronization:
  // Updates.params_changed (watched ids, from submit/undo/redo alike)
  // sets a flag we drain here. No mutation path carries its own
  // hand-placed resync call.
  if (onParamsDirty && app.take_params_dirty()) {
    onParamsDirty(activeSliderId);
  }
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
    ["cube-chamfer",    "cube chamfer",    "cubeChamfer",    (v) => app.set_cube_chamfer(v)],
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
      // The drag itself lands in params_changed too (unidirectional
      // data flow); pass our id so the pump-triggered resync skips the
      // slider that is actively being dragged.
      refresh(app, op, id);
    });
  }

  // Resynchronize the slider DOM from the document (the single source
  // of truth). Triggered ONLY by the dirty pump via refresh(); the pump
  // reports per-entity ids, but the reaction deliberately resyncs the
  // whole panel — with seven sliders the win is what *triggers* the
  // sync, not per-slider granularity. Feedback-loop guards: the actively
  // dragged slider is skipped entirely, and other sliders' DOM values
  // are only written when they actually differ.
  const syncSlidersFromDocument = (skipId) => {
    const params = JSON.parse(app.params_json());
    for (const [id, , key] of sliders) {
      if (id === skipId) continue;
      const input = $(id);
      const v = params[key];
      if (parseFloat(input.value) !== v) {
        input.value = String(v);
      }
      $(`${id}-val`).textContent = `${v.toFixed(2)} m`;
    }
  };
  onParamsDirty = (skipId) => {
    syncSlidersFromDocument(skipId);
    renderProjectPanel();
    renderLevelsPanel();
  };

  // -- Authoring: project settings (Site) --------------------------------
  const siteFields = ["site-lat", "site-lon", "site-elev"];
  const submitSite = () => {
    app.set_site(
      parseFloat($("site-lat").value),
      parseFloat($("site-lon").value),
      parseFloat($("site-elev").value),
    );
    refresh(app, "site");
  };
  for (const id of siteFields) {
    $(id).addEventListener("input", () => {
      if (siteFields.every((f) => $(f).value !== "" && !isNaN(parseFloat($(f).value)))) {
        submitSite();
      }
    });
  }

  const renderProjectPanel = () => {
    const site = JSON.parse(app.site_json());
    if (!site) return;
    const assign = (id, v) => {
      const input = $(id);
      // Focus guard: never rewrite the field being edited.
      if (document.activeElement === input) return;
      if (parseFloat(input.value) !== v) input.value = String(v);
    };
    assign("site-lat", site.latitude);
    assign("site-lon", site.longitude);
    assign("site-elev", site.elevation);
  };

  // -- Authoring: level manager -------------------------------------------
  const floatToHex = (c) =>
    "#" + [c[0], c[1], c[2]]
      .map((v) => Math.round(Math.max(0, Math.min(1, v)) * 255).toString(16).padStart(2, "0"))
      .join("");
  const hexToFloat = (hex) => [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255);

  const CASCADE_WORDING = (name) =>
    `Delete level "${name}"?\n\n` +
    `Geometry ATTACHED to this level (control points and everything built ` +
    `on them) will be deleted.\n` +
    `Elements merely ASSOCIATED with it lose their element wrapper and ` +
    `instances, but their geometry survives as standalone meshes.\n\n` +
    `One undo restores everything.`;

  const buildLevelRow = (lvl) => {
    const row = document.createElement("div");
    row.className = "level-row";
    row.dataset.id = String(lvl.id);
    row.innerHTML =
      `<input type="radio" name="active-level" class="lvl-active" title="active level" />` +
      `<input type="color" class="lvl-color" title="level color" />` +
      `<input type="text" class="lvl-name" title="level name" />` +
      `<input type="number" class="lvl-elev" step="0.1" title="elevation (m)" />` +
      `<label class="lvl-story-label"><input type="checkbox" class="lvl-story" />story</label>` +
      `<button class="lvl-delete" title="delete level">✕</button>`;
    const q = (sel) => row.querySelector(sel);
    q(".lvl-active").addEventListener("change", () => {
      // Session state, not a document change: no pump traffic, so this
      // hand-placed re-render is legitimate (AUTHORING §5).
      app.set_active_level(lvl.id);
      renderLevelsPanel();
    });
    q(".lvl-color").addEventListener("input", (e) => {
      const [r, g, b] = hexToFloat(e.target.value);
      app.update_level_color(lvl.id, r, g, b);
      refresh(app, "level color");
    });
    q(".lvl-name").addEventListener("input", (e) => {
      app.update_level_name(lvl.id, e.target.value);
      refresh(app, "level name");
    });
    q(".lvl-elev").addEventListener("input", (e) => {
      const v = parseFloat(e.target.value);
      if (!isNaN(v)) {
        app.update_level_elevation(lvl.id, v);
        refresh(app, "level elevation");
      }
    });
    // Re-sorting is deferred to commit (blur/Enter) so the list does not
    // reorder under the user's cursor mid-edit.
    q(".lvl-elev").addEventListener("change", () => renderLevelsPanel());
    q(".lvl-story").addEventListener("change", (e) => {
      app.update_level_story(lvl.id, e.target.checked);
      refresh(app, "level story");
    });
    q(".lvl-delete").addEventListener("click", () => {
      const name = row.querySelector(".lvl-name").value;
      const result = app.delete_level(lvl.id);
      if (result === "deleted") {
        refresh(app, "delete level");
      } else if (result === "has_dependents") {
        if (confirm(CASCADE_WORDING(name))) {
          app.delete_level_cascade(lvl.id);
          refresh(app, "delete level (cascade)");
        }
      } else {
        console.error("delete level:", result);
      }
      renderLevelsPanel();
    });
    return row;
  };

  const renderLevelsPanel = () => {
    const state = JSON.parse(app.levels_json());
    const list = $("level-list");
    // Display order: top story first (descending elevation); the sort
    // itself always derives from elevations (AUTHORING §2).
    const desired = [...state.levels].reverse();
    const focused = list.contains(document.activeElement) ? document.activeElement : null;

    const rows = new Map([...list.children].map((r) => [r.dataset.id, r]));
    for (const [id, row] of rows) {
      if (!desired.some((l) => String(l.id) === id)) {
        row.remove();
        rows.delete(id);
      }
    }
    for (const lvl of desired) {
      if (!rows.has(String(lvl.id))) rows.set(String(lvl.id), buildLevelRow(lvl));
    }
    // Reordering moves DOM nodes (which would blur a focused input), so
    // it is skipped while the user is editing inside the list; the
    // 'change' handler re-renders on commit.
    if (!focused) {
      for (const lvl of desired) list.appendChild(rows.get(String(lvl.id)));
    } else {
      for (const lvl of desired) {
        const row = rows.get(String(lvl.id));
        if (!row.isConnected) list.appendChild(row);
      }
    }
    for (const lvl of desired) {
      const row = rows.get(String(lvl.id));
      const active = state.activeId === lvl.id;
      row.classList.toggle("active", active);
      const assign = (sel, value, prop = "value") => {
        const input = row.querySelector(sel);
        if (input === focused) return; // never fight the edited field
        if (input[prop] !== value) input[prop] = value;
      };
      assign(".lvl-active", active, "checked");
      assign(".lvl-color", floatToHex(lvl.color));
      assign(".lvl-name", lvl.name);
      assign(".lvl-elev", String(lvl.elevation));
      assign(".lvl-story", lvl.isStory, "checked");
    }
  };

  $("add-level").addEventListener("click", () => {
    app.add_level();
    refresh(app, "add level");
    renderLevelsPanel();
  });

  // Test hooks for the authoring specs.
  window.__vim = {
    bbox: () => JSON.parse(app.scene_bbox_json()),
    levels: () => JSON.parse(app.levels_json()),
    site: () => JSON.parse(app.site_json()),
  };

  // -- Display options ---------------------------------------------------
  $("wireframe").addEventListener("change", (e) => {
    app.set_wireframe(e.target.checked);
    refresh(app);
  });

  // -- Undo / redo: submit + poll only — slider resync arrives through
  // the dirty pump inside refresh(), same as every other mutation.
  $("undo").addEventListener("click", () => {
    if (app.undo()) refresh(app, "undo");
  });
  $("redo").addEventListener("click", () => {
    if (app.redo()) refresh(app, "redo");
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

  // Initial slider state flows through the same pump path: the scene
  // build dirtied every watched entity, so this first poll-processing
  // refresh() fires the resync — no hand-placed startup sync.
  refresh(app, "initial scene");
  requestAnimationFrame(frame);
}

main().catch((e) => {
  console.error(e);
  window.__vimError = String(e);
  statusbar.textContent = `error: ${e}`;
});
