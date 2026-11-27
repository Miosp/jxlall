//! Builds the vendored SVT-AV1 (Tritium) static library and the C shim.
//!
//! Release builds can opt into an optimized SVT-AV1 (about 10% faster
//! encodes) with two environment variables:
//!
//! - `JXLALL_SVT_LLVM`: an LLVM `bin` directory. SVT-AV1 is then compiled to
//!   bitcode by that clang (clang-cl for MSVC targets) and the bitcode is
//!   linked and optimized as one module into a single native object, so any
//!   final linker gets whole-library LTO.
//! - `JXLALL_SVT_PGO`: `generate:<dir>` instruments SVT-AV1 so that a run of
//!   jxlall writes raw profiles to `<dir>`; `use:<file>` optimizes with the
//!   `llvm-profdata merge` of them. Needs `JXLALL_SVT_LLVM`.

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const LLVM_ENV: &str = "JXLALL_SVT_LLVM";
const PGO_ENV: &str = "JXLALL_SVT_PGO";
/// Leading bytes of LLVM bitcode: raw, and in the wrapper used for Mach-O.
const BITCODE_MAGICS: [&[u8]; 2] = [b"BC\xC0\xDE", b"\xDE\xC0\x17\x0B"];

enum Pgo {
    Generate(PathBuf),
    Use(PathBuf),
}

/// LLVM toolchain building SVT-AV1 when `JXLALL_SVT_LLVM` is set.
struct Llvm {
    bin: PathBuf,
    msvc: bool,
}

impl Llvm {
    fn tool(&self, name: &str) -> PathBuf {
        self.bin
            .join(name)
            .with_extension(env::consts::EXE_EXTENSION)
    }

    /// The C compiler cmake drives.
    fn cc(&self) -> PathBuf {
        self.tool(if self.msvc { "clang-cl" } else { "clang" })
    }

    /// A clang driver flag in the form the cmake compiler accepts.
    fn flag(&self, f: &str) -> String {
        if self.msvc {
            format!("/clang:{f}")
        } else {
            f.to_owned()
        }
    }
}

