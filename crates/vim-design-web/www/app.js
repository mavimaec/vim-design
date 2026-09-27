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
          wireframe: app.wireframe(),
          thickness: app.plate_thickness_setting(),
          shape: app.shape(),
          camera: app.camera_json(),
        }));
      }, SESSION_SAVE_DEBOUNCE_MS);
    };
  })();

  let snapEnabled = session.snapEnabled ?? true;
  let snapStep = SNAP_STEPS.includes(session.snapStep) ? session.snapStep : (COARSE ? 0.5 : 0.25);
  app.set_snap(snapEnabled, snapStep);
  if (typeof session.thickness === "number") app.set_plate_thickness_setting(session.thickness);
  if (session.shape === "rect") app.set_shape("rect");
  if (session.wireframe === true) app.set_wireframe(true);

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
  if (restored && session.camera) {
    app.set_camera_json(session.camera);
  } else {
    app.set_view_mode(session.view === "3d" ? "3d" : "plan");
    app.zoom_fit();
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
    if (app.revision() === savedRevision) return;
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
  const isDrawing = () => stats.tool === "plate" || stats.tool === "hole";

  const pointers = new Map(); // id -> {x, y, type} (client px)
  let mode = "none"; // none | press | nav | pan | place | pinch | pinch-rest
  let press = null; // {x, y, dev, type, button, moved, anchored}
  let pinch = null; // {d, mid, angle}
  let lastTap = null;
  let hoverType = "mouse";
  let sheetOpenedAt = 0; // see the ghost-click shield on the sheet
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
      mode = "pinch";
      pinch = pinchState();
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
      }
      return;
    }
    const prev = { x: p.x, y: p.y };
    p.x = e.clientX;
    p.y = e.clientY;
    if (mode === "pinch" && pointers.size >= 2) {
      const now = pinchState();
      if (pinch && now.d > 1) {
        app.zoom_at(pinch.d / now.d, now.mid[0], now.mid[1]);
        app.pan(pinch.mid[0], pinch.mid[1], now.mid[0], now.mid[1]);
        if (stats.view === "3d") {
          let da = now.angle - pinch.angle;
          if (da > Math.PI) da -= 2 * Math.PI;
          if (da < -Math.PI) da += 2 * Math.PI;
          app.orbit(-da / 0.0065, 0);
        }
      }
      pinch = now;
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
    } else if (mode === "press" && press && !cancelled) {
      handleTap(press);
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
    const id = app.pick(p.dev[0], p.dev[1]);
    const isDouble = lastTap && now - lastTap.t < 330 && Math.hypot(p.x - lastTap.x, p.y - lastTap.y) < 30;
    if (id < 0 && isDouble && lastTap.empty) {
      app.zoom_fit();
      lastTap = null;
      sessionSave();
      requestRender();
      return;
    }
    lastTap = { t: now, x: p.x, y: p.y, empty: id < 0 };
    // A touch tap that opens the sheet arms the ghost-click shield.
    if (p.type !== "mouse") sheetOpenedAt = performance.now();
    selectElement(id);
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
    if (!isDrawing()) {
      if (hudActive) ctx.clearRect(0, 0, hud.width, hud.height);
      hudActive = false;
      lastHud = { active: false };
      return;
    }
    let h;
    try { h = JSON.parse(app.hud_json()); } catch { return; }
    lastHud = h;
    hudActive = true;
    ctx.clearRect(0, 0, hud.width, hud.height);
    if (!h.active) return;
    const ok = h.previewOk;
    const color = ok ? HUD.accent : HUD.bad;
    const P = h.preview;
    // Fill.
    if (P.length >= 3) {
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
      if (h.shape === "polygon" && P.length >= 3) {
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
      const kindText = { first: "Close shape", vertex: "Corner", axis: "Aligned" }[c.kind];
      const text = kindText ? `${kindText} · ${c.label}` : c.label;
      if (touchPlacing) {
        pill(ctx, c.x, c.y - TOUCH_READOUT_OFFSET_PX * dpr, text, { bg: "rgba(28,34,48,0.92)", fg: "#fff", border: "rgba(0,0,0,0)" });
      } else {
        pill(ctx, c.x + 18 * dpr + 60 * dpr, c.y - 26 * dpr, text, { bg: "rgba(28,34,48,0.88)", fg: "#fff", border: "rgba(0,0,0,0)" });
      }
    }
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
    chip.querySelector(".chip-name").textContent = lvl ? lvl.name : "No level";
    chip.querySelector(".chip-elev").textContent = lvl ? fmtM(lvl.elevation) : "add one in Levels";
    chip.querySelector(".swatch").style.background = lvl ? cssColor(lvl.color) : "#98a2b3";
  }

  function hintText() {
    if (!stats.canAuthor) return "Add a level to start drawing (Menu → Levels)";
    if (!isDrawing()) return "";
    const h = lastHud.active ? lastHud : JSON.parse(app.hud_json());
    const n = h.count ?? 0;
    const tap = COARSE ? "Tap" : "Click";
    if (stats.tool === "hole" && stats.platesOnLevel === 0) return "Draw a floor plate on this level first";
    const what = stats.tool === "hole" ? "hole" : "floor plate";
    if (stats.shape === "rect") {
      return n === 0 ? `${COARSE ? "Drag" : "Drag"}, or ${tap.toLowerCase()} two opposite corners of the ${what}` : `${tap} the opposite corner`;
    }
    if (n === 0) return `${tap} to place the first corner of the ${what}`;
    if (n < 3) return `${tap} to place the next corner`;
    return `${tap} the first corner or press Finish to close`;
  }

  function renderChrome() {
    $("undo").disabled = !stats.canUndo;
    $("redo").disabled = !stats.canRedo;
    for (const b of document.querySelectorAll("#view-toggle button")) {
      b.classList.toggle("on", b.dataset.view === stats.view);
      b.setAttribute("aria-selected", String(b.dataset.view === stats.view));
    }
    for (const b of document.querySelectorAll(".tool[data-tool]")) {
      b.classList.toggle("on", b.dataset.tool === stats.tool);
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
      $("thickness-stepper").hidden = stats.tool !== "plate";
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
    popover.replaceChildren();
    const desc = [...levelsState.levels].reverse();
    for (const l of desc) {
      const on = l.id === levelsState.activeId;
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
  for (const b of document.querySelectorAll(".tool[data-tool]")) {
    b.addEventListener("click", () => setTool(b.dataset.tool));
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
    app.zoom_fit();
    requestRender();
    sessionSave();
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
      setThickness(app.plate_thickness_setting() + Number(b.dataset.step) * 0.05);
    });
  }
  $("thickness-input").addEventListener("change", (e) => setThickness(parseNum(e.target.value)));

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
    } else if ((e.key === "Delete" || e.key === "Backspace") && app.selection() >= 0) {
      e.preventDefault();
      deleteElement(app.selection());
    }
    if (mod || e.altKey) return;
    const k = e.key.toLowerCase();
    if (k === "v" || k === "s") setTool("select");
    else if (k === "f") setTool("plate");
    else if (k === "h") setTool("hole");
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
    requestRender();
  }

  /** Phones: frame the selected element in the band between the top
   *  bar and the properties sheet (pan, and zoom out if it is too big). */
  function revealSelection() {
    if (innerWidth >= 760) return;
    const e = JSON.parse(app.selected_json());
    if (!e?.outline?.length) return;
    const lvl = JSON.parse(app.levels_json()).levels.find((l) => l.id === e.levelId);
    const z = lvl ? lvl.elevation : 0;
    const pts = e.outline.map(([x, y]) => JSON.parse(app.world_to_screen(x, y, z))).filter(Boolean);
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
      toast(`Deleted ${name}`, {
        action: { label: "Undo", fn: () => { if (app.undo()) refresh(); } },
      });
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
    properties: "Properties",
  };
  function openSheet(page, { root = false } = {}) {
    if (root) sheetStack = [];
    else if (sheetPage && sheetPage !== page) sheetStack.push(sheetPage);
    sheetPage = page;
    sheet.hidden = false;
    sheet.classList.toggle("compact", page === "properties");
    renderSheet();
  }
  function closeSheet(deselect = true) {
    const was = sheetPage;
    sheet.hidden = true;
    sheetPage = null;
    sheetStack = [];
    sheetBody.replaceChildren();
    if (was === "properties" && deselect && app.selection() >= 0) {
      app.select(-1);
      requestRender();
    }
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
  for (const type of ["mousedown", "click"]) {
    sheet.addEventListener(type, (e) => {
      if (performance.now() - sheetOpenedAt < 450) {
        e.preventDefault();
        e.stopPropagation();
      }
    }, true);
  }

  function renderSheet() {
    if (!sheetPage) return;
    $("sheet-title").textContent = PAGE_TITLES[sheetPage] ?? "";
    $("sheet-back").hidden = sheetStack.length === 0;
    const pages = { menu: pageMenu, levels: pageLevels, project: pageProject, about: pageAbout, properties: pageProperties };
    (pages[sheetPage] ?? (() => {}))();
  }

  /** Repaint document-bound UI: triggered only by the dirty pump. */
  function renderDocPanels() {
    renderLevelChip();
    if (!popover.hidden) openPopover();
    if (sheetPage === "properties") {
      if (app.selection() < 0) closeSheet(false); else updateProperties();
    } else if (sheetPage === "levels") updateLevels();
    else if (sheetPage === "project") updateProject();
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
    const wire = el("label", { class: "switch" }, el("input", { type: "checkbox", id: "wireframe-toggle" }), el("span"));
    wire.querySelector("input").checked = app.wireframe();
    wire.querySelector("input").addEventListener("change", (e) => {
      app.set_wireframe(e.target.checked);
      requestRender();
      sessionSave();
    });
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
      group("Display",
        el("div", { class: "field" }, el("label", { for: "wireframe-toggle", text: "Wireframe" }), wire),
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
    app.zoom_fit();
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
    app.zoom_fit();
    closeSheet(false);
    refresh();
    saveNow();
    toast(`Imported ${file.name}`, { kind: "ok" });
  });

  // -- Properties ------------------------------------------------------------------------
  let propsFor = null;
  function pageProperties() {
    const e = JSON.parse(app.selected_json());
    if (!e) { closeSheet(false); return; }
    propsFor = e.id;
    const isPlate = e.kind === "floor_plate";
    $("sheet-title").textContent = e.name;
    const nameInput = el("input", { type: "text", class: "wide", id: "prop-name", value: e.name, autocomplete: "off" });
    nameInput.addEventListener("input", () => { app.set_element_name(e.id, nameInput.value); refresh(); });
    nameInput.addEventListener("change", () => app.end_gesture());
    const children = [
      el("div", { class: "group kind-group" }, el("span", { class: "kind-badge", html: `${icon("cube")} ${isPlate ? "Floor plate" : "Element"}` })),
      group(null,
        el("div", { class: "field" }, el("label", { for: "prop-name", text: "Name" }), nameInput),
        el("div", { class: "field" }, el("span", { class: "field-label", text: "Level" }), el("span", { class: "value", id: "prop-level", text: e.levelName ?? "—" })),
      ),
    ];
    if (isPlate) {
      const tInput = el("input", { type: "text", inputmode: "decimal", id: "prop-thickness", value: e.thickness.toFixed(2) });
      const slider = el("input", { type: "range", min: "0.05", max: "1.00", step: "0.01", id: "prop-thickness-slider", style: "width:100%;accent-color:var(--accent)" });
      slider.value = String(e.thickness);
      const applyT = (v) => {
        if (!Number.isFinite(v)) return;
        app.set_plate_thickness(e.id, v);
        refresh();
      };
      tInput.addEventListener("input", () => applyT(parseNum(tInput.value)));
      tInput.addEventListener("change", () => { app.end_gesture(); tInput.value = currentThickness().toFixed(2); });
      slider.addEventListener("input", () => applyT(parseFloat(slider.value)));
      slider.addEventListener("change", () => app.end_gesture());
      const step = (d) => { applyT(Math.round((currentThickness() + d) * 100) / 100); app.end_gesture(); };
      const stepper = el("div", { class: "stepper" },
        el("button", { type: "button", text: "−", "aria-label": "Thinner", onclick: () => step(-0.05) }),
        tInput, el("span", { class: "unit", text: "m" }),
        el("button", { type: "button", text: "+", "aria-label": "Thicker", onclick: () => step(0.05) }));
      children.push(
        group("Thickness",
          el("div", { class: "field" }, el("span", { class: "field-label", text: "Below level" }), stepper),
          el("div", { class: "field" }, slider),
        ),
        el("div", { class: "stat-grid" },
          el("div", { class: "stat" }, el("div", { class: "k", text: "Net area" }), el("div", { class: "v", id: "prop-area", text: fmtArea(e.area) })),
          el("div", { class: "stat" }, el("div", { class: "k", text: "Holes" }), el("div", { class: "v", id: "prop-hole-count", text: String(e.holes.length) })),
        ),
        el("div", { class: "group" }, el("div", { class: "group-title", text: "Holes" }), el("div", { class: "card", id: "prop-holes" })),
      );
    }
    children.push(el("button", {
      type: "button", class: "btn subtle-danger block", id: "prop-delete",
      html: `${icon("trash")} Delete ${isPlate ? "floor plate" : "element"}`,
      onclick: () => deleteElement(e.id),
    }));
    sheetBody.replaceChildren(...children);
    updateProperties();
  }
  const currentThickness = () => JSON.parse(app.selected_json())?.thickness ?? 0.3;
  function updateProperties() {
    const e = JSON.parse(app.selected_json());
    if (!e) { closeSheet(false); return; }
    if (e.id !== propsFor) { pageProperties(); return; }
    $("sheet-title").textContent = e.name;
    const name = $("prop-name");
    if (name) guardAssign(name, e.name);
    if ($("prop-level")) $("prop-level").textContent = e.levelName ?? "—";
    if (e.kind !== "floor_plate") return;
    guardAssign($("prop-thickness"), e.thickness.toFixed(2));
    guardAssign($("prop-thickness-slider"), String(e.thickness));
    $("prop-area").textContent = fmtArea(e.area);
    $("prop-hole-count").textContent = String(e.holes.length);
    const list = $("prop-holes");
    list.replaceChildren();
    if (e.holes.length === 0) {
      list.append(el("div", { class: "empty-note", text: "No holes yet — use the Hole tool to cut one." }));
    }
    for (const h of e.holes) {
      list.append(el("div", { class: "hole-row" },
        el("span", { class: "hole-name", text: `Hole ${h.index}` }),
        el("span", { class: "hole-area", text: fmtArea(h.area) }),
        el("button", {
          type: "button", class: "icon-btn ghost", "aria-label": `Delete hole ${h.index}`,
          "data-testid": "delete-hole", html: icon("trash", "ico"), style: "color:var(--danger)",
          onclick: () => {
            if (app.delete_hole(e.id, h.wire)) {
              refresh();
              toast(`Hole ${h.index} removed`, { action: { label: "Undo", fn: () => { if (app.undo()) refresh(); } } });
            }
          },
        }),
      ));
    }
  }

  // -- Levels --------------------------------------------------------------------------------
  const CASCADE_WORDING = (name, count) =>
    `Level "${name}" has ${count} element${count === 1 ? "" : "s"}.\n\n` +
    "Deleting it also deletes every element associated with it, and all of their geometry.\n\n" +
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
      `</div>`;
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
    q(".lvl-story").addEventListener("change", (e) => { app.update_level_story(lvl.id, e.target.checked); app.end_gesture(); refresh(); });
    q(".lvl-delete").addEventListener("click", async () => {
      const name = q(".lvl-name").value;
      const result = app.delete_level(lvl.id);
      if (result === "deleted") {
        refresh();
        toast(`Level "${name}" deleted`, { action: { label: "Undo", fn: () => { if (app.undo()) refresh(); } } });
      } else if (result === "has_dependents") {
        const info = JSON.parse(app.levels_json()).levels.find((l) => l.id === lvl.id);
        const ok = await confirmDialog({
          title: `Delete level "${name}"?`,
          message: CASCADE_WORDING(name, info?.elements ?? 0),
          ok: "Delete level and elements", danger: true,
        });
        if (ok && app.delete_level_cascade(lvl.id)) {
          refresh();
          toast(`Level "${name}" and its elements deleted`, { action: { label: "Undo", fn: () => { if (app.undo()) refresh(); } } });
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
    hud: () => JSON.parse(app.hud_json()),
    selected: () => JSON.parse(app.selected_json()),
    toasts: toastLog,
    errors: errorLog,
    saveNow,
    debugInfo,
    refresh,
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
