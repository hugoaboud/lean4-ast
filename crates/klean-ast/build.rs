//! Links this crate against the static library `build.py` produces.
//!
//! `build.py` writes `lean4-ast/build/link.json` after it has built the wrapper
//! library. Everything version- or path-specific lives there: the archive, the
//! toolchain's include and library directories, the flags `leanc` itself uses,
//! and the mangled name of the wrapper's module initializer. This script only
//! translates that file into cargo directives, so an upstream rename or a new
//! toolchain location never needs a code change here.

use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let default = manifest.join("../../build/link.json");
    let link_file = std::env::var_os("KLEAN_AST_LINK")
        .map(PathBuf::from)
        .unwrap_or(default);

    println!("cargo:rerun-if-env-changed=KLEAN_AST_LINK");
    println!("cargo:rerun-if-changed={}", link_file.display());

    let text = std::fs::read_to_string(&link_file).unwrap_or_else(|error| {
        panic!(
            "cannot read {}: {error}\n\n\
             Build the Lean parser library first:\n    python3 lean4-ast/build.py",
            link_file.display()
        )
    });
    let link: serde_json::Value =
        serde_json::from_str(&text).expect("lean4-ast: link.json is not valid JSON");

    let static_lib = link
        .get("static_lib")
        .and_then(|value| value.as_str())
        .expect("lean4-ast: link.json has no `static_lib`");
    if !Path::new(static_lib).is_file() {
        panic!("lean4-ast: {static_lib} is missing; run `python3 lean4-ast/build.py`");
    }

    // The parser is statically linked against *one* Lean: its oleans must be
    // read by a matching toolchain, so the version it was built with is baked
    // into the crate.
    let version = link
        .get("lean_semver")
        .and_then(|value| value.as_str())
        .or_else(|| link.get("tag").and_then(|value| value.as_str()))
        .unwrap_or("unknown");
    println!("cargo:rustc-env=KLEAN_AST_LEAN_VERSION={version}");
    let toolchain = link
        .get("toolchain")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    println!("cargo:rustc-env=KLEAN_AST_TOOLCHAIN={toolchain}");

    // Lean's own flags, in Lean's own order: the archives have to be repeated
    // inside the same groups `leanc` uses, or the static link fails.
    println!("cargo:rustc-link-arg={static_lib}");
    // The interpreter resolves compiled symbols through the dynamic table, which
    // is what `supportInterpreter := true` sets up for a Lake executable.
    println!("cargo:rustc-link-arg=-rdynamic");
    if let Some(flags) = link.get("ldflags").and_then(|value| value.as_array()) {
        for flag in flags {
            if let Some(flag) = flag.as_str() {
                println!("cargo:rustc-link-arg={flag}");
            }
        }
    }
}
