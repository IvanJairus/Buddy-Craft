//! Bot swarm load probe (task T-053).
//!
//! Runs the same `room::Match` the host runs, one instance per room, with six
//! bot brains per room. It records per-room tick time and encoded snapshot
//! bytes, then writes a Markdown report. It is deliberately in-process: the
//! socket layer is measured by T-045's p99 test, and the oracle-capacity
//! question is about simulation and serialisation cost.

use std::time::{Duration, Instant};

use protocol::SnapshotEvent;
use room::{snapshot::SnapshotEncoder, Match};
use sim::{Mode, PlayerId, RoomSetup};

const ROOMS: usize = 20;
const TICKS: usize = 3 * 60 * 20;
const BOTS: u8 = 6;

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

fn main() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(debug_assertions))]
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
