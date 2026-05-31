// Copyright (c) 2026, FreeOCD
// SPDX-License-Identifier: BSD-3-Clause

//! Build script: copy the shared freeocd-web target definitions into `OUT_DIR`
//! so the whole directory can be embedded into the binary via `include_dir!`
//! at compile time.
//!
//! Staging the embed source in `OUT_DIR` (rather than pointing the embed macro
//! at the submodule path directly) makes the embedded snapshot an explicit
//! build artifact and keeps the JSON baked into the binary in every profile.

use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo"),
    );
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set by cargo"));

    let src = manifest_dir
        .join("vendor")
        .join("freeocd-web")
        .join("public")
        .join("targets");
    let dst = out_dir.join("targets");

    if !src.is_dir() {
        panic!(
            "Target definitions not found at {}. The freeocd-web submodule is \
             probably not checked out; run: git submodule update --init --recursive",
            src.display()
        );
    }

    // Re-run when this script changes (emitting any rerun-if-changed disables
    // the default "rerun on any package change" behaviour).
    println!("cargo:rerun-if-changed=build.rs");

    // Refresh the destination so files removed upstream do not linger.
    if dst.exists() {
        fs::remove_dir_all(&dst).expect("failed to clear OUT_DIR/targets");
    }
    copy_dir(&src, &dst);
}

/// Recursively copy `src` into `dst`, emitting `rerun-if-changed` for every
/// source path so upstream edits, additions and removals trigger a rebuild
/// (which re-runs this script and re-embeds the refreshed copy).
fn copy_dir(src: &Path, dst: &Path) {
    println!("cargo:rerun-if-changed={}", src.display());
    fs::create_dir_all(dst).expect("failed to create OUT_DIR target subdirectory");

    for entry in fs::read_dir(src).expect("failed to read target source directory") {
        let entry = entry.expect("failed to read directory entry");
        let file_type = entry.file_type().expect("failed to stat directory entry");
        let from = entry.path();
        let to = dst.join(entry.file_name());

        if file_type.is_dir() {
            copy_dir(&from, &to);
        } else {
            println!("cargo:rerun-if-changed={}", from.display());
            fs::copy(&from, &to)
                .unwrap_or_else(|e| panic!("failed to copy {}: {e}", from.display()));
        }
    }
}
