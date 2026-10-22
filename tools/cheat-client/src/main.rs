//! T-050 cheat harness.
//!
//! Runs all fifteen §11.2 attacks against the same guards the host uses and
//! writes a JSON fixture. Network transport is covered by the host tests; this
//! binary is deterministic and runs with `cargo run -p cheat-client`.

use std::path::Path;
use std::process::Command;

use protocol::codec::{decode_checked, Bounds};
use protocol::{edit_action, edit_reject, C2s, Edit, FixedBytes, Hello, PROTOCOL_VERSION};
use room::ConnectionBudget;
use room::{EditPipeline, EditRequest, EditWorld, Match, TokenBook};
use sim::{InputFrame, Mode, Pad, Pads, PlayerId, RoomSetup, Tick, Vec3};
use world::block::CRATE;

fn attempt(
    results: &mut Vec<serde_json::Value>,
    id: &str,
    attack: &str,
    outcome: &str,
    detail: &str,
) {
    results.push(serde_json::json!({
        "id": id,
        "attack": attack,
        "outcome": outcome,
        "detail": detail,
    }));
}

fn match_fixture() -> Match {
    let setup = RoomSetup {
        mode: Mode::Ffa,
        teams: None,
        humans: 1,
        bots: 1,
    };
    Match::new(0x00C0_FFEE, setup).expect("legal fixture")
}

fn pipeline_fixture() -> (EditWorld, Pads, sim::TeamId) {
    let world = EditWorld::new(0x00C0_FFEE, 2);
    let mut pads = Pads::new();
    let team = pads.push(Pad::new([0, 60, 0]));
    (world, pads, team)
}

fn edit(seq: u16, x: i16, y: i16, z: i16) -> Edit {
    Edit {
        seq,
        x,
        y,
        z,
        block: CRATE.0,
        action: edit_action::PLACE,
    }
}

