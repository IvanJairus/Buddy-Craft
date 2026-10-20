//! Print a determinism hash sequence, so two builds can be diffed by hand.
//!
//! The golden test (T-015) compares against a committed file. This does the
//! same run and prints the sequence, which is what you want when the test
//! fails and you need to know *which* build is wrong — run it on both and diff.
//!
//! ```text
//! cargo run -p sim --release --example hash_trace -- --mode duel --ticks 20000
//! cargo run -p sim --release --example hash_trace -- --mode teams --seed 0x5EED
//! ```
//!
//! Options: `--mode duel|teams|brawl`, `--seed <u64>`, `--ticks <u32>`,
//! `--every <u32>` (checkpoint interval, default 100).

use std::process::ExitCode;

use sim::scenario::{Scenario, ScenarioConfig};

fn main() -> ExitCode {
    let mut mode = String::from("duel");
    let mut seed: u64 = 0x00C0_FFEE;
    let mut ticks: u32 = 20_000;
    let mut every: u32 = 100;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--mode" => match args.next().as_deref() {
                Some(m @ ("duel" | "teams" | "brawl")) => mode = m.to_string(),
                _ => return usage("--mode needs duel, teams or brawl"),
            },
            "--seed" => match args.next().map(|s| parse_u64(&s)) {
                Some(Ok(v)) => seed = v,
                _ => return usage("--seed needs a u64, decimal or 0x-prefixed"),
            },
            "--ticks" => match args.next().map(|s| s.parse::<u32>()) {
                Some(Ok(v)) => ticks = v,
                _ => return usage("--ticks needs a positive integer"),
            },
            "--every" => match args.next().map(|s| s.parse::<u32>()) {
                Some(Ok(v)) if v > 0 => every = v,
                _ => return usage("--every needs a positive integer"),
            },
            "--help" | "-h" => {
                println!("{}", USAGE);
                return ExitCode::SUCCESS;
            }
            other => return usage(&format!("unknown argument {other}")),
        }
    }

    let config = match mode.as_str() {
        "duel" => ScenarioConfig::duel(seed, ticks),
        "teams" => ScenarioConfig::teams(seed, ticks),
        _ => ScenarioConfig::brawl(seed, ticks),
    };

    let mut scenario = Scenario::new(config);
    eprintln!(
        "mode {mode}, seed {seed:#x}, {} teams, {ticks} ticks",
        scenario.state().layout().teams
    );
    scenario.run(every, |frame| println!("{}", frame.to_line()));
    ExitCode::SUCCESS
}

const USAGE: &str =
    "usage: hash_trace [--mode duel|teams|brawl] [--seed N] [--ticks N] [--every N]";

fn usage(problem: &str) -> ExitCode {
    eprintln!("{problem}\n{USAGE}");
    ExitCode::from(2)
}

fn parse_u64(s: &str) -> Result<u64, std::num::ParseIntError> {
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => s.parse(),
    }
}
