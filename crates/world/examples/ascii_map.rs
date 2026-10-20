//! Print a top-down ASCII map of a generated world.
//!
//! `cargo run -p world --example ascii_map -- 0xC0FFEE 4`
//!
//! Arguments are `seed` (decimal or `0x`-prefixed) and team count. This exists
//! because the worldgen tests assert *properties* — heights in range, pads flat,
//! drop sites legal — and properties do not tell you whether the result looks
//! like a place. Ten seconds of eyeballing catches "every base is on a cliff"
//! faster than any assertion will.

use world::block::{registry, BlockId};
use world::gen::{WorldGen, WORLD_X, WORLD_Z};

fn main() {
    let mut args = std::env::args().skip(1);
    let seed = match args.next() {
        Some(s) => {
            if let Some(hex) = s.strip_prefix("0x") {
                u64::from_str_radix(hex, 16).expect("seed must be hex after 0x")
            } else {
                s.parse().expect("seed must be an integer")
            }
        }
        None => 0x00C0_FFEE,
    };
    let teams: u8 = args.next().and_then(|s| s.parse().ok()).unwrap_or(4);

    let g = WorldGen::new(seed, teams);
    let sea = world::gen::sea_level();
    println!(
        "seed {seed:#x}  teams {teams}  drop sites {}",
        g.drop_sites().len()
    );

    // Sample every 4 blocks so a 256-wide map fits in a terminal. Columns are
    // generated once per chunk row rather than per sample point: generating a
    // column is ~0.15 ms, and 4,096 of them would make this example take longer
    // than the tests.
    let step = 4;
    let r = registry();
    let mut rows = Vec::new();
    let mut cache: Vec<world::Column> = Vec::new();
    let mut cached_row = i32::MIN;

    for z in (0..WORLD_Z).step_by(step) {
        let cz = z / 16;
        if cz != cached_row {
            cache = (0..WORLD_X / 16).map(|cx| g.column(cx, cz)).collect();
            cached_row = cz;
        }
        let mut row = String::new();
        for x in (0..WORLD_X).step_by(step) {
            let h = g.height_at(x, z);
            let col = &cache[(x / 16) as usize];
            let surface = col.get((x % 16) as usize, h as usize, (z % 16) as usize);
            // Show water rather than the bed under it, otherwise every lake
            // reads as a clay or sand flat.
            let glyph = if h < sea {
                '.'
            } else {
                glyph(r.def(surface).name.as_str(), h, surface)
            };
            row.push(glyph);
        }
        rows.push(row);
    }
    for row in &rows {
        println!("{row}");
    }

    println!();
    print_histogram(&g);

    println!();
    println!("legend  . water  , sand  \" grass  # stone  : gravel  ~ clay  * snow  T tree  P pad");
    println!("heights {}..{}", min_max(&g).0, min_max(&g).1);

    // Overlay the strategic locations so the layout can be judged.
    for (label, list) in [
        (
            "base",
            g.bases().iter().map(|b| (b.x, b.z)).collect::<Vec<_>>(),
        ),
        (
            "drop",
            g.drop_sites()
                .iter()
                .map(|s| (s.x, s.z))
                .collect::<Vec<_>>(),
        ),
    ] {
        println!();
        println!("{label} positions (map coords):");
        for (x, z) in list {
            println!("  ({x:3}, {z:3})  height {}", g.height_at(x, z));
        }
    }
}

/// Height distribution. A map can satisfy `28..=64` and still be a flat
/// rectangle, so the shape of the distribution matters as much as its bounds.
fn print_histogram(g: &WorldGen) {
    let mut buckets = [0u32; 40];
    let mut total = 0u32;
    for z in (0..WORLD_Z).step_by(2) {
        for x in (0..WORLD_X).step_by(2) {
            let h = g.height_at(x, z);
            buckets[((h - 28).clamp(0, 39)) as usize] += 1;
            total += 1;
        }
    }
    println!("height distribution (bar = share of the map):");
    for (i, n) in buckets.iter().enumerate() {
        let pct = *n as f64 * 100.0 / total as f64;
        if pct < 0.15 {
            continue;
        }
        let bar = "#".repeat((pct * 1.5) as usize);
        println!("  {:3}  {:5.1}%  {bar}", 28 + i, pct);
    }
}

fn glyph(name: &str, h: i32, id: BlockId) -> char {
    if id == world::block::AIR {
        return ' ';
    }
    match name {
        "water" => '.',
        "sand" => ',',
        "grass" => '"',
        "stone" | "cobble" | "basalt" => '#',
        "snow" => '*',
        "clay" => '~',
        "gravel" => ':',
        "pad_floor" => 'P',
        "log" | "leaves" => 'T',
        _ => {
            if h >= 56 {
                '^'
            } else {
                '"'
            }
        }
    }
}

fn min_max(g: &WorldGen) -> (i32, i32) {
    let mut lo = i32::MAX;
    let mut hi = i32::MIN;
    for z in (0..WORLD_Z).step_by(8) {
        for x in (0..WORLD_X).step_by(8) {
            let h = g.height_at(x, z);
            lo = lo.min(h);
            hi = hi.max(h);
        }
    }
    (lo, hi)
}