fn main() {
    let mut results = Vec::new();

    // T1: maximum input, the authoritative body is clamped by movement.
    {
        let mut game = match_fixture();
        let start = game.state().body(PlayerId(0)).expect("body").state.pos;
        for tick in 0..60 {
            game.tick(&[(
                PlayerId(0),
                InputFrame {
                    tick: Tick(tick + 1),
                    move_x: 0,
                    move_z: 100,
                    yaw: 0,
                    pitch: 0,
                    buttons: 0,
                },
            )]);
        }
        let end = game.state().body(PlayerId(0)).expect("body").state.pos;
        let travelled = start.horizontal_distance(end).to_f32();
        let budget = 4.5 * 3.0 + 1.0;
        attempt(
            &mut results,
            "T1",
            "speed / teleport via input",
            if travelled <= budget {
                "clamped"
            } else {
                "failed"
            },
            &format!("{travelled:.3} m over 3 s, budget {budget:.1} m"),
        );
    }

    // T2: edit 20 m away.
    {
        let mut game = match_fixture();
        let body = game.state().body(PlayerId(0)).expect("body").state.pos;
        let far = edit(
            1,
            (body.x.round_int() + 20) as i16,
            body.y.round_int() as i16,
            body.z.round_int() as i16,
        );
        let result = game.submit_edit(PlayerId(0), &far);
        attempt(
            &mut results,
            "T2",
            "reach hack",
            if !result.accepted && result.reason == edit_reject::TOO_FAR {
                "rejected"
            } else {
                "failed"
            },
            &format!("reason {}", result.reason),
        );
    }

    // T3: there is no damage message on the wire.
    {
        let decoded = C2s::decode(&[0x0a]);
        attempt(
            &mut results,
            "T3",
            "damage spoof",
            if decoded.is_err() {
                "rejected"
            } else {
                "failed"
            },
            "tag 0x0A is not a client message",
        );
    }

    // T4: ninth edit in one tick is rate limited.
    {
        let (mut world, mut pads, team) = pipeline_fixture();
        let mut pipeline = EditPipeline::new();
        let mut accepted = 0;
        let mut last = 0;
        for index in 0..9u16 {
            let message = edit(index, (index % 5) as i16, 60, (index / 5) as i16);
            let request = EditRequest {
                player: PlayerId(0),
                team,
                eye: Vec3::from_ints(4, 61, 4),
                message: &message,
            };
            let result = pipeline.submit(Tick(0), &request, &mut world, &mut pads);
            if result.accepted {
                accepted += 1;
            }
            last = result.reason;
        }
        attempt(
            &mut results,
            "T4",
            "edit spam",
            if accepted == 8 && last == edit_reject::RATE_LIMITED {
                "rejected"
            } else {
                "failed"
            },
            &format!("{accepted} accepted, last reason {last}"),
        );
    }

    // T5: duplicate placement cannot duplicate a crate.
    {
        let (mut world, mut pads, team) = pipeline_fixture();
        let mut pipeline = EditPipeline::new();
        let message = edit(1, 0, 60, 0);
        let request = EditRequest {
            player: PlayerId(0),
            team,
            eye: Vec3::from_ints(4, 61, 4),
            message: &message,
        };
        let first = pipeline.submit(Tick(0), &request, &mut world, &mut pads);
        let second = pipeline.submit(Tick(1), &request, &mut world, &mut pads);
        attempt(
            &mut results,
            "T5",
            "crate duplication",
            if first.accepted && !second.accepted && second.reason == edit_reject::OCCUPIED {
                "rejected"
            } else {
                "failed"
            },
            "the second edit sees the first crate",
        );
    }

    // T6: two players race for one cell.
    {
        let (mut world, mut pads, team) = pipeline_fixture();
        let mut pipeline = EditPipeline::new();
        let message = edit(1, 1, 60, 0);
        let mut race = |player: u8| {
            let request = EditRequest {
                player: PlayerId(player),
                team,
                eye: Vec3::from_ints(4, 61, 4),
                message: &message,
            };
            pipeline.submit(Tick(0), &request, &mut world, &mut pads)
        };
        let first = race(0);
        let second = race(1);
        attempt(
            &mut results,
            "T6",
            "block-edit race",
            if first.accepted && !second.accepted && second.reason == edit_reject::OCCUPIED {
                "rejected"
            } else {
                "failed"
            },
            "edits are serial per tick",
        );
    }

    // T7: message bucket empties on a flood.
    {
        let mut rate = host_axum::ws::MessageRate::new();
        let mut accepted = 0;
        for _ in 0..61 {
            if rate.allow() {
                accepted += 1;
            }
        }
        attempt(
            &mut results,
            "T7",
            "snapshot / message flood",
            if accepted == 60 { "rejected" } else { "failed" },
            &format!("{accepted} of 61 messages admitted"),
        );
    }

    // T8: oversize frame.
    {
        let error = decode_checked(&vec![0u8; 4_097], Bounds::STANDARD).expect_err("oversize");
        attempt(
            &mut results,
            "T8",
            "message-size DoS",
            if error.close_code() == Some(1009) {
                "rejected"
            } else {
                "failed"
            },
            &format!("close code {:?}", error.close_code()),
        );
    }

    // T9: fourth socket from one address.
    {
        let mut budget = ConnectionBudget::default();
        let admitted = (0..4).filter(|_| budget.open("203.0.113.9")).count();
        attempt(
            &mut results,
            "T9",
            "connection flood",
            if admitted == 3 { "rejected" } else { "failed" },
            &format!("{admitted} of 4 admitted"),
        );
    }

    // T10: room-code brute force.
    {
        let mut registry = host_worker::registry::Registry::new(10_000);
        for _ in 0..host_worker::registry::MAX_FAILURES {
            let _ = registry.join("ZZZZZZ", "203.0.113.5", 0);
        }
        let locked = matches!(
            registry.join("ZZZZZZ", "203.0.113.5", 1_000),
            Err(host_worker::registry::JoinError::Locked { .. })
        );
        attempt(
            &mut results,
            "T10",
            "room-code brute force",
            if locked { "rejected" } else { "failed" },
            "lockout after six failures",
        );
    }

    // T11: XSS in a Hello name is sanitised by the codec.
    {
        let hello = C2s::Hello(Hello {
            protocol: PROTOCOL_VERSION,
            name: FixedBytes::from_str_lossy("<img src=x onerror=alert(1)>"),
            room_code: FixedBytes::from_str_lossy("ABC123"),
            token: None,
        });
        let rejected = matches!(
            decode_checked(&hello.encode(), Bounds::STANDARD),
            Err(protocol::codec::Error::BadName)
        );
        attempt(
            &mut results,
            "T11",
            "XSS via name",
            if rejected { "rejected" } else { "failed" },
            "the name codec rejects characters outside its alphabet",
        );
    }

    // T12: the committed bundle secret scan.
    {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("repo root");
        let clean = Command::new("node")
            .arg("tools/secret-scan.mjs")
            .current_dir(root)
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        attempt(
            &mut results,
            "T12",
            "secret leakage",
            if clean { "rejected" } else { "failed" },
            "secret-scan.mjs found no planted pattern",
        );
    }

    // T13: mutated postcard frames never panic.
    {
        let mut seed = 0x1234_5678u32;
        let mut survived = true;
        for length in 0..64usize {
            let mut bytes = vec![0u8; length];
            for byte in &mut bytes {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *byte = (seed >> 24) as u8;
            }
            if std::panic::catch_unwind(|| C2s::decode(&bytes)).is_err() {
                survived = false;
            }
        }
        attempt(
            &mut results,
            "T13",
            "prototype pollution / postcard mutation",
            if survived { "rejected" } else { "failed" },
            "64 mutated frames decoded without a panic",
        );
    }

    // T14: elimination revokes the token.
    {
        let mut book = TokenBook::default();
        book.issue(7, PlayerId(0), [9; 16]);
        book.revoke_player(PlayerId(0));
        let refused = book.accept(7, &[9; 16]).is_none();
        attempt(
            &mut results,
            "T14",
            "eliminated player rejoin",
            if refused { "rejected" } else { "failed" },
            "revoked token is not accepted",
        );
    }

    // T15: a token belongs to one room and dies at cleanup.
    {
        let mut book = TokenBook::default();
        book.issue(7, PlayerId(0), [8; 16]);
        let wrong_room = book.accept(8, &[8; 16]).is_none();
        book.revoke_room(7);
        let cleaned = book.accept(7, &[8; 16]).is_none();
        attempt(
            &mut results,
            "T15",
            "token replay",
            if wrong_room && cleaned {
                "rejected"
            } else {
                "failed"
            },
            "room scope and cleanup revocation",
        );
    }

    let failed = results
        .iter()
        .filter(|result| result["outcome"] == "failed")
        .count();
    let report = serde_json::json!({
        "protocol": PROTOCOL_VERSION,
        "attempts": results,
        "failed": failed,
    });
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("report.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&report).expect("json")).expect("write report");
    println!("{}", serde_json::to_string_pretty(&report).expect("json"));
    if failed != 0 {
        std::process::exit(1);
    }
}
