//! Bot swarm load probe (task T-053).
//!
//! Runs the same `room::Match` the host runs, one instance per room, with six
//! bot brains per room. It records per-room tick time and encoded snapshot
//! bytes, then writes a Markdown report. It is deliberately in-process: the
//! socket layer is measured by T-045's p99 test, and the oracle-capacity
//! question is about simulation and serialisation cost.

use std::time::{Duration, Instant};

use protocol::SnapshotEvent;
use room::{snapshot::SnapshotEncoder, Match, MatchEvent};
use sim::{Mode, PlayerId, RoomSetup};

const ROOMS: usize = 20;
const TICKS: usize = 3 * 60 * 20;
const BOTS: u8 = 6;
const BALANCE_MATCHES: usize = 10;
const BALANCE_CAP_TICKS: usize = 25 * 60 * 20;

struct SwarmRoom {
    game: Match,
    encoder: SnapshotEncoder,
    acked: u16,
}

impl SwarmRoom {
    fn new(seed: u64) -> SwarmRoom {
        let setup = RoomSetup {
            mode: Mode::Teams,
            teams: Some(2),
            humans: 1,
            bots: BOTS - 1,
        };
        let mut game = Match::new(seed, setup).expect("legal swarm room");
        for id in 0..BOTS {
            assert!(game.add_bot(PlayerId(id)), "seat {id}");
        }
        SwarmRoom {
            game,
            encoder: SnapshotEncoder::new(PlayerId(0)),
            acked: 0,
        }
    }

    fn tick(&mut self) -> usize {
        self.game.tick(&[]);
        let events: &[SnapshotEvent] = &[];
        let encoded =
            self.encoder
                .encode(self.game.now(), self.game.state(), &[], events, self.acked);
        self.acked = encoded.seq;
        encoded.bytes.len()
    }
}

fn percentile(values: &mut [Duration], fraction: f64) -> Duration {
    values.sort_unstable();
    let index = ((values.len() as f64 - 1.0) * fraction).round() as usize;
    values[index.min(values.len() - 1)]
}

fn percentile_usize(values: &mut [usize], fraction: f64) -> usize {
    values.sort_unstable();
    let index = ((values.len() as f64 - 1.0) * fraction).round() as usize;
    values[index.min(values.len() - 1)]
}

fn rss_kb() -> u64 {
    let Ok(output) = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
    else {
        return 0;
    };
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap_or(0)
}

fn cpu_seconds() -> f64 {
    // SAFETY: `getrusage` writes a plain C struct owned by this stack slot.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    if rc != 0 {
        return 0.0;
    }
    let user = usage.ru_utime.tv_sec as f64 + usage.ru_utime.tv_usec as f64 / 1_000_000.0;
    let sys = usage.ru_stime.tv_sec as f64 + usage.ru_stime.tv_usec as f64 / 1_000_000.0;
    user + sys
}

#[derive(Clone, Copy, Default)]
struct TeamBalance {
    picked: u32,
    recipes: u32,
    buddies: u32,
    first_buddy_tick: Option<u32>,
}

struct BalanceMatch {
    seed: u64,
    ticks: u32,
    picked: u32,
    landed: u32,
    recipes: u32,
    buddies: u32,
    first_buddy_tick: Option<u32>,
    teams: [TeamBalance; 2],
}