fn main() {
    let src = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("svt-av1");
    println!("cargo:rerun-if-changed=shim/shim.c");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=NASM");
    println!("cargo:rerun-if-env-changed={LLVM_ENV}");
    println!("cargo:rerun-if-env-changed={PGO_ENV}");
    if !src.join("CMakeLists.txt").exists() {
        panic!(
            "SVT-AV1 sources missing at {}: run `git submodule update --init`",
            src.display()
        );
    }

    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let msvc = target_env == "msvc";
    let llvm = env::var_os(LLVM_ENV)
        .filter(|v| !v.is_empty())
        .map(|bin| Llvm {
            bin: PathBuf::from(bin),
            msvc,
        });
    let pgo = pgo_mode();
    if pgo.is_some() && llvm.is_none() {
        panic!("{PGO_ENV} needs {LLVM_ENV}");
    }

    // Its own CMake build dir for the LLVM build: switching generators
    // (Visual Studio vs Ninja) in one dir fails.
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let cmake_out = if llvm.is_some() {
        out_dir.join("llvm")
    } else {
        out_dir
    };
    let mut cfg = cmake::Config::new(&src);
    cfg.out_dir(&cmake_out)
        .profile("Release")
        .define("BUILD_SHARED_LIBS", "OFF")
        .define("BUILD_APPS", "OFF")
        .define("BUILD_TESTING", "OFF")
        .define("SVT_AV1_LTO", "OFF")
        .define("EXCLUDE_HASH", "ON")
        .define("CMAKE_INSTALL_LIBDIR", "lib")
        // SVT-AV1 otherwise writes the library into its source tree, shared
        // by every build (target, profile, LLVM or not).
        .define("CMAKE_OUTPUT_DIRECTORY", cmake_out.join("bin"));
    if msvc {
        // SVT-AV1's CMake (policy CMP0091) picks the C runtime from this
        // variable and ignores the /MT flag the cmake crate adds, so a
        // `+crt-static` build would mix runtimes and fail to link.
        let crt_static = env::var("CARGO_CFG_TARGET_FEATURE")
            .unwrap_or_default()
            .split(',')
            .any(|f| f == "crt-static");
        cfg.define(
            "CMAKE_MSVC_RUNTIME_LIBRARY",
            if crt_static {
                "MultiThreaded"
            } else {
                "MultiThreadedDLL"
            },
        );
    }
    if let Some(llvm) = &llvm {
        configure_llvm(&mut cfg, llvm, pgo.as_ref());
    }
    if arch == "aarch64" && msvc {
        match &llvm {
            // CMake finds no assembler for clang-cl on its own, and its
            // MSVC-platform ASM rules need clang-cl's command line.
            Some(llvm) => {
                cfg.define("CMAKE_ASM_COMPILER", llvm.cc());
            }
            // SVT-AV1's Arm SIMD uses GCC-style -march flags and GNU
            // assembly, which cl.exe cannot build.
            None => {
                cfg.define("COMPILE_C_ONLY", "ON");
            }
        }
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
    let lib_dir = dst.join("lib");
    if let Some(llvm) = &llvm
        && !matches!(pgo, Some(Pgo::Generate(_)))
    {
        let lib = lib_dir.join(if msvc {
            "SvtAv1Enc.lib"
        } else {
            "libSvtAv1Enc.a"
        });
        prelink(llvm, &lib, &os);
    }

    let include = dst.join("include").join("svt-av1");
    let mut shim = cc::Build::new();
    shim.file("shim/shim.c").include(&include).warnings(true);
    if matches!(pgo, Some(Pgo::Generate(_))) {
        // Pulls in the profile runtime, which writes the profiles at exit.
        shim.define("JXLALL_SVT_PGO_GENERATE", None);
    }
    shim.compile("jxlall_svt_shim");

    // After the shim: static linkers resolve left to right.
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=SvtAv1Enc");
    if let (Some(llvm), Some(Pgo::Generate(_))) = (&llvm, &pgo) {
        link_profile_runtime(llvm, &arch);
    }
    match os.as_str() {
        "linux" | "android" => {
            println!("cargo:rustc-link-lib=pthread");
            println!("cargo:rustc-link-lib=m");
        }
        "macos" | "ios" => println!("cargo:rustc-link-lib=pthread"),
        _ => {}
    }
}

fn pgo_mode() -> Option<Pgo> {
    let v = env::var(PGO_ENV).ok().filter(|v| !v.is_empty())?;
    let (mode, path) = v
        .split_once(':')
        .unwrap_or_else(|| panic!("{PGO_ENV} must be generate:<dir> or use:<file>"));
    let path = PathBuf::from(path);
    match mode {
        "generate" => Some(Pgo::Generate(path)),
        "use" => {
            println!("cargo:rerun-if-changed={}", path.display());
            Some(Pgo::Use(path))
        }
        _ => panic!("{PGO_ENV} must be generate:<dir> or use:<file>"),
    }
}

fn configure_llvm(cfg: &mut cmake::Config, llvm: &Llvm, pgo: Option<&Pgo>) {
    let cc = llvm.cc();
    cfg.define("CMAKE_C_COMPILER", &cc)
        .define("CMAKE_CXX_COMPILER", &cc);
    // Only LLVM's archivers index bitcode (Apple's ar and lib.exe do not).
    if llvm.msvc {
        // The Visual Studio generators ignore CMAKE_C_COMPILER.
        cfg.generator("Ninja")
            .define("CMAKE_AR", llvm.tool("llvm-lib"));
    } else {
        cfg.define("CMAKE_AR", llvm.tool("llvm-ar"))
            .define("CMAKE_RANLIB", llvm.tool("llvm-ranlib"));
    }
    let mut flags: Vec<String> = match pgo {
        // Instrumented builds skip LTO: they only collect profiles.
        Some(Pgo::Generate(dir)) => vec![format!("-fprofile-generate={}", dir.display())],
        Some(Pgo::Use(file)) => vec![
            "-flto=thin".into(),
            format!("-fprofile-use={}", file.display()),
            "-Wno-profile-instr-unprofiled".into(),
            "-Wno-profile-instr-out-of-date".into(),
        ],
        None => vec!["-flto=thin".into()],
    };
    if llvm.msvc {
        // SVT-AV1 adds this GCC-style for identical output across
        // platforms; clang-cl ignores it without the /clang: prefix.
        flags.push("-ffp-contract=off".into());
    }
    for f in flags {
        let f = llvm.flag(&f);
        cfg.cflag(&f).cxxflag(&f);
    }
}

/// Replaces the bitcode members of the static library `lib` with one native
/// object built from all of them, keeping the assembly objects.
fn prelink(llvm: &Llvm, lib: &Path, os: &str) {
    let work = PathBuf::from(env::var("OUT_DIR").unwrap()).join("prelink");
    let members = work.join("members");
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&members).unwrap();
    run(Command::new(llvm.tool("llvm-ar"))
        .arg("x")
        .arg(lib)
        .current_dir(&members));
    let (bitcode, native): (Vec<PathBuf>, Vec<PathBuf>) = fs::read_dir(&members)
        .unwrap()
        .map(|e| e.unwrap().path())
        .partition(|p| {
            let head = fs::read(p).unwrap();
            BITCODE_MAGICS.iter().any(|m| head.starts_with(m))
        });
    // Extraction keeps only file names: same-named members overwrite.
    let listed = Command::new(llvm.tool("llvm-ar"))
        .arg("t")
        .arg(lib)
        .output()
        .expect("run llvm-ar t");
    let member_count = String::from_utf8_lossy(&listed.stdout).lines().count();
    assert_eq!(
        member_count,
        bitcode.len() + native.len(),
        "{}: members with the same name",
        lib.display()
    );
    if bitcode.is_empty() {
        panic!("{}: no bitcode to prelink", lib.display());
    }

    let merged_bc = work.join("svt.bc");
    // A response file: hundreds of object paths overflow Windows' command
    // line. Quoted forward-slash paths read the same under GNU and Windows
    // tokenizing.
    let rsp = work.join("link.rsp");
    let quoted: Vec<String> = bitcode
        .iter()
        .map(|p| format!("\"{}\"", p.display().to_string().replace('\\', "/")))
        .collect();
    fs::write(&rsp, quoted.join("\n")).unwrap();
    let mut rsp_arg = std::ffi::OsString::from("@");
    rsp_arg.push(&rsp);
    run(Command::new(llvm.tool("llvm-link"))
        .arg(rsp_arg)
        .arg("-o")
        .arg(&merged_bc));
    let merged = work.join(if llvm.msvc { "svt.obj" } else { "svt.o" });
    let mut compile = Command::new(llvm.tool("clang"));
    compile
        .arg(format!("--target={}", env::var("TARGET").unwrap()))
        .args(["-c", "-O3", "-w", "-ffunction-sections", "-fdata-sections"])
        .arg(&merged_bc)
        .arg("-o")
        .arg(&merged);
    if !llvm.msvc {
        compile.arg("-fPIC");
    }
    run(&mut compile);

    fs::remove_file(lib).unwrap();
    let objects = std::iter::once(&merged).chain(&native);
    if llvm.msvc {
        let mut out = std::ffi::OsString::from("/out:");
        out.push(lib);
        run(Command::new(llvm.tool("llvm-lib")).arg(out).args(objects));
    } else {
        let format = if os == "macos" || os == "ios" {
            "--format=darwin"
        } else {
            "--format=gnu"
        };
        run(Command::new(llvm.tool("llvm-ar"))
            .arg(format)
            .arg("rcs")
            .arg(lib)
            .args(objects));
    }
}

