//! Inbound validation — §7.4, non-negotiable.
//!
//! [`crate::C2s::decode`] answers "can these bytes be a message". This module
//! answers "will this server act on it", and the two are different questions
//! with different failure modes:
//!
//! | Failure | Response |
//! | --- | --- |
//! | Frame over 4 KB | close, code 1009 |
//! | Unknown tag, or a payload that does not match it | close, code 1008 |
//! | Token bucket empty | close, code 1008 |
//! | Bad name, empty chat, out-of-bounds edit | reject, connection stays open |
//!
//! The split matters because a malformed stream is a bug or an attack and both
//! deserve termination, while aiming at the sky is something players do. Rule 4
//! of §7.4 says exactly that: an out-of-bounds edit is answered with a reason,
//! not a disconnect.
//!
//! # Sanitisation returns a message, not a verdict
//!
//! [`decode_checked`] hands back a [`C2s`] whose name and chat are already
//! canonical. A function that only *reported* "this is dirty" would leave the
//! caller holding the dirty copy, and the next person to add a caller would have
//! to remember to clean it. Returning the clean value makes the sanitised form
//! the only one that escapes the codec.

use core::fmt;

use sim::Tick;

use crate::{
    edit_action, edit_reject, C2s, Chat, DecodeError, FixedBytes, Hello, CHAT_LEN, MAX_FRAME_BYTES,
    NAME_LEN,
};

/// Largest name after trimming (§7.4 rule 7).
pub const MAX_NAME_LEN: usize = NAME_LEN;

/// Smallest accepted name.
pub const MIN_NAME_LEN: usize = 3;

/// Chat messages per second, per connection.
pub const CHAT_RATE_PER_SECOND: u32 = 2;

/// Messages per second, per connection (§7.4 rule 6).
pub const MESSAGE_RATE_PER_SECOND: u32 = 30;

/// Messages a connection may send back to back (§7.4 rule 6).
pub const MESSAGE_BURST: u32 = 60;

/// The world's coordinate bounds, for §7.4 rule 4.
///
/// Deliberately derived from `world` rather than restated: a protocol that
/// believed in a different world size than the generator would accept edits to
/// cells that do not exist.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Bounds {
    /// Exclusive upper bound on X.
    pub x: u16,
    /// Exclusive upper bound on Y.
    pub y: u16,
    /// Exclusive upper bound on Z.
    pub z: u16,
}

impl Bounds {
    /// The shipped world: 256 × 96 × 256 (§4.3).
    pub const STANDARD: Bounds = Bounds {
        x: world::gen::WORLD_X as u16,
        y: world::chunk::CHUNK_Y as u16,
        z: world::gen::WORLD_Z as u16,
    };

    /// Build bounds from exclusive upper limits.
    pub const fn new(x: u16, y: u16, z: u16) -> Bounds {
        Bounds { x, y, z }
    }

    /// True when the cell exists.
    pub const fn contains(&self, x: i16, y: i16, z: i16) -> bool {
        x >= 0
            && (x as u16) < self.x
            && y >= 0
            && (y as u16) < self.y
            && z >= 0
            && (z as u16) < self.z
    }
}

/// Why an inbound frame was refused.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Error {
    /// The frame is larger than [`MAX_FRAME_BYTES`]. Fatal: §7.4 rule 1.
    Oversize {
        /// The frame's actual length.
        len: usize,
    },
    /// The frame is not a message this protocol defines. Fatal: rules 2 and 3.
    Decode(DecodeError),
    /// The token bucket is empty. Fatal: rule 6.
    RateLimited,
    /// The name is empty, too short, too long, or contains a character outside
    /// `[A-Za-z0-9_-]`. Not fatal: rule 7.
    BadName,
    /// The chat message is empty after stripping. Not fatal: rule 5.
    EmptyChat,
    /// A well-formed edit outside the world. Not fatal: rule 4.
    EditRejected {
        /// One of [`edit_reject`], for the `S_EDIT_ACK`.
        reason: u8,
        /// The client's sequence number, echoed so the caller can answer.
        seq: u16,
    },
}

impl Error {
    /// The WebSocket close code this error demands, or `None` to stay open.
    pub fn close_code(&self) -> Option<u16> {
        match self {
            Error::Oversize { .. } => Some(1009),
            Error::Decode(_) | Error::RateLimited => Some(1008),
            Error::BadName | Error::EmptyChat | Error::EditRejected { .. } => None,
        }
    }