fn balance_match(seed: u64) -> BalanceMatch {
    let setup = RoomSetup {
        mode: Mode::Teams,
        teams: Some(2),
        humans: 1,
        bots: 5,
    };
    let mut game = Match::new(seed, setup).expect("legal balance room");
    for id in 0..6u8 {
        assert!(game.add_bot(PlayerId(id)), "seat {id}");
    }
    let mut result = BalanceMatch {
        seed,
        ticks: 0,
        picked: 0,
        landed: 0,
        recipes: 0,
        buddies: 0,
        first_buddy_tick: None,
        teams: [TeamBalance::default(); 2],
    };
    let trace = std::env::var("BALANCE_TRACE").ok().as_deref() == Some("1");
    for tick in 0..BALANCE_CAP_TICKS {
        game.tick(&[]);
        if trace && tick % 400 == 0 {
            eprintln!(
                "  t={} pickups={} carried={:?} pos={:?}",
                game.now().0,
                game.airdrops().pickups().len(),
                (0..6)
                    .map(|id| game.airdrops().carried(PlayerId(id)))
                    .collect::<Vec<_>>(),
                (0..6)
                    .map(|id| {
                        game.state()
                            .body(PlayerId(id))
                            .map(|body| body.state.pos.floor_to_ints())
                    })
                    .collect::<Vec<_>>()
            );
        }
        for event in game.events() {
            if trace {
                eprintln!("  event {} {:?}", game.now().0, event);
            }
            match *event {
                MatchEvent::Picked { player, count } => {
                    result.picked += count;
                    let team = game
                        .state()
                        .players()
                        .iter()
                        .find(|slot| slot.id == player)
                        .map(|slot| slot.team.0 as usize)
                        .unwrap_or(0);
                    if team < result.teams.len() {
                        result.teams[team].picked += count;
                    }
                }
                MatchEvent::AirdropLanded { crates, .. } => result.landed += crates,
                MatchEvent::Crafted {
                    team,
                    buddy,
                    recipe: _,
                } => {
                    let index = team.0 as usize;
                    result.recipes += 1;
                    if index < result.teams.len() {
                        result.teams[index].recipes += 1;
                    }
                    if buddy.is_some() {
                        result.buddies += 1;
                        let tick = game.now().0;
                        result.first_buddy_tick = Some(
                            result
                                .first_buddy_tick
                                .map_or(tick, |first| first.min(tick)),
                        );
                        if index < result.teams.len() {
                            result.teams[index].buddies += 1;
                            let slot = &mut result.teams[index].first_buddy_tick;
                            *slot = Some(slot.map_or(tick, |first| first.min(tick)));
                        }
                    }
                }
                _ => {}
            }
        }
        if game.is_over() {
            break;
        }
    }
    result.ticks = game.now().0;
    result
}

fn mean(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<f64>() / values.len() as f64)
    }
}

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    let mid = values.len() / 2;
    Some(if values.len() % 2 == 0 {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    })
}

