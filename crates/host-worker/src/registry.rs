//! Room registry policy (task T-047).
//!
//! This module is runtime-agnostic and testable natively: code allocation,
//! collision checks, per-IP brute-force lockout and the daily quota guard. The
//! Durable Object HTTP surface in T-046 is the only thing that calls it.

use std::collections::BTreeMap;

/// Crockford base32, without I, L, O and U.
pub const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// A room code is six symbols.
pub const CODE_LEN: usize = 6;
/// Failed joins before a lockout.
pub const MAX_FAILURES: u8 = 6;
/// Lockout duration: 15 minutes, in milliseconds.
pub const LOCKOUT_MS: u64 = 15 * 60 * 1_000;
/// New rooms are refused at 85% of the daily request budget.
pub const QUOTA_PERCENT: u64 = 85;

/// Why a room could not be created.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CreateError {
    /// The code is already used.
    Exists,
    /// The daily budget guard has tripped.
    Quota,
    /// Not six canonical Crockford symbols.
    BadCode,
}

/// Why a join was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JoinError {
    /// No room with that code.
    NotFound,
    /// Too many failed attempts from this address.
    Locked {
        /// Milliseconds until the address may try again.
        retry_after_ms: u64,
    },
    /// The room exists but has no free slot.
    Full,
}

/// One registered room.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RoomInfo {
    /// Occupied slots.
    pub players: u8,
    /// Slot cap.
    pub max_players: u8,
}

#[derive(Clone, Copy, Debug)]
struct Attempts {
    failures: u8,
    locked_until: u64,
}

/// Daily request accounting. The host records every request; creation is the
/// only operation that is refused at the guard.
#[derive(Clone, Copy, Debug)]
pub struct Quota {
    used: u64,
    budget: u64,
}

impl Quota {
    /// A day starts with a fresh budget.
    pub fn new(budget: u64) -> Quota {
        Quota { used: 0, budget }
    }

    /// Record one request.
    pub fn record(&mut self) {
        self.used = self.used.saturating_add(1);
    }

    /// True when new rooms may still be created.
    pub fn can_create(&self) -> bool {
        self.budget == 0 || self.used.saturating_mul(100) < self.budget * QUOTA_PERCENT
    }
}

/// The registry state. One instance is one Durable Object.
#[derive(Debug)]
pub struct Registry {
    rooms: BTreeMap<String, RoomInfo>,
    attempts: BTreeMap<String, Attempts>,
    quota: Quota,
}

impl Registry {
    /// An empty registry with a daily request budget.
    pub fn new(daily_budget: u64) -> Registry {
        Registry {
            rooms: BTreeMap::new(),
            attempts: BTreeMap::new(),
            quota: Quota::new(daily_budget),
        }
    }

    /// Register a code. Collision-checked; the caller may retry with a new code.
    pub fn create(&mut self, code: &str, players: u8, max_players: u8) -> Result<(), CreateError> {
        self.quota.record();
        let Some(code) = normalise(code) else {
            return Err(CreateError::BadCode);
        };
        if self.rooms.contains_key(&code) {
            return Err(CreateError::Exists);
        }
        if !self.quota.can_create() {
            return Err(CreateError::Quota);
        }
        self.rooms.insert(
            code,
            RoomInfo {
                players,
                max_players,
            },
        );
        Ok(())
    }

    /// True when a code exists. This is the `GET /rooms/:code` answer.
    pub fn exists(&self, code: &str) -> Option<RoomInfo> {
        normalise(code).and_then(|code| self.rooms.get(&code).copied())
    }

    /// Attempt to join from `ip` at `now_ms`.
    pub fn join(&mut self, code: &str, ip: &str, now_ms: u64) -> Result<RoomInfo, JoinError> {
        self.quota.record();
        if let Some(attempts) = self.attempts.get(ip) {
            if attempts.locked_until > now_ms {
                return Err(JoinError::Locked {
                    retry_after_ms: attempts.locked_until - now_ms,
                });
            }
        }
        let Some(code) = normalise(code) else {
            self.fail(ip, now_ms);
            return Err(JoinError::NotFound);
        };
        let Some(room) = self.rooms.get_mut(&code) else {
            self.fail(ip, now_ms);
            return Err(JoinError::NotFound);
        };
        if room.players >= room.max_players {
            self.fail(ip, now_ms);
            return Err(JoinError::Full);
        }
        room.players += 1;
        self.attempts.remove(ip);
        Ok(*room)
    }