    /// True when the connection should be closed rather than answered.
    pub fn is_fatal(&self) -> bool {
        self.close_code().is_some()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Oversize { len } => {
                write!(f, "frame is {len} B, limit {MAX_FRAME_BYTES} B")
            }
            Error::Decode(e) => write!(f, "{e}"),
            Error::RateLimited => write!(f, "rate limited"),
            Error::BadName => write!(f, "name must be 3-16 of [A-Za-z0-9_-]"),
            Error::EmptyChat => write!(f, "chat is empty after sanitisation"),
            Error::EditRejected { reason, seq } => {
                write!(f, "edit {seq} rejected with reason {reason}")
            }
        }
    }
}

impl std::error::Error for Error {}

/// True for the bytes a name may contain.
#[inline]
fn name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// True for the bytes a chat message may contain.
///
/// `[A-Za-z0-9 _.,!?-]` — the set in §7.4 rule 5. No quotes, no brackets, no
/// `<` or `>`. That is stricter than it needs to be for a client that renders
/// chat as text rather than HTML, and the strictness is the point: the rule has
/// to hold for every client, including the one somebody writes in six months.
#[inline]
fn chat_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b' ' | b'_' | b'.' | b',' | b'!' | b'?' | b'-')
}

/// Strip and validate a name, returning its canonical form.
///
/// Trailing NULs are padding, not content: `[u8; 16]` cannot carry a length, so
/// a name ends at the first zero byte.
pub fn sanitise_name(raw: &FixedBytes<NAME_LEN>) -> Result<FixedBytes<NAME_LEN>, Error> {
    let end = raw.0.iter().position(|&b| b == 0).unwrap_or(NAME_LEN);
    let text = &raw.0[..end];
    if text.len() < MIN_NAME_LEN || !text.iter().all(|&b| name_byte(b)) {
        return Err(Error::BadName);
    }
    // Already canonical: no padding to strip, no characters to drop, and the
    // length is within bounds. A name that passes is its own sanitiser, which is
    // the property that makes it safe to compare two names for uniqueness.
    Ok(*raw)
}

/// Strip a chat message to `[A-Za-z0-9 _.,!?-]{0,120}`.
///
/// Returns the *cleaned* message, not a boolean, so the caller cannot keep the
/// original by accident. Disallowed bytes are dropped rather than replaced:
/// replacing them with spaces would let a message of 120 punctuation marks become
/// 120 spaces, which is not what the sender typed and not obviously nothing.
pub fn sanitise_chat(raw: &Chat) -> Result<Chat, Error> {
    let mut out = FixedBytes::<CHAT_LEN>::new();
    let mut len = 0usize;
    for &b in raw.text() {
        if chat_byte(b) && len < CHAT_LEN {
            out.0[len] = b;
            len += 1;
        }
    }
    if len == 0 {
        return Err(Error::EmptyChat);
    }
    Ok(Chat {
        len: len as u8,
        text: out,
    })
}

/// Validate one inbound frame against every rule in §7.4 that can be checked
/// without per-connection state.
///
/// Rules 1–5 and 7 are here. Rule 6 (the token bucket) is stateful, so it lives
/// in [`RateLimiter`]; rule 7's "uniqueness per room" needs the room's roster and
/// belongs there too — this function checks the name's *format*, which is the
/// part that must never be skipped.
///
/// `world` is passed in rather than assumed so a test can shrink it.
pub fn decode_checked(bytes: &[u8], world: Bounds) -> Result<C2s, Error> {
    // Rule 1, before touching the payload: a 4 KB frame is cheap to reject and a
    // 4 MB frame must never be parsed.
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(Error::Oversize { len: bytes.len() });
    }

    // Rules 2 and 3. A decode error is fatal, not "ignore this one".
    let message = C2s::decode(bytes).map_err(Error::Decode)?;

    Ok(match message {
        C2s::Hello(hello) => C2s::Hello(Hello {
            name: sanitise_name(&hello.name)?,
            ..hello
        }),
        C2s::Chat(chat) => C2s::Chat(sanitise_chat(&chat)?),
        C2s::Edit(edit) => {
            if !world.contains(edit.x, edit.y, edit.z) {
                return Err(Error::EditRejected {
                    reason: edit_reject::OUT_OF_BOUNDS,
                    seq: edit.seq,
                });
            }
            // A break and a place are the only actions; anything else is a client
            // bug, and answering with a reason is cheaper than closing.
            if edit.action != edit_action::BREAK && edit.action != edit_action::PLACE {
                return Err(Error::EditRejected {
                    reason: edit_reject::UNKNOWN_BLOCK,
                    seq: edit.seq,
                });
            }
            C2s::Edit(edit)
        }
        other => other,
    })
}