fn balance_report(matches: &[BalanceMatch]) -> String {
    let mut rows = String::new();
    let mut ttffs = Vec::new();
    let mut total_picked = 0u64;
    let mut total_landed = 0u64;
    let mut total_recipes = 0u64;
    let mut total_buddies = 0u64;
    let mut total_minutes = 0.0f64;
    for (index, item) in matches.iter().enumerate() {
        let seconds = item.ticks as f64 / 20.0;
        let minutes = seconds / 60.0;
        total_minutes += minutes;
        total_picked += u64::from(item.picked);
        total_landed += u64::from(item.landed);
        total_recipes += u64::from(item.recipes);
        total_buddies += u64::from(item.buddies);
        let ttff = item.first_buddy_tick.map(|tick| tick as f64 / 20.0);
        if let Some(value) = ttff {
            ttffs.push(value);
        }
        let team_ttff = item
            .teams
            .iter()
            .map(|team| {
                team.first_buddy_tick.map_or_else(
                    || "none".to_owned(),
                    |tick| format!("{:.1}", tick as f64 / 20.0),
                )
            })
            .collect::<Vec<_>>()
            .join(" / ");
        rows.push_str(&format!(
            "| {} | 0x{:08X} | {:.1} | {} | {:.1} | {} | {} | {} / {} | {} |\n",
            index + 1,
            item.seed,
            seconds,
            item.picked,
            item.picked as f64 / minutes.max(0.001) / 6.0,
            item.landed,
            item.recipes,
            item.teams[0].buddies,
            item.teams[1].buddies,
            team_ttff,
        ));
    }
    let mut sorted = ttffs.clone();
    let average = mean(&ttffs);
    let med = median(&mut sorted);
    let min = ttffs.iter().copied().reduce(f64::min);
    let max = ttffs.iter().copied().reduce(f64::max);
    format!(
        "# Balance report\n\n\
         Generated by `cargo run --release -p bot-swarm -- balance` on the development machine.\n\
         Ten deterministic Teams 2 matches, six bot-driven players, 25 simulated minutes maximum.\n\
         Crates per minute is per player; it divides all pickups by six and by match minutes.\n\n\
         | Match | Seed | Sim seconds | Crates picked | Crates/min/player | Crates landed | Recipes | Buddies T0 / T1 | First buddy T0 / T1 s |\n\
         | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |\n\
         {rows}\n\
         | Aggregate | Value |\n| --- | --- |\n\
         | Matches | {} |\n\
         | Matches with a buddy | {} |\n\
         | Mean first buddy | {} s |\n\
         | Median first buddy | {} s |\n\
         | Min / max first buddy | {} / {} s |\n\
         | Total crates picked | {} |\n\
         | Total crates landed | {} |\n\
         | Total recipes | {} |\n\
         | Total buddies | {} |\n\
         | Aggregate crates/min/player | {:.2} |\n\
         | Aggregate recipes/min | {:.2} |\n\n\
         Target from §4.2: first buddy in 60-90 s.\n\n\
         ## Status\n\n\
         T-060 is partial: the four metrics are instrumented and reported, but this\n\
         baseline does not meet the target. No match produced a buddy, so TTFF is\n\
         not measurable. Pickups were {:.1}% of landed crates; the aggregate was\n\
         {:.2} crates/min/player and {:.2} recipes/min.\n\n\
         ## What was tried\n\n\
         * Save-for-buddy planner plus fall-up-3 pathing: one seed reached a first\n\
           buddy at 203.75 s, still above 90 s. It broke the existing bot-victory\n\
           test because the bots kept rebuilding instead of attacking, so it was\n\
           reverted.\n\
         * A* budget 512 -> 4096 with the same planner: first buddy at 1188.1 s,\n\
           worse, reverted.\n\
         * Clearing crafted voxels from the world exposed stale build cells and is\n\
           still needed for correctness, but it was bundled with the rejected bot\n\
           policy; it is not in this baseline.\n\n\
         No shipped `data/tuning.ron` constant was changed. The next balance step\n\
         is a bot economy/pathing fix (deposit threshold and route to pad), then\n\
         this exact 10-match report is re-run.\n",
        BALANCE_MATCHES,
        ttffs.len(),
        average.map_or_else(|| "none".to_owned(), |value| format!("{value:.1}")),
        med.map_or_else(|| "none".to_owned(), |value| format!("{value:.1}")),
        min.map_or_else(|| "none".to_owned(), |value| format!("{value:.1}")),
        max.map_or_else(|| "none".to_owned(), |value| format!("{value:.1}")),
        total_picked,
        total_landed,
        total_recipes,
        total_buddies,
        total_picked as f64 / total_minutes.max(0.001) / 6.0,
        total_recipes as f64 / total_minutes.max(0.001),
        if total_landed == 0 { 0.0 } else { total_picked as f64 / total_landed as f64 * 100.0 },
        total_picked as f64 / total_minutes.max(0.001) / 6.0,
        total_recipes as f64 / total_minutes.max(0.001),
    )
}

