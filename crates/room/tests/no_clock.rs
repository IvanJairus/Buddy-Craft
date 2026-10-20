//! The no-clock guard (plan §6.10, T-019).
//!
//! `room` is deliberately runtime-agnostic: the same state machine runs inside a
//! Cloudflare Durable Object and inside a `tokio` task. That property is easy to
//! lose by accident — one `Instant::now()` for a log line, one `tokio::spawn` for
//! a timer, and the crate now has an opinion about its runtime.
//!
//! So the rule is: **time enters as a [`sim::Tick`], and nothing here is
//! `async`.** This test reads the crate's own source and fails the build if that
//! stops being true.
//!
//! What it does *not* forbid is the `std` crate itself — `Vec`, `String` and
//! `core::fmt` are all fine. The forbidden list is about *ambient* time and
//! *ambient* concurrency, which are the two things that would make the crate
//! untestable in microseconds.

use std::path::{Path, PathBuf};

/// Tokens that mean "this crate now depends on when it is running" or "on what".
///
/// Matched against code with comments and doc comments removed, so a doc link to
/// `std::time` is not a violation.
const FORBIDDEN: &[&str] = &[
    "std::time",
    "core::time",
    "Instant",
    "SystemTime",
    "async fn",
    ".await",
    "tokio",
    "thread::spawn",
    "std::thread",
    "std::net",
    "std::fs",
];

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Strip line comments, block comments and the test module, so that a doc link
/// or a test's own `println!` does not count.
fn strip(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut in_block = false;
    for line in src.lines() {
        if line.trim_start().starts_with("#[cfg(test)]") {
            break;
        }
        let mut kept = String::with_capacity(line.len());
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if in_block {
                if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    in_block = false;
                    i += 2;
                    continue;
                }
                i += 1;
                continue;
            }
            if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
                in_block = true;
                i += 2;
                continue;
            }
            if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
                break;
            }
            kept.push(bytes[i] as char);
            i += 1;
        }
        out.push_str(&kept);
        out.push('\n');
    }
    out
}

#[test]
fn no_std_clock() {
    let dir = src_dir();
    let mut checked = 0;
    let mut violations = Vec::new();

    let entries = std::fs::read_dir(&dir).expect("room/src must be readable");
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
            for token in FORBIDDEN {
                if line.contains(token) {
                    violations.push(format!("{name}:{}: {}", lineno + 1, line.trim()));
                }
            }
        }
    }

    assert!(
        checked >= 2,
        "expected to scan several files, scanned {checked}"
    );
    assert!(
        violations.is_empty(),
        "`room` must stay runtime-agnostic: time is a sim::Tick, nothing is async, \
         and the host owns the socket (plan §6.10):\n  {}",
        violations.join("\n  ")
    );
}

#[test]
fn the_stripper_actually_strips() {
    // A guard that passes because it strips everything would be worse than no
    // guard. Each of these must be removed or kept as stated.
    let sample = "\
use std::time::Instant;         // a violation, and the comment is irrelevant
/// Doc links to `std::time::Instant` are fine.
/* a block comment
   spanning std::time */
fn ok() -> u32 { 1 }
#[cfg(test)]
mod t { fn f() { std::time::Instant::now(); } }
";
    let stripped = strip(sample);
    assert_eq!(
        stripped.matches("std::time").count(),
        1,
        "only the real use should survive: {stripped}"
    );
    assert!(!stripped.contains("spanning"), "block comments must go");
    assert!(!stripped.contains("fn f()"), "the test module must go");
    assert!(stripped.contains("fn ok"), "real code must survive");
}
