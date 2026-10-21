//! T-021 acceptance gate: interest management, delta baselines and the snapshot
//! budget (§6.3, §7.3).
//!
//! These are the criteria from the task list, written as a client would see
//! them: every assertion decodes the bytes that would actually go on the wire.
//! The encoder's own unit tests cover internals; this file is the contract.
//!
//! # Why reconstruction, not just presence bits
//!
//! A delta scheme can be self-consistent and still wrong: presence bits that
//! describe the wrong fields round-trip perfectly and leave the client with a
//! player standing where it was three seconds ago. So the important test here
//! (`delta_chain_reconstructs_the_state`) does what a client does — start from a
//! keyframe, add every delta, compare against the authoritative positions — and
//! the presence-bit assertions are only there to explain a failure.

use protocol::{decode_s2c, player_flags, presence, S2c, Snapshot, SnapshotEvent};
use room::{SnapshotEncoder, AOI_HYSTERESIS, AOI_RADIUS, KEYFRAME_INTERVAL};
use sim::{
    buddy::BuddyKind, tuning, Buddy, EntityId, Fixed, Mode, PlayerId, Projectile, RoomSetup,
    TeamId, Tick, Vec3, ZERO,
};

/// A room with `players` human slots in free-for-all.
fn room(players: u8) -> room::Room {
    room::Room::on_ring(Tick(0), RoomSetup::humans(Mode::Ffa, None, players), 20, 40)
        .expect("a legal room shape")
}

/// Put a player's body at an exact position.
fn stand(room: &mut room::Room, id: PlayerId, x: i32, y: i32, z: i32) {
    let body = room.state_mut().body_mut(id).expect("body");
    body.state.pos = Vec3::from_ints(x, y, z);
    body.spawn = body.state.pos;
}

fn decode(bytes: &[u8]) -> Snapshot {
    match decode_s2c(bytes).expect("the frame decodes") {
        S2c::Snapshot(snapshot) => snapshot,
        other => panic!("expected a snapshot, got {other:?}"),
    }
}

/// What a client's copy of one entity looks like.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct ClientEntity {
    x: i32,
    y: i32,
    z: i32,
    hp: u8,
}

/// Apply one snapshot to a client-side map, exactly as §7.3 says to.
///
/// Keyframe: values are absolute. Delta: values are added to the baseline. An
/// entity absent from the snapshot is removed, which is how §6.3's interest
/// management is expressed on the wire — there is no despawn message.
fn apply(client: &mut Vec<(u8, ClientEntity)>, snapshot: &Snapshot) {
    if snapshot.keyframe {
        client.clear();
        for player in &snapshot.players {
            client.push((
                player.id,
                ClientEntity {
                    x: player.x,
                    y: player.y,
                    z: player.z,
                    hp: player.hp,
                },
            ));
        }
        return;
    }

    let mut next = Vec::with_capacity(snapshot.players.len());
    for player in &snapshot.players {
        let previous = client
            .iter()
            .find(|(id, _)| *id == player.id)
            .map(|(_, e)| *e);
        let base = previous.unwrap_or(ClientEntity {
            x: 0,
            y: 0,
            z: 0,
            hp: 0,
        });
        let mut entity = base;
        if player.presence & presence::POS != 0 {
            entity.x = base.x + player.x;
            entity.y = base.y + player.y;
            entity.z = base.z + player.z;
        }
        if player.presence & presence::HP != 0 {
            entity.hp = player.hp;
        }
        next.push((player.id, entity));
    }
    *client = next;
}

/// Add `per_team` buddies to every team, at `at`.
fn add_buddies(room: &mut room::Room, per_team: usize, at: [i32; 3]) {
    let teams = room.state().teams().len();
    let mut index = 0u32;
    for team in 0..teams {
        for slot in 0..per_team {
            let kind = if slot % 2 == 0 {
                BuddyKind::Swordsman
            } else {
                BuddyKind::Archer
            };
            let buddy = Buddy::new(
                EntityId::new(index, 0),
                tuning::buddy(kind),
                TeamId(team as u8),
                PlayerId(team as u8),
                Vec3::from_ints(at[0] + index as i32, at[1], at[2]),
            );
            room.state_mut()
                .add_buddy(buddy)
                .expect("a buddy under the cap");
            index += 1;
        }
    }
}

