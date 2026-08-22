//! Generates include/vim_design.h from the extern "C" surface via cbindgen.

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=cbindgen.toml");

    let crate_dir = match env::var("CARGO_MANIFEST_DIR") {
        Ok(dir) => PathBuf::from(dir),
        Err(e) => {
            println!("cargo:warning=CARGO_MANIFEST_DIR unavailable: {e}");
            return;
        }
    };
    let out_path = crate_dir.join("include").join("vim_design.h");

    match cbindgen::generate(&crate_dir) {
        Ok(bindings) => {
            bindings.write_to_file(&out_path);
        }
        Err(e) => {
            // Fail the build: a stale/missing header would silently break
            // the C++ consumers. (exit(1) rather than panic! — the crate's
            // clippy::panic denial applies to the build script too.)
            eprintln!("cbindgen failed to generate {}: {e}", out_path.display());
            std::process::exit(1);
        }
    }
}
