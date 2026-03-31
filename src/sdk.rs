//! Visual Studio and Windows SDK path auto-detection + build helpers.
//!
//! Used from consumer crate `build.rs` scripts to compile the generic APO C++
//! skeleton and link against the Windows Audio Processing Object libraries.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Resolved paths to MSVC toolchain and Windows SDK components.
pub struct SdkPaths {
    /// Windows SDK Include root (e.g. `.../Include/10.0.26100.0`)
    pub sdk_include: PathBuf,
    /// Windows SDK Lib um/x64 (e.g. `.../Lib/10.0.26100.0/um/x64`)
    pub sdk_lib: PathBuf,
    /// MSVC include (e.g. `.../MSVC/14.44.35207/include`)
    pub msvc_include: PathBuf,
    /// MSVC lib/x64
    pub msvc_lib: PathBuf,
    /// ATL/MFC include
    pub atl_include: PathBuf,
    /// ATL/MFC lib/x64
    pub atl_lib: PathBuf,
}

/// Path to our shipped generic C++ skeleton files.
///
/// This is resolved at compile time of the `windows-apo` crate itself,
/// so it always points to the correct directory regardless of where the
/// consumer crate lives.
const CPP_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/cpp");

/// Auto-detect Visual Studio and Windows SDK installation paths.
///
/// Detection strategy:
/// 1. Run `vswhere.exe` to find the VS installation root
/// 2. Glob for the latest MSVC toolchain version
/// 3. Scan `Program Files (x86)/Windows Kits/10/` for the latest SDK version
pub fn detect_sdk_paths() -> Result<SdkPaths, String> {
    let vs_root = find_vs_installation()?;
    let (msvc_include, msvc_lib, atl_include, atl_lib) = find_msvc_paths(&vs_root)?;
    let (sdk_include, sdk_lib) = find_windows_sdk()?;

    Ok(SdkPaths {
        sdk_include,
        sdk_lib,
        msvc_include,
        msvc_lib,
        atl_include,
        atl_lib,
    })
}

/// Configure a `cc::Build` with all necessary includes and defines for APO compilation.
pub fn configure_cc_build(build: &mut cc::Build, paths: &SdkPaths) {
    build
        .include(&paths.msvc_include)
        .include(&paths.atl_include)
        .include(paths.sdk_include.join("um"))
        .include(paths.sdk_include.join("shared"))
        .include(paths.sdk_include.join("ucrt"))
        .define("UNICODE", None)
        .define("_UNICODE", None)
        .flag("/EHsc");
}

/// Emit cargo link directives for APO dependencies.
pub fn emit_link_directives(paths: &SdkPaths) {
    println!(
        "cargo:rustc-link-search=native={}",
        paths.sdk_lib.display()
    );
    println!(
        "cargo:rustc-link-search=native={}",
        paths.atl_lib.display()
    );
    println!(
        "cargo:rustc-link-search=native={}",
        paths.msvc_lib.display()
    );
    println!("cargo:rustc-link-lib=static=audiobaseprocessingobject");
    println!("cargo:rustc-link-lib=static=audiomediatypecrt");
    println!("cargo:rustc-link-lib=dylib=ole32");
    println!("cargo:rustc-link-lib=dylib=oleaut32");
    println!("cargo:rustc-link-lib=dylib=uuid");
    println!("cargo:rustc-link-lib=dylib=avrt");
    println!("cargo:rustc-link-lib=static=legacy_stdio_definitions");
}

/// High-level build helper: compiles the generic APO C++ skeleton plus any
/// consumer-specific C++ files, and emits all necessary link directives.
///
/// Call this from your consumer crate's `build.rs`.
///
/// # Arguments
/// * `extra_cpp_files` — Additional C++ files to compile (consumer-specific DSP, etc.)
/// * `extra_includes` — Additional include directories (e.g. for consumer's `guids.h`)
pub fn build_apo(extra_cpp_files: &[&str], extra_includes: &[&str]) {
    let paths = detect_sdk_paths().expect("Failed to detect Visual Studio / Windows SDK paths");

    let cpp_dir = CPP_DIR.replace('\\', "/");

    let mut build = cc::Build::new();
    build.cpp(true);
    configure_cc_build(&mut build, &paths);

    // Compile generic APO skeleton
    build.file(format!("{}/GenericApo.cpp", cpp_dir));
    build.file(format!("{}/ClassFactory.cpp", cpp_dir));
    build.file(format!("{}/DllMain.cpp", cpp_dir));
    build.include(&cpp_dir);

    // Compile consumer-specific files
    for f in extra_cpp_files {
        build.file(f);
    }
    for inc in extra_includes {
        build.include(inc);
    }

    build.compile("apo_cpp");
    emit_link_directives(&paths);
}