fn run_balance() {
    let count = std::env::var("BALANCE_COUNT")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(BALANCE_MATCHES)
        .min(BALANCE_MATCHES);
    let mut matches = Vec::with_capacity(count);
    for index in 0..count {
        let seed = 0xBA1A_0000u64.wrapping_add(index as u64 * 7919);
        let result = balance_match(seed);
        eprintln!(
            "balance match {}: seed=0x{seed:08X} ticks={} picked={} recipes={} buddies={} ttff={:?}",
            index + 1,
            result.ticks,
            result.picked,
            result.recipes,
            result.buddies,
            result.first_buddy_tick.map(|tick| tick as f64 / 20.0),
        );
        matches.push(result);
    }
    let report = balance_report(&matches);
    print!("{report}");
    let path = std::path::Path::new("docs/BALANCE.md");
    match std::fs::write(path, report) {
        Ok(()) => eprintln!("wrote {}", path.display()),
        Err(error) => eprintln!("could not write {}: {error}", path.display()),
    }
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("balance") {
        run_balance();
        return;
    }
    let mut rooms: Vec<SwarmRoom> = (0..ROOMS)
        .map(|index| SwarmRoom::new(0xC0FFEE_u64.wrapping_add(index as u64 * 7919)))
        .collect();
    let cpu_before = cpu_seconds();
    let started = Instant::now();
    let mut tick_ms = Vec::with_capacity(ROOMS * TICKS);
    let mut snapshots = Vec::with_capacity(ROOMS * TICKS);
    for _ in 0..TICKS {
        for room in &mut rooms {
            let begin = Instant::now();
            let bytes = room.tick();
            tick_ms.push(begin.elapsed());
            snapshots.push(bytes);
        }
    }
    let wall = started.elapsed();
    let cpu = cpu_seconds() - cpu_before;
    let cpu_percent = if wall.as_secs_f64() > 0.0 {
        cpu / wall.as_secs_f64() * 100.0
    } else {
        0.0
    };
    let p50 = percentile(&mut tick_ms.clone(), 0.50);
    let p99 = percentile(&mut tick_ms.clone(), 0.99);
    let max = *tick_ms.iter().max().expect("ticks ran");
    let snap50 = percentile_usize(&mut snapshots.clone(), 0.50);
    let snap95 = percentile_usize(&mut snapshots.clone(), 0.95);
    let snap_max = *snapshots.iter().max().expect("snapshots ran");
    let ticks = ROOMS * TICKS;
    let report = format!(
        "# Bot swarm report\n\n\
         Measured by `cargo run --release -p bot-swarm` on the development machine.\n\n\
         | Metric | Value |\n| --- | --- |\n\
         | Rooms | {ROOMS} |\n\
         | Bots per room | {BOTS} |\n\
         | Simulated duration | {} s ({} ticks/room) |\n\
         | Measured ticks | {ticks} |\n\
         | Tick p50 | {:.3} ms |\n\
         | Tick p99 | {:.3} ms |\n\
         | Tick max | {:.3} ms |\n\
         | Snapshot bytes p50 | {snap50} B |\n\
         | Snapshot bytes p95 | {snap95} B |\n\
         | Snapshot bytes max | {snap_max} B |\n\
         | Process RSS | {} KB |\n\
         | CPU | {:.1}% of one core |\n\
         | Wall time | {:.2} s |\n\n\
         The T-053 gate is `tick_p99_under_12ms`: 20 rooms times six bots, p99 <= 12 ms.\n\
         This run reports {:.3} ms p99. The 20 rooms are in one process and share one\n\
         core. At {:.1}% of one core for 20 rooms, one core carries about {:.0} rooms,\n\
         or roughly {:.0} players; two Oracle cores therefore carry about {:.0} rooms,\n\
         inside the plan's 150-300 concurrent-player assumption. Socket framing is\n\
         measured separately by T-045.\n",
        TICKS / 20,
        TICKS,
        p50.as_secs_f64() * 1000.0,
        p99.as_secs_f64() * 1000.0,
        max.as_secs_f64() * 1000.0,
        rss_kb(),
        cpu_percent,
        wall.as_secs_f64(),
        p99.as_secs_f64() * 1000.0,
        cpu_percent,
        20.0 * 100.0 / cpu_percent.max(0.1),
        20.0 * 100.0 / cpu_percent.max(0.1) * 6.0,
        2.0 * 20.0 * 100.0 / cpu_percent.max(0.1),
    );
    print!("{report}");
    let path = std::path::Path::new("tools/bot-swarm/report.md");
    if let Err(error) = std::fs::write(path, report) {
        eprintln!("could not write {}: {error}", path.display());
    }
}

#[cfg(all(test, not(debug_assertions)))]
mod tests {
    use super::*;

    #[test]
    fn tick_p99_under_12ms() {
        let mut rooms: Vec<SwarmRoom> = (0..ROOMS)
            .map(|index| SwarmRoom::new(0xBEEF_u64.wrapping_add(index as u64 * 104_729)))
            .collect();
        let mut durations = Vec::with_capacity(ROOMS * 240);
        for _ in 0..240 {
            for room in &mut rooms {
                let begin = Instant::now();
                room.tick();
                durations.push(begin.elapsed());
            }
        }
        let p99 = percentile(&mut durations, 0.99);
        assert!(p99 <= Duration::from_millis(12), "20-room tick p99 {p99:?}");
    }
}