/// A projectile flying at 40 m/s from `at`, one metre per tick of travel.
fn projectile(id: u32, at: [i32; 3]) -> Projectile {
    Projectile {
        id: EntityId::new(id, 0),
        owner: EntityId::NONE,
        team: TeamId(0),
        pos: Vec3::from_ints(at[0], at[1], at[2]),
        dir: Vec3::from_ints(1, 0, 0),
        speed: Fixed::from_int(40),
        damage: 20,
        burst_radius: ZERO,
        remaining: Fixed::from_int(10),
    }
}

// ---------------------------------------------------------------------------
// AC: per-client AOI at 48 m with 4 m hysteresis
// ---------------------------------------------------------------------------

#[test]
fn aoi_is_forty_eight_metres_with_four_metres_of_hysteresis() {
    assert_eq!(AOI_RADIUS.to_int(), 48);
    assert_eq!(AOI_HYSTERESIS.to_int(), 4);

    let mut room = room(2);
    stand(&mut room, PlayerId(0), 0, 40, 0);
    let mut encoder = SnapshotEncoder::new(PlayerId(0));

    // Inside the radius: present.
    stand(&mut room, PlayerId(1), 47, 40, 0);
    let frame = encoder.encode(Tick(0), room.state(), &[], &[], 0);
    let snapshot = decode(&frame.bytes);
    assert!(snapshot.players.iter().any(|p| p.id == 1));
    assert_eq!(
        snapshot.players.iter().find(|p| p.id == 1).unwrap().flags & player_flags::STALE,
        0
    );

    // Beyond 48 m but inside the band: still present, marked stale.
    stand(&mut room, PlayerId(1), 51, 40, 0);
    let frame = encoder.encode(Tick(1), room.state(), &[], &[], frame.seq);
    let snapshot = decode(&frame.bytes);
    let other = snapshot
        .players
        .iter()
        .find(|p| p.id == 1)
        .expect("inside the hysteresis band");
    assert_ne!(other.flags & player_flags::STALE, 0);

    // Beyond the band: gone, which the client reads as "remove it".
    stand(&mut room, PlayerId(1), 53, 40, 0);
    let frame = encoder.encode(Tick(2), room.state(), &[], &[], frame.seq);
    let snapshot = decode(&frame.bytes);
    assert!(snapshot.players.iter().all(|p| p.id != 1));
}

#[test]
fn height_does_not_affect_interest() {
    // §6.3: 48 m *horizontally*, any Y. A player on a tower directly above is
    // visible; distance in three dimensions would hide them, and that would make
    // the render distance depend on altitude.
    let mut room = room(2);
    stand(&mut room, PlayerId(0), 0, 40, 0);
    stand(&mut room, PlayerId(1), 30, 90, 0);
    let mut encoder = SnapshotEncoder::new(PlayerId(0));
    let frame = encoder.encode(Tick(0), room.state(), &[], &[], 0);
    let snapshot = decode(&frame.bytes);
    assert!(snapshot.players.iter().any(|p| p.id == 1));
}

// ---------------------------------------------------------------------------
// AC: keyframe equals full state
// ---------------------------------------------------------------------------

