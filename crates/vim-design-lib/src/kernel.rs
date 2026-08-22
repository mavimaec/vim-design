//! Thin seam over the `monstertruck` geometry kernel (docs/ARCHITECTURE.md §5.1).
//!
//! Kernel types must not leak out of this module. For now it contains a
//! single probe that exercises `monstertruck-modeling` so the build proves
//! the kernel compiles and links on every supported target (native + wasm32).

use monstertruck_modeling::{Point3, builder};

/// Build a trivial kernel object and describe it. Exists only to verify
/// that the monstertruck crates compile and link on this target.
pub fn probe() -> String {
    let point = Point3::new(0.0, 0.0, 1.0);
    let vertex = builder::vertex(point);
    // Debug-format the topological vertex so the kernel code is actually
    // linked in (not optimized away as unused).
    format!("monstertruck vertex created: {:?}", vertex)
}

#[cfg(test)]
mod tests {
    #[test]
    fn kernel_probe_links_monstertruck() {
        let desc = super::probe();
        assert!(desc.contains("monstertruck vertex created"));
    }
}