    /// The daily quota, for tests and the health endpoint.
    pub fn quota(&self) -> Quota {
        self.quota
    }

    fn fail(&mut self, ip: &str, now_ms: u64) {
        let entry = self.attempts.entry(ip.to_owned()).or_insert(Attempts {
            failures: 0,
            locked_until: 0,
        });
        if entry.locked_until > 0 && entry.locked_until <= now_ms {
            entry.failures = 0;
            entry.locked_until = 0;
        }
        entry.failures = entry.failures.saturating_add(1);
        if entry.failures >= MAX_FAILURES {
            entry.locked_until = now_ms + LOCKOUT_MS;
            entry.failures = 0;
        }
    }
}

/// Uppercase, strip separators, resolve Crockford lookalikes.
pub fn normalise(value: &str) -> Option<String> {
    let mut out = String::with_capacity(CODE_LEN);
    for byte in value.bytes() {
        if byte == b'-' || byte == b' ' {
            continue;
        }
        let upper = byte.to_ascii_uppercase();
        let mapped = match upper {
            b'I' | b'L' => b'1',
            b'O' => b'0',
            other => other,
        };
        if !ALPHABET.contains(&mapped) {
            return None;
        }
        out.push(mapped as char);
    }
    (out.len() == CODE_LEN).then_some(out)
}

/// Generate a code from caller-provided bytes. The caller supplies entropy so
/// the registry itself has no RNG.
pub fn generate(bytes: &[u8; CODE_LEN]) -> String {
    let mut out = String::with_capacity(CODE_LEN);
    for byte in bytes {
        out.push(ALPHABET[(*byte & 0x1f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_crockford_and_collision_checked() {
        let code = generate(&[0, 1, 31, 18, 7, 9]);
        assert_eq!(code.len(), 6);
        assert!(code.bytes().all(|byte| ALPHABET.contains(&byte)));
        assert!(!code.contains(['I', 'L', 'O', 'U']));

        let mut registry = Registry::new(10_000);
        assert_eq!(registry.create(&code, 0, 6), Ok(()));
        assert_eq!(registry.create(&code, 0, 6), Err(CreateError::Exists));
        assert!(registry.exists(&code.to_lowercase()).is_some());
    }

    #[test]
    fn t10_room_code_brute_force() {
        let mut registry = Registry::new(10_000);
        for _ in 0..MAX_FAILURES {
            assert_eq!(
                registry.join("ZZZZZZ", "203.0.113.5", 0),
                Err(JoinError::NotFound)
            );
        }
        assert_eq!(
            registry.join("ZZZZZZ", "203.0.113.5", 1_000),
            Err(JoinError::Locked {
                retry_after_ms: LOCKOUT_MS - 1_000
            })
        );
        assert_eq!(
            registry.join("ZZZZZZ", "203.0.113.5", LOCKOUT_MS + 1),
            Err(JoinError::NotFound),
            "the lockout is a wait, not a permanent ban"
        );
        assert_eq!(
            registry.join("ZZZZZZ", "203.0.113.6", 1_000),
            Err(JoinError::NotFound),
            "another address is unaffected"
        );
    }

    #[test]
    fn quota_rejects_new_rooms_at_eighty_five_percent() {
        let mut registry = Registry::new(1_000);
        for _ in 0..849 {
            registry.quota.record();
        }
        assert!(registry.quota().can_create());
        assert_eq!(
            registry.create("ABCDEF", 0, 6),
            Err(CreateError::Quota),
            "850 requests is 85 percent of 1,000"
        );
    }

    #[test]
    fn joining_increments_until_full() {
        let mut registry = Registry::new(10_000);
        registry.create("ABCDEF", 0, 1).expect("create");
        assert_eq!(
            registry.join("ABCDEF", "ip", 0),
            Ok(RoomInfo {
                players: 1,
                max_players: 1
            })
        );
        assert_eq!(registry.join("ABCDEF", "ip", 1), Err(JoinError::Full));
    }
}
