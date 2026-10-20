//! The no-float guard for `world` (plan A-08, §6.7).
//!
//! This crate is the reason the guard exists. The client regenerates terrain
//! from a seed instead of downloading it, so a float in [`world::gen`] means
//! two machines generate *different maps* from the same seed — with no packet
//! involved and nothing in the logs. Terrain heights, base positions, tree
//! placement and mesh vertices must all be integer-derived.
//!
//! `sim::fixed` supplies `sin`/`cos`/`sqrt` in Q16.16 precisely so that worldgen
//! never has to reach for `libm`.
//!
//! Unlike `sim`, nothing here is allowlisted: `world` has no reason to touch a
//! float at all, not even at a data boundary.

use std::path::{Path, PathBuf};

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Remove `//` line comments and everything from the test module onward.
fn strip(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        if line.trim_start().starts_with("#[cfg(test)]") {
            break;
        }
        match line.find("//") {
            Some(i) => out.push_str(&line[..i]),
            None => out.push_str(line),
        }
        out.push('\n');
    }
    out
}

#[test]
fn no_float_anywhere_in_world() {
    let mut checked = 0;
    let mut violations = Vec::new();

    let entries = std::fs::read_dir(src_dir()).expect("world/src must be readable");
    for entry in entries {
        let path = entry.expect("directory entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("file name")
            .to_string();
        let src = std::fs::read_to_string(&path).expect("source file must be readable");
        let code = strip(&src);
        checked += 1;

        for (lineno, line) in code.lines().enumerate() {
            for token in ["f32", "f64"] {
                if line.contains(token) {
                    violations.push(format!("{name}:{}: {}", lineno + 1, line.trim()));
                }
            }
        }
    }

    assert!(
        checked >= 5,
        "expected to scan every module, scanned {checked}"
    );
    assert!(
        violations.is_empty(),
        "floating point leaked into worldgen or meshing. The client regenerates \
         terrain from the seed, so this desyncs maps across targets (plan §6.7). \
         Use `sim::fixed::Fixed` instead:\n  {}",
        violations.join("\n  ")
    );
}
