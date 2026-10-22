//! Structured operational events and kill switches (task T-056, §13).
//!
//! One JSON object per line. The host writes these to stdout; a log shipper can
//! parse them without a second format. Thresholds and alert responses live in
//! `docs/RUNBOOK.md`.

use std::env;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};

/// Produce one log line. Split from [`emit`] so a test can parse it.
pub fn line(event: &str, fields: Value) -> String {
    let mut object = Map::new();
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    object.insert("ts_ms".to_owned(), json!(ms));
    object.insert("event".to_owned(), json!(event));
    if let Value::Object(fields) = fields {
        for (key, value) in fields {
            object.insert(key, value);
        }
    }
    Value::Object(object).to_string()
}

/// Write one structured line to stdout.
pub fn emit(event: &str, fields: Value) {
    println!("{}", line(event, fields));
}

/// Runtime switches, read once at boot.
#[derive(Clone, Debug)]
pub struct Switches {
    /// Reject new rooms and upgrades when false.
    pub rooms_enabled: bool,
    /// Oldest `C_HELLO.protocol` accepted.
    pub protocol_min_version: u8,
    /// Optional client build pin, surfaced in health and startup logs.
    pub client_build_override: Option<String>,
}

impl Default for Switches {
    fn default() -> Switches {
        Switches {
            rooms_enabled: true,
            protocol_min_version: 1,
            client_build_override: None,
        }
    }
}

impl Switches {
    /// Read the documented environment variables.
    pub fn from_env() -> Switches {
        Switches {
            rooms_enabled: env::var("ROOMS_ENABLED")
                .map(|value| value != "false" && value != "0")
                .unwrap_or(true),
            protocol_min_version: env::var("PROTOCOL_MIN_VERSION")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(1),
            client_build_override: env::var("CLIENT_BUILD_OVERRIDE").ok(),
        }
    }

    /// Whether a client protocol version may enter.
    pub fn protocol_allowed(&self, version: u32) -> bool {
        version >= u32::from(self.protocol_min_version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_line_is_json_with_event_and_timestamp() {
        let text = line("room_started", json!({"room": "DEV001", "tick": 12u32}));
        let value: Value = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(value["event"], "room_started");
        assert_eq!(value["room"], "DEV001");
        assert_eq!(value["tick"], 12);
        assert!(value["ts_ms"].is_u64());
        assert!(!text.contains('\n'));
    }

    #[test]
    fn kill_switches_have_safe_defaults_and_gate_protocol() {
        let switches = Switches::default();
        assert!(switches.rooms_enabled);
        assert!(switches.protocol_allowed(1));
        assert!(!switches.protocol_allowed(0));

        let strict = Switches {
            rooms_enabled: false,
            protocol_min_version: 3,
            client_build_override: Some("abc123".to_owned()),
        };
        assert!(!strict.rooms_enabled);
        assert!(!strict.protocol_allowed(2));
        assert!(strict.protocol_allowed(3));
        assert_eq!(strict.client_build_override.as_deref(), Some("abc123"));
    }
}
