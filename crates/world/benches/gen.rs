//! Worldgen throughput benchmark (plan T-006 acceptance criterion).
//!
//! Dependency-free on purpose: pulling in `criterion` would add ~40 crates to
//! the build graph for a number the plan already fixed at 1.5 ms per column.
//! Run with `cargo bench -p world --bench gen`.
//!
//! The budget is **asserted**, not printed. A regression that doubles worldgen
//! cost would otherwise be invisible until someone noticed a slow load on a
//! phone. `WORLD_BENCH_SLACK` multiplies the budget so a noisy CI runner can be
//! accommodated without deleting the check.

use std::time::Instant;

use world::gen::WorldGen;
use world::mesh;

/// Budget for generating one 16x96x16 column, in milliseconds.
const GEN_BUDGET_MS: f64 = 1.5;
/// Budget for meshing one full terrain column, in milliseconds.
const MESH_BUDGET_MS: f64 = 1.2;

fn slack() -> f64 {
    std::env::var("WORLD_BENCH_SLACK")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(1.0)
}

fn main() {
    let slack = slack();
    let g = WorldGen::new(0xBEEF_CAFE, 6);

    // Warm the block registry and the palette OnceLock so the first sample does
    // not pay for one-time initialisation.
    let warm = g.column(0, 0);
    let _ = mesh::mesh_column(&warm);

    // --- worldgen -----------------------------------------------------------
    let coords: Vec<(i32, i32)> = (0..64).map(|i| (i % 16, (i * 7) % 16)).collect();
    let t0 = Instant::now();
    let mut sink = 0usize;
    for (cx, cz) in &coords {
        sink += g.column(*cx, *cz).count_non_air();
    }
    let gen_ms = t0.elapsed().as_secs_f64() * 1000.0 / coords.len() as f64;

    // --- meshing ------------------------------------------------------------
    let col = g.column(5, 5);
    let t1 = Instant::now();
    for _ in 0..16 {
        sink += mesh::mesh_column(&col).quad_count();
    }
    let mesh_ms = t1.elapsed().as_secs_f64() * 1000.0 / 16.0;

    println!("gen     {gen_ms:8.3} ms/column   budget {GEN_BUDGET_MS:.1} ms");
    println!("mesh    {mesh_ms:8.3} ms/column   budget {MESH_BUDGET_MS:.1} ms");
    println!("(checksum {sink} — prevents the optimiser removing the work)");

    // `cargo test --all-targets` compiles this bench with the test profile,
    // which is unoptimised and 20-50x slower than release. Timing that would
    // fail every time, so the assertion only applies to a real `cargo bench`
    // (or an explicit `--release`) build.
    if cfg!(debug_assertions) {
        println!("note: unoptimised build, budget check skipped — run `cargo bench -p world --bench gen`");
        return;
    }

    let mut failed = false;
    if gen_ms > GEN_BUDGET_MS * slack {
        eprintln!(
            "FAIL gen: {gen_ms:.3} ms exceeds {:.1} ms (slack {slack})",
            GEN_BUDGET_MS
        );
        failed = true;
    }
    if mesh_ms > MESH_BUDGET_MS * slack {
        eprintln!(
            "FAIL mesh: {mesh_ms:.3} ms exceeds {:.1} ms (slack {slack})",
            MESH_BUDGET_MS
        );
        failed = true;
    }
    if failed {
        std::process::exit(1);
    }
}
