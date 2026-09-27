// VIM Design — authoring app driver (mobile + desktop single-page app).
//
// Division of labour: the wasm AuthorApp owns the Document (the single
// source of truth), evaluation, rendering, picking, snapping, and the
// sketch rules. This file owns the DOM, decodes pointer/keyboard input
// into app calls (canvas positions in DEVICE pixels), draws the sketch
// HUD on a 2D overlay canvas, and persists the document to localStorage.
//
// Document-bound panels repaint ONLY when app.take_params_dirty() fires
// (the dirty pump: user edits, undo, redo, load all report through it).
// Session-state changes (tool, active level, selection, view) repaint by
// hand — they are not document changes.
//
// Test hooks: window.__author (see the bottom of main()).

const DOC_KEY = "vim-design/doc/v1";
const SESSION_KEY = "vim-design/session/v1";
const SAVE_DEBOUNCE_MS = 300;
const SNAP_STEPS = [0.1, 0.25, 0.5, 1.0];
// Snap capture radius per pointer type (CSS px): fingers are imprecise.
const SNAP_CAPTURE_PX = { mouse: 10, pen: 14, touch: 24 };
// Movement (CSS px) that turns a press into a drag.
const DRAG_THRESHOLD_PX = { mouse: 4, touch: 10 };
// Height (CSS px) of the touch readout bubble above the finger.
const TOUCH_READOUT_OFFSET_PX = 78;
const SESSION_SAVE_DEBOUNCE_MS = 400;
const STARTUP_TOAST_DELAY_MS = 400;
const ERROR_TOAST_MS = 6000;
const LONG_ERROR_TOAST_MS = 7000;
// Walls are thin in plan: a tap this close to one (CSS px) still picks it.
const WALL_PICK_PX = { mouse: 12, pen: 16, touch: 28 };
// Stepper increments (meters).
const PLATE_THICKNESS_STEP_M = 0.05;
const WALL_HEIGHT_STEP_M = 0.1;
const WALL_THICKNESS_STEP_M = 0.05;
// Double tap/click on empty space = zoom to fit.
const DOUBLE_TAP_MS = 330;
const DOUBLE_TAP_PX = 30;
// A touch tap's compatibility click arrives this soon and this close to
// the tap (CSS px): swallowed when it would hit a sheet the tap opened.
const GHOST_CLICK_MS = 450;
const GHOST_CLICK_PX = 24;
// Edit Mode: press-and-hold on an edge (Points mode) inserts a point.
const LONG_PRESS_MS = 500;
const LONG_PRESS_VIBRATE_MS = 12;
const INSERT_RING_MS = 450;
// Edit Mode pick radius (CSS px) for points and edges.
const EDIT_PICK_PX = { mouse: 10, pen: 14, touch: 22 };
// Two-finger touch: the gesture is classified once its combined travel
// reaches TOUCH_COMMIT_PX (CSS px) — pan, zoom, or (3D) twist, whichever
// leads the others by TOUCH_DOMINANCE; an ambiguous start waits up to
// TOUCH_COMMIT_MAX_PX, then takes the leader. The class is locked until
// a finger lifts. A twist must clearly dominate: its travel is weighted
// down by TOUCH_TWIST_WEIGHT.
const TOUCH_COMMIT_PX = 12;
const TOUCH_COMMIT_MAX_PX = 40;
const TOUCH_DOMINANCE = 1.6;
const TOUCH_TWIST_WEIGHT = 0.6;
/** Orbit angle per pointer pixel (the camera's orbit rate). */
const ORBIT_RAD_PER_PX = 0.0065;
// Edit Mode stepper increments (meters).
const FACE_THICKNESS_STEP_M = 0.05;
const VOID_DEPTH_STEP_M = 0.05;
const WALL_TOP_OFFSET_STEP_M = 0.05;
/** Openings mode: the size steppers' step, and a new niche's depth (m). */
const OPENING_STEP_M = 0.1;
const OPENING_NICHE_DEPTH_M = 0.1;
/** Workplane offset stepper step and slider range (meters). */
const WORKPLANE_OFFSET_STEP_M = 0.05;
const WORKPLANE_OFFSET_RANGE_M = 6.0;
/** Range of the wall top offset slider (meters, either way). */
const WALL_TOP_OFFSET_RANGE_M = 3.0;
/** A wall opening whose bottom is below this (m above the base) is a door. */
const OPENING_BOTTOM_EPS_M = 0.001;
// Desktop: the Model tree panel opens by default at this width and up.
const TREE_DEFAULT_OPEN_MIN_PX = 760;
const COARSE = matchMedia("(pointer: coarse)").matches;

const $ = (id) => document.getElementById(id);
const el = (tag, props = {}, ...children) => {
  const node = document.createElement(tag);
  for (const [k, v] of Object.entries(props)) {
    if (k === "class") node.className = v;
    else if (k === "text") node.textContent = v;
    else if (k === "html") node.innerHTML = v;
    else if (k.startsWith("on")) node.addEventListener(k.slice(2), v);
    else if (v === true) node.setAttribute(k, "");
    else if (v !== false && v != null) node.setAttribute(k, v);
  }
  for (const c of children) if (c != null) node.append(c);
  return node;
};
const ICON = {
  plus: '<path d="M12 5v14M5 12h14"/>',
  trash: '<path d="M4 7h16M9 7V4.5h6V7M6.5 7l1 13h9l1-13"/>',
  file: '<path d="M6 3h8l4 4v14H6z"/><path d="M14 3v4h4"/><path d="M12 11v6M9 14h6"/>',
  download: '<path d="M12 4v11M7 10l5 5 5-5"/><path d="M5 20h14"/>',
  upload: '<path d="M12 16V5M7 10l5-5 5 5"/><path d="M5 20h14"/>',
  layers: '<path d="M12 3l9 5-9 5-9-5z"/><path d="M3 13l9 5 9-5"/>',
  pin: '<path d="M12 21s-6-5.5-6-11a6 6 0 0112 0c0 5.5-6 11-6 11z"/><circle cx="12" cy="10" r="2.2"/>',
  info: '<circle cx="12" cy="12" r="9"/><path d="M12 11v6M12 7.5v.5"/>',
  chev: '<path d="M9 6l6 6-6 6"/>',
  copy: '<rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V5a1 1 0 00-1-1H5a1 1 0 00-1 1v10a1 1 0 001 1h3"/>',
  magnet: '<path d="M6 3v8a6 6 0 0012 0V3"/><path d="M6 7h3.5M14.5 7H18"/>',
  cube: '<path d="M12 3l8 4.5v9L12 21l-8-4.5v-9z"/><path d="M12 12l8-4.5M12 12v9M12 12L4 7.5"/>',
  check: '<path d="M5 12.5l4.5 4.5L19 7.5"/>',
};
const icon = (name, cls = "ico sm") =>
  `<svg class="${cls}" viewBox="0 0 24 24">${ICON[name] ?? ""}</svg>`;

// ---- never-crash storage -------------------------------------------------
const store = {
  get(key) {
    try { return localStorage.getItem(key); } catch { return null; }
  },
  set(key, value) {
    try { localStorage.setItem(key, value); return true; } catch { return false; }
  },
  remove(key) {
    try { localStorage.removeItem(key); } catch { /* ignore */ }
  },
  usageBytes() {
    let total = 0;
    try {
      for (let i = 0; i < localStorage.length; i++) {
        const k = localStorage.key(i);
        if (k && k.startsWith("vim-design/")) total += (k.length + (localStorage.getItem(k)?.length ?? 0)) * 2;
      }
    } catch { /* ignore */ }
    return total;
  },
};

function bytesToBase64(bytes) {
  let bin = "";
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    bin += String.fromCharCode.apply(null, bytes.subarray(i, i + chunk));
  }
  return btoa(bin);
}