/// Links clang's profile runtime, which instrumented SVT-AV1 calls into.
fn link_profile_runtime(llvm: &Llvm, arch: &str) {
    let mut print = Command::new(llvm.tool("clang"));
    // Native builds keep clang's own triple: distributions (Alpine) name
    // their runtime dirs after it, not after Rust's.
    let target = env::var("TARGET").unwrap();
    if env::var("HOST").unwrap() != target {
        print.arg(format!("--target={target}"));
    }
    let out = print
        .arg("-print-runtime-dir")
        .output()
        .expect("run clang -print-runtime-dir");
    let dir = PathBuf::from(String::from_utf8(out.stdout).unwrap().trim());
    // Per-target runtime dirs drop the arch suffix; older layouts keep it,
    // and macOS has one universal `_osx` runtime.
    let suffix_arch = if dir.ends_with("darwin") { "osx" } else { arch };
    let lib = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("clang runtime dir {}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| matches!(p.extension().and_then(OsStr::to_str), Some("a" | "lib")))
        .filter_map(|p| {
            let stem = p.file_stem()?.to_str()?;
            let name = stem.strip_prefix("lib").unwrap_or(stem);
            let suffix = name.strip_prefix("clang_rt.profile")?;
            (suffix.is_empty() || suffix.contains(suffix_arch)).then(|| name.to_owned())
        })
        .min_by_key(String::len)
        .unwrap_or_else(|| panic!("no clang profile runtime in {}", dir.display()));
    println!("cargo:rustc-link-search=native={}", dir.display());
    println!("cargo:rustc-link-lib=static={lib}");
}

fn run(cmd: &mut Command) {
    let status = cmd.status().unwrap_or_else(|e| panic!("{cmd:?}: {e}"));
    assert!(status.success(), "{cmd:?} failed: {status}");
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
