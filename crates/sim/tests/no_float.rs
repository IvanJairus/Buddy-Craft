//! The no-float guard (plan A-08).
//!
//! `sim` is compiled to `wasm32-unknown-unknown` for client prediction and to
//! native for the authoritative server. Floating point is *deterministic per
//! operation* but not *portable*: `f32::sqrt`, `sin`, `pow` and fused multiply
//! add are all allowed to differ between targets, and LLVM is free to contract
//! `a * b + c` into an FMA on one architecture and not another. A single float
//! in movement code therefore produces a client and a server that disagree
//! about where a player is — the hardest possible bug to reproduce.
//!
//! So the rule is: **no `f32`/`f64` outside the two conversion boundaries.**
//! This test enforces it by reading the crate's own source at test time.
//!
//! Two files are allowlisted, both deliberately:
//!
//! * `fixed.rs` — owns `from_f32`/`to_f32`/`from_f64`/`to_f64`, the only place
//!   a float may cross into `Fixed`.
//! * `tuning.rs` — parses the designer-facing `data/tuning.ron`, whose numbers
//!   are written as decimals. They are converted to `Fixed` exactly once, at
//!   load, and never read as floats afterwards.
//!
//! Everything else — `ids`, `rng`, `time`, `sin_table`, `lib` — must be pure
//! integer arithmetic.

use std::path::{Path, PathBuf};

/// Files permitted to mention a float type.
const ALLOWED: &[&str] = &["fixed.rs", "tuning.rs"];

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Remove `//` line comments and everything from the test module onward, so
/// that test-only float usage (assertions, `to_f64` oracles) does not count.
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
fn no_float_outside_conversion_boundaries() {
    let dir = src_dir();
    let mut checked = 0;
    let mut violations = Vec::new();

    let entries = std::fs::read_dir(&dir).expect("sim/src must be readable");
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
        if ALLOWED.contains(&name.as_str()) {
            continue;
        }

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
        checked >= 4,
        "expected to scan several files, scanned {checked}"
    );
    assert!(
        violations.is_empty(),
        "floating point leaked into authoritative state (plan A-08). \
         Use `Fixed` instead:\n  {}",
        violations.join("\n  ")
    );
}

#[test]
fn the_stripper_actually_strips() {
    // A guard test that silently passes because it strips everything would be
    // worse than no guard at all.
    let sample =
        "let x: f32 = 1.0;\n// let y: f32 = 2.0;\n#[cfg(test)]\nmod t { fn f() -> f64 { 0.0 } }\n";
    let stripped = strip(sample);
    assert!(stripped.contains("f32"), "real code must survive stripping");
    assert!(!stripped.contains("f64"), "the test module must be removed");
    assert_eq!(
        stripped.matches("f32").count(),
        1,
        "comments must be removed"
    );
}
