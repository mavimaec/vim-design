//! VimDesignWeb — WASM threading probe + minimal canvas placeholder.
//!
//! The point of this skeleton is to de-risk Rust multithreading in the
//! browser (docs/ARCHITECTURE.md §6.2 and Open Question 5). Rendering is a
//! deliberate placeholder: a 2D canvas fill driven from Rust. The wgpu
//! renderer comes later.
//!
//! Two build configurations (see crate README.md and devops/vbuild.ps1):
//! - `--features threads`: nightly + atomics + wasm-bindgen-rayon. JS must
//!   call `initThreadPool(n)` after `init()`.
//! - default: single-threaded fallback; `sum_parallel` degrades to the
//!   sequential path.

use wasm_bindgen::prelude::*;

// Re-export so wasm-bindgen emits `initThreadPool` in the JS glue
// (threaded builds only).
#[cfg(feature = "threads")]
pub use wasm_bindgen_rayon::init_thread_pool;

#[cfg(feature = "threads")]
use std::sync::atomic::{AtomicU64, Ordering};

/// Bitmask of rayon worker indices observed during the last parallel run.
#[cfg(feature = "threads")]
static WORKER_MASK: AtomicU64 = AtomicU64::new(0);

/// The compute kernel: deliberately non-trivial floating point work so the
/// probe measures computation, not memory bandwidth.
#[inline]
fn work(i: u32) -> f64 {
    let x = i as f64;
    (x.sqrt() + 1.0).sin().abs()
}

/// Version of the core library compiled into this wasm module.
#[wasm_bindgen]
pub fn lib_version() -> String {
    vim_design_lib::version().to_string()
}

/// Exercises the monstertruck kernel inside the browser (compile/link probe).
#[wasm_bindgen]
pub fn kernel_probe() -> String {
    vim_design_lib::kernel::probe()
}

/// True when this module was built with the `threads` feature.
#[wasm_bindgen]
pub fn threads_supported() -> bool {
    cfg!(feature = "threads")
}

/// Number of rayon worker threads in the initialized pool (1 when built
/// without threads, or before `initThreadPool` resolves).
#[wasm_bindgen]
pub fn pool_threads() -> u32 {
    #[cfg(feature = "threads")]
    {
        rayon::current_num_threads() as u32
    }
    #[cfg(not(feature = "threads"))]
    {
        1
    }
}

/// Single-threaded reference computation: sum of `work(i)` for i in 0..n.
#[wasm_bindgen]
pub fn sum_sequential(n: u32) -> f64 {
    (0..n).map(work).sum()
}

/// Parallel computation of the same sum. In threaded builds this runs on
/// the rayon pool and records which workers participated; in
/// single-threaded builds it falls back to the sequential path.
#[wasm_bindgen]
pub fn sum_parallel(n: u32) -> f64 {
    #[cfg(feature = "threads")]
    {
        use rayon::prelude::*;
        WORKER_MASK.store(0, Ordering::Relaxed);
        let (sum, mask) = (0..n)
            .into_par_iter()
            .fold(
                || (0.0f64, 0u64),
                |(sum, mask), i| {
                    let idx = rayon::current_thread_index().unwrap_or(0).min(63);
                    (sum + work(i), mask | (1u64 << idx))
                },
            )
            .reduce(|| (0.0, 0), |a, b| (a.0 + b.0, a.1 | b.1));
        WORKER_MASK.store(mask, Ordering::Relaxed);
        sum
    }
    #[cfg(not(feature = "threads"))]
    {
        sum_sequential(n)
    }
}

/// Number of distinct rayon workers that did work during the most recent
/// `sum_parallel` call (1 in single-threaded builds).
#[wasm_bindgen]
pub fn last_parallel_worker_count() -> u32 {
    #[cfg(feature = "threads")]
    {
        WORKER_MASK.load(Ordering::Relaxed).count_ones().max(1)
    }
    #[cfg(not(feature = "threads"))]
    {
        1
    }
}

/// Rendering placeholder: clear the canvas to the VIM Design blue.
/// (wgpu renderer intentionally deferred — see crate docs.)
#[wasm_bindgen]
pub fn paint_canvas(canvas_id: &str) -> Result<(), JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let document = window
        .document()
        .ok_or_else(|| JsValue::from_str("no document"))?;
    let canvas = document
        .get_element_by_id(canvas_id)
        .ok_or_else(|| JsValue::from_str("canvas not found"))?
        .dyn_into::<web_sys::HtmlCanvasElement>()?;
    let ctx = canvas
        .get_context("2d")?
        .ok_or_else(|| JsValue::from_str("no 2d context"))?
        .dyn_into::<web_sys::CanvasRenderingContext2d>()?;

    let (w, h) = (canvas.width() as f64, canvas.height() as f64);
    ctx.set_fill_style_str("#1c4587");
    ctx.fill_rect(0.0, 0.0, w, h);
    ctx.set_fill_style_str("#e8f0fe");
    ctx.set_font("16px sans-serif");
    ctx.fill_text("VIM Design — wasm canvas placeholder", 16.0, 32.0)?;
    Ok(())
}
