//! Builds the vendored SVT-AV1 (Tritium) static library and the C shim.

use std::env;
use std::path::PathBuf;

fn main() {
    let src = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("svt-av1");
    println!("cargo:rerun-if-changed=shim/shim.c");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=NASM");
    if !src.join("CMakeLists.txt").exists() {
        panic!(
            "SVT-AV1 sources missing at {}: run `git submodule update --init`",
            src.display()
        );
    }

    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let mut cfg = cmake::Config::new(&src);
    cfg.profile("Release")
        .define("BUILD_SHARED_LIBS", "OFF")
        .define("BUILD_APPS", "OFF")
        .define("BUILD_TESTING", "OFF")
        .define("SVT_AV1_LTO", "OFF")
        .define("EXCLUDE_HASH", "ON")
        .define("CMAKE_INSTALL_LIBDIR", "lib");
    if arch == "aarch64" && target_env == "msvc" {
        // SVT-AV1's Arm SIMD uses GCC-style -march flags and GNU assembly,
        // which cl.exe cannot build.
        cfg.define("COMPILE_C_ONLY", "ON");
    } else if arch == "x86_64" {
        match find_nasm() {
            Some(nasm) => {
                cfg.define("CMAKE_ASM_NASM_COMPILER", nasm);
            }
            None => {
                println!(
                    "cargo:warning=nasm not found (PATH or NASM env var): building SVT-AV1 without SIMD, encoding will be several times slower"
                );
                cfg.define("COMPILE_C_ONLY", "ON");
            }
        }
    }
    let dst = cfg.build();

    let include = dst.join("include").join("svt-av1");
    cc::Build::new()
        .file("shim/shim.c")
        .include(&include)
        .warnings(true)
        .compile("jxlall_svt_shim");

    // After the shim: static linkers resolve left to right.
    println!(
        "cargo:rustc-link-search=native={}",
        dst.join("lib").display()
    );
    println!("cargo:rustc-link-lib=static=SvtAv1Enc");
    match os.as_str() {
        "linux" | "android" => {
            println!("cargo:rustc-link-lib=pthread");
            println!("cargo:rustc-link-lib=m");
        }
        "macos" | "ios" => println!("cargo:rustc-link-lib=pthread"),
        _ => {}
    }
}

/// `NASM` env var, else `nasm` on PATH (plus the winget user install dir).
fn find_nasm() -> Option<PathBuf> {
    if let Some(p) = env::var_os("NASM")
        .map(PathBuf::from)
        .filter(|p| p.exists())
    {
        return Some(p);
    }
    let exe = if cfg!(windows) { "nasm.exe" } else { "nasm" };
    let mut dirs: Vec<PathBuf> = env::var_os("PATH")
        .map(|p| env::split_paths(&p).collect())
        .unwrap_or_default();
    // Windows installers (winget user scope, Chocolatey) do not always
    // update PATH for the current session.
    for (var, sub) in [
        ("LOCALAPPDATA", &["bin", "NASM"][..]),
        ("ProgramFiles", &["NASM"][..]),
    ] {
        if let Some(base) = env::var_os(var) {
            dirs.push(sub.iter().fold(PathBuf::from(base), |p, s| p.join(s)));
        }
    }
    dirs.into_iter().map(|d| d.join(exe)).find(|p| p.is_file())
}
