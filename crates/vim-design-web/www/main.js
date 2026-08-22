// VIM Design — WASM threading probe driver.
// Works with both wasm builds: threaded (pkg built with --features threads,
// exports initThreadPool) and single-threaded fallback (no initThreadPool).

const N = 20_000_000; // elements in the probe workload
const SPEEDUP_THRESHOLD = 1.5; // parallel must be materially faster

const $ = (id) => document.getElementById(id);
const status = (t) => { $("status").textContent = t; };

function row(name, value) {
  const tr = document.createElement("tr");
  const td0 = document.createElement("td");
  const td1 = document.createElement("td");
  td0.textContent = name;
  td1.textContent = String(value);
  tr.append(td0, td1);
  $("results").append(tr);
}

function bestOf(runs, fn) {
  let best = Infinity;
  let result;
  for (let i = 0; i < runs; i++) {
    const t0 = performance.now();
    result = fn();
    best = Math.min(best, performance.now() - t0);
  }
  return { ms: best, result };
}

async function main() {
  status("loading wasm module…");
  const mod = await import("./pkg/vim_design_web.js");
  await mod.default(); // wasm-bindgen init

  const hw = navigator.hardwareConcurrency ?? 1;
  const isolated = globalThis.crossOriginIsolated === true;
  const builtWithThreads = mod.threads_supported();

  let poolThreads = 1;
  let poolError = "";
  if (builtWithThreads && isolated) {
    status(`initializing thread pool (${hw} workers)…`);
    try {
      await mod.initThreadPool(hw);
      poolThreads = mod.pool_threads();
    } catch (e) {
      poolError = `initThreadPool failed: ${e}`;
    }
  }

  mod.paint_canvas("view");

  status("running single-threaded workload…");
  await new Promise((r) => setTimeout(r, 0)); // let the UI update
  const seq = bestOf(2, () => mod.sum_sequential(N));

  status("running parallel workload…");
  await new Promise((r) => setTimeout(r, 0));
  const par = bestOf(2, () => mod.sum_parallel(N));
  const workers = mod.last_parallel_worker_count();

  const sumsMatch = Math.abs(seq.result - par.result) < 1e-6 * Math.abs(seq.result);
  const speedup = seq.ms / par.ms;
  const pass =
    builtWithThreads &&
    isolated &&
    poolThreads > 1 &&
    workers >= 2 &&
    speedup >= SPEEDUP_THRESHOLD &&
    sumsMatch;

  row("vim-design-lib version", mod.lib_version());
  row("kernel probe", mod.kernel_probe());
  row("navigator.hardwareConcurrency", hw);
  row("crossOriginIsolated (SharedArrayBuffer available)", isolated);
  row("wasm built with threads feature", builtWithThreads);
  row("rayon pool threads", poolThreads);
  row("workers observed in parallel run", workers);
  row(`single-threaded time (n=${N.toLocaleString()})`, `${seq.ms.toFixed(1)} ms`);
  row(`parallel time (n=${N.toLocaleString()})`, `${par.ms.toFixed(1)} ms`);
  row("speedup", `${speedup.toFixed(2)}x`);
  row("results identical", sumsMatch);
  if (poolError) row("thread pool error", poolError);

  const verdictText = pass
    ? `PASS — true parallelism achieved (${speedup.toFixed(2)}x on ${workers} workers)`
    : `FAIL — no true parallelism (threads=${builtWithThreads}, isolated=${isolated}, pool=${poolThreads}, workers=${workers}, speedup=${speedup.toFixed(2)}x)`;

  const v = $("verdict");
  v.textContent = verdictText;
  v.classList.add(pass ? "pass" : "fail");
  v.dataset.verdict = pass ? "PASS" : "FAIL";

  window.__probeResult = {
    verdict: pass ? "PASS" : "FAIL",
    hardwareConcurrency: hw,
    crossOriginIsolated: isolated,
    builtWithThreads,
    poolThreads,
    workersObserved: workers,
    sequentialMs: seq.ms,
    parallelMs: par.ms,
    speedup,
    sumsMatch,
    poolError,
  };
  status("done");
}

main().catch((e) => {
  status(`error: ${e}`);
  const v = $("verdict");
  v.textContent = `FAIL — probe crashed: ${e}`;
  v.classList.add("fail");
  v.dataset.verdict = "FAIL";
  window.__probeResult = { verdict: "FAIL", error: String(e) };
});
