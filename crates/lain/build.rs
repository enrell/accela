//! gpui links `-lxkbcommon-x11`, which needs the unversioned `.so` from the -dev
//! package. If only the runtime library (`.so.0`) is installed, link against it via a
//! symlink in OUT_DIR so the build works without root.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let dirs = ["/usr/lib/x86_64-linux-gnu", "/usr/lib64", "/usr/lib", "/usr/lib/aarch64-linux-gnu"];
    if dirs.iter().any(|d| Path::new(d).join("libxkbcommon-x11.so").exists()) {
        return;
    }
    let Some(runtime) = dirs.iter().map(|d| Path::new(d).join("libxkbcommon-x11.so.0")).find(|p| p.exists()) else {
        println!("cargo:warning=libxkbcommon-x11 not found; install libxkbcommon-x11-dev");
        return;
    };
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let link = out.join("libxkbcommon-x11.so");
    let _ = std::fs::remove_file(&link);
    if std::os::unix::fs::symlink(&runtime, &link).is_ok() {
        println!("cargo:rustc-link-search=native={}", out.display());
    }
}
