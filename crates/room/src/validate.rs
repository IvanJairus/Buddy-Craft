//! Anti-cheat primitives that sit above the rules (task T-050, §11).
//!
//! The simulation already owns movement, reach, damage and rate limits. This
//! module owns the connection-scoped pieces the host needs: a per-IP seat cap,
//! room-scoped tokens and elimination revocation. It is pure state, so the
//! threat matrix can exercise it without a socket.

use std::collections::BTreeMap;

use sim::PlayerId;

/// Maximum simultaneous connections from one address (§11.3).
pub const MAX_CONNECTIONS_PER_IP: u8 = 3;

/// A per-IP connection budget.
#[derive(Clone, Debug, Default)]
pub struct ConnectionBudget {
    open: BTreeMap<String, u8>,
}

impl ConnectionBudget {
    /// Try to admit one connection from `ip`.
    pub fn open(&mut self, ip: &str) -> bool {
        let count = self.open.entry(ip.to_owned()).or_insert(0);
        if *count >= MAX_CONNECTIONS_PER_IP {
            return false;
        }
        *count += 1;
        true
    }

    /// Release one connection from `ip`.
    pub fn close(&mut self, ip: &str) {
        if let Some(count) = self.open.get_mut(ip) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.open.remove(ip);
            }
        }
    }

    /// Connections currently admitted for `ip`.
    pub fn count(&self, ip: &str) -> u8 {
        self.open.get(ip).copied().unwrap_or(0)
    }
}

#[derive(Clone, Copy, Debug)]
struct Token {
    room: u64,
    player: PlayerId,
    revoked: bool,
}

/// Room-scoped reconnect tokens (§6.11, §11.2 T14/T15).
#[derive(Clone, Debug, Default)]
pub struct TokenBook {
    tokens: BTreeMap<[u8; 16], Token>,
}

impl TokenBook {
    /// Issue a token for a player in a room.
    pub fn issue(&mut self, room: u64, player: PlayerId, token: [u8; 16]) {
        self.tokens.insert(
            token,
            Token {
                room,
                player,
                revoked: false,
            },
        );
    }

    /// Accept a token only for the same room and before revocation.
    pub fn accept(&self, room: u64, token: &[u8; 16]) -> Option<PlayerId> {
        self.tokens
            .get(token)
            .filter(|entry| entry.room == room && !entry.revoked)
            .map(|entry| entry.player)
    }

    /// Invalidate every token for a player, e.g. on elimination.
    pub fn revoke_player(&mut self, player: PlayerId) {
        for entry in self.tokens.values_mut() {
            if entry.player == player {
                entry.revoked = true;
            }
        }
    }

    /// Invalidate every token in a room, e.g. at cleanup.
    pub fn revoke_room(&mut self, room: u64) {
        for entry in self.tokens.values_mut() {
            if entry.room == room {
                entry.revoked = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: [u8; 16] = [7; 16];

    #[test]
    fn t9_connection_flood_cap() {
        let mut budget = ConnectionBudget::default();
        assert!(budget.open("203.0.113.9"));
        assert!(budget.open("203.0.113.9"));
        assert!(budget.open("203.0.113.9"));
        assert!(
            !budget.open("203.0.113.9"),
            "the fourth socket must be refused"
        );
        budget.close("203.0.113.9");
        assert!(budget.open("203.0.113.9"));
        assert!(budget.open("203.0.113.10"), "another address is unaffected");
    }

    #[test]
    fn t14_eliminated_player_token_is_revoked() {
        let mut book = TokenBook::default();
        book.issue(11, PlayerId(4), TOKEN);
        assert_eq!(book.accept(11, &TOKEN), Some(PlayerId(4)));
        book.revoke_player(PlayerId(4));
        assert_eq!(book.accept(11, &TOKEN), None, "elimination is permanent");
    }

    #[test]
    fn t15_token_is_room_scoped_and_reuse_fails_after_cleanup() {
        let mut book = TokenBook::default();
        book.issue(11, PlayerId(4), TOKEN);
        assert_eq!(
            book.accept(12, &TOKEN),
            None,
            "a token from another room is not a credential"
        );
        book.revoke_room(11);
        assert_eq!(book.accept(11, &TOKEN), None);
    }
}