function base64ToBytes(b64) {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

// ---- error log (feeds "Copy debug info") --------------------------------
const errorLog = [];
const logError = (msg) => {
  errorLog.push(`${new Date().toISOString()} ${msg}`);
  if (errorLog.length > 30) errorLog.shift();
};
window.addEventListener("error", (e) => logError(`error: ${e.message}`));
window.addEventListener("unhandledrejection", (e) => logError(`rejection: ${e.reason}`));
const origConsoleError = console.error.bind(console);
console.error = (...args) => {
  logError(args.map(String).join(" "));
  origConsoleError(...args);
};

// ---- toasts + dialog ----------------------------------------------------------
const toastLog = [];
let lastToast = { msg: "", t: 0 };
function toast(msg, { kind = "info", action = null, ms = 2600 } = {}) {
  if (!msg) return;
  const now = performance.now();
  if (msg === lastToast.msg && now - lastToast.t < 1500) return;
  lastToast = { msg, t: now };
  toastLog.push({ msg, kind });
  const box = $("toasts");
  const node = el("div", { class: `toast ${kind}` }, el("span", { class: "toast-msg", text: msg }));
  if (action) {
    node.append(el("button", {
      type: "button", text: action.label,
      onclick: () => { action.fn(); dismiss(); },
    }));
  }
  const dismiss = () => {
    node.classList.add("leaving");
    setTimeout(() => node.remove(), 220);
  };
  box.append(node);
  while (box.children.length > 3) box.firstChild.remove();
  setTimeout(dismiss, action ? Math.max(ms, 4500) : ms);
}

function confirmDialog({ title, message, ok = "OK", danger = false }) {
  return new Promise((resolve) => {
    $("dialog-title").textContent = title;
    $("dialog-message").textContent = message;
    const okBtn = $("dialog-ok");
    okBtn.textContent = ok;
    okBtn.className = `btn ${danger ? "danger" : "primary"}`;
    const backdrop = $("dialog-backdrop");
    backdrop.hidden = false;
    const done = (v) => {
      backdrop.hidden = true;
      okBtn.onclick = null;
      $("dialog-cancel").onclick = null;
      resolve(v);
    };
    okBtn.onclick = () => done(true);
    $("dialog-cancel").onclick = () => done(false);
    setTimeout(() => okBtn.focus(), 30);
  });
}

// ---- formatting ------------------------------------------------------------------
const fmtM = (v) => `${(Math.abs(v) < 0.005 ? 0 : v).toFixed(2).replace("-", "−")} m`;
/** A preset dimension without trailing zeros ("1.2", "0.9", "2.1"). */
const fmtDim = (v) => String(Math.round(v * 100) / 100);
/** A workplane offset from its parent: "+2.40 m" / "−0.30 m". */
const fmtOffset = (v) => `${v >= 0 ? "+" : "−"}${Math.abs(v).toFixed(2)} m`;
const fmtArea = (v) => `${Math.max(0, v).toFixed(2)} m²`;
const floatToHex = (c) =>
  "#" + [c[0], c[1], c[2]]
    .map((v) => Math.round(Math.max(0, Math.min(1, v)) * 255).toString(16).padStart(2, "0"))
    .join("");
const hexToFloat = (hex) => [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255);
const cssColor = (c) => `rgb(${c.slice(0, 3).map((v) => Math.round(v * 255)).join(",")})`;
const parseNum = (s) => {
  const v = parseFloat(String(s).replace(",", ".").replace("−", "-"));
  return Number.isFinite(v) ? v : NaN;
};

async function main() {
  const loadingStatus = $("loading-status");
  loadingStatus.textContent = "Loading the modeling engine…";
  const t0 = performance.now();
  const mod = await import("./pkg/vim_design_web.js");
  await mod.default();
  if (mod.threads_supported?.() && globalThis.crossOriginIsolated === true && mod.initThreadPool) {
    try {
      await mod.initThreadPool(navigator.hardwareConcurrency ?? 1);
    } catch (e) {
      console.warn("initThreadPool failed (continuing single-threaded):", e);
    }
  }
  const wasmMs = performance.now() - t0;

  const canvas = $("view");
  const hud = $("hud");
  const hctx = hud.getContext("2d");
  let dpr = 1;
  const fitCanvas = () => {
    dpr = Math.min(window.devicePixelRatio || 1, 2);
    const w = Math.max(1, Math.round(canvas.clientWidth * dpr));
    const h = Math.max(1, Math.round(canvas.clientHeight * dpr));
    for (const c of [canvas, hud]) {
      if (c.width !== w) c.width = w;
      if (c.height !== h) c.height = h;
    }
  };
  fitCanvas();

  loadingStatus.textContent = "Starting the renderer…";
  const app = await mod.AuthorApp.create("view");

  // -- session state (non-critical; never in the document) -----------------
  let session = {};
  try {
    session = JSON.parse(store.get(SESSION_KEY) ?? "{}") ?? {};
  } catch {
    session = {};
  }
  const sessionSave = (() => {
    let timer = 0;
    return () => {
      clearTimeout(timer);
      timer = setTimeout(() => {
        const levels = JSON.parse(app.levels_json());
        store.set(SESSION_KEY, JSON.stringify({
          view: app.view_mode(),
          snapEnabled: snapEnabled,
          snapStep: snapStep,
          activeLevel: levels.activeId,
          renderMode: app.render_mode(),
          thickness: app.plate_thickness_setting(),
          shape: app.shape(),
          wall: JSON.parse(app.wall_settings_json()),
          camera: app.camera_json(),
          treeOpen,
          treeCollapsed: [...treeCollapsed],
        }));
      }, SESSION_SAVE_DEBOUNCE_MS);
    };
  })();

  // Model tree: open on desktop by default; collapsed rows by key.
  let treeOpen = typeof session.treeOpen === "boolean" ? session.treeOpen : innerWidth >= TREE_DEFAULT_OPEN_MIN_PX;
  const treeCollapsed = new Set(Array.isArray(session.treeCollapsed) ? session.treeCollapsed : []);
  let snapEnabled = session.snapEnabled ?? true;
  let snapStep = SNAP_STEPS.includes(session.snapStep) ? session.snapStep : (COARSE ? 0.5 : 0.25);
  app.set_snap(snapEnabled, snapStep);
  if (typeof session.thickness === "number") app.set_plate_thickness_setting(session.thickness);
  if (session.shape === "rect") app.set_shape("rect");
  if (session.wall && typeof session.wall === "object") {
    const w = session.wall;
    app.set_wall_settings(Number(w.height), Number(w.thickness), w.flip === true);
  }
  if (typeof session.renderMode === "string") app.set_render_mode(session.renderMode);
  else if (session.wireframe === true) app.set_wireframe(true); // older sessions

  // -- restore the persisted document ------------------------------------------
  const stored = store.get(DOC_KEY);
  let restored = false;
  let autosaveBlocked = false;
  if (stored) {
    let err = "";
    try {
      err = app.load_document(base64ToBytes(stored));
    } catch (e) {
      err = String(e);
    }
    if (err) {
      const backupKey = `${DOC_KEY}.corrupt-${Date.now()}`;
      if (store.set(backupKey, stored)) store.remove(DOC_KEY);
      else autosaveBlocked = true; // no backup: autosave must not overwrite the only copy
      logError(`stored document unreadable: ${err}`);
      setTimeout(() => toast(
        "Your saved project could not be opened, so a new one was started. The old data was kept as a backup.",
        { kind: "error", ms: LONG_ERROR_TOAST_MS },
      ), STARTUP_TOAST_DELAY_MS);
    } else {
      restored = true;
    }
  }
  if (typeof session.activeLevel === "number") app.set_active_level(session.activeLevel);
  if (session.wall?.mode === "upto" && typeof session.wall.topPlane === "number") {
    app.set_wall_height_mode("upto", session.wall.topPlane, Number(session.wall.topOffset) || 0);
  }
  if (restored && session.camera) {
    app.set_camera_json(session.camera);
  } else {
    app.set_view_mode(session.view === "3d" ? "3d" : "plan");
    fitView();
  }

  // -- persistence --------------------------------------------------------------
  let savedRevision = app.revision();
  let saveTimer = 0;
  let lastSaveError = "";
  const saveNow = () => {
    clearTimeout(saveTimer);
    saveTimer = 0;
    const rev = app.revision();
    if (rev === savedRevision) return true;
    if (app.edit_active() || app.openings_active()) return false; // saved when the session ends
    if (autosaveBlocked) {
      const msg = "Autosave is off: storage is full and your old project could not be backed up. Use Export to keep your work.";
      if (msg !== lastSaveError) toast(msg, { kind: "error", ms: LONG_ERROR_TOAST_MS });
      lastSaveError = msg;
      return false;
    }
    try {
      const bytes = app.save_document();
      if (!store.set(DOC_KEY, bytesToBase64(bytes))) throw new Error("storage refused the write");
      savedRevision = rev;
      lastSaveError = "";
      return true;
    } catch (e) {
      const msg = `Could not save your project (${e.message ?? e}). Storage may be full or disabled.`;
      if (msg !== lastSaveError) toast(msg, { kind: "error", ms: ERROR_TOAST_MS });
      lastSaveError = msg;
      logError(msg);
      return false;
    }
  };
  const scheduleSave = () => {
    if (app.revision() === savedRevision || app.edit_active() || app.openings_active()) return;
    clearTimeout(saveTimer);
    saveTimer = setTimeout(saveNow, SAVE_DEBOUNCE_MS);
  };
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden") saveNow();
  });
  window.addEventListener("pagehide", () => saveNow());

  // -- render loop (on demand: idle = no GPU work, kind to phone batteries) ----
  let needsRender = true;
  let rendered = 0;
  const requestRender = () => { needsRender = true; };
  let hudActive = false;
  const frame = () => {
    try {
      fitCanvas();
      if (canvas.width !== lastW || canvas.height !== lastH) {
        lastW = canvas.width;
        lastH = canvas.height;
        app.resize(canvas.width, canvas.height);
        needsRender = true;
      }
      if (needsRender) {
        needsRender = false;
        app.render();
        rendered++;
        drawHud();
        if (!window.__author.ready && rendered >= 2) {
          const s = JSON.parse(app.stats_json());
          if (s.settled) {
            window.__author.ready = true;
            window.__author.loadMs = performance.now() - t0;
            $("loading").classList.add("done");
          }
        }
      }
    } catch (e) {
      console.error("render failed:", e);
      window.__author.error = String(e);
      return;
    }
    requestAnimationFrame(frame);
  };
  let lastW = 0, lastH = 0;

  // -- refresh: stats + dirty pump -> panels --------------------------------------
  let stats = JSON.parse(app.stats_json());
  // Opening presets (window / door sizes) — the source is Rust (presets.rs).
  const PRESETS = JSON.parse(app.presets_json());
  const refresh = () => {
    stats = JSON.parse(app.stats_json());
    renderChrome();
    if (app.take_params_dirty()) {
      renderDocPanels();
    }
    const notice = app.take_notice();
    if (notice) toast(notice, { kind: "ok", ms: 1800 });
    scheduleSave();
    requestRender();
    return stats;
  };

  // ============================================================================
  // Input
  // ============================================================================
  const devPoint = (e) => {
    const r = canvas.getBoundingClientRect();
    return [(e.clientX - r.left) * (canvas.width / r.width), (e.clientY - r.top) * (canvas.height / r.height)];
  };
  const tolPx = (type) => (SNAP_CAPTURE_PX[type] ?? SNAP_CAPTURE_PX.touch) * dpr;
  const wallPickPx = (type) => (WALL_PICK_PX[type] ?? WALL_PICK_PX.touch) * dpr;
  const editPickPx = (type) => (EDIT_PICK_PX[type] ?? EDIT_PICK_PX.touch) * dpr;
  let editState = { active: false };
  const editing = () => editState.active === true;
  // Openings mode (placing windows and doors): its own Edit Mode.
  let openingsState = { active: false };
  const inOpenings = () => openingsState.active === true;
  /** Any Edit Mode chrome: a profile (floor, wall run) or openings. */
  const inEditChrome = () => editing() || inOpenings();
  const isDrawing = () =>
    (stats.tool === "wall" && !inEditChrome()) ||
    (editing() && ["solid", "void", "split"].includes(editState.tool));

  const pointers = new Map(); // id -> {x, y, type} (client px)
  let mode = "none"; // none | press | nav | pan | place | pinch | pinch-rest
  let press = null; // {x, y, dev, type, button, moved, anchored}
  // Two-finger navigation: classify once, then lock (see TOUCH_*).
  let pinch = null; // {start, prev, cls: null | "pan" | "zoom" | "twist"}
  let lastTouchClass = null; // for tests
  let lastTap = null;
  let hoverType = "mouse";
  // The touch tap that opened a sheet (see the ghost-click shield).
  let ghostTap = null; // {x, y, t} client px
  let touchPlacing = false;

  const pinchState = () => {
    const [a, b] = [...pointers.values()];
    const r = canvas.getBoundingClientRect();
    const sx = canvas.width / r.width;
    const ax = (a.x - r.left) * sx, ay = (a.y - r.top) * sx;
    const bx = (b.x - r.left) * sx, by = (b.y - r.top) * sx;
    return {
      d: Math.hypot(bx - ax, by - ay),
      mid: [(ax + bx) / 2, (ay + by) / 2],
      angle: Math.atan2(by - ay, bx - ax),
    };
  };

  const wrapAngle = (a) => (a > Math.PI ? a - 2 * Math.PI : a < -Math.PI ? a + 2 * Math.PI : a);
  /** Two fingers: accumulate until the travel commits, classify ONCE
   *  (pan = the fingers move together, zoom = they spread or close about
   *  the pinch centre, twist = they turn — 3D only, orbits), then lock
   *  that class until a finger lifts. Nothing mixes. */
  function twoFingerMove(now) {
    const p = pinch;
    if (!p || now.d < 1) return;
    const k = canvas.width / canvas.getBoundingClientRect().width; // device px per CSS px
    if (!p.cls) {
      const s0 = p.start;
      const pan = Math.hypot(now.mid[0] - s0.mid[0], now.mid[1] - s0.mid[1]) / k;
      const zoom = Math.abs(now.d - s0.d) / k;
      const twist = stats.view === "3d" ? Math.abs(wrapAngle(now.angle - s0.angle)) * (s0.d / 2) / k : 0;
      const travel = 2 * pan + zoom + twist; // both fingers' combined travel, roughly
      if (travel < TOUCH_COMMIT_PX) return;
      const ranked = [["pan", pan], ["zoom", zoom], ["twist", twist * TOUCH_TWIST_WEIGHT]].sort((a, b) => b[1] - a[1]);
      const clear = ranked[0][1] >= TOUCH_DOMINANCE * ranked[1][1];
      if (!clear && travel < TOUCH_COMMIT_MAX_PX) return; // ambiguous: wait a little longer
      p.cls = ranked[0][0];
      lastTouchClass = p.cls;
      // The motion so far belongs to the class: apply it from the start.
      p.prev = s0;
    }
    if (p.cls === "pan") {
      app.pan(p.prev.mid[0], p.prev.mid[1], now.mid[0], now.mid[1]);
    } else if (p.cls === "zoom") {
      // Anchored at the initial pinch centre, never panning.
      app.zoom_at(p.prev.d / now.d, p.start.mid[0], p.start.mid[1]);
    } else {
      app.orbit(-wrapAngle(now.angle - p.prev.angle) / ORBIT_RAD_PER_PX, 0);
    }
    p.prev = now;
  }

  canvas.addEventListener("contextmenu", (e) => e.preventDefault());

  canvas.addEventListener("pointerdown", (e) => {
    // Suppress compatibility mouse events (focus steals, ghost clicks).
    if (e.pointerType !== "mouse") e.preventDefault();
    closePopover();
    try { canvas.setPointerCapture(e.pointerId); } catch { /* ignore */ }
    pointers.set(e.pointerId, { x: e.clientX, y: e.clientY, type: e.pointerType });
    hoverType = e.pointerType;
    if (pointers.size === 2) {
      // Two fingers ALWAYS navigate and cancel a pending placement.
      if (mode === "place") {
        app.sketch_leave();
        touchPlacing = false;
      }
      if (mode.startsWith("edit")) {
        clearTimeout(longPressTimer);
        app.edit_gesture_cancel();
      }
      if (mode === "open-drag") app.openings_drag_end();
      mode = "pinch";
      const st = pinchState();
      pinch = { start: st, prev: st, cls: null };
      requestRender();
      return;
    }
    if (pointers.size > 2) return;
    const dev = devPoint(e);
    press = { x: e.clientX, y: e.clientY, dev, type: e.pointerType, button: e.button, moved: false, anchored: false };
    if (e.pointerType === "mouse" && (e.button === 1 || e.button === 2)) {
      mode = "pan";
      canvas.classList.add("grabbing");
      return;
    }
    if (e.button !== 0) return;
    if (isDrawing()) {
      mode = "place";
      touchPlacing = e.pointerType !== "mouse";
      app.sketch_hover(dev[0], dev[1], tolPx(e.pointerType));
      requestRender();
    } else if (inOpenings()) {
      mode = "open-press";
    } else if (editing()) {
      // Tap = select, drag on an item = move, drag on empty = marquee,
      // hold on an edge (Points mode) = insert a point.
      mode = "edit-press";
      press.additive = e.shiftKey || e.ctrlKey || e.metaKey;
      clearTimeout(longPressTimer);
      if (editState.mode === "points") {
        longPressTimer = setTimeout(() => onLongPress(press), LONG_PRESS_MS);
      }
    } else {
      mode = "press";
    }
  });

  canvas.addEventListener("pointermove", (e) => {
    const p = pointers.get(e.pointerId);
    const dev = devPoint(e);
    if (!p) {
      // Hover (mouse/pen without buttons).
      hoverType = e.pointerType;
      if (isDrawing() && e.pointerType !== "touch") {
        app.sketch_hover(dev[0], dev[1], tolPx(e.pointerType));
        requestRender();
      } else if (editing() && e.pointerType !== "touch") {
        app.edit_hover(dev[0], dev[1], editPickPx(e.pointerType));
        requestRender();
      } else if (inOpenings() && e.pointerType !== "touch") {
        app.openings_hover(dev[0], dev[1], editPickPx(e.pointerType));
        requestRender();
      }
      return;
    }
    const prev = { x: p.x, y: p.y };
    p.x = e.clientX;
    p.y = e.clientY;
    if (mode === "pinch" && pointers.size >= 2) {
      twoFingerMove(pinchState());
      requestRender();
      return;
    }
    if (!press) return;
    const moved = Math.hypot(e.clientX - press.x, e.clientY - press.y);
    const threshold = press.type === "mouse" ? DRAG_THRESHOLD_PX.mouse : DRAG_THRESHOLD_PX.touch;
    if (moved > threshold) press.moved = true;
    const r = canvas.getBoundingClientRect();
    const s = canvas.width / r.width;
    const prevDev = [(prev.x - r.left) * s, (prev.y - r.top) * s];
    if (mode === "open-press" && press.moved) {
      // On an opening: move it; elsewhere: navigate.
      mode = app.openings_drag_begin(press.dev[0], press.dev[1], editPickPx(press.type)) ? "open-drag" : "nav";
      if (mode === "nav") canvas.classList.add("grabbing");
    }
    if (mode === "open-drag") {
      const res = JSON.parse(app.openings_drag_move(dev[0], dev[1]));
      if (res.result === "rejected") showReason(res.reason);
      refresh();
      return;
    }
    if (mode === "edit-press" && press.moved && !press.consumed) {
      clearTimeout(longPressTimer);
      const kind = app.edit_drag_begin(press.dev[0], press.dev[1], editPickPx(press.type), press.additive);
      mode = kind === "move" ? "edit-move" : "edit-marquee";
    }
    if (mode === "edit-move") {
      app.edit_drag_move(dev[0], dev[1], editPickPx(press.type));
      requestRender();
      return;
    }
    if (mode === "edit-marquee") {
      app.edit_marquee(press.dev[0], press.dev[1], dev[0], dev[1]);
      renderChrome();
      requestRender();
      return;
    }
    if (mode === "pan") {
      app.pan(prevDev[0], prevDev[1], dev[0], dev[1]);
      requestRender();
    } else if (mode === "press" && press.moved) {
      mode = "nav";
      canvas.classList.add("grabbing");
    }
    if (mode === "nav") {
      if (stats.view === "3d") app.orbit(e.clientX - prev.x, e.clientY - prev.y);
      else app.pan(prevDev[0], prevDev[1], dev[0], dev[1]);
      requestRender();
    } else if (mode === "place") {
      const sketch = JSON.parse(app.sketch_json());
      if (stats.shape === "rect" && sketch.points.length === 0 && press.moved && !press.anchored) {
        // Press–drag–release rectangle: the press point is corner 1.
        app.sketch_hover(press.dev[0], press.dev[1], tolPx(press.type));
        handlePlaced(app.sketch_place());
        press.anchored = true;
      }
      app.sketch_hover(dev[0], dev[1], tolPx(press.type));
      requestRender();
    }
  });

  const endPointer = (e, cancelled) => {
    if (!pointers.has(e.pointerId)) return;
    pointers.delete(e.pointerId);
    canvas.classList.remove("grabbing");
    if (mode === "pinch" || mode === "pinch-rest") {
      mode = pointers.size === 0 ? "none" : "pinch-rest";
      pinch = null;
      press = null;
      sessionSave();
      return;
    }
    if (mode === "place" && press) {
      touchPlacing = false;
      if (!cancelled) {
        const dev = devPoint(e);
        app.sketch_hover(dev[0], dev[1], tolPx(press.type));
        handlePlaced(app.sketch_place());
      } else {
        app.sketch_leave();
      }
      if (press.type === "touch") app.sketch_leave();
    } else if ((mode === "press" || mode === "open-press") && press && !cancelled) {
      handleTap(press);
    } else if (mode === "open-drag") {
      app.openings_drag_end();
      lastReason = "";
      refresh();
    } else if (mode.startsWith("edit") && press) {
      clearTimeout(longPressTimer);
      if (cancelled) {
        app.edit_gesture_cancel();
      } else if (mode === "edit-press" && !press.consumed && editState.tool === "extend") {
        const res = JSON.parse(app.edit_extend(press.dev[0], press.dev[1]));
        if (res.result === "rejected") toast(res.reason, { kind: "error" });
      } else if (mode === "edit-press" && !press.consumed) {
        app.edit_tap(press.dev[0], press.dev[1], editPickPx(press.type), press.additive);
      } else if (mode === "edit-move") {
        const res = JSON.parse(app.edit_drag_end());
        if (res.result === "rejected") toast(`${res.reason} — the move was undone`, { kind: "error" });
      } else if (mode === "edit-marquee") {
        app.edit_marquee_end();
      }
      refreshEdit();
    }
    if (mode === "nav" || mode === "pan") sessionSave();
    mode = "none";
    press = null;
    requestRender();
  };
  canvas.addEventListener("pointerup", (e) => endPointer(e, false));
  canvas.addEventListener("pointercancel", (e) => endPointer(e, true));
  canvas.addEventListener("pointerleave", (e) => {
    if (!pointers.has(e.pointerId) && isDrawing()) {
      app.sketch_leave();
      requestRender();
    }
  });

  canvas.addEventListener("wheel", (e) => {
    e.preventDefault();
    const dev = devPoint(e);
    const k = e.deltaMode === 1 ? 0.05 : e.deltaMode === 2 ? 0.5 : 0.0015;
    app.zoom_at(Math.exp(e.deltaY * k), dev[0], dev[1]);
    if (isDrawing() && hoverType !== "touch") app.sketch_hover(dev[0], dev[1], tolPx("mouse"));
    requestRender();
    sessionSave();
  }, { passive: false });

  function handleTap(p) {
    const now = performance.now();
    if (inOpenings()) {
      // Openings mode: select an opening, or place the preset on a wall.
      const res = JSON.parse(app.openings_tap(p.dev[0], p.dev[1], editPickPx(p.type), wallPickPx(p.type)));
      if (res.result === "rejected") toast(res.reason, { kind: "error" });
      else if (res.result === "cleared" && openingsState.count === 0) toast("Tap a wall to place an opening", { ms: 1800 });
      refresh();
      return;
    }
    if (stats.tool === "hole") {
      // Hole tool: the tap picks the floor plate to cut voids into.
      const hit = app.pick(p.dev[0], p.dev[1]);
      const plate = JSON.parse(app.elements_json()).find((x) => x.id === hit && x.kind === "floor_plate");
      if (plate) beginEdit(plate.id, "void");
      else toast("Tap a floor plate to cut holes into it", { ms: 1800 });
      return;
    }
    let id = app.pick(p.dev[0], p.dev[1]);
    if (id < 0) id = app.pick_wall(p.dev[0], p.dev[1], wallPickPx(p.type));
    const isDouble = lastTap && now - lastTap.t < DOUBLE_TAP_MS &&
      Math.hypot(p.x - lastTap.x, p.y - lastTap.y) < DOUBLE_TAP_PX;
    if (id < 0 && isDouble && lastTap.empty) {
      fitView();
      lastTap = null;
      sessionSave();
      requestRender();
      return;
    }
    lastTap = { t: now, x: p.x, y: p.y, empty: id < 0 };
    // A touch tap that opens the sheet arms the ghost-click shield.
    if (p.type !== "mouse") ghostTap = { x: p.x, y: p.y, t: performance.now() };
    selectElement(id);
  }

  // Long press (Edit Mode, Points mode): insert a point on the edge.
  let longPressTimer = 0;
  const rings = []; // {x, y, t0}: insert feedback animations
  function onLongPress(p) {
    if (!p || p.moved || mode !== "edit-press") return;
    const res = JSON.parse(app.edit_long_press(p.dev[0], p.dev[1], editPickPx(p.type)));
    if (res.result === "inserted") {
      p.consumed = true;
      rings.push({ x: res.x, y: res.y, t0: performance.now() });
      try { navigator.vibrate?.(LONG_PRESS_VIBRATE_MS); } catch { /* not supported */ }
      refreshEdit();
    } else if (res.result === "rejected") {
      p.consumed = true;
      toast(res.reason, { kind: "error" });
    }
  }

  function handlePlaced(json) {
    let res;
    try { res = JSON.parse(json); } catch { return; }
    if (res.result === "committed") {
      refresh();
    } else if (res.result === "rejected") {
      toast(res.reason, { kind: "error" });
    } else if (res.result === "added" && res.reason) {
      toast(res.reason, { kind: "error" });
    }
    renderChrome();
    requestRender();
  }

  // ============================================================================
  // Sketch HUD (2D overlay canvas, device pixels)
  // ============================================================================
  const HUD = {
    accent: "#2f6fed",
    bad: "#d92d20",
    guide: "#f59e0b",
    vertex: "#f97316",
    first: "#12a150",
  };
  function roundRect(ctx, x, y, w, h, r) {
    ctx.beginPath();
    ctx.moveTo(x + r, y);
    ctx.arcTo(x + w, y, x + w, y + h, r);
    ctx.arcTo(x + w, y + h, x, y + h, r);
    ctx.arcTo(x, y + h, x, y, r);
    ctx.arcTo(x, y, x + w, y, r);
    ctx.closePath();
  }
  function pill(ctx, x, y, text, { bg = "rgba(255,255,255,0.96)", fg = "#1c2230", border = "rgba(16,24,40,0.12)" } = {}) {
    ctx.font = `600 ${12.5 * dpr}px system-ui, -apple-system, Segoe UI, sans-serif`;
    const w = ctx.measureText(text).width + 16 * dpr;
    const h = 24 * dpr;
    const bx = Math.max(4 * dpr, Math.min(hud.width - w - 4 * dpr, x - w / 2));
    const by = Math.max(4 * dpr, Math.min(hud.height - h - 4 * dpr, y - h / 2));
    ctx.shadowColor = "rgba(16,24,40,0.18)";
    ctx.shadowBlur = 6 * dpr;
    ctx.fillStyle = bg;
    roundRect(ctx, bx, by, w, h, h / 2);
    ctx.fill();
    ctx.shadowBlur = 0;
    ctx.strokeStyle = border;
    ctx.lineWidth = 1;
    ctx.stroke();
    ctx.fillStyle = fg;
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.fillText(text, bx + w / 2, by + h / 2 + 0.5 * dpr);
  }
  let lastHud = { active: false };
  function drawHud() {
    const ctx = hctx;
    const edit = editing();
    const opening = inOpenings();
    if (!isDrawing() && !edit && !opening && rings.length === 0) {
      if (hudActive) ctx.clearRect(0, 0, hud.width, hud.height);
      hudActive = false;
      lastHud = { active: false };
      return;
    }
    hudActive = true;
    ctx.clearRect(0, 0, hud.width, hud.height);
    if (edit) drawEditHud(ctx);
    if (opening) drawOpeningsHud(ctx);
    drawRings(ctx);
    if (!isDrawing()) {
      lastHud = { active: false };
      return;
    }
    let h;
    try { h = JSON.parse(app.hud_json()); } catch { return; }
    lastHud = h;
    if (!h.active) return;
    const ok = h.previewOk;
    const color = ok ? HUD.accent : HUD.bad;
    const P = h.preview;
    // Wall footprints: where the thickness goes.
    for (const band of h.bands ?? []) {
      if (band.length < 3) continue;
      ctx.beginPath();
      band.forEach(([x, y], i) => (i ? ctx.lineTo(x, y) : ctx.moveTo(x, y)));
      ctx.closePath();
      ctx.fillStyle = ok ? "rgba(47,111,237,0.22)" : "rgba(217,45,32,0.2)";
      ctx.fill();
      ctx.strokeStyle = ok ? "rgba(47,111,237,0.55)" : "rgba(217,45,32,0.5)";
      ctx.lineWidth = 1 * dpr;
      ctx.stroke();
    }
    // Fill.
    if (h.fill && P.length >= 3) {
      ctx.beginPath();
      P.forEach(([x, y], i) => (i ? ctx.lineTo(x, y) : ctx.moveTo(x, y)));
      ctx.closePath();
      ctx.fillStyle = ok ? "rgba(47,111,237,0.13)" : "rgba(217,45,32,0.14)";
      ctx.fill();
    }
    // Outline.
    if (P.length >= 2) {
      ctx.lineJoin = "round";
      ctx.lineCap = "round";
      ctx.strokeStyle = color;
      ctx.lineWidth = 2.25 * dpr;
      ctx.setLineDash([]);
      ctx.beginPath();
      P.forEach(([x, y], i) => (i ? ctx.lineTo(x, y) : ctx.moveTo(x, y)));
      if (h.shape === "rect") ctx.closePath();
      ctx.stroke();
      if (h.closeHint && h.shape === "polygon" && P.length >= 3) {
        ctx.setLineDash([6 * dpr, 6 * dpr]);
        ctx.lineWidth = 1.5 * dpr;
        ctx.globalAlpha = 0.55;
        ctx.beginPath();
        ctx.moveTo(...P[P.length - 1]);
        ctx.lineTo(...P[0]);
        ctx.stroke();
        ctx.globalAlpha = 1;
        ctx.setLineDash([]);
      }
    }
    // Guides.
    if (h.guides.length) {
      ctx.strokeStyle = HUD.guide;
      ctx.lineWidth = 1.5 * dpr;
      ctx.setLineDash([5 * dpr, 5 * dpr]);
      for (const [x1, y1, x2, y2] of h.guides) {
        ctx.beginPath();
        ctx.moveTo(x1, y1);
        ctx.lineTo(x2, y2);
        ctx.stroke();
      }
      ctx.setLineDash([]);
    }
    // Placed points.
    for (const [x, y] of h.points) {
      ctx.beginPath();
      ctx.arc(x, y, 4.5 * dpr, 0, Math.PI * 2);
      ctx.fillStyle = "#fff";
      ctx.fill();
      ctx.lineWidth = 2 * dpr;
      ctx.strokeStyle = color;
      ctx.stroke();
    }
    if (h.first) {
      ctx.beginPath();
      ctx.arc(h.first[0], h.first[1], 10 * dpr, 0, Math.PI * 2);
      ctx.strokeStyle = HUD.first;
      ctx.lineWidth = 2 * dpr;
      ctx.stroke();
    }
    // Segment length.
    if (h.seg) pill(ctx, h.seg.x, h.seg.y - 18 * dpr, h.seg.text);
    // Cursor.
    const c = h.cursor;
    if (c) {
      const r = 7 * dpr;
      if (touchPlacing) {
        // Long crosshair so the point reads around the finger.
        ctx.strokeStyle = "rgba(28,34,48,0.55)";
        ctx.lineWidth = 1 * dpr;
        const arm = 46 * dpr;
        ctx.beginPath();
        ctx.moveTo(c.x - arm, c.y); ctx.lineTo(c.x - r * 1.6, c.y);
        ctx.moveTo(c.x + r * 1.6, c.y); ctx.lineTo(c.x + arm, c.y);
        ctx.moveTo(c.x, c.y - arm); ctx.lineTo(c.x, c.y - r * 1.6);
        ctx.moveTo(c.x, c.y + r * 1.6); ctx.lineTo(c.x, c.y + arm);
        ctx.stroke();
      }
      ctx.lineWidth = 2.25 * dpr;
      ctx.fillStyle = "#fff";
      switch (c.kind) {
        case "first":
          ctx.beginPath();
          ctx.arc(c.x, c.y, r * 1.35, 0, Math.PI * 2);
          ctx.fillStyle = HUD.first;
          ctx.fill();
          ctx.strokeStyle = "#fff";
          ctx.stroke();
          break;
        case "vertex":
          ctx.beginPath();
          ctx.rect(c.x - r, c.y - r, 2 * r, 2 * r);
          ctx.fill();
          ctx.strokeStyle = HUD.vertex;
          ctx.stroke();
          break;
        case "axis":
          ctx.beginPath();
          ctx.moveTo(c.x, c.y - r * 1.3); ctx.lineTo(c.x + r * 1.3, c.y);
          ctx.lineTo(c.x, c.y + r * 1.3); ctx.lineTo(c.x - r * 1.3, c.y);
          ctx.closePath();
          ctx.fill();
          ctx.strokeStyle = HUD.guide;
          ctx.stroke();
          break;
        case "grid":
          ctx.beginPath();
          ctx.arc(c.x, c.y, r, 0, Math.PI * 2);
          ctx.fill();
          ctx.strokeStyle = color;
          ctx.stroke();
          ctx.beginPath();
          ctx.arc(c.x, c.y, 2 * dpr, 0, Math.PI * 2);
          ctx.fillStyle = color;
          ctx.fill();
          break;
        default:
          ctx.beginPath();
          ctx.arc(c.x, c.y, r * 0.7, 0, Math.PI * 2);
          ctx.fillStyle = color;
          ctx.fill();
      }
      // Readout bubble: above the finger (touch) / beside the cursor.
      const kindText = { first: "Close shape", vertex: "Corner", axis: "Aligned", edge: "On edge" }[c.kind];
      const text = kindText ? `${kindText} · ${c.label}` : c.label;
      if (touchPlacing) {
        pill(ctx, c.x, c.y - TOUCH_READOUT_OFFSET_PX * dpr, text, { bg: "rgba(28,34,48,0.92)", fg: "#fff", border: "rgba(0,0,0,0)" });
      } else {
        pill(ctx, c.x + 18 * dpr + 60 * dpr, c.y - 26 * dpr, text, { bg: "rgba(28,34,48,0.88)", fg: "#fff", border: "rgba(0,0,0,0)" });
      }
    }
  }

  /** Openings mode overlay: every opening on every wall, outlined; the
   *  selected one filled. */
  function drawOpeningsHud(ctx) {
    let h;
    try { h = JSON.parse(app.openings_hud_json()); } catch { return; }
    if (!h.active) return;
    ctx.lineJoin = "round";
    for (const it of h.items) {
      if (it.pts.length < 2) continue;
      ctx.beginPath();
      it.pts.forEach(([x, y], i) => (i ? ctx.lineTo(x, y) : ctx.moveTo(x, y)));
      ctx.closePath();
      const a = it.sel ? 0.32 : it.hover ? 0.22 : 0.12;
      ctx.fillStyle = it.kind === "door" ? `rgba(22,163,74,${a})` : `rgba(47,111,237,${a})`;
      ctx.fill();
      ctx.setLineDash(it.niche ? [5 * dpr, 4 * dpr] : []);
      ctx.strokeStyle = it.kind === "door" ? "#16a34a" : HUD.accent;
      ctx.lineWidth = (it.sel ? 3.2 : it.hover ? 2.4 : 1.6) * dpr;
      ctx.stroke();
      ctx.setLineDash([]);
    }
  }

  /** Edit Mode overlay: the profile's faces (solid filled, void
   *  dotted), edges, point handles, hover/selection, marquee, snap; a
   *  wall run's footprint (mitered). */
  function drawEditHud(ctx) {
    let h;
    try { h = JSON.parse(app.edit_hud_json()); } catch { return; }
    if (!h.active) return;
    const accent = h.invalid ? HUD.bad : HUD.accent;
    const rgbaAccent = (a) => (h.invalid ? `rgba(217,45,32,${a})` : `rgba(47,111,237,${a})`);
    const path = (pts) => {
      ctx.beginPath();
      pts.forEach(([x, y], i) => (i ? ctx.lineTo(x, y) : ctx.moveTo(x, y)));
      ctx.closePath();
    };
    const facesMode = h.mode === "faces";
    ctx.lineJoin = "round";
    ctx.lineCap = "round";
    // A wall run's footprint: the thickened line (a ring pair when closed).
    if (h.footprint && h.footprint.length) {
      ctx.beginPath();
      for (const ring of h.footprint) {
        ring.forEach(([x, y], i) => (i ? ctx.lineTo(x, y) : ctx.moveTo(x, y)));
        ctx.closePath();
      }
      ctx.fillStyle = rgbaAccent(0.18);
      ctx.fill("evenodd");
      ctx.strokeStyle = rgbaAccent(0.55);
      ctx.lineWidth = 1.2 * dpr;
      ctx.stroke();
    }
    for (const f of h.faces) {
      if (f.pts.length < 3) continue;
      path(f.pts);
      if (f.kind === "solid") {
        ctx.fillStyle = rgbaAccent(f.sel ? 0.3 : f.hover ? 0.2 : 0.1);
        ctx.fill();
      } else {
        // Voids: the background shows through, with a light hatch.
        ctx.save();
        ctx.clip();
        ctx.clearRect(0, 0, hud.width, hud.height);
        // Hatch only: the model under a void (a pocket floor, a cut)
        // stays visible.
        if (f.sel || f.hover) {
          ctx.fillStyle = rgbaAccent(f.sel ? 0.16 : 0.1);
          ctx.fill();
        }
        ctx.strokeStyle = rgbaAccent(f.sel ? 0.35 : 0.18);
        ctx.lineWidth = 1 * dpr;
        const xs = f.pts.map((p) => p[0]), ys = f.pts.map((p) => p[1]);
        const [x0, x1, y0, y1] = [Math.min(...xs), Math.max(...xs), Math.min(...ys), Math.max(...ys)];
        const gap = 9 * dpr;
        ctx.beginPath();
        for (let x = x0 - (y1 - y0); x < x1; x += gap) {
          ctx.moveTo(x, y1);
          ctx.lineTo(x + (y1 - y0), y0);
        }
        ctx.stroke();
        ctx.restore();
      }
    }
    for (const e of h.edges) {
      const [x0, y0, x1, y1] = e.p;
      ctx.beginPath();
      ctx.moveTo(x0, y0);
      ctx.lineTo(x1, y1);
      ctx.setLineDash(e.void ? [5 * dpr, 4 * dpr] : []);
      ctx.strokeStyle = e.sel || e.hover ? accent : rgbaAccent(0.75);
      ctx.lineWidth = (e.sel ? 4 : e.hover ? 3.2 : 1.8) * dpr;
      ctx.stroke();
    }
    ctx.setLineDash([]);
    const r = (facesMode ? 3 : h.mode === "edges" ? 3.5 : 6) * dpr;
    for (const p of h.points) {
      const [x, y] = p.p;
      const pr = p.hover && !p.sel ? r * 1.35 : r;
      ctx.beginPath();
      ctx.arc(x, y, pr, 0, Math.PI * 2);
      ctx.fillStyle = p.sel ? accent : "#fff";
      ctx.fill();
      ctx.lineWidth = 2 * dpr;
      ctx.strokeStyle = accent;
      ctx.stroke();
    }
    for (const [x1, y1, x2, y2] of h.guides ?? []) {
      ctx.beginPath();
      ctx.moveTo(x1, y1);
      ctx.lineTo(x2, y2);
      ctx.setLineDash([5 * dpr, 5 * dpr]);
      ctx.strokeStyle = HUD.guide;
      ctx.lineWidth = 1.5 * dpr;
      ctx.stroke();
      ctx.setLineDash([]);
    }
    if (h.dragging && h.snap && h.snap.kind !== "grid" && h.snap.kind !== "free") {
      ctx.beginPath();
      ctx.arc(h.snap.x, h.snap.y, 9 * dpr, 0, Math.PI * 2);
      ctx.strokeStyle = h.snap.kind === "vertex" ? HUD.vertex : HUD.guide;
      ctx.lineWidth = 2 * dpr;
      ctx.stroke();
    }
    if (h.invalid && h.invalidReason && h.snap) {
      pill(ctx, h.snap.x, h.snap.y - 34 * dpr, h.invalidReason, { bg: "rgba(217,45,32,0.95)", fg: "#fff", border: "rgba(0,0,0,0)" });
    }
    if (h.marquee) {
      const [x0, y0, x1, y1] = h.marquee;
      ctx.fillStyle = "rgba(47,111,237,0.08)";
      ctx.fillRect(Math.min(x0, x1), Math.min(y0, y1), Math.abs(x1 - x0), Math.abs(y1 - y0));
      ctx.setLineDash([6 * dpr, 4 * dpr]);
      ctx.strokeStyle = HUD.accent;
      ctx.lineWidth = 1.5 * dpr;
      ctx.strokeRect(Math.min(x0, x1), Math.min(y0, y1), Math.abs(x1 - x0), Math.abs(y1 - y0));
      ctx.setLineDash([]);
    }
  }

  /** Point-insert feedback: an expanding, fading ring. */
  function drawRings(ctx) {
    const now = performance.now();
    for (let i = rings.length - 1; i >= 0; i--) {
      const t = (now - rings[i].t0) / INSERT_RING_MS;
      if (t >= 1) { rings.splice(i, 1); continue; }
      ctx.beginPath();
      ctx.arc(rings[i].x, rings[i].y, (8 + 26 * t) * dpr, 0, Math.PI * 2);
      ctx.strokeStyle = `rgba(47,111,237,${1 - t})`;
      ctx.lineWidth = 3 * dpr * (1 - t) + 1;
      ctx.stroke();
    }
    if (rings.length) requestRender(); // keep animating
  }

  // ============================================================================
  // Chrome: top bar, tool dock, drawing bar, hint
  // ============================================================================
  let levelsState = JSON.parse(app.levels_json());
  const activeLevelInfo = () => levelsState.levels.find((l) => l.id === levelsState.activeId) ?? null;

  function renderLevelChip() {
    levelsState = JSON.parse(app.levels_json());
    const lvl = activeLevelInfo();
    const chip = $("level-chip");
    // The active construction plane: "Level 2 › Ceiling" for a workplane.
    const plane = levelsState.activePlane;
    // Phones: the plane's own name (the full path is the chip's title).
    const narrow = innerWidth < TREE_DEFAULT_OPEN_MIN_PX;
    chip.querySelector(".chip-name").textContent = plane ? (narrow ? plane.name : plane.path) : lvl ? lvl.name : "No level";
    chip.title = plane ? `Drawing on ${plane.path}` : "";
    chip.querySelector(".chip-elev").textContent = plane && !plane.isLevel
      ? fmtOffset(plane.offset ?? 0)
      : plane ? fmtM(plane.elevation) : lvl ? fmtM(lvl.elevation) : "add one in Levels";
    chip.querySelector(".swatch").style.background = lvl ? cssColor(lvl.color) : "#98a2b3";
  }

  function wallHint(tap, n) {
    if (stats.shape === "rect") {
      return n === 0 ? `Drag, or ${tap.toLowerCase()} two opposite corners of the room` : `${tap} the opposite corner`;
    }
    if (n === 0) return `${tap} where the wall starts — thickness grows to the left`;
    if (n < 3) return `${tap} the next corner, or Finish`;
    return `${tap} the first point to close the loop, or Finish`;
  }
  function hintText() {
    if (!stats.canAuthor) return "Add a level to start drawing (Menu → Levels)";
    const tap = COARSE ? "Tap" : "Click";
    if (editing() && editState.tool === "select") {
      if (editState.faces === 0) return "Draw a solid face to start the shape";
      const noun = { points: "points", edges: "edges", faces: "faces" }[editState.mode];
      if (COARSE) {
        return editState.mode === "points"
          ? "Tap · drag to move · hold an edge to add a point"
          : `Tap ${noun} · drag to move or box-select`;
      }
      const hold = editState.mode === "points" ? " · hold an edge to add a point" : "";
      return `${tap} ${noun} to select · drag to move · drag empty space to box-select${hold}`;
    }
    if (inOpenings()) {
      if (openingsState.selected) return COARSE ? "Drag the opening along its wall · set its size below" : `Drag the opening along its wall · ${tap.toLowerCase()} another wall to place more`;
      if (openingsState.preset === "door") {
        const d = PRESETS.door;
        return `${tap} a wall to place a door (${fmtDim(d.width)} × ${fmtDim(d.height)} m)`;
      }
      const w = PRESETS.window;
      return `${tap} a wall to place a window (${fmtDim(w.width)} × ${fmtDim(w.height)} m, sill ${fmtDim(w.sill)} m)`;
    }
    if (editing() && editState.tool === "extend") {
      return `${tap} where the wall goes on: a point after its nearest end`;
    }
    if (editing() && editState.tool === "split") {
      return `${tap} two points: a line across the faces to split`;
    }
    if (stats.tool === "hole") {
      return stats.platesOnLevel === 0
        ? "Draw a floor plate on this level first"
        : `${tap} a floor plate to cut holes or indentations into it`;
    }
    if (!isDrawing()) return "";
    const h = lastHud.active ? lastHud : JSON.parse(app.hud_json());
    const n = h.count ?? 0;
    if (stats.tool === "wall") {
      const base = wallHint(tap, n);
      // Up to: say how high the new walls come out.
      const ws = JSON.parse(app.wall_settings_json());
      if (ws.mode !== "upto") return base;
      return ws.effectiveHeight == null ? "The wall top must be above its base: pick a higher plane" : `${base} · ${fmtM(ws.effectiveHeight)} high`;
    }
    const what = { hole: "hole", window: "window" }[stats.tool] ?? "floor plate";
    if (stats.shape === "rect") {
      return n === 0 ? `Drag, or ${tap.toLowerCase()} two opposite corners of the ${what}` : `${tap} the opposite corner`;
    }
    if (n === 0) return `${tap} to place the first corner of the ${what}`;
    if (n < 3) return `${tap} to place the next corner`;
    return `${tap} the first corner or press Finish to close`;
  }

  function renderChrome() {
    editState = JSON.parse(app.edit_state_json());
    renderViewCluster();
    renderEditChrome();
    $("undo").disabled = !stats.canUndo;
    $("redo").disabled = !stats.canRedo;
    for (const b of document.querySelectorAll("#view-toggle button")) {
      b.classList.toggle("on", b.dataset.view === stats.view);
      b.setAttribute("aria-selected", String(b.dataset.view === stats.view));
    }
    for (const b of document.querySelectorAll(".tool[data-tool]")) {
      const tools = (b.dataset.tools ?? b.dataset.tool).split(" ");
      b.classList.toggle("on", tools.includes(stats.tool));
      b.disabled = b.dataset.tool !== "select" && !stats.canAuthor;
    }
    const snapBtn = $("snap-toggle");
    snapBtn.classList.toggle("on", snapEnabled);
    snapBtn.setAttribute("aria-pressed", String(snapEnabled));
    const drawing = isDrawing();
    document.body.classList.toggle("drawing", drawing);
    canvas.classList.toggle("drawing", drawing);
    $("drawbar").hidden = !drawing;
    if (drawing) {
      const h = JSON.parse(app.hud_json());
      lastHud = h;
      for (const b of document.querySelectorAll("#shape-toggle button")) {
        b.classList.toggle("on", b.dataset.shape === stats.shape);
      }
      const wall = stats.tool === "wall";
      const polyLabel = { wall: "Polyline", window: "Polygon" }[stats.tool] ?? "Polygon";
      const rectLabel = wall ? "Room" : "Rectangle";
      for (const b of document.querySelectorAll("#shape-toggle button")) {
        const label = b.dataset.shape === "rect" ? rectLabel : polyLabel;
        if (b.lastChild.textContent !== label) b.lastChild.textContent = label;
      }
      $("thickness-stepper").hidden = stats.tool !== "plate";
      $("flip-toggle").hidden = !wall;
      $("flip-toggle").classList.toggle("on", stats.wall.flip);
      $("flip-toggle").setAttribute("aria-pressed", String(stats.wall.flip));
      $("wall-settings").hidden = !wall;
      renderWallHeightMode(wall);
      for (const [id, v] of [["wall-height-input", stats.wall.height], ["wall-thickness-input", stats.wall.thickness]]) {
        if (document.activeElement !== $(id)) $(id).value = v.toFixed(2);
      }
      const ti = $("thickness-input");
      if (document.activeElement !== ti) ti.value = app.plate_thickness_setting().toFixed(2);
      $("finish-draw").disabled = !h.canFinish;
      $("undo-point").disabled = !(h.count > 0);
      const reason = $("draw-reason");
      reason.textContent = h.reason ?? "";
      reason.classList.toggle("bad", !!h.reason);
    }
    const hint = hintText();
    $("hint").textContent = hint;
    $("hint").hidden = !hint;
  }

  // Top bar -------------------------------------------------------------------
  // ONE history, one control: the same Undo / Redo in every mode (inside
  // an Edit Mode they stop at the session's entry).
  $("undo").addEventListener("click", () => { if (app.undo()) refresh(); });
  $("redo").addEventListener("click", () => { if (app.redo()) refresh(); });
  for (const b of document.querySelectorAll("#view-toggle button")) {
    b.addEventListener("click", () => {
      app.set_view_mode(b.dataset.view);
      stats = JSON.parse(app.stats_json());
      renderChrome();
      requestRender();
      sessionSave();
    });
  }
  $("menu-btn").addEventListener("click", () => {
    if (!$("sheet").hidden && sheetPage === "menu") closeSheet();
    else openSheet("menu", { root: true });
  });

  // Level chip + quick switcher.
  const popover = $("level-popover");
  function closePopover() { popover.hidden = true; }
  function openPopover() {
    levelsState = JSON.parse(app.levels_json());
    const workplanes = JSON.parse(app.workplanes_json());
    popover.replaceChildren();
    const desc = [...levelsState.levels].reverse();
    for (const l of desc) {
      // A level is "on" when it is the active plane itself.
      const on = l.id === levelsState.activeId && levelsState.activePlane?.isLevel !== false;
      const item = el("button", { class: `pop-item${on ? " on" : ""}`, type: "button", role: "option", "data-level": String(l.id) },
        el("span", { class: "swatch" }),
        el("span", { class: "pop-name", text: l.name }),
        el("span", { class: "pop-elev", text: fmtM(l.elevation) }),
        el("span", { class: "pop-check", html: on ? icon("check") : "" }),
      );
      item.querySelector(".swatch").style.background = cssColor(l.color);
      item.addEventListener("click", () => {
        setActiveLevel(l.id);
        closePopover();
      });
      popover.append(item);
      // The level's workplanes, nested (highest first).
      const addPlanes = (parent, depth) => {
        for (const w of workplanes.filter((x) => x.parent === parent).reverse()) {
          const p = el("button", {
            class: `pop-item pop-plane${w.active ? " on" : ""}`, type: "button", role: "option",
            "data-plane": String(w.id), style: `--depth:${depth}`,
          },
            el("span", { class: "swatch" }),
            el("span", { class: "pop-name", text: w.name }),
            el("span", { class: "pop-elev", text: fmtOffset(w.offset) }),
            el("span", { class: "pop-check", html: w.active ? icon("check") : "" }),
          );
          p.querySelector(".swatch").style.background = cssColor(w.color);
          p.addEventListener("click", () => { activatePlane(w.id); closePopover(); });
          popover.append(p);
          addPlanes(w.id, depth + 1);
        }
      };
      addPlanes(l.id, 1);
    }
    if (desc.length) popover.append(el("div", { class: "pop-sep" }));
    popover.append(el("button", {
      class: "pop-item pop-link", type: "button", text: "Manage levels…",
      onclick: () => { closePopover(); openSheet("levels", { root: true }); },
    }));
    popover.hidden = false;
  }
  $("level-chip").addEventListener("click", (e) => {
    e.stopPropagation();
    if (popover.hidden) openPopover(); else closePopover();
  });
  document.addEventListener("pointerdown", (e) => {
    if (!popover.hidden && !popover.contains(e.target) && !$("level-chip").contains(e.target)) closePopover();
  });

  function setActiveLevel(id) {
    queueMicrotask(() => renderTreePanel());
    // Session state: no document change, so the panels repaint by hand.
    app.set_active_level(id);
    stats = JSON.parse(app.stats_json());
    renderLevelChip();
    renderChrome();
    if (sheetPage === "levels") renderSheet();
    requestRender();
    sessionSave();
  }

  // Tool dock ---------------------------------------------------------------------
  function setTool(tool) {
    if (tool === "plate") {
      // A new floor plate is authored as a profile, in Edit Mode.
      if (app.edit_begin_new()) enterEditChrome();
      else toast("Add a level first (Menu → Levels)", { kind: "error" });
      return true;
    }
    if (!app.set_tool(tool)) {
      toast("Add a level first (Menu → Levels)", { kind: "error" });
      return false;
    }
    stats = JSON.parse(app.stats_json());
    if (tool !== "select" && sheetPage === "properties") closeSheet(false);
    if (tool === "hole" && stats.platesOnLevel === 0) {
      toast("Draw a floor plate on this level first — holes are cut into plates", { ms: 3200 });
    }
    renderChrome();
    requestRender();
    return true;
  }
  // The Window tool opens Openings mode with the last preset used.
  let openingTool = "window";
  for (const b of document.querySelectorAll(".tool[data-tool]")) {
    b.addEventListener("click", () => (b.dataset.tool === "window" ? beginOpenings(openingTool) : setTool(b.dataset.tool)));
  }
  $("snap-toggle").addEventListener("click", () => {
    snapEnabled = !snapEnabled;
    app.set_snap(snapEnabled, snapStep);
    toast(snapEnabled ? `Snapping on (${snapStep} m grid)` : "Snapping off", { ms: 1400 });
    renderChrome();
    requestRender();
    sessionSave();
  });
  $("fit-btn").addEventListener("click", () => {
    fitView();
    requestRender();
    sessionSave();
  });

  // Render mode: shaded, shaded + wireframe, wireframe (the triangle
  // edges: the real mesh topology). Session state.
  const RENDER_LABEL = { shaded: "Shaded", "shaded-wire": "Shaded + wireframe", wire: "Wireframe" };
  function renderViewCluster() {
    const mode = app.render_mode();
    $("render-btn").classList.toggle("on", mode !== "shaded");
    $("render-btn").title = `Render mode: ${RENDER_LABEL[mode]}`;
    for (const b of document.querySelectorAll("#render-menu button")) {
      b.classList.toggle("on", b.dataset.render === mode);
      b.setAttribute("aria-checked", String(b.dataset.render === mode));
    }
    // While a wireframe shows, the triangle count of the focus (the
    // selection or the element being edited) or of the model.
    const badge = $("tri-badge");
    badge.hidden = mode === "shaded";
    if (!badge.hidden) {
      const st = JSON.parse(app.stats_json());
      const focus = st.focusTriangles;
      const name = focus == null ? "Model"
        : editing() ? editState.name
        : inOpenings() ? openingsState.selected?.wallName ?? "Wall"
        : JSON.parse(app.selected_json())?.name;
      const n = focus ?? st.triangles;
      badge.textContent = `${name} · ${n.toLocaleString("en-US")} triangle${n === 1 ? "" : "s"}`;
    }
  }
  const closeRenderMenu = () => { $("render-menu").hidden = true; $("render-btn").setAttribute("aria-expanded", "false"); };
  $("render-btn").addEventListener("click", (e) => {
    e.stopPropagation();
    const menu = $("render-menu");
    menu.hidden = !menu.hidden;
    $("render-btn").setAttribute("aria-expanded", String(!menu.hidden));
  });
  for (const b of document.querySelectorAll("#render-menu button")) {
    b.addEventListener("click", () => {
      app.set_render_mode(b.dataset.render);
      closeRenderMenu();
      stats = JSON.parse(app.stats_json());
      renderViewCluster();
      requestRender();
      sessionSave();
    });
  }
  document.addEventListener("pointerdown", (e) => {
    if (!$("render-menu").hidden && !$("render-menu").contains(e.target) && !$("render-btn").contains(e.target)) closeRenderMenu();
  });

  // Drawing bar ---------------------------------------------------------------------
  for (const b of document.querySelectorAll("#shape-toggle button")) {
    b.addEventListener("click", () => {
      app.set_shape(b.dataset.shape);
      stats = JSON.parse(app.stats_json());
      renderChrome();
      requestRender();
      sessionSave();
    });
  }
  $("undo-point").addEventListener("click", () => {
    app.sketch_undo_point();
    renderChrome();
    requestRender();
  });
  $("cancel-draw").addEventListener("click", () => cancelDraw());
  $("finish-draw").addEventListener("click", () => handlePlaced(app.sketch_finish()));
  function cancelDraw() {
    const discarded = app.sketch_cancel();
    if (discarded === 0 && editing()) {
      app.edit_set_tool("select");
      refreshEdit();
      return;
    }
    if (discarded === 0) setTool("select");
    renderChrome();
    requestRender();
  }
  const setThickness = (v) => {
    if (!Number.isFinite(v)) return;
    app.set_plate_thickness_setting(Math.round(v * 100) / 100);
    $("thickness-input").value = app.plate_thickness_setting().toFixed(2);
    sessionSave();
  };
  for (const b of document.querySelectorAll("#thickness-stepper button")) {
    b.addEventListener("click", (e) => {
      e.preventDefault();
      setThickness(app.plate_thickness_setting() + Number(b.dataset.step) * PLATE_THICKNESS_STEP_M);
    });
  }
  $("thickness-input").addEventListener("change", (e) => setThickness(parseNum(e.target.value)));

  // Wall settings (for new walls) and the flip-side toggle.
  const setWallSettings = ({ height = stats.wall.height, thickness = stats.wall.thickness, flip = stats.wall.flip }) => {
    app.set_wall_settings(height, thickness, flip);
    stats = JSON.parse(app.stats_json());
    renderChrome();
    requestRender();
    sessionSave();
  };
  for (const b of document.querySelectorAll("#wall-settings button[data-wall]")) {
    b.addEventListener("click", (e) => {
      e.preventDefault();
      const step = Number(b.dataset.step);
      if (b.dataset.wall === "height") {
        setWallSettings({ height: Math.round((stats.wall.height + step * WALL_HEIGHT_STEP_M) * 100) / 100 });
      } else {
        setWallSettings({ thickness: Math.round((stats.wall.thickness + step * WALL_THICKNESS_STEP_M) * 100) / 100 });
      }
    });
  }
  $("wall-height-input").addEventListener("change", (e) => {
    const v = parseNum(e.target.value);
    if (Number.isFinite(v)) setWallSettings({ height: v });
  });
  $("wall-thickness-input").addEventListener("change", (e) => {
    const v = parseNum(e.target.value);
    if (Number.isFinite(v)) setWallSettings({ thickness: v });
  });
  $("flip-toggle").addEventListener("click", () => setWallSettings({ flip: !stats.wall.flip }));

  // Wall height mode for new walls (preview): Fixed uses the height
  // stepper; Up to uses a plane picker and an offset from that plane.
  function renderWallHeightMode(wall) {
    const row = $("wall-height-mode");
    row.hidden = !wall;
    if (row.hidden) { $("wall-height-stepper").hidden = false; return; }
    const ws = JSON.parse(app.wall_settings_json());
    const upto = ws.mode === "upto";
    for (const b of document.querySelectorAll("#wall-mode-toggle button")) {
      b.classList.toggle("on", b.dataset.wallMode === ws.mode);
    }
    const picker = $("wall-top-plane");
    const key = `${innerWidth < TREE_DEFAULT_OPEN_MIN_PX}|` + ws.planes.map((p) => `${p.id}:${p.path}:${p.elevation}`).join("|");
    if (picker.dataset.key !== key) {
      picker.dataset.key = key;
      planeOptions(picker, ws.planes, ws.topPlane);
    }
    if (ws.topPlane != null) picker.value = String(ws.topPlane);
    picker.hidden = !upto;
    $("wall-top-offset-stepper").hidden = !upto;
    if (document.activeElement !== $("wall-top-offset")) $("wall-top-offset").value = ws.topOffset.toFixed(2);
    // Up to: the plane decides the height, so the fixed stepper hides.
    $("wall-height-stepper").hidden = upto;
    row.classList.toggle("bad", upto && ws.effectiveHeight == null);
    row.title = upto
      ? (ws.effectiveHeight == null ? "The wall top must be above its base: pick a higher plane" : `New walls: ${fmtM(ws.effectiveHeight)} high`)
      : "";
  }
  const setWallHeightMode = ({ mode, plane, offset }) => {
    const ws = JSON.parse(app.wall_settings_json());
    const m = mode ?? ws.mode;
    let p = plane ?? ws.topPlane;
    if (m === "upto" && p == null) {
      const base = JSON.parse(app.levels_json()).activePlane?.elevation ?? 0;
      p = planeAbove(ws.planes, base);
    }
    app.set_wall_height_mode(m, p ?? -1, offset ?? ws.topOffset);
    renderChrome();
    sessionSave();
  };
  for (const b of document.querySelectorAll("#wall-mode-toggle button")) {
    b.addEventListener("click", (e) => { e.preventDefault(); setWallHeightMode({ mode: b.dataset.wallMode }); });
  }
  $("wall-top-plane").addEventListener("change", (e) => setWallHeightMode({ plane: Number(e.target.value) }));
  for (const b of document.querySelectorAll("#wall-top-offset-stepper button")) {
    b.addEventListener("click", (e) => {
      e.preventDefault();
      const ws = JSON.parse(app.wall_settings_json());
      setWallHeightMode({ offset: Math.round((ws.topOffset + Number(b.dataset.topOffset) * WALL_TOP_OFFSET_STEP_M) * 100) / 100 });
    });
  }
  $("wall-top-offset").addEventListener("change", (e) => {
    const v = parseNum(e.target.value);
    if (Number.isFinite(v)) setWallHeightMode({ offset: v });
  });


  // Edit Mode ---------------------------------------------------------------------------
  // Autosave is suspended while editing: the document is saved as the
  // session ends (✓), so an interrupted session (tab closed, crash) comes
  // back as it was before editing — the same as ✗.
  function refreshEdit() {
    stats = JSON.parse(app.stats_json());
    renderChrome();
    requestRender();
  }
  /** Enter Edit Mode on a wall's run, in plan: its points and segments
   *  (legacy walls are converted). */
  function beginWallEdit(id) {
    if (!app.edit_begin_wall(id)) {
      toast("This wall cannot be edited", { kind: "error" });
      return;
    }
    enterEditChrome();
    fitView(); // the run, clear of the edit chrome
    requestRender();
  }
  /** Enter Openings mode: place, move, and size windows and doors on any
   *  wall. */
  function beginOpenings(preset = openingTool) {
    if (inEditChrome()) return;
    if (!app.openings_begin(preset)) {
      toast("Add a level first (Menu → Levels)", { kind: "error" });
      return;
    }
    openingTool = preset;
    if (stats.walls === 0) toast("Draw some walls first — windows and doors go into walls", { ms: 3200 });
    enterEditChrome();
  }
  /** Enter Edit Mode on a floor plate (a legacy plate is converted). */
  function beginEdit(id, tool = null) {
    if (!app.edit_begin(id)) {
      toast("This element cannot be edited as a shape", { kind: "error" });
      return;
    }
    if (tool) app.edit_set_tool(tool);
    enterEditChrome();
  }
  function enterEditChrome() {
    if (!$("sheet").hidden) closeSheet(false);
    closePopover();
    // A floor plate keeps the selection mode used last (a run starts in
    // Points, openings have none).
    const st = JSON.parse(app.edit_state_json());
    if (st.active && st.target === "floor") app.edit_set_mode(editState.mode ?? "faces");
    refreshEdit();
  }
  function exitEditChrome() {
    refreshEdit();
    renderTreePanel();
    refresh();
    saveNow();
  }
  function renderEditChrome() {
    openingsState = JSON.parse(app.openings_state_json());
    const on = inEditChrome();
    document.body.classList.toggle("editing", on);
    $("edit-bar").hidden = !on;
    $("edit-dock").hidden = !on;
    $("edit-panel").hidden = !on;
    const run = editing() && editState.target === "run";
    document.body.classList.toggle("edit-run", run);
    document.body.classList.toggle("edit-openings", inOpenings());
    if (!on) return;
    for (const b of document.querySelectorAll("#edit-view-toggle button")) {
      b.classList.toggle("on", b.dataset.view === stats.view);
    }
    if (inOpenings()) { renderOpeningsChrome(); return; }
    $("edit-caption").textContent = "Editing";
    $("edit-name").textContent = editState.name;
    for (const b of document.querySelectorAll("[data-edit-mode]")) {
      b.classList.toggle("on", b.dataset.editMode === editState.mode && editState.tool === "select");
    }
    for (const b of document.querySelectorAll("[data-edit-tool]")) {
      b.classList.toggle("on", b.dataset.editTool === editState.tool);
    }
    $("edit-delete").disabled = !editState.canDelete;
    if (run) { renderRunPanel(); return; }
    // Thickness panel: the selected faces, or the defaults for new faces.
    const panel = editState.panel;
    const sel = panel.target === "selection";
    $("edit-panel").classList.toggle("target-new", !sel);
    $("edit-panel-title").textContent = sel
      ? `${editState.selection} face${editState.selection === 1 ? "" : "s"} selected`
      : "New faces";
    $("edit-panel-note").textContent = sel ? "" : "select faces to change them";
    const solid = panel.solid, voidP = panel.void;
    $("edit-solid-row").hidden = !solid;
    $("edit-void-row").hidden = !voidP;
    if (solid) {
      guardValue($("edit-thickness"), solid.mixed ? "" : solid.thickness.toFixed(2));
      $("edit-thickness").placeholder = solid.mixed ? "mixed" : "";
      guardValue($("edit-thickness-slider"), String(solid.thickness));
    }
    if (voidP) {
      guardValue($("edit-depth"), voidP.mixed ? "" : voidP.depth.toFixed(2));
      guardValue($("edit-depth-slider"), String(voidP.depth));
      $("edit-through").classList.toggle("on", voidP.through);
      $("edit-through").setAttribute("aria-pressed", String(voidP.through));
      $("edit-void-row").classList.toggle("dim", voidP.through);
    }
    const shapeRow = $("shape-toggle");
    shapeRow.hidden = editState.tool === "split";
  }
  /** The wall run panel: thickness, side, closed, height mode. */
  function renderRunPanel() {
    const r = editState.run;
    if (!r) return;
    $("edit-panel").classList.remove("target-new");
    $("edit-panel-title").textContent = `Wall run · ${r.walls} wall${r.walls === 1 ? "" : "s"}`;
    $("edit-panel-note").textContent = `${r.points} points · ${r.closed ? "closed" : "open"}`;
    guardValue($("run-thickness"), r.thickness.toFixed(2));
    $("run-flip").classList.toggle("on", r.flip);
    $("run-flip").setAttribute("aria-pressed", String(r.flip));
    $("run-closed").classList.toggle("on", r.closed);
    $("run-closed").setAttribute("aria-pressed", String(r.closed));
    $("run-closed").disabled = !r.closed && r.points < 3;
    for (const b of document.querySelectorAll("#run-mode button")) b.classList.toggle("on", b.dataset.runMode === r.mode);
    $("run-fixed-line").hidden = r.mode !== "fixed";
    $("run-upto-line").hidden = r.mode !== "upto";
    guardValue($("run-height"), r.height.toFixed(2));
    guardValue($("run-offset"), r.topOffset.toFixed(2));
    const picker = $("run-top");
    const planes = JSON.parse(app.wall_settings_json()).planes;
    const key = planes.map((p) => `${p.id}:${p.path}`).join("|");
    if (picker.dataset.key !== key) { picker.dataset.key = key; planeOptions(picker, planes, r.topPlane); }
    if (r.topPlane != null) guardValue(picker, String(r.topPlane));
  }
  /** Openings mode chrome: the preset, and the selected opening's size. */
  function renderOpeningsChrome() {
    const o = openingsState;
    $("edit-caption").textContent = "Placing";
    $("edit-name").textContent = "Windows and doors";
    for (const b of document.querySelectorAll("[data-opening-preset]")) b.classList.toggle("on", b.dataset.openingPreset === o.preset);
    $("edit-delete").disabled = !o.selected;
    const wallBtn = $("edit-view-toggle").querySelector('[data-view="elevation"]');
    wallBtn.disabled = o.faced == null;
    const sel = o.selected;
    $("edit-panel").classList.toggle("target-new", !sel);
    const noun = sel ? (sel.kind === "door" ? "Door" : "Window") : null;
    $("edit-panel-title").textContent = sel ? `${noun} in ${sel.wallName}` : `${o.count} opening${o.count === 1 ? "" : "s"}`;
    $("edit-panel-note").textContent = sel ? `${fmtM(sel.offset)} from the wall start` : "tap an opening to change it";
    for (const row of document.querySelectorAll("#edit-panel .edit-row.openings-only")) row.hidden = !sel;
    if (!sel) return;
    guardValue($("opening-width"), sel.width.toFixed(2));
    guardValue($("opening-height"), sel.height.toFixed(2));
    guardValue($("opening-sill"), sel.sill.toFixed(2));
    $("opening-sill-line").hidden = sel.kind === "door";
    const through = sel.depth == null;
    $("opening-through").classList.toggle("on", through);
    $("opening-through").setAttribute("aria-pressed", String(through));
    $("opening-depth-stepper").classList.toggle("dim", through);
    guardValue($("opening-depth"), (sel.depth ?? OPENING_NICHE_DEPTH_M).toFixed(2));
  }
  let lastReason = "";
  /** A drag's refusal, once per message. */
  const showReason = (reason) => {
    if (reason !== lastReason) toast(reason, { kind: "error" });
    lastReason = reason;
  };
  function guardValue(input, v) {
    if (document.activeElement !== input && input.value !== v) input.value = v;
  }
  $("edit-confirm").addEventListener("click", () => editConfirm());
  $("edit-cancel").addEventListener("click", () => editCancel());
  function editConfirm() {
    if (inOpenings()) {
      const changed = app.openings_confirm();
      exitEditChrome();
      if (changed) toast("Openings saved", { kind: "ok", ms: 1600 });
      return;
    }
    const res = JSON.parse(app.edit_confirm());
    exitEditChrome();
    if (res.deleted) {
      toast(`${res.name} deleted — it had no faces left`);
    } else if (res.changed) {
      toast(`${res.name} saved`, { kind: "ok", ms: 1600 });
    }
  }
  async function editCancel() {
    const openings = inOpenings();
    if (openings ? openingsState.canUndo : editState.canUndo) {
      const ok = await confirmDialog({
        title: "Discard your changes?",
        message: openings
          ? "Every window and door you placed or changed since you started will be undone."
          : `Everything you changed in ${editState.name} since you started editing will be undone.`,
        ok: "Discard", danger: true,
      });
      if (!ok) return;
    }
    if (openings) app.openings_cancel(); else app.edit_cancel();
    exitEditChrome();
  }
  for (const b of document.querySelectorAll("#edit-view-toggle button")) {
    b.addEventListener("click", () => {
      app.set_view_mode(b.dataset.view);
      refreshEdit();
      sessionSave();
    });
  }
  // Wall run settings (one undo step each; typing coalesces).
  const runSet = (o) => {
    const res = JSON.parse(app.edit_run_settings(
      o.thickness ?? NaN, o.flip === undefined ? -1 : o.flip ? 1 : 0, o.closed === undefined ? -1 : o.closed ? 1 : 0,
      o.mode ?? "", o.plane ?? -1, o.offset ?? NaN, o.height ?? NaN));
    if (res.result === "rejected") toast(res.reason, { kind: "error" });
    refresh();
  };
  const RUN_STEP = { thickness: WALL_THICKNESS_STEP_M, height: WALL_HEIGHT_STEP_M, offset: WALL_TOP_OFFSET_STEP_M };
  for (const b of document.querySelectorAll("[data-run-step]")) {
    b.addEventListener("click", () => {
      const r = editState.run;
      if (!r) return;
      const f = b.dataset.runStep;
      const cur = { thickness: r.thickness, height: r.height, offset: r.topOffset }[f];
      const v = Math.round((cur + Number(b.dataset.step) * RUN_STEP[f]) * 100) / 100;
      runSet(f === "thickness" ? { thickness: v } : f === "height" ? { mode: "fixed", height: v } : { mode: "upto", offset: v });
      app.edit_end_gesture();
    });
  }
  for (const [id, f] of [["run-thickness", "thickness"], ["run-height", "height"], ["run-offset", "offset"]]) {
    $(id).addEventListener("change", (e) => {
      const v = parseNum(e.target.value);
      if (Number.isFinite(v)) runSet(f === "thickness" ? { thickness: v } : f === "height" ? { mode: "fixed", height: v } : { mode: "upto", offset: v });
      app.edit_end_gesture();
    });
  }
  $("run-flip").addEventListener("click", () => runSet({ flip: !editState.run?.flip }));
  $("run-closed").addEventListener("click", () => runSet({ closed: !editState.run?.closed }));
  for (const b of document.querySelectorAll("#run-mode button")) {
    b.addEventListener("click", () => {
      const r = editState.run;
      if (!r || r.mode === b.dataset.runMode) return;
      if (b.dataset.runMode === "fixed") runSet({ mode: "fixed" });
      else {
        const planes = JSON.parse(app.wall_settings_json()).planes;
        const base = planes.find((p) => p.id === r.base)?.elevation ?? 0;
        runSet({ mode: "upto", plane: planeAbove(planes, base) ?? -1 });
      }
    });
  }
  $("run-top").addEventListener("change", (e) => runSet({ mode: "upto", plane: Number(e.target.value) }));

  // Openings mode: preset, and the selected opening's size (typing and
  // stepping coalesce per field until the change ends).
  for (const b of document.querySelectorAll("[data-opening-preset]")) {
    b.addEventListener("click", () => {
      openingTool = b.dataset.openingPreset;
      app.openings_set_preset(openingTool);
      refresh();
    });
  }
  const openingSet = (o) => {
    const res = JSON.parse(app.openings_set(o.width ?? NaN, o.height ?? NaN, o.sill ?? NaN, o.depth ?? NaN));
    if (res.result === "rejected") toast(res.reason, { kind: "error" });
    refresh();
  };
  for (const b of document.querySelectorAll("[data-opening-step]")) {
    b.addEventListener("click", () => {
      const sel = openingsState.selected;
      if (!sel) return;
      const f = b.dataset.openingStep;
      const cur = f === "depth" ? (sel.depth ?? OPENING_NICHE_DEPTH_M) : sel[f];
      openingSet({ [f]: Math.round((cur + Number(b.dataset.step) * OPENING_STEP_M) * 100) / 100 });
      app.end_gesture();
    });
  }
  for (const f of ["width", "height", "sill", "depth"]) {
    $(`opening-${f}`).addEventListener("change", (e) => {
      const v = parseNum(e.target.value);
      if (Number.isFinite(v)) openingSet({ [f]: v });
      app.end_gesture();
    });
  }
  $("opening-through").addEventListener("click", () => {
    const sel = openingsState.selected;
    if (sel) openingSet({ depth: sel.depth == null ? OPENING_NICHE_DEPTH_M : -1 });
    app.end_gesture();
  });

  for (const b of document.querySelectorAll("[data-edit-mode]")) {
    b.addEventListener("click", () => {
      app.edit_set_tool("select");
      app.edit_set_mode(b.dataset.editMode);
      refreshEdit();
    });
  }
  for (const b of document.querySelectorAll("[data-edit-tool]")) {
    b.addEventListener("click", () => {
      const tool = editState.tool === b.dataset.editTool ? "select" : b.dataset.editTool;
      app.edit_set_tool(tool);
      refreshEdit();
    });
  }
  $("edit-delete").addEventListener("click", () => editDelete());
  function editDelete() {
    if (inOpenings()) {
      if (app.openings_delete()) refresh();
      return;
    }
    const res = JSON.parse(app.edit_delete());
    if (res.result === "rejected") toast(res.reason, { kind: "error" });
    refreshEdit();
  }
  const editResult = (json) => {
    const res = JSON.parse(json);
    if (res.result === "rejected") toast(res.reason, { kind: "error" });
    refreshEdit();
  };
  const bumpEdit = (which, step) => {
    const panel = editState.panel;
    if (which === "thickness" && panel.solid) {
      editResult(app.edit_set_thickness(Math.round((panel.solid.thickness + step * FACE_THICKNESS_STEP_M) * 100) / 100));
    } else if (which === "depth" && panel.void) {
      editResult(app.edit_set_depth(Math.round((panel.void.depth + step * VOID_DEPTH_STEP_M) * 100) / 100));
    }
    app.edit_end_gesture();
  };
  for (const b of document.querySelectorAll("[data-edit-step]")) {
    b.addEventListener("click", (e) => {
      e.preventDefault();
      bumpEdit(b.dataset.editStep, Number(b.dataset.step));
    });
  }
  // Typing and slider drags coalesce into one undo step per gesture.
  $("edit-thickness").addEventListener("input", (e) => {
    const v = parseNum(e.target.value);
    if (Number.isFinite(v)) editResult(app.edit_set_thickness(v));
  });
  $("edit-thickness").addEventListener("change", () => { app.edit_end_gesture(); refreshEdit(); });
  $("edit-thickness-slider").addEventListener("input", (e) => editResult(app.edit_set_thickness(parseFloat(e.target.value))));
  $("edit-thickness-slider").addEventListener("change", () => app.edit_end_gesture());
  $("edit-depth").addEventListener("input", (e) => {
    const v = parseNum(e.target.value);
    if (Number.isFinite(v)) editResult(app.edit_set_depth(v));
  });
  $("edit-depth").addEventListener("change", () => { app.edit_end_gesture(); refreshEdit(); });
  $("edit-depth-slider").addEventListener("input", (e) => editResult(app.edit_set_depth(parseFloat(e.target.value))));
  $("edit-depth-slider").addEventListener("change", () => app.edit_end_gesture());
  $("edit-through").addEventListener("click", () => {
    editResult(app.edit_set_through(!(editState.panel.void?.through ?? true)));
  });

  // Keyboard (desktop) ----------------------------------------------------------------
  window.addEventListener("keydown", (e) => {
    const t = e.target;
    const typing = t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement;
    const mod = e.ctrlKey || e.metaKey;
    if (mod && (e.key === "z" || e.key === "Z")) {
      if (typing) return;
      e.preventDefault();
      if (e.shiftKey ? app.redo() : app.undo()) refresh();
      return;
    }
    if (mod && (e.key === "y" || e.key === "Y")) {
      if (typing) return;
      e.preventDefault();
      if (app.redo()) refresh();
      return;
    }
    if (typing) {
      if (e.key === "Enter") t.blur();
      return;
    }
    if (!$("dialog-backdrop").hidden) return;
    if (inOpenings()) {
      if (e.key === "Enter") { e.preventDefault(); editConfirm(); }
      else if (e.key === "Escape") { e.preventDefault(); app.openings_tap(-1e9, -1e9, 0, 0); refresh(); }
      else if (e.key === "Delete" || e.key === "Backspace") { e.preventDefault(); editDelete(); }
      return;
    }
    if (editing() && !isDrawing()) {
      if (e.key === "Enter") { e.preventDefault(); editConfirm(); }
      else if (e.key === "Escape") { e.preventDefault(); app.edit_tap(-1e9, -1e9, 0, false); refreshEdit(); }
      else if (e.key === "Delete" || e.key === "Backspace") { e.preventDefault(); editDelete(); }
      else if (["1", "2", "3"].includes(e.key) && !mod) {
        app.edit_set_mode(["points", "edges", "faces"][Number(e.key) - 1]);
        refreshEdit();
      }
      return;
    }
    if (isDrawing()) {
      if (e.key === "Enter") { e.preventDefault(); handlePlaced(app.sketch_finish()); }
      else if (e.key === "Escape") { e.preventDefault(); cancelDraw(); }
      else if (e.key === "Backspace" || e.key === "Delete") {
        e.preventDefault();
        app.sketch_undo_point();
        renderChrome();
        requestRender();
      }
    } else if (e.key === "Escape") {
      if (!$("sheet").hidden) closeSheet();
      else if (stats.tool !== "select") setTool("select");
    } else if ((e.key === "Delete" || e.key === "Backspace") && app.selection() >= 0) {
      e.preventDefault();
      deleteElement(app.selection());
    }
    if (mod || e.altKey || inEditChrome()) return;
    const k = e.key.toLowerCase();
    if (k === "v" || k === "s") setTool("select");
    else if (k === "f") setTool("plate");
    else if (k === "h") setTool("hole");
    else if (k === "w") setTool("wall");
    else if (k === "n") beginOpenings(openingTool);
    else if (k === "r" && isDrawing()) { app.set_shape(stats.shape === "rect" ? "polygon" : "rect"); stats = JSON.parse(app.stats_json()); renderChrome(); requestRender(); }
    else if (k === "p") { app.set_view_mode("plan"); stats = JSON.parse(app.stats_json()); renderChrome(); requestRender(); }
    else if (k === "3") { app.set_view_mode("3d"); stats = JSON.parse(app.stats_json()); renderChrome(); requestRender(); }
  });

  // ============================================================================
  // Selection + element actions
  // ============================================================================
  function selectElement(id) {
    app.select(id);
    stats = JSON.parse(app.stats_json());
    if (app.selection() >= 0) {
      openSheet("properties", { root: true });
      revealSelection();
    }
    else if (sheetPage === "properties") closeSheet(false);
    renderTreePanel();
    renderViewCluster();
    requestRender();
  }

  /** Phones: frame the selected element in the band between the top
   *  bar and the properties sheet (pan, and zoom out if it is too big). */
  function revealSelection() {
    if (innerWidth >= 760) return;
    const e = JSON.parse(app.selected_json());
    const outline = e?.outline ?? e?.faces?.flatMap((f) => f.outline) ?? (e?.start ? [e.start, e.end] : null);
    if (!outline?.length) return;
    const lvl = JSON.parse(app.levels_json()).levels.find((l) => l.id === e.levelId);
    const z = lvl ? lvl.elevation : 0;
    const zs = e.kind === "wall" ? [z, z + e.height] : [z];
    const pts = outline.flatMap(([x, y]) => zs.map((zz) => JSON.parse(app.world_to_screen(x, y, zz)))).filter(Boolean);
    if (!pts.length) return;
    const xs = pts.map((p) => p[0]), ys = pts.map((p) => p[1]);
    const box = { x0: Math.min(...xs), x1: Math.max(...xs), y0: Math.min(...ys), y1: Math.max(...ys) };
    // offsetHeight ignores the entrance transform: the sheet's final top.
    const sheetTop = (innerHeight - sheet.offsetHeight) * dpr;
    const bandTop = $("topbar").getBoundingClientRect().bottom * dpr + 12 * dpr;
    const bandBottom = sheetTop - 12 * dpr;
    const margin = 16 * dpr;
    const fits = box.y0 >= bandTop && box.y1 <= bandBottom && box.x0 >= margin && box.x1 <= canvas.width - margin;
    if (fits) return;
    const center = [(box.x0 + box.x1) / 2, (box.y0 + box.y1) / 2];
    const target = [canvas.width / 2, (bandTop + bandBottom) / 2];
    app.pan(center[0], center[1], target[0], target[1]);
    const k = Math.max((box.y1 - box.y0) / ((bandBottom - bandTop) * 0.85), (box.x1 - box.x0) / ((canvas.width - 2 * margin) * 0.9));
    if (k > 1) app.zoom_at(k, target[0], target[1]);
    requestRender();
  }

  function deleteElement(id) {
    const e = JSON.parse(app.selected_json() ?? "null");
    const name = e?.id === id ? e.name : "Element";
    if (app.delete_element(id)) {
      if (sheetPage === "properties") closeSheet(false);
      refresh();
      toast(`Deleted ${name}`);
    }
  }

  // ============================================================================
  // Sheets (bottom sheet on phones, side panel on desktop)
  // ============================================================================
  const sheet = $("sheet");
  const sheetBody = $("sheet-body");
  let sheetPage = null;
  let sheetStack = [];
  const PAGE_TITLES = {
    menu: "Menu", levels: "Levels", project: "Project location", about: "About",
    properties: "Properties", model: "Model", workplane: "Workplane",
  };
  function openSheet(page, { root = false } = {}) {
    if (root) sheetStack = [];
    else if (sheetPage && sheetPage !== page) sheetStack.push(sheetPage);
    sheetPage = page;
    sheet.hidden = false;
    document.body.classList.add("sheet-open");
    sheet.classList.toggle("compact", page === "properties");
    renderSheet();
  }
  function closeSheet(deselect = true) {
    const was = sheetPage;
    sheet.hidden = true;
    document.body.classList.remove("sheet-open");
    sheetPage = null;
    sheetStack = [];
    sheetBody.replaceChildren();
    if (was === "properties" && deselect && app.selection() >= 0) {
      app.select(-1);
      requestRender();
    }
    renderTreePanel();
  }
  $("sheet-close").addEventListener("click", () => closeSheet());
  $("sheet-back").addEventListener("click", () => {
    const prev = sheetStack.pop();
    if (prev) { sheetPage = prev; renderSheet(); } else closeSheet();
  });
  sheet.querySelector(".sheet-handle").addEventListener("click", () => closeSheet());
  // Ghost-click shield: on touch devices the compatibility click of the
  // tap that OPENED the sheet is hit-tested after the sheet appears under
  // the finger — it must not focus an input (keyboard pop-up) or press a
  // button.
  // Only an event at the tap's own position shortly after it is the
  // ghost; deliberate taps elsewhere on the sheet pass.
  for (const type of ["mousedown", "click"]) {
    sheet.addEventListener(type, (e) => {
      const g = ghostTap;
      if (!g || performance.now() - g.t > GHOST_CLICK_MS) return;
      if (Math.hypot(e.clientX - g.x, e.clientY - g.y) > GHOST_CLICK_PX) return;
      e.preventDefault();
      e.stopPropagation();
      if (type === "click") ghostTap = null;
    }, true);
  }

  function renderSheet() {
    if (!sheetPage) return;
    $("sheet-title").textContent = PAGE_TITLES[sheetPage] ?? "";
    $("sheet-back").hidden = sheetStack.length === 0;
    const pages = {
      menu: pageMenu, levels: pageLevels, project: pageProject, about: pageAbout, properties: pageProperties,
      model: () => renderTree(sheetBody),
      workplane: pageWorkplane,
    };
    (pages[sheetPage] ?? (() => {}))();
  }

  /** Repaint document-bound UI: triggered only by the dirty pump. */
  function renderDocPanels() {
    renderLevelChip();
    renderTreePanel();
    if (sheetPage === "model") renderTree(sheetBody);
    if (!popover.hidden) openPopover();
    if (sheetPage === "properties") {
      if (app.selection() < 0) closeSheet(false); else updateProperties();
    } else if (sheetPage === "levels") updateLevels();
    else if (sheetPage === "project") updateProject();
    else if (sheetPage === "workplane") updateWorkplane();
    else if (sheetPage === "menu") pageMenu();
  }

  const group = (title, ...children) => el("div", { class: "group" },
    title ? el("div", { class: "group-title", text: title }) : null,
    el("div", { class: "card" }, ...children));
  const row = ({ ico, label, value, chev = false, danger = false, onclick, testid }) => el("button", {
    class: `row${danger ? " danger" : ""}`, type: "button", onclick, "data-testid": testid,
    html: `<span class="row-ico">${icon(ico, "ico")}</span><span class="row-label"></span>` +
      `<span class="row-value"></span>${chev ? `<span class="chev">${icon("chev")}</span>` : ""}`,
  });
  const setRowText = (r, label, value = "") => {
    r.querySelector(".row-label").textContent = label;
    r.querySelector(".row-value").textContent = value;
    return r;
  };
  const guardAssign = (input, value, prop = "value") => {
    if (document.activeElement === input) return;
    if (input[prop] !== value) input[prop] = value;
  };

  // -- Model tree ------------------------------------------------------------------------
  // Levels first (top story first), each with its nested construction
  // planes, then its elements by category. Tapping an element selects and
  // frames it; tapping a level or plane makes it the active plane.
  const TREE_ICON = {
    floor_plate: '<path d="M2.5 10.5L12 5.5l9.5 5-9.5 5z"/><path d="M2.5 10.5v3l9.5 5 9.5-5v-3"/>',
    wall: '<path d="M3 20V9l5-3v11z"/><path d="M8 17l13-4V4L8 6"/>',
    element: '<path d="M12 3l8 4.5v9L12 21l-8-4.5v-9z"/>',
    pencil: '<path d="M4 20h4L19 9l-4-4L4 16z"/><path d="M13.5 6.5l4 4"/>',
    twist: '<path d="M7 10l5 5 5-5"/>',
    plane: '<path d="M3 15l9-5 9 5-9 5z"/>',
    add: '<path d="M12 5v14M5 12h14"/>',
  };
  /** A small action button inside a tree row. */
  const treeAction = (attr, title, paths) =>
    `<button type="button" class="tree-edit" ${attr} title="${title}" aria-label="${title}">${svg(paths)}</button>`;
  const svg = (paths, cls = "ico sm") => `<svg class="${cls}" viewBox="0 0 24 24">${paths}</svg>`;
  function toggleCollapsed(key) {
    if (treeCollapsed.has(key)) treeCollapsed.delete(key); else treeCollapsed.add(key);
    sessionSave();
  }
  function renderTree(container) {
    let tree;
    try { tree = JSON.parse(app.tree_json()); } catch { return; }
    const scroll = container.scrollTop;
    const nodes = [];
    for (const lvl of tree.levels) {
      const key = `level:${lvl.id}`;
      const collapsed = treeCollapsed.has(key);
      const count = lvl.groups.reduce((n, g) => n + g.items.length, 0);
      const head = el("div", {
        role: "button", tabindex: "0", class: `tree-row level${lvl.active ? " active" : ""}`, "data-tree-level": String(lvl.id),
        title: "Make this the active level",
        html: `<span class="twist${collapsed ? " collapsed" : ""}" data-twist>${svg(TREE_ICON.twist, "ico sm")}</span>` +
          `<i class="swatch"></i><span class="tree-name"></span>` +
          (lvl.active ? `<span class="tree-tag">Active</span>` : "") +
          `<span class="tree-meta">${fmtM(lvl.elevation)}</span>` +
          treeAction("data-tree-add", "Add a workplane in this level", TREE_ICON.add),
      });
      head.querySelector(".swatch").style.background = cssColor(lvl.color);
      head.querySelector(".tree-name").textContent = lvl.name;
      head.addEventListener("click", (e) => {
        if (e.target.closest("[data-twist]")) {
          toggleCollapsed(key);
          renderTree(container);
          return;
        }
        if (e.target.closest("[data-tree-add]")) { addWorkplane(lvl.id); return; }
        activatePlane(lvl.id);
      });
      const block = el("div", { class: "tree-level" }, head);
      if (!collapsed) {
        const addPlanes = (planes, depth) => {
          for (const pl of planes) {
            const r = el("div", {
              role: "button", tabindex: "0", class: `tree-row plane${pl.active ? " active" : ""}`, "data-tree-plane": String(pl.id),
              style: `--depth:${depth}`, title: "Draw on this workplane",
              html: `<i class="swatch plane-swatch"></i><span class="tree-name"></span>` +
                (pl.active ? `<span class="tree-tag">Active</span>` : "") +
                `<span class="tree-meta"></span>` +
                treeAction("data-tree-plane-edit", "Workplane settings", TREE_ICON.pencil),
            });
            r.querySelector(".swatch").style.background = cssColor(pl.color);
            r.querySelector(".tree-name").textContent = pl.name;
            r.querySelector(".tree-meta").textContent = fmtOffset(pl.offset);
            r.addEventListener("click", (e) => {
              if (e.target.closest("[data-tree-plane-edit]")) { openWorkplane(pl.id); return; }
              activatePlane(pl.id);
            });
            block.append(r);
            addPlanes(pl.planes ?? [], depth + 1);
          }
        };
        addPlanes(lvl.planes ?? [], 1);
        if (count === 0) block.append(el("div", { class: "tree-empty", text: "No elements yet" }));
        for (const g of lvl.groups) {
          const gkey = `group:${lvl.id}:${g.key}`;
          const gcollapsed = treeCollapsed.has(gkey);
          const gh = el("button", {
            type: "button", class: "tree-group", "data-tree-group": g.key,
            html: `<span class="twist${gcollapsed ? " collapsed" : ""}">${svg(TREE_ICON.twist, "ico sm")}</span>` +
              `<span></span><span class="count">${g.items.length}</span>`,
          });
          gh.children[1].textContent = g.label;
          gh.addEventListener("click", () => { toggleCollapsed(gkey); renderTree(container); });
          block.append(gh);
          if (gcollapsed) continue;
          for (const it of g.items) {
            const canEdit = it.editable;
            const r = el("div", {
              class: `tree-row item${it.selected ? " selected" : ""}`, role: "button", tabindex: "0",
              "data-tree-item": String(it.id),
              html: `<span class="tree-kind">${svg(TREE_ICON[it.kind] ?? TREE_ICON.element)}</span>` +
                `<span class="tree-name"></span><span class="tree-meta"></span>` +
                (canEdit ? `<button type="button" class="tree-edit" data-tree-edit title="Edit shape" aria-label="Edit shape">${svg(TREE_ICON.pencil)}</button>` : ""),
            });
            r.querySelector(".tree-name").textContent = it.name;
            r.querySelector(".tree-meta").textContent = it.meta;
            r.addEventListener("click", (e) => {
              if (e.target.closest("[data-tree-edit]")) {
                if (sheetPage === "model") closeSheet(false);
                if (it.kind === "wall") beginWallEdit(it.id); else beginEdit(it.id);
                return;
              }
              pickFromTree(it.id);
            });
            block.append(r);
          }
        }
      }
      nodes.push(block);
    }
    if (tree.levels.length === 0) nodes.push(el("div", { class: "tree-empty", text: "No levels — add one in Menu → Levels" }));
    container.replaceChildren(...nodes);
    container.scrollTop = scroll;
  }
  function renderTreePanel() {
    const panel = $("tree-panel");
    // Hidden in the focused flows (Edit Mode, a wall's elevation for windows).
    const show = treeOpen && innerWidth >= TREE_DEFAULT_OPEN_MIN_PX && !inEditChrome();
    panel.hidden = !show;
    $("tree-btn").classList.toggle("on", show || sheetPage === "model");
    $("tree-btn").setAttribute("aria-pressed", String(show || sheetPage === "model"));
    if (show) renderTree($("tree-body"));
  }
  /** Tell the app which canvas margins the page's panels cover (the
   *  bars, docks, the Model panel, the edit panel, the sheet), so a fit
   *  frames the free part of the view. A panel counts on the edge it is
   *  attached to. */
  function updateViewInsets() {
    const r = canvas.getBoundingClientRect();
    const k = canvas.width / r.width;
    const ins = { left: 0, top: 0, right: 0, bottom: 0 }; // CSS px
    // (The small Fit / View pill sits in a corner: it does not count.)
    for (const id of ["topbar", "edit-bar", "toolbar", "edit-dock", "tree-panel", "edit-panel", "sheet", "drawbar"]) {
      const node = $(id);
      // (Fixed panels have no offsetParent: ask the layout directly.)
      if (!node || node.hidden || getComputedStyle(node).display === "none") continue;
      const b = node.getBoundingClientRect();
      if (b.width === 0 || b.height === 0) continue;
      // A wide bar belongs to the top or bottom, a tall one to a side, a
      // small panel to its nearest edge.
      const wide = b.width >= r.width * 0.5, tall = b.height >= r.height * 0.5;
      if (wide && tall) continue;
      const gap = { left: b.left - r.left, right: r.right - b.right, top: b.top - r.top, bottom: r.bottom - b.bottom };
      const edge = wide ? (gap.top < gap.bottom ? "top" : "bottom")
        : tall ? (gap.left < gap.right ? "left" : "right")
        : Object.keys(gap).reduce((a, e) => (gap[e] < gap[a] ? e : a));
      const cover = { left: b.right - r.left, right: r.right - b.left, top: b.bottom - r.top, bottom: r.bottom - b.top }[edge];
      ins[edge] = Math.max(ins[edge], cover);
    }
    app.set_view_insets(ins.left * k, ins.top * k, ins.right * k, ins.bottom * k);
  }
  /** Fit the view to the focus (the selection, the element edited, or
   *  the model), clear of the panels. */
  function fitView() {
    updateViewInsets();
    app.zoom_fit();
  }

  /** Select an element from the tree: select, frame, show properties. */
  function pickFromTree(id) {
    if (sheetPage === "model") closeSheet(false);
    updateViewInsets();
    app.frame_element(id);
    selectElement(id);
    renderLevelChip();
    renderTreePanel();
    sessionSave();
  }
  function activatePlane(id) {
    app.set_active_plane(id);
    stats = JSON.parse(app.stats_json());
    renderLevelChip();
    renderChrome();
    renderTreePanel();
    if (sheetPage === "model") renderTree(sheetBody);
    requestRender();
    sessionSave();
  }
  $("tree-btn").addEventListener("click", () => {
    if (innerWidth >= TREE_DEFAULT_OPEN_MIN_PX) {
      treeOpen = !treeOpen;
      renderTreePanel();
      sessionSave();
    } else if (sheetPage === "model") {
      closeSheet(false);
      renderTreePanel();
    } else {
      openSheet("model", { root: true });
      renderTreePanel();
    }
  });
  $("tree-close").addEventListener("click", () => {
    treeOpen = false;
    renderTreePanel();
    sessionSave();
  });
  window.addEventListener("resize", () => renderTreePanel());

  // -- Menu --------------------------------------------------------------------------
  function pageMenu() {
    levelsState = JSON.parse(app.levels_json());
    const site = JSON.parse(app.site_json());
    const snapSeg = el("div", { class: "seg small" });
    for (const s of SNAP_STEPS) {
      snapSeg.append(el("button", {
        type: "button", class: s === snapStep ? "on" : "", text: `${s}`, "data-snap": String(s),
        onclick: () => {
          snapStep = s;
          app.set_snap(snapEnabled, snapStep);
          sessionSave();
          requestRender();
          pageMenu();
        },
      }));
    }
    sheetBody.replaceChildren(
      group("Project",
        setRowText(row({ ico: "file", label: "", testid: "menu-new", onclick: newProject }), "New project"),
        setRowText(row({ ico: "download", label: "", testid: "menu-export", onclick: exportProject }), "Export", ".vimd file"),
        setRowText(row({ ico: "upload", label: "", testid: "menu-import", onclick: () => $("import-input").click() }), "Import", ".vimd file"),
      ),
      group("Model",
        setRowText(row({ ico: "layers", chev: true, testid: "menu-levels", onclick: () => openSheet("levels") }),
          "Levels", String(levelsState.levels.length)),
        setRowText(row({ ico: "pin", chev: true, testid: "menu-project", onclick: () => openSheet("project") }),
          "Location", site ? `${site.latitude.toFixed(4)}, ${site.longitude.toFixed(4)}` : "not set"),
      ),
      group("Snap step",
        el("div", { class: "field" }, el("span", { class: "field-label", html: `${icon("magnet")} Grid (m)` }), snapSeg),
      ),
      group(null,
        setRowText(row({ ico: "info", chev: true, testid: "menu-about", onclick: () => openSheet("about") }), "About", ""),
      ),
      el("div", { class: "empty-note", text: "Your project is saved in this browser automatically." }),
    );
  }

  async function newProject() {
    const ok = await confirmDialog({
      title: "Start a new project?",
      message: "The current project will be replaced by an empty one. Export it first if you want to keep a copy.",
      ok: "New project", danger: true,
    });
    if (!ok) return;
    app.new_project();
    fitView();
    closeSheet(false);
    refresh();
    saveNow();
    toast("New project started", { kind: "ok" });
  }

  function exportProject() {
    try {
      const bytes = app.save_document();
      const blob = new Blob([bytes], { type: "application/octet-stream" });
      const url = URL.createObjectURL(blob);
      const a = el("a", { href: url, download: `vim-design-${new Date().toISOString().slice(0, 10)}.vimd` });
      document.body.append(a);
      a.click();
      a.remove();
      setTimeout(() => URL.revokeObjectURL(url), 5000);
      toast("Project exported", { kind: "ok" });
    } catch (e) {
      toast(`Export failed: ${e}`, { kind: "error" });
    }
  }

  $("import-input").addEventListener("change", async (e) => {
    const file = e.target.files?.[0];
    e.target.value = "";
    if (!file) return;
    let bytes;
    try {
      bytes = new Uint8Array(await file.arrayBuffer());
    } catch (err) {
      toast(`Could not read the file: ${err}`, { kind: "error" });
      return;
    }
    const problem = app.check_document(bytes);
    if (problem) {
      toast(`Import failed: ${problem}`, { kind: "error", ms: 5000 });
      return;
    }
    const ok = await confirmDialog({
      title: "Replace the current project?",
      message: `"${file.name}" will replace what is on screen now.`,
      ok: "Import", danger: true,
    });
    if (!ok) return;
    const err = app.load_document(bytes);
    if (err) {
      toast(`Import failed: ${err}`, { kind: "error" });
      return;
    }
    fitView();
    closeSheet(false);
    refresh();
    saveNow();
    toast(`Imported ${file.name}`, { kind: "ok" });
  });

  // -- Properties ------------------------------------------------------------------------
  let propsFor = null;
  const KIND_LABEL = { floor_plate: "Floor plate", wall: "Wall", element: "Element" };
  // A numeric property edited by stepper, text field, and slider. One
  // undo step per edit gesture: typing and dragging coalesce until the
  // field commits; each stepper tap is its own step.
  function measureGroup({ title, label, id, value, min, max, step, apply, current }) {
    const input = el("input", { type: "text", inputmode: "decimal", id, value: value.toFixed(2) });
    const slider = el("input", {
      type: "range", min: String(min), max: String(max), step: "0.01", id: `${id}-slider`,
      style: "width:100%;accent-color:var(--accent)",
    });
    slider.value = String(value);
    const set = (v) => {
      if (!Number.isFinite(v)) return;
      apply(v);
      refresh();
    };
    input.addEventListener("input", () => set(parseNum(input.value)));
    input.addEventListener("change", () => { app.end_gesture(); input.value = current().toFixed(2); });
    slider.addEventListener("input", () => set(parseFloat(slider.value)));
    slider.addEventListener("change", () => app.end_gesture());
    const bump = (d) => { set(Math.round((current() + d) * 100) / 100); app.end_gesture(); };
    const stepper = el("div", { class: "stepper" },
      el("button", { type: "button", text: "−", "aria-label": `Decrease ${title.toLowerCase()}`, onclick: () => bump(-step) }),
      input, el("span", { class: "unit", text: "m" }),
      el("button", { type: "button", text: "+", "aria-label": `Increase ${title.toLowerCase()}`, onclick: () => bump(step) }));
    return group(title,
      el("div", { class: "field" }, el("span", { class: "field-label", id: `${id}-label`, text: label }), stepper),
      el("div", { class: "field" }, slider));
  }
  const stat = (k, id, v) =>
    el("div", { class: "stat" }, el("div", { class: "k", text: k }), el("div", { class: "v", id, text: v }));
  const selectedValue = (key, fallback) => JSON.parse(app.selected_json())?.[key] ?? fallback;

  function pageProperties() {
    const e = JSON.parse(app.selected_json());
    if (!e) { closeSheet(false); return; }
    propsFor = e.id;
    $("sheet-title").textContent = e.name;
    const nameInput = el("input", { type: "text", class: "wide", id: "prop-name", value: e.name, autocomplete: "off" });
    nameInput.addEventListener("input", () => { app.set_element_name(e.id, nameInput.value); refresh(); });
    nameInput.addEventListener("change", () => app.end_gesture());
    const kind = KIND_LABEL[e.kind] ?? "Element";
    const children = [
      el("div", { class: "group kind-group" }, el("span", { class: "kind-badge", html: `${icon("cube")} ${kind}` })),
      group(null,
        el("div", { class: "field" }, el("label", { for: "prop-name", text: "Name" }), nameInput),
        el("div", { class: "field" }, el("span", { class: "field-label", text: "Level" }), el("span", { class: "value", id: "prop-level", text: e.levelName ?? "—" })),
      ),
    ];
    const editButton = (note, onclick = () => beginEdit(e.id)) => el("button", {
      type: "button", class: "btn primary block", id: "prop-edit", style: "margin-bottom:10px",
      title: note ?? "Edit the shape: points, edges, faces, holes, thickness",
      html: `<svg class="ico sm" viewBox="0 0 24 24"><path d="M4 20h4L19 9l-4-4L4 16z"/><path d="M13.5 6.5l4 4"/></svg> Edit shape`,
      onclick,
    });
    if (e.kind === "floor_plate" && e.sketch) {
      const solids = e.faces.filter((f) => f.kind === "solid").length;
      children.push(
        el("div", { class: "stat-grid" },
          stat("Faces", "prop-face-count", String(e.faceCount)),
          stat("Top area", "prop-area", fmtArea(e.area)),
        ),
        el("div", { class: "empty-note", id: "prop-face-note", style: "padding:0 4px 10px;text-align:left",
          text: `${solids} solid · ${e.faceCount - solids} void — thickness is set per face in Edit shape.` }),
        editButton(),
      );
    } else if (e.kind === "floor_plate") {
      children.push(editButton("Converts this plate to an editable shape, then opens Edit Mode"));
      children.push(
        measureGroup({
          title: "Thickness", label: "Below level", id: "prop-thickness", value: e.thickness,
          min: 0.05, max: 1.0, step: PLATE_THICKNESS_STEP_M,
          apply: (v) => app.set_plate_thickness(e.id, v), current: () => selectedValue("thickness", 0.3),
        }),
        el("div", { class: "stat-grid" }, stat("Net area", "prop-area", fmtArea(e.area)), stat("Holes", "prop-hole-count", String(e.holes.length))),
        el("div", { class: "group" }, el("div", { class: "group-title", text: "Holes" }), el("div", { class: "card", id: "prop-holes" })),
      );
    } else if (e.kind === "wall") {
      const legacy = e.legacy === true;
      children.push(editButton(
        legacy ? "Edit the wall's shape (converts this wall from an earlier version)" : "Edit the wall's shape: openings, doors, top anchors",
        () => beginWallEdit(e.id),
      ));
      if (legacy) {
        children.push(el("div", { class: "empty-note", id: "prop-legacy-note", text: "Drawn with an earlier version. Edit shape converts it (windows become openings)." }));
      } else {
        children.push(...wallHeightModeGroup(e));
      }
      const heightGroup = measureGroup({
        title: "Height", label: "Above level", id: "prop-height", value: e.height,
        min: 0.5, max: 6.0, step: WALL_HEIGHT_STEP_M,
        apply: (v) => app.set_wall_height(e.id, v), current: () => selectedValue("height", 2.7),
      });
      // Up to: the plane sets the height, so the fixed stepper hides.
      heightGroup.hidden = e.mode === "upto";
      heightGroup.id = "prop-height-group";
      const openings = legacy ? e.windows.length : e.openings;
      children.push(
        heightGroup,
        measureGroup({
          title: "Thickness", label: "Into the wall", id: "prop-wall-thickness", value: e.thickness,
          min: 0.05, max: 0.6, step: WALL_THICKNESS_STEP_M,
          apply: (v) => app.set_wall_thickness(e.id, v), current: () => selectedValue("thickness", 0.2),
        }),
        el("div", { class: "stat-grid" }, stat("Length", "prop-length", fmtM(e.length)), stat(legacy ? "Windows" : "Openings", "prop-window-count", String(openings))),
        el("div", { class: "group" }, el("div", { class: "group-title", text: legacy ? "Windows" : "Openings" }), el("div", { class: "card", id: "prop-windows" })),
        el("div", { class: "btn-row", style: "margin-bottom:10px" },
          el("button", {
            type: "button", class: "btn", id: "prop-add-window",
            html: `${icon("plus")} Window`, onclick: () => beginOpenings("window"),
          }),
          el("button", {
            type: "button", class: "btn", id: "prop-add-door",
            html: `${icon("plus")} Door`, onclick: () => beginOpenings("door"),
          })),
      );
    }
    children.push(el("button", {
      type: "button", class: "btn subtle-danger block", id: "prop-delete",
      html: `${icon("trash")} Delete ${kind.toLowerCase()}`,
      onclick: () => deleteElement(e.id),
    }));
    sheetBody.replaceChildren(...children);
    updateProperties();
  }

  // -- Wall height mode --------------------------------------------------------------------
  // Fixed: the height stepper. Up to: a plane picker (levels and their
  // workplanes) plus an offset from that plane; the height follows it.
  // Read from the document (the wall's top slot and top offset).
  function planeOptions(select, planes, selected) {
    select.replaceChildren();
    for (const p of planes) {
      // Phones: the path only (the elevation is in the title).
      const text = innerWidth < TREE_DEFAULT_OPEN_MIN_PX ? p.path : `${p.path} · ${fmtM(p.elevation)}`;
      const o = el("option", { value: String(p.id), text, title: `${p.path} · ${fmtM(p.elevation)}` });
      if (p.id === selected) o.selected = true;
      select.append(o);
    }
  }
  /** The first plane above `elevation`, else the highest. */
  function planeAbove(planes, elevation) {
    const above = planes.filter((p) => p.elevation > elevation + 1e-6);
    return (above.length ? above[above.length - 1] : planes[0])?.id ?? null;
  }
  const WALL_TOP_REFUSED = "The wall top must be above its base and its openings: pick a higher plane";
  function wallHeightModeGroup(e) {
    const planes = JSON.parse(app.wall_settings_json()).planes;
    const upto = e.mode === "upto";
    const setTop = (plane, offset) => {
      if (app.set_wall_top(e.id, plane, offset) < 0) toast(WALL_TOP_REFUSED, { kind: "error" });
      refresh();
    };
    const seg = el("div", { class: "seg small", id: "prop-wall-mode" });
    for (const [m, label] of [["fixed", "Fixed"], ["upto", "Up to"]]) {
      seg.append(el("button", {
        type: "button", text: label, "data-wall-mode": m, class: e.mode === m ? "on" : "",
        onclick: () => {
          if (m === e.mode) return;
          if (m === "fixed") setTop(-1, 0);
          else setTop(planeAbove(planes, e.baseElevation) ?? -1, 0);
          app.end_gesture();
          pageProperties();
        },
      }));
    }
    const rows = [el("div", { class: "field" }, el("span", { class: "field-label", text: "Height" }), seg)];
    const groups = [];
    if (upto) {
      const picker = el("select", { class: "plane-picker", id: "prop-wall-top", "aria-label": "Wall top plane" });
      planeOptions(picker, planes, e.topPlane);
      picker.addEventListener("change", () => { setTop(Number(picker.value), e.topOffset); app.end_gesture(); });
      rows.push(
        el("div", { class: "field" }, el("span", { class: "field-label", text: "Top at" }), picker),
        el("div", { class: "field" }, el("span", { class: "field-label", text: "Wall height" }),
          el("span", { class: "value", id: "prop-wall-effective", text: fmtM(e.height) })),
      );
      // The offset: stepper presses are one step each, a slider drag or
      // typing coalesces into one.
      groups.push(measureGroup({
        title: "Top offset", label: "From the top plane", id: "prop-wall-offset", value: e.topOffset,
        min: -WALL_TOP_OFFSET_RANGE_M, max: WALL_TOP_OFFSET_RANGE_M, step: WALL_TOP_OFFSET_STEP_M,
        apply: (v) => {
          const plane = JSON.parse(app.selected_json())?.topPlane ?? e.topPlane;
          if (app.set_wall_top(e.id, plane, v) < 0) toast(WALL_TOP_REFUSED, { kind: "error" });
        },
        current: () => selectedValue("topOffset", 0),
      }));
    }
    const g = group("Height mode", ...rows);
    g.id = "prop-wall-height-mode";
    g.classList.toggle("upto", upto);
    return [g, ...groups];
  }

  /** Area of a (u, v) polygon. */
  const polyArea = (pts) => Math.abs(pts.reduce((a, p, i) => {
    const q = pts[(i + 1) % pts.length];
    return a + p[0] * q[1] - q[0] * p[1];
  }, 0)) / 2;
  /** What a wall's void face is: a door crosses the bottom edge, a niche
   *  has a depth, anything else is a window. */
  const openingNoun = (f) => {
    if (Math.min(...f.outline.map((p) => p[1])) < OPENING_BOTTOM_EPS_M) return "Door";
    return f.depth == null ? "Window" : "Niche";
  };
  /** A wall's openings (its profile's void faces), each deletable. */
  function renderWallOpenings(list, e) {
    list.replaceChildren();
    const voids = e.faces.filter((f) => f.kind === "void");
    if (voids.length === 0) list.append(el("div", { class: "empty-note", text: "No openings yet — add a window or a door." }));
    const counts = {};
    for (const f of voids) {
      const noun = openingNoun(f);
      counts[noun] = (counts[noun] ?? 0) + 1;
      const label = `${noun} ${counts[noun]}`;
      list.append(el("div", { class: "hole-row" },
        el("span", { class: "hole-name", text: label }),
        el("span", { class: "hole-area", text: fmtArea(polyArea(f.outline)) }),
        el("button", {
          type: "button", class: "icon-btn ghost", "aria-label": `Delete ${label.toLowerCase()}`,
          "data-testid": "delete-opening", html: icon("trash", "ico"), style: "color:var(--danger)",
          onclick: () => {
            if (app.delete_opening(e.id, f.id)) {
              refresh();
              toast(`${label} removed`);
            }
          },
        }),
      ));
    }
  }

  /** Rows of a plate's holes or a wall's windows, each deletable. */
  function renderOpenings(list, e, items, noun, testid, emptyText) {
    list.replaceChildren();
    if (items.length === 0) list.append(el("div", { class: "empty-note", text: emptyText }));
    for (const h of items) {
      list.append(el("div", { class: "hole-row" },
        el("span", { class: "hole-name", text: `${noun} ${h.index}` }),
        el("span", { class: "hole-area", text: fmtArea(h.area) }),
        el("button", {
          type: "button", class: "icon-btn ghost", "aria-label": `Delete ${noun.toLowerCase()} ${h.index}`,
          "data-testid": testid, html: icon("trash", "ico"), style: "color:var(--danger)",
          onclick: () => {
            if (app.delete_hole(e.id, h.wire)) {
              refresh();
              toast(`${noun} ${h.index} removed`);
            }
          },
        }),
      ));
    }
  }

  function updateProperties() {
    const e = JSON.parse(app.selected_json());
    if (!e) { closeSheet(false); return; }
    if (e.id !== propsFor) { pageProperties(); return; }
    $("sheet-title").textContent = e.name;
    const name = $("prop-name");
    if (name) guardAssign(name, e.name);
    if ($("prop-level")) $("prop-level").textContent = e.levelName ?? "—";
    if (e.kind === "floor_plate" && e.sketch) {
      $("prop-face-count").textContent = String(e.faceCount);
      $("prop-area").textContent = fmtArea(e.area);
    } else if (e.kind === "floor_plate") {
      guardAssign($("prop-thickness"), e.thickness.toFixed(2));
      guardAssign($("prop-thickness-slider"), String(e.thickness));
      $("prop-area").textContent = fmtArea(e.area);
      $("prop-hole-count").textContent = String(e.holes.length);
      renderOpenings($("prop-holes"), e, e.holes, "Hole", "delete-hole", "No holes yet — use the Hole tool to cut one.");
    } else if (e.kind === "wall") {
      // A mode change (undo, another view) re-renders the group.
      const modeGroup = $("prop-wall-height-mode");
      if (modeGroup && modeGroup.classList.contains("upto") !== (e.mode === "upto")) { pageProperties(); return; }
      guardAssign($("prop-height"), e.height.toFixed(2));
      guardAssign($("prop-height-slider"), String(e.height));
      $("prop-height-group").hidden = e.mode === "upto";
      if ($("prop-wall-effective")) $("prop-wall-effective").textContent = fmtM(e.height);
      if ($("prop-wall-offset")) {
        guardAssign($("prop-wall-offset"), e.topOffset.toFixed(2));
        guardAssign($("prop-wall-offset-slider"), String(e.topOffset));
      }
      if ($("prop-wall-top")) guardAssign($("prop-wall-top"), String(e.topPlane));
      const openings = e.legacy ? e.windows.length : e.openings;
      $("prop-height-label").textContent = openings ? `Above level (min ${fmtM(e.minHeight)})` : "Above level";
      guardAssign($("prop-wall-thickness"), e.thickness.toFixed(2));
      guardAssign($("prop-wall-thickness-slider"), String(e.thickness));
      $("prop-length").textContent = fmtM(e.length);
      $("prop-window-count").textContent = String(openings);
      if (e.legacy) renderOpenings($("prop-windows"), e, e.windows, "Window", "delete-window", "No windows.");
      else renderWallOpenings($("prop-windows"), e);
    }
  }

  // -- Workplanes ----------------------------------------------------------------------------
  // Construction planes nested in a level (a ceiling, a sill plane). The
  // tree and Menu → Levels add them; their sheet edits name, offset,
  // color, and deletes (with the cascade prompt when they hold things).
  let workplaneFor = null;
  function addWorkplane(parent) {
    const id = app.add_workplane(parent);
    if (id < 0) { toast("Could not add a workplane here", { kind: "error" }); return; }
    refresh();
    activatePlane(id);
    openWorkplane(id);
  }
  function openWorkplane(id) {
    workplaneFor = id;
    openSheet("workplane", { root: sheetPage !== "levels" });
  }
  const WORKPLANE_CASCADE_WORDING = (w) => {
    const c = w.contents;
    const parts = [];
    if (c.elements) parts.push(`${c.elements} element${c.elements === 1 ? "" : "s"} drawn on it`);
    if (c.workplanes) parts.push(`${c.workplanes} nested workplane${c.workplanes === 1 ? "" : "s"}`);
    let text = `Workplane "${w.name}" has ${parts.join(" and ") || "dependents"}.\n\nDeleting it also deletes them, and all of their geometry.`;
    if (c.toppedWalls) {
      text += `\n\n${c.toppedWalls} wall${c.toppedWalls === 1 ? " reaches" : "s reach"} up to it: ${c.toppedWalls === 1 ? "it keeps its" : "they keep their"} current height.`;
    }
    return `${text}\n\nA single Undo restores everything.`;
  };
  async function deleteWorkplane(id) {
    const w = JSON.parse(app.workplane_json(id));
    if (!w) return;
    const result = app.delete_workplane(id);
    if (result === "deleted") {
      if (sheetPage === "workplane") closeSheet(false);
      refresh();
      toast(`Workplane "${w.name}" deleted`);
    } else if (result === "has_dependents") {
      const ok = await confirmDialog({
        title: `Delete workplane "${w.name}"?`,
        message: WORKPLANE_CASCADE_WORDING(w),
        ok: "Delete workplane and contents", danger: true,
      });
      if (ok && app.delete_workplane_cascade(id)) {
        if (sheetPage === "workplane") closeSheet(false);
        refresh();
        toast(`Workplane "${w.name}" and its contents deleted`);
      }
    } else {
      toast(`Could not delete the workplane (${result})`, { kind: "error" });
    }
  }
  function pageWorkplane() {
    const w = JSON.parse(app.workplane_json(workplaneFor ?? -1));
    if (!w) { closeSheet(false); return; }
    const id = w.id;
    $("sheet-title").textContent = w.name;
    const nameInput = el("input", { type: "text", class: "wide", id: "wp-name", value: w.name, autocomplete: "off" });
    nameInput.addEventListener("input", () => { app.update_workplane_name(id, nameInput.value); refresh(); });
    nameInput.addEventListener("change", () => app.end_gesture());
    const color = el("input", { type: "color", id: "wp-color", "aria-label": "Workplane color", value: floatToHex(w.color) });
    color.addEventListener("input", () => {
      const [r, g, b] = hexToFloat(color.value);
      app.update_workplane_color(id, r, g, b);
      refresh();
    });
    color.addEventListener("change", () => app.end_gesture());
    sheetBody.replaceChildren(
      el("div", { class: "group kind-group" }, el("span", { class: "kind-badge", html: `${svg(TREE_ICON.plane)} Workplane` })),
      group(null,
        el("div", { class: "field" }, el("label", { for: "wp-name", text: "Name" }), nameInput),
        el("div", { class: "field" }, el("span", { class: "field-label", text: "In" }), el("span", { class: "value", id: "wp-parent", text: w.path })),
        el("div", { class: "field" }, el("span", { class: "field-label", text: "Elevation" }), el("span", { class: "value", id: "wp-elevation", text: fmtM(w.elevation) })),
        el("div", { class: "field" }, el("label", { for: "wp-color", text: "Color" }), color),
      ),
      measureGroup({
        title: "Offset", label: `Above ${w.parentName}`, id: "wp-offset", value: w.offset,
        min: -WORKPLANE_OFFSET_RANGE_M, max: WORKPLANE_OFFSET_RANGE_M, step: WORKPLANE_OFFSET_STEP_M,
        apply: (v) => app.update_workplane_offset(id, v),
        current: () => JSON.parse(app.workplane_json(id))?.offset ?? 0,
      }),
      el("div", { class: "btn-row", style: "margin-bottom:10px" },
        el("button", {
          type: "button", class: "btn", id: "wp-activate", html: `${svg(TREE_ICON.plane)} Draw on it`,
          onclick: () => { activatePlane(id); closeSheet(false); },
        }),
        el("button", {
          type: "button", class: "btn", id: "wp-add-nested", html: `${icon("plus")} Workplane inside`,
          onclick: () => addWorkplane(id),
        })),
      el("button", {
        type: "button", class: "btn subtle-danger block", id: "wp-delete",
        html: `${icon("trash")} Delete workplane`, onclick: () => deleteWorkplane(id),
      }),
    );
    updateWorkplane();
  }
  function updateWorkplane() {
    const w = JSON.parse(app.workplane_json(workplaneFor ?? -1));
    if (!w) { closeSheet(false); return; }
    if (!$("wp-name")) return;
    $("sheet-title").textContent = w.name;
    guardAssign($("wp-name"), w.name);
    guardAssign($("wp-color"), floatToHex(w.color));
    guardAssign($("wp-offset"), w.offset.toFixed(2));
    guardAssign($("wp-offset-slider"), String(w.offset));
    $("wp-parent").textContent = w.path;
    $("wp-elevation").textContent = fmtM(w.elevation);
    $("wp-activate").classList.toggle("on", w.active);
  }

  // -- Levels --------------------------------------------------------------------------------
  const CASCADE_WORDING = (name, count, planes = 0) =>
    `Level "${name}" has ${count} element${count === 1 ? "" : "s"}` +
    (planes ? ` and ${planes} workplane${planes === 1 ? "" : "s"}` : "") + ".\n\n" +
    `Deleting it also deletes every element associated with it${planes ? ", its workplanes," : ""} and all of their geometry.\n\n` +
    "A single Undo restores everything.";
  function buildLevelRow(lvl) {
    const r = el("div", { class: "level-row", "data-id": String(lvl.id) });
    r.innerHTML =
      `<div class="level-line">` +
      `<input type="radio" name="active-level" class="lvl-active" aria-label="Make active" />` +
      `<input type="color" class="lvl-color" aria-label="Level color" />` +
      `<input type="text" class="lvl-name" aria-label="Level name" />` +
      `<button type="button" class="icon-btn lvl-delete" aria-label="Delete level">${icon("trash", "ico")}</button>` +
      `</div><div class="level-line">` +
      `<span class="lvl-meta"></span>` +
      `<label class="story"><input type="checkbox" class="lvl-story" />Story</label>` +
      `<input type="text" inputmode="decimal" class="lvl-elev" aria-label="Elevation (m)" /><span class="lvl-unit">m</span>` +
      `</div><div class="lvl-planes"></div>` +
      `<button type="button" class="lvl-add-plane link-btn">${icon("plus")} Workplane</button>`;
    const q = (s) => r.querySelector(s);
    q(".lvl-active").addEventListener("change", () => setActiveLevel(lvl.id));
    q(".lvl-color").addEventListener("input", (e) => {
      const [cr, cg, cb] = hexToFloat(e.target.value);
      app.update_level_color(lvl.id, cr, cg, cb);
      refresh();
    });
    q(".lvl-color").addEventListener("change", () => app.end_gesture());
    q(".lvl-name").addEventListener("input", (e) => { app.update_level_name(lvl.id, e.target.value); refresh(); });
    q(".lvl-name").addEventListener("change", () => app.end_gesture());
    q(".lvl-elev").addEventListener("input", (e) => {
      const v = parseNum(e.target.value);
      if (Number.isFinite(v)) { app.update_level_elevation(lvl.id, v); refresh(); }
    });
    // Re-sort on commit only, so the list never reorders mid-edit.
    q(".lvl-elev").addEventListener("change", () => { app.end_gesture(); updateLevels(true); });
    q(".lvl-add-plane").addEventListener("click", () => addWorkplane(lvl.id));
    q(".lvl-story").addEventListener("change", (e) => { app.update_level_story(lvl.id, e.target.checked); app.end_gesture(); refresh(); });
    q(".lvl-delete").addEventListener("click", async () => {
      const name = q(".lvl-name").value;
      const result = app.delete_level(lvl.id);
      if (result === "deleted") {
        refresh();
        toast(`Level "${name}" deleted`);
      } else if (result === "has_dependents") {
        const info = JSON.parse(app.levels_json()).levels.find((l) => l.id === lvl.id);
        const ok = await confirmDialog({
          title: `Delete level "${name}"?`,
          message: CASCADE_WORDING(name, info?.elements ?? 0,
            JSON.parse(app.workplanes_json()).filter((w) => w.root === lvl.id).length),
          ok: "Delete level and elements", danger: true,
        });
        if (ok && app.delete_level_cascade(lvl.id)) {
          refresh();
          toast(`Level "${name}" and its elements deleted`);
        }
      } else {
        toast(`Could not delete the level (${result})`, { kind: "error" });
      }
    });
    return r;
  }
  function pageLevels() {
    const list = el("div", { class: "card", id: "level-list" });
    sheetBody.replaceChildren(
      el("div", { id: "no-levels-hint", class: "hint-box", hidden: true, text: "There are no levels. Add one to start drawing — every element belongs to a level." }),
      el("div", { class: "group" }, el("div", { class: "group-title", text: "Top to bottom · you draw on the active level" }), list),
      el("button", {
        type: "button", class: "btn block", id: "add-level", html: `${icon("plus")} Add level`,
        onclick: () => { app.add_level(); refresh(); },
      }),
    );
    updateLevels(true);
  }
  function updateLevels(reorder = false) {
    const list = $("level-list");
    if (!list) return;
    levelsState = JSON.parse(app.levels_json());
    const workplanes = JSON.parse(app.workplanes_json());
    $("no-levels-hint").hidden = levelsState.levels.length > 0;
    const desired = [...levelsState.levels].reverse();
    const focused = list.contains(document.activeElement) ? document.activeElement : null;
    const rows = new Map([...list.children].map((r) => [r.dataset.id, r]));
    for (const [id, r] of rows) {
      if (!desired.some((l) => String(l.id) === id)) { r.remove(); rows.delete(id); }
    }
    for (const l of desired) if (!rows.has(String(l.id))) rows.set(String(l.id), buildLevelRow(l));
    for (const l of desired) {
      const r = rows.get(String(l.id));
      if (!r.isConnected || (reorder && !focused) || !focused) list.append(r);
    }
    for (const l of desired) {
      const r = rows.get(String(l.id));
      const active = levelsState.activeId === l.id;
      r.classList.toggle("active", active);
      guardAssign(r.querySelector(".lvl-active"), active, "checked");
      guardAssign(r.querySelector(".lvl-color"), floatToHex(l.color));
      guardAssign(r.querySelector(".lvl-name"), l.name);
      guardAssign(r.querySelector(".lvl-elev"), l.elevation.toFixed(2));
      guardAssign(r.querySelector(".lvl-story"), l.isStory, "checked");
      r.querySelector(".lvl-meta").textContent =
        `${l.elements} element${l.elements === 1 ? "" : "s"}${active ? " · active" : ""}`;
      renderLevelPlanes(r.querySelector(".lvl-planes"), workplanes, l.id, 0);
    }
  }
  /** A level's workplanes in Menu → Levels (nested, each opens its sheet). */
  function renderLevelPlanes(box, all, parent, depth) {
    if (depth === 0) box.replaceChildren();
    for (const w of all.filter((x) => x.parent === parent)) {
      const b = el("button", {
        type: "button", class: `wp-row${w.active ? " active" : ""}`, "data-workplane": String(w.id),
        style: `--depth:${depth}`,
        html: `<i class="swatch"></i><span class="wp-name"></span><span class="wp-offset">${fmtOffset(w.offset)}</span>${icon("chev", "ico sm")}`,
      });
      b.querySelector(".swatch").style.background = cssColor(w.color);
      b.querySelector(".wp-name").textContent = w.name;
      b.addEventListener("click", () => openWorkplane(w.id));
      box.append(b);
      renderLevelPlanes(box, all, w.id, depth + 1);
    }
  }

  // -- Project (site) ------------------------------------------------------------------------
  function pageProject() {
    const field = (id, label) => el("div", { class: "field" },
      el("label", { for: id, text: label }),
      el("input", { type: "text", inputmode: "decimal", id, autocomplete: "off" }));
    sheetBody.replaceChildren(
      group("Site", field("site-lat", "Latitude (°)"), field("site-lon", "Longitude (°)"), field("site-elev", "Elevation (m)")),
      el("div", { class: "empty-note", text: "The model is built in local meters around one origin; the site only places that origin on Earth." }),
    );
    const ids = ["site-lat", "site-lon", "site-elev"];
    for (const id of ids) {
      $(id).addEventListener("input", () => {
        const [lat, lon, elev] = ids.map((i) => parseNum($(i).value));
        if ([lat, lon, elev].every(Number.isFinite)) { app.set_site(lat, lon, elev); refresh(); }
      });
      $(id).addEventListener("change", () => { app.end_gesture(); updateProject(true); });
    }
    updateProject(true);
  }
  function updateProject(force = false) {
    const site = JSON.parse(app.site_json());
    if (!site || !$("site-lat")) return;
    const set = (id, v) => { if (force && document.activeElement !== $(id)) $(id).value = String(v); else guardAssign($(id), String(v)); };
    set("site-lat", site.latitude);
    set("site-lon", site.longitude);
    set("site-elev", site.elevation);
  }

  // -- About -------------------------------------------------------------------------------
  let version = null;
  fetch("./version.json", { cache: "no-store" })
    .then((r) => (r.ok ? r.json() : null))
    .then((v) => { version = v; if (sheetPage === "about") pageAbout(); })
    .catch(() => { version = null; });
  const debugInfo = () => ({
    version: version ?? "dev build (no version.json)",
    backend: stats.backend,
    msaa: stats.msaa,
    userAgent: navigator.userAgent,
    viewport: { w: innerWidth, h: innerHeight, canvas: [canvas.width, canvas.height] },
    dpr: window.devicePixelRatio,
    coarsePointer: COARSE,
    crossOriginIsolated: globalThis.crossOriginIsolated === true,
    threads: mod.threads_supported?.() ?? false,
    wasmLoadMs: Math.round(wasmMs),
    storageBytes: store.usageBytes(),
    stats,
    document: JSON.parse(app.debug_json()),
    lastErrors: errorLog.slice(-15),
  });
  function pageAbout() {
    const v = version;
    const build = v ? `${v.short}${v.dirty ? "+dirty" : ""} · ${v.ref ?? ""}` : "dev build";
    const built = v?.built_at ? new Date(v.built_at).toLocaleString() : "—";
    const kb = (store.usageBytes() / 1024).toFixed(1);
    const fieldRow = (k, val, id) => el("div", { class: "field" }, el("span", { class: "field-label", text: k }), el("span", { class: "value", id, text: val }));
    sheetBody.replaceChildren(
      group("Build",
        fieldRow("Version", build, "about-version"),
        fieldRow("Built", built),
        fieldRow("Renderer", `${stats.backend}${stats.msaa > 1 ? ` · ${stats.msaa}× MSAA` : ""}`, "about-backend"),
        fieldRow("Saved project", `${kb} KB in this browser`, "about-storage"),
      ),
      el("button", {
        type: "button", class: "btn primary block", id: "copy-debug", html: `${icon("copy")} Copy debug info`,
        onclick: async () => {
          const text = JSON.stringify(debugInfo(), null, 2);
          let ok = false;
          try { await navigator.clipboard.writeText(text); ok = true; } catch { /* fallback below */ }
          if (!ok) {
            const ta = el("textarea", { style: "position:fixed;opacity:0" });
            ta.value = text;
            document.body.append(ta);
            ta.select();
            try { ok = document.execCommand("copy"); } catch { ok = false; }
            ta.remove();
          }
          window.__author.lastDebugInfo = text;
          toast(ok ? "Debug info copied — paste it into your feedback" : "Copy failed — see the text below", { kind: ok ? "ok" : "error" });
          if (!ok) sheetBody.append(el("pre", { class: "about-pre", text }));
        },
      }),
      el("div", { class: "btn-row" },
        el("a", { class: "btn", href: v ? "./demo.html" : "./index.html", style: "display:inline-grid;place-items:center;text-decoration:none", text: "Parametric demo" })),
      el("div", { class: "empty-note", text: "VIM Design — authoring in the browser. No account, no server: your work stays on this device." }),
    );
  }

  // ============================================================================
  // Boot finish
  // ============================================================================
  new ResizeObserver(() => requestRender()).observe(canvas);
  window.addEventListener("orientationchange", () => setTimeout(requestRender, 200));
  window.visualViewport?.addEventListener("resize", () => requestRender());

  window.__author = {
    ready: false,
    error: null,
    app,
    wasmMs,
    storageKey: DOC_KEY,
    stats: () => JSON.parse(app.stats_json()),
    elements: () => JSON.parse(app.elements_json()),
    levels: () => JSON.parse(app.levels_json()),
    site: () => JSON.parse(app.site_json()),
    sketch: () => JSON.parse(app.sketch_json()),
    editState: () => JSON.parse(app.edit_state_json()),
    editProfile: () => JSON.parse(app.edit_profile_json()),
    editHud: () => JSON.parse(app.edit_hud_json()),
    tree: () => JSON.parse(app.tree_json()),
    touchClass: () => lastTouchClass,
    gesture: () => ({ pointers: pointers.size, mode }),
    camera: () => JSON.parse(app.camera_json()),
    wallSettings: () => JSON.parse(app.wall_settings_json()),
    hud: () => JSON.parse(app.hud_json()),
    selected: () => JSON.parse(app.selected_json()),
    toasts: toastLog,
    errors: errorLog,
    saveNow,
    debugInfo,
    refresh,
    // Wall-local (u along the wall from its start, v up from its base)
    // -> world coordinates.
    wallToWorld: (wallId, u, v) => {
      const w = JSON.parse(app.elements_json()).find((e) => e.id === wallId && e.kind === "wall");
      if (!w) return null;
      // The base plane (a level or a workplane) of a wall; legacy walls
      // stand on their level.
      const lvl = JSON.parse(app.levels_json()).levels.find((l) => l.id === w.levelId);
      const base = w.baseElevation ?? lvl?.elevation ?? 0;
      const len = Math.hypot(w.end[0] - w.start[0], w.end[1] - w.start[1]);
      const d = [(w.end[0] - w.start[0]) / len, (w.end[1] - w.start[1]) / len];
      return [w.start[0] + d[0] * u, w.start[1] + d[1] * u, base + w.baseW + v];
    },
    // World -> page CSS pixels (specs compute tap targets from world
    // coordinates instead of hardcoding pixels).
    worldToClient: (x, y, z) => {
      const p = JSON.parse(app.world_to_screen(x, y, z));
      if (!p) return null;
      const r = canvas.getBoundingClientRect();
      return [r.left + p[0] * (r.width / canvas.width), r.top + p[1] * (r.height / canvas.height)];
    },
  };

  stats = JSON.parse(app.stats_json());
  renderLevelChip();
  renderChrome();
  refresh();
  requestAnimationFrame(frame);
}

window.__author = { ready: false, error: null };
main().catch((e) => {
  console.error(e);
  window.__author.error = String(e);
  const loading = $("loading");
  loading.classList.add("error");
  $("loading-status").textContent = `Could not start: ${e?.message ?? e}. Try reloading; if it persists, your browser may not support WebGL2.`;
});