// ── Internal helpers ──────────────────────────────────────────────────

/// Find VS installation root via vswhere.exe.
fn find_vs_installation() -> Result<PathBuf, String> {
    // vswhere.exe is shipped with VS 2017+ at a well-known location
    let vswhere_paths = [
        "C:/Program Files (x86)/Microsoft Visual Studio/Installer/vswhere.exe",
        "C:/Program Files/Microsoft Visual Studio/Installer/vswhere.exe",
    ];

    for vswhere in &vswhere_paths {
        if Path::new(vswhere).exists() {
            let output = Command::new(vswhere)
                .args([
                    "-latest",
                    "-products",
                    "*",
                    "-requires",
                    "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
                    "-property",
                    "installationPath",
                    "-format",
                    "value",
                ])
                .output()
                .map_err(|e| format!("Failed to run vswhere: {}", e))?;

            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !path.is_empty() {
                    return Ok(PathBuf::from(path));
                }
            }
        }
    }

    // Fallback: scan common installation directories
    let candidates = [
        "C:/Program Files/Microsoft Visual Studio/2022/Community",
        "C:/Program Files/Microsoft Visual Studio/2022/Professional",
        "C:/Program Files/Microsoft Visual Studio/2022/Enterprise",
        "C:/Program Files (x86)/Microsoft Visual Studio/2022/BuildTools",
        "C:/Program Files/Microsoft Visual Studio/2022/BuildTools",
    ];

    for candidate in &candidates {
        let vc_tools = format!("{}/VC/Tools/MSVC", candidate);
        if Path::new(&vc_tools).exists() {
            return Ok(PathBuf::from(candidate));
        }
    }

    Err("Could not find Visual Studio installation. Install VS 2022 with C++ build tools.".into())
}

/// Find MSVC toolchain paths (include, lib, ATL) under a VS installation.
fn find_msvc_paths(vs_root: &Path) -> Result<(PathBuf, PathBuf, PathBuf, PathBuf), String> {
    let msvc_root = vs_root.join("VC/Tools/MSVC");
    if !msvc_root.exists() {
        return Err(format!("MSVC tools not found at {}", msvc_root.display()));
    }

    // Find the latest MSVC version by sorting directory names
    let latest_msvc = find_latest_version_dir(&msvc_root)?;

    let include = latest_msvc.join("include");
    let lib = latest_msvc.join("lib/x64");
    let atl_include = latest_msvc.join("atlmfc/include");
    let atl_lib = latest_msvc.join("atlmfc/lib/x64");

    if !include.exists() {
        return Err(format!("MSVC include not found at {}", include.display()));
    }
    if !lib.exists() {
        return Err(format!("MSVC lib/x64 not found at {}", lib.display()));
    }

    Ok((include, lib, atl_include, atl_lib))
}

/// Find Windows SDK include and lib paths.
fn find_windows_sdk() -> Result<(PathBuf, PathBuf), String> {
    let sdk_root_candidates = [
        "C:/Program Files (x86)/Windows Kits/10",
        "C:/Program Files/Windows Kits/10",
    ];

    for root in &sdk_root_candidates {
        let include_root = format!("{}/Include", root);
        if !Path::new(&include_root).exists() {
            continue;
        }

        let latest = find_latest_version_dir(Path::new(&include_root))?;
        let version = latest
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();

        let sdk_include = PathBuf::from(format!("{}/Include/{}", root, version));
        let sdk_lib = PathBuf::from(format!("{}/Lib/{}/um/x64", root, version));

        if sdk_include.join("um").exists() && sdk_lib.exists() {
            return Ok((sdk_include, sdk_lib));
        }
    }

    Err("Could not find Windows SDK 10. Install the Windows 10/11 SDK.".into())
}

/// Find the latest version directory by sorting entries as version numbers.
fn find_latest_version_dir(parent: &Path) -> Result<PathBuf, String> {
    let mut versions: Vec<_> = std::fs::read_dir(parent)
        .map_err(|e| format!("Cannot read {}: {}", parent.display(), e))?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            if entry.file_type().ok()?.is_dir() {
                Some(entry.path())
            } else {
                None
            }
        })
        .collect();

    if versions.is_empty() {
        return Err(format!("No version directories found in {}", parent.display()));
    }

    // Sort by version components (e.g. 14.44.35207 or 10.0.26100.0)
    versions.sort_by(|a, b| {
        let va = version_tuple(a);
        let vb = version_tuple(b);
        va.cmp(&vb)
    });

    Ok(versions.pop().unwrap())
}

/// Parse a path's filename as a dot-separated version tuple for sorting.
fn version_tuple(path: &Path) -> Vec<u64> {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .split('.')
        .map(|s| s.parse::<u64>().unwrap_or(0))
        .collect()
}