/// A per-connection token bucket (§7.4 rule 6).
///
/// Integer arithmetic in millitokens, so the bucket is exactly reproducible: a
/// float refill would let two identical connections that arrived in a different
/// order get different verdicts on the same frame, which is the sort of thing
/// that turns into "the game kicks me on my phone" and takes a week to find.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RateLimiter {
    per_second: u32,
    capacity: u32,
    millitokens: u32,
    last_tick: Tick,
}

impl RateLimiter {
    /// A bucket that refills `per_second` messages a second and holds `burst`.
    pub const fn new(per_second: u32, burst: u32) -> RateLimiter {
        RateLimiter {
            per_second,
            capacity: burst * 1000,
            millitokens: burst * 1000,
            last_tick: Tick(0),
        }
    }

    /// The §7.4 rule 6 bucket: 30/s with a burst of 60.
    pub const fn messages() -> RateLimiter {
        RateLimiter::new(MESSAGE_RATE_PER_SECOND, MESSAGE_BURST)
    }

    /// The §6.6 edit bucket: 8/s with a burst of 8, so a player cannot bank a
    /// minute of edits and then spend them in one tick.
    pub const fn edits() -> RateLimiter {
        RateLimiter::new(8, 8)
    }

    /// Millitokens currently available. A message costs 1,000.
    pub fn available(&self) -> u32 {
        self.millitokens
    }

    /// Refill for the time since the last call.
    fn refill(&mut self, now: Tick) {
        // `saturating_sub` rather than a wrapping difference: a tick that went
        // backwards must grant nothing. Ticks wrap by design (§`sim::time`), and
        // a wrapped tick is not 6.8 years of credit.
        let elapsed = now.0.saturating_sub(self.last_tick.0);
        if elapsed == 0 {
            return;
        }
        self.last_tick = now;
        let gain = (elapsed as u64 * self.per_second as u64 * 1000) / sim::time::TICK_HZ as u64;
        self.millitokens = (self.millitokens as u64 + gain).min(self.capacity as u64) as u32;
    }