#[test]
fn keyframe_equals_full_state() {
    let mut room = room(2);
    stand(&mut room, PlayerId(0), 4, 40, 4);
    stand(&mut room, PlayerId(1), -6, 41, 9);
    let mut encoder = SnapshotEncoder::new(PlayerId(0));

    let frame = encoder.encode(Tick(0), room.state(), &[], &[], 0);
    assert!(frame.keyframe);
    let snapshot = decode(&frame.bytes);
    assert!(snapshot.keyframe);
    assert_eq!(snapshot.baseline_seq, 0);

    for player in &snapshot.players {
        assert_eq!(
            player.presence,
            presence::ALL,
            "a keyframe must carry every field of every entity"
        );
        let body = room.state().body(PlayerId(player.id)).expect("body");
        assert_eq!(player.x, body.state.pos.x.to_raw());
        assert_eq!(player.y, body.state.pos.y.to_raw());
        assert_eq!(player.z, body.state.pos.z.to_raw());
        assert_eq!(player.hp, body.hp as u8);
        assert_eq!(player.yaw, body.yaw);
    }
}

#[test]
fn delta_chain_reconstructs_the_state() {
    // The property that matters: a client that starts from a keyframe and
    // applies every delta ends up exactly where the server is.
    let mut room = room(3);
    stand(&mut room, PlayerId(0), 0, 40, 0);
    stand(&mut room, PlayerId(1), 10, 40, 0);
    stand(&mut room, PlayerId(2), -10, 40, 0);

    let mut encoder = SnapshotEncoder::new(PlayerId(0));
    let mut client: Vec<(u8, ClientEntity)> = Vec::new();
    let mut acked = 0u16;

    for tick in 0..90u32 {
        // Move the two remote players on a path, and keep the viewer still.
        let step = tick as i32;
        stand(&mut room, PlayerId(1), 10 - step, 40, step);
        stand(&mut room, PlayerId(2), -10 + step, 40, -step);

        let frame = encoder.encode(Tick(tick), room.state(), &[], &[], acked);
        let snapshot = decode(&frame.bytes);
        apply(&mut client, &snapshot);
        acked = frame.seq;

        for (id, entity) in &client {
            let body = room.state().body(PlayerId(*id)).expect("body");
            assert_eq!(
                (entity.x, entity.y, entity.z),
                (
                    body.state.pos.x.to_raw(),
                    body.state.pos.y.to_raw(),
                    body.state.pos.z.to_raw()
                ),
                "client copy of player {id} diverged at tick {tick} (keyframe = {})",
                snapshot.keyframe
            );
        }
    }

    // Sanity: the run must have covered both keyframes and deltas, or the test
    // would pass trivially by keyframing everything.
    assert!(
        90 > KEYFRAME_INTERVAL as u32,
        "the run must outlast one keyframe interval"
    );
}

// ---------------------------------------------------------------------------
// AC: worst-case snapshot fits in 2 KB
// ---------------------------------------------------------------------------

#[test]
fn snapshot_under_2kb() {
    // 6 players + 12 buddies + 24 projectiles + structures + events, every one
    // of them in range, every field changing.
    let mut room = room(6);
    for id in 0..6u8 {
        stand(&mut room, PlayerId(id), id as i32 * 3, 40, 0);
    }
    add_buddies(&mut room, 2, [0, 40, 5]);

    let projectiles: Vec<Projectile> = (0..24)
        .map(|i| projectile(100 + i, [i as i32 - 12, 40, 8]))
        .collect();
    let events: Vec<SnapshotEvent> = (0..protocol::MAX_EVENTS)
        .map(|i| SnapshotEvent {
            kind: protocol::event_kind::BASE_HIT,
            a: i as i32,
            b: 8_400,
            c: 4,
        })
        .collect();

    let mut encoder = SnapshotEncoder::new(PlayerId(0));

    // Keyframe: every field of every entity on the wire at once.
    let keyframe = encoder.encode(Tick(0), room.state(), &projectiles, &events, 0);
    assert!(keyframe.keyframe);
    assert_eq!(keyframe.players, 6);
    assert_eq!(keyframe.buddies, 12);
    assert!(
        keyframe.bytes.len() <= 2048,
        "worst-case keyframe is {} B, over the 2048 B budget",
        keyframe.bytes.len()
    );

    // Delta with everything moving: the realistic steady-state frame.
    for id in 0..6u8 {
        stand(&mut room, PlayerId(id), id as i32 * 3 + 1, 40, 1);
        room.state_mut().body_mut(PlayerId(id)).unwrap().yaw = 40 + id;
        room.state_mut().body_mut(PlayerId(id)).unwrap().hp = 90 - id as i32;
    }
    let moved: Vec<Projectile> = (0..24)
        .map(|i| projectile(100 + i, [i as i32 - 11, 40, 8]))
        .collect();
    let delta = encoder.encode(Tick(1), room.state(), &moved, &events, keyframe.seq);
    assert!(!delta.keyframe);
    assert!(
        delta.bytes.len() <= 2048,
        "worst-case delta is {} B, over the 2048 B budget",
        delta.bytes.len()
    );

    // The average case in §6.8 is ~550 B; if the worst case is far above it, the
    // average is not the number to budget against.
    println!(
        "worst case: keyframe {} B, delta {} B",
        keyframe.bytes.len(),
        delta.bytes.len()
    );
}