    /// Spend one message, or report that the bucket is empty.
    pub fn try_take(&mut self, now: Tick) -> bool {
        self.refill(now);
        if self.millitokens >= 1000 {
            self.millitokens -= 1000;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        edit_action, Chat, Edit, FixedBytes, Input, Ping, Ready, Report, TeamChoice, WireInput,
        CHAT_LEN, INPUT_FRAMES, MAX_FRAME_BYTES, TOKEN_LEN,
    };

    fn hello(name: &str) -> C2s {
        C2s::Hello(Hello {
            protocol: crate::PROTOCOL_VERSION,
            name: FixedBytes::from_str_lossy(name),
            room_code: FixedBytes(*b"AB12CD"),
            token: None,
        })
    }

    fn chat(text: &str) -> C2s {
        let mut bytes = FixedBytes::<CHAT_LEN>::new();
        let raw = text.as_bytes();
        let n = raw.len().min(CHAT_LEN);
        bytes.0[..n].copy_from_slice(&raw[..n]);
        C2s::Chat(Chat {
            len: n as u8,
            text: bytes,
        })
    }

    fn checked(message: &C2s) -> Result<C2s, Error> {
        decode_checked(&message.encode(), Bounds::STANDARD)
    }

    // -- frame-level rules --------------------------------------------------

    #[test]
    fn rejects_oversize() {
        // Rule 1. 4,097 bytes is the first size that fails; 4,096 is the last
        // that does not.
        let frame = vec![0u8; MAX_FRAME_BYTES + 1];
        assert_eq!(
            decode_checked(&frame, Bounds::STANDARD),
            Err(Error::Oversize {
                len: MAX_FRAME_BYTES + 1
            })
        );
        assert_eq!(frame.len(), 4097);
        assert_eq!(Error::Oversize { len: 4097 }.close_code(), Some(1009));

        // Exactly at the cap is not oversize — it will fail as a bad payload
        // instead, which is the right error for a frame full of zeroes.
        let at_cap = vec![0u8; MAX_FRAME_BYTES];
        assert!(matches!(
            decode_checked(&at_cap, Bounds::STANDARD),
            Err(Error::Decode(_))
        ));
    }

    #[test]
    fn unknown_tag_is_fatal() {
        for tag in [0x00u8, 0x10, 0x42, 0x81, 0xFF] {
            let err = decode_checked(&[tag], Bounds::STANDARD).unwrap_err();
            assert!(matches!(err, Error::Decode(DecodeError::UnknownTag(t)) if t == tag));
            assert_eq!(err.close_code(), Some(1008), "tag {tag:#04x}");
        }
    }

    #[test]
    fn truncated_payload_is_fatal() {
        // A valid tag with half a payload: a malformed stream, so close.
        let full = C2s::Input(Input {
            seq: 1,
            first_tick: Tick(10),
            frames: [WireInput::default(); INPUT_FRAMES],
        })
        .encode();
        let err = decode_checked(&full[..full.len() - 3], Bounds::STANDARD).unwrap_err();
        assert!(err.is_fatal(), "{err}");
        assert_eq!(err.close_code(), Some(1008));
    }

    #[test]
    fn empty_frame_is_fatal() {
        assert!(decode_checked(&[], Bounds::STANDARD)
            .unwrap_err()
            .is_fatal());
    }

    // -- names --------------------------------------------------------------

    #[test]
    fn accepts_legal_names() {
        for name in ["ivan", "Ivan_Jairus", "bot-01", "abc", "a123456789012345"] {
            let out = checked(&hello(name)).expect(name);
            match out {
                C2s::Hello(h) => assert_eq!(h.name.as_slice(), name.as_bytes()),
                other => panic!("expected hello, got {other:?}"),
            }
        }
    }

    #[test]
    fn rejects_illegal_names() {
        for name in [
            "",       // empty
            "ab",     // too short
            "a b",    // space
            "ivan!",  // punctuation
            "ivan<b", // angle bracket
            "ïvan",   // non-ASCII
        ] {
            assert_eq!(checked(&hello(name)), Err(Error::BadName), "name {name:?}");
        }
        // Sixteen characters is the maximum, and a full array has no NUL, so the
        // name runs to the end rather than being rejected as unterminated.
        assert!(checked(&hello("abcdefghijklmnop")).is_ok());
    }

    #[test]
    fn a_rejected_name_does_not_close_the_connection() {
        // Rule 7 is a rejection, not a protocol violation: the client can send a
        // different name on the same connection.
        let err = checked(&hello("ab")).unwrap_err();
        assert_eq!(err.close_code(), None);
    }

    #[test]
    fn name_sanitisation_is_idempotent() {
        let once = checked(&hello("ivan")).unwrap();
        let twice = decode_checked(&once.encode(), Bounds::STANDARD).unwrap();
        assert_eq!(once, twice);
    }

    // -- chat ---------------------------------------------------------------

    #[test]
    fn strips_xss() {
        // The AC's example. Every byte that could start a tag, close a tag or
        // break out of an attribute is gone; what is left is the letters, which
        // render as text in any client.
        let out = checked(&chat("<img src=x onerror=alert(1)>")).unwrap();
        let text = match out {
            C2s::Chat(c) => c.text().to_vec(),
            other => panic!("expected chat, got {other:?}"),
        };
        assert_eq!(text, b"img srcx onerroralert1");
        for bad in b"<>()\"'&=;/" {
            assert!(
                !text.contains(bad),
                "{:?} survived sanitisation: {}",
                *bad as char,
                String::from_utf8_lossy(&text)
            );
        }
        assert!(text.iter().all(|&b| chat_byte(b)));
    }

    #[test]
    fn chat_of_only_punctuation_is_empty_and_rejected() {
        for text in ["<>(){}[]", "\u{0}\u{1}\u{2}", "☃☃☃"] {
            assert_eq!(checked(&chat(text)), Err(Error::EmptyChat), "{text:?}");
        }
        // Empty chat is not fatal either: the client can try again.
        assert_eq!(Error::EmptyChat.close_code(), None);
    }

    #[test]
    fn chat_keeps_every_allowed_character() {
        let allowed = "abcXYZ019 _.,!?-";
        let out = checked(&chat(allowed)).unwrap();
        match out {
            C2s::Chat(c) => assert_eq!(c.text(), allowed.as_bytes()),
            other => panic!("expected chat, got {other:?}"),
        }
    }

    #[test]
    fn chat_is_truncated_to_its_wire_length() {
        // 200 allowed characters cannot be represented: the array is 120 bytes
        // and the length byte cannot exceed that, so the tail is dropped rather
        // than overflowing.
        let out = checked(&chat(&"a".repeat(200))).unwrap();
        match out {
            C2s::Chat(c) => assert_eq!(c.text().len(), CHAT_LEN),
            other => panic!("expected chat, got {other:?}"),
        }
    }

    // -- edits --------------------------------------------------------------

    fn edit(x: i16, y: i16, z: i16, action: u8) -> C2s {
        C2s::Edit(Edit {
            seq: 42,
            x,
            y,
            z,
            block: 3,
            action,
        })
    }

    #[test]
    fn in_bounds_edits_pass() {
        for (x, y, z) in [(0i16, 0i16, 0i16), (255, 95, 255), (128, 40, 64)] {
            assert!(checked(&edit(x, y, z, edit_action::PLACE)).is_ok());
        }
    }

    #[test]
    fn out_of_bounds_edits_are_rejected_not_fatal() {
        // Rule 4: players aim at the sky, so this must not close the connection.
        for (x, y, z) in [
            (-1i16, 0i16, 0i16),
            (0, -1, 0),
            (0, 0, -1),
            (256, 0, 0),
            (0, 96, 0),
            (0, 0, 256),
        ] {
            let err = checked(&edit(x, y, z, edit_action::PLACE)).unwrap_err();
            assert_eq!(
                err,
                Error::EditRejected {
                    reason: edit_reject::OUT_OF_BOUNDS,
                    seq: 42,
                },
                "({x}, {y}, {z}) should be out of bounds"
            );
            assert_eq!(err.close_code(), None);
        }
    }

    #[test]
    fn unknown_edit_action_is_rejected_with_the_sequence_number() {
        // The seq travels with the rejection so the caller can answer with
        // S_EDIT_ACK without decoding the frame a second time.
        let err = checked(&edit(10, 40, 10, 7)).unwrap_err();
        assert_eq!(
            err,
            Error::EditRejected {
                reason: edit_reject::UNKNOWN_BLOCK,
                seq: 42
            }
        );
    }

    // -- pass-through -------------------------------------------------------

    #[test]
    fn harmless_messages_pass_unchanged() {
        for message in [
            C2s::Ready(Ready { ready: true }),
            C2s::TeamChoice(TeamChoice { team: 2 }),
            C2s::Ping(Ping { t: 12_345 }),
            C2s::Report(Report {
                kind: 1,
                tick: Tick(9_000),
                hash: 0xABCD_1234,
            }),
        ] {
            assert_eq!(checked(&message), Ok(message));
        }
    }

    #[test]
    fn hello_keeps_its_token_through_sanitisation() {
        let message = C2s::Hello(Hello {
            protocol: crate::PROTOCOL_VERSION,
            name: FixedBytes::from_str_lossy("ivan"),
            room_code: FixedBytes(*b"AB12CD"),
            token: Some(FixedBytes([9u8; TOKEN_LEN])),
        });
        assert_eq!(checked(&message), Ok(message));
    }

    // -- the token bucket ---------------------------------------------------

    #[test]
    fn burst_then_rate_limit() {
        let mut bucket = RateLimiter::messages();
        // 60 back to back at tick 0: all allowed.
        for i in 0..MESSAGE_BURST {
            assert!(bucket.try_take(Tick(0)), "message {i} of the burst");
        }
        // The 61st, in the same tick, is not.
        assert!(!bucket.try_take(Tick(0)));
        // One tick later buys 1.5 messages' worth of tokens, i.e. one message.
        assert!(bucket.try_take(Tick(1)));
        assert!(!bucket.try_take(Tick(1)));
        // And twenty ticks (one second) buys thirty.
        let mut allowed = 0;
        for _ in 0..40 {
            if bucket.try_take(Tick(21)) {
                allowed += 1;
            }
        }
        assert_eq!(allowed, 30, "one second should buy exactly the rate");
    }

    #[test]
    fn sustained_rate_is_the_configured_rate() {
        // 15 messages/s for 10 seconds: well under 30/s, so nothing is dropped
        // after the initial burst is accounted for.
        let mut bucket = RateLimiter::messages();
        let mut dropped = 0;
        for tick in 0..200u32 {
            if tick % 2 == 0 && !bucket.try_take(Tick(tick)) {
                dropped += 1;
            }
        }
        assert_eq!(dropped, 0);
    }

    #[test]
    fn a_backwards_clock_grants_nothing() {
        let mut bucket = RateLimiter::new(30, 1);
        assert!(bucket.try_take(Tick(100)));
        assert!(!bucket.try_take(Tick(100)));
        // A tick that went backwards must not be read as a 4-billion-tick gap.
        assert!(!bucket.try_take(Tick(50)));
        assert_eq!(bucket.available(), 0);
        // It recovers forward from the new position.
        assert!(bucket.try_take(Tick(120)));
    }

    #[test]
    fn edit_bucket_is_tighter_than_the_message_bucket() {
        let mut edits = RateLimiter::edits();
        for i in 0..8 {
            assert!(edits.try_take(Tick(0)), "edit {i}");
        }
        assert!(!edits.try_take(Tick(0)), "the ninth edit in one tick");
    }

    // -- a fuzz-like sweep --------------------------------------------------
    //
    // `cargo fuzz` needs nightly and a separate crate; this runs in CI on the
    // stable toolchain and covers the same property — no panic, no unbounded
    // allocation — over a deterministic corpus. See `fuzz/fuzz_targets/decode.rs`
    // for the libFuzzer harness that covers the same entry point with real
    // coverage guidance.

    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }

        fn byte(&mut self) -> u8 {
            (self.next() & 0xFF) as u8
        }
    }

    #[test]
    fn random_frames_never_panic() {
        let mut rng = Lcg(0x00C0_FFEE);
        for _ in 0..20_000 {
            let len = (rng.next() % 64) as usize;
            let frame: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
            // The only requirement is that this returns rather than panics.
            let _ = decode_checked(&frame, Bounds::STANDARD);
        }
    }

    #[test]
    fn mutated_valid_frames_never_panic() {
        // Every byte of every message replaced by every interesting value, plus
        // single-bit flips. This is the corpus that actually finds decoder bugs,
        // because it starts from bytes that are almost valid.
        let seeds: Vec<Vec<u8>> = [
            hello("ivan"),
            chat("gg ez"),
            edit(10, 40, 10, edit_action::PLACE),
            C2s::Ready(Ready { ready: true }),
            C2s::TeamChoice(TeamChoice { team: 1 }),
        ]
        .iter()
        .map(|m| m.encode())
        .collect();

        for seed in &seeds {
            for index in 0..seed.len() {
                for value in [0x00u8, 0x01, 0x7F, 0x80, 0xFF] {
                    let mut frame = seed.clone();
                    frame[index] = value;
                    let _ = decode_checked(&frame, Bounds::STANDARD);
                }
                for bit in 0..8 {
                    let mut frame = seed.clone();
                    frame[index] ^= 1 << bit;
                    let _ = decode_checked(&frame, Bounds::STANDARD);
                }
            }
            // Truncations, which is how a length field usually gets desynced.
            for cut in 0..seed.len() {
                let _ = decode_checked(&seed[..cut], Bounds::STANDARD);
            }
        }
    }

    #[test]
    fn a_huge_declared_length_does_not_allocate() {
        // A snapshot tag followed by a varint claiming four billion players.
        // serde's Vec visitor allocates cautiously, so this fails on the first
        // missing element instead of asking for 4 GB — the property the fuzz
        // target's "no OOM" criterion is actually about.
        let mut frame = vec![0x84u8];
        frame.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF, 0x0F]);
        assert!(matches!(
            crate::S2c::decode(&frame),
            Err(DecodeError::Payload(_))
        ));
    }
}