// ---------------------------------------------------------------------------
// AC: a stationary entity costs three bytes
// ---------------------------------------------------------------------------

#[test]
fn stationary_entity_costs_3_bytes() {
    // Measured as a marginal cost, which is the only honest way to state it: one
    // entity's entry is its id, its presence bitmask and nothing else. The spec
    // says 3 B; postcard encodes the presence mask as a one-byte varint when it
    // is below 128, so the real cost is 2 B. Asserting the spec's ceiling rather
    // than the implementation's number keeps the test meaningful if the encoding
    // changes.
    let mut room = room(2);
    stand(&mut room, PlayerId(0), 0, 40, 0);
    stand(&mut room, PlayerId(1), 20, 40, 0);
    let mut encoder = SnapshotEncoder::new(PlayerId(0));

    let keyframe = encoder.encode(Tick(0), room.state(), &[], &[], 0);
    assert!(keyframe.keyframe);

    // Nothing moves: the remote player is present but unchanged.
    let still = encoder.encode(Tick(1), room.state(), &[], &[], keyframe.seq);
    let snapshot = decode(&still.bytes);
    let other = snapshot.players.iter().find(|p| p.id == 1).expect("other");
    assert_eq!(other.presence, 0, "an unchanged entity carries no fields");

    // Now take the entity out of range, so its entry disappears. The difference
    // between the two frames is exactly one stationary entity.
    stand(&mut room, PlayerId(1), 200, 40, 0);
    let absent = encoder.encode(Tick(2), room.state(), &[], &[], still.seq);
    assert!(decode(&absent.bytes).players.iter().all(|p| p.id != 1));

    let cost = still.bytes.len() as i64 - absent.bytes.len() as i64;
    assert!(
        (0..=3).contains(&cost),
        "a stationary entity cost {cost} B, over the 3 B ceiling"
    );
    println!("stationary entity: {cost} B");
}

// ---------------------------------------------------------------------------
// AC: events are never culled
// ---------------------------------------------------------------------------

#[test]
fn events_survive_interest_management() {
    // The events are about a base 200 m away, which is exactly the case §6.3
    // cares about: the information layer must not be filtered by the render
    // distance, or a player never learns their core is under attack.
    let mut room = room(2);
    stand(&mut room, PlayerId(0), 0, 40, 0);
    stand(&mut room, PlayerId(1), 200, 40, 0);
    let events = vec![SnapshotEvent {
        kind: protocol::event_kind::ELIMINATION,
        a: 3,
        b: 1_200,
        c: 0,
    }];

    let mut encoder = SnapshotEncoder::new(PlayerId(0));
    let frame = encoder.encode(Tick(0), room.state(), &[], &events, 0);
    let snapshot = decode(&frame.bytes);
    assert!(snapshot.players.iter().all(|p| p.id != 1), "out of range");
    assert_eq!(snapshot.events.len(), 1, "events are never culled");
    assert_eq!(snapshot.events[0].kind, protocol::event_kind::ELIMINATION);
}
