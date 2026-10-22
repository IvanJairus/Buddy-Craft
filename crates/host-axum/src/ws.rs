//! The `/ws` upgrade and per-connection pump (task T-045).
//!
//! The HTTP layer validates the room code and Origin before upgrading. After
//! that, the connection only moves bytes: decoded client messages become room
//! commands, and encoded server frames go into the connection's bounded queue.
//! It never calls `Match` directly.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use room::ConnectionBudget;
use serde::Deserialize;

use protocol::{C2s, S2c};

use crate::rooms::RoomCommand;
use crate::telemetry;
use crate::AppState;

/// A browser must send `C_HELLO` within this window (§11.2 T9).
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// Per-connection message bucket (§7.4 rule 6).
///
/// Public so `tools/cheat-client` can attempt the flood against the exact
/// guard the socket loop uses.
pub struct MessageRate {
    tokens: f64,
    last: Instant,
}

impl Default for MessageRate {
    fn default() -> Self {
        Self::new()
    }
}

impl MessageRate {
    /// A full bucket.
    pub fn new() -> MessageRate {
        MessageRate {
            tokens: protocol::codec::MESSAGE_BURST as f64,
            last: Instant::now(),
        }
    }

    /// Consume one message, or report that the bucket is empty.
    pub fn allow(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * f64::from(protocol::codec::MESSAGE_RATE_PER_SECOND))
            .min(f64::from(protocol::codec::MESSAGE_BURST));
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

/// Chat is 0.5 messages per second with a burst of two (§11.3).
struct ChatRate {
    tokens: f64,
    last: Instant,
}

impl ChatRate {
    fn new() -> ChatRate {
        ChatRate {
            tokens: 2.0,
            last: Instant::now(),
        }
    }

    fn allow(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * 0.5).min(2.0);
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

/// Releases one per-IP slot when the socket task ends, including early returns.
struct ConnectionGuard {
    budget: Arc<Mutex<ConnectionBudget>>,
    ip: String,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        if let Ok(mut budget) = self.budget.lock() {
            budget.close(&self.ip);
        }
    }
}

/// Query string accepted by `/ws`.
#[derive(Debug, Deserialize)]
pub struct WsQuery {
    /// Six-character room code.
    pub room: Option<String>,
    /// Reconnect token; unused until the registry in T-047.
    pub token: Option<String>,
}

/// `GET /ws`.
pub async fn ws_handler(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Query(query): Query<WsQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let origin = headers.get("origin").and_then(|value| value.to_str().ok());
    if !state.switches.rooms_enabled {
        return (StatusCode::SERVICE_UNAVAILABLE, "rooms are disabled").into_response();
    }
    if !origin_allowed(origin, &state.allowed_origins) {
        return (StatusCode::FORBIDDEN, "origin is not allowed").into_response();
    }
    let Some(room) = query.room.as_deref().and_then(normalise_code) else {
        return (
            StatusCode::BAD_REQUEST,
            "room must be six ASCII letters or digits",
        )
            .into_response();
    };
    let ip = peer.ip().to_string();
    let admitted = state
        .connections
        .lock()
        .map(|mut budget| budget.open(&ip))
        .unwrap_or(false);
    if !admitted {
        telemetry::emit("connection_refused", serde_json::json!({"ip": ip}));
        return (StatusCode::TOO_MANY_REQUESTS, "too many connections").into_response();
    }
    let guard = ConnectionGuard {
        budget: Arc::clone(&state.connections),
        ip,
    };
    ws.max_message_size(protocol::MAX_FRAME_BYTES)
        .max_frame_size(protocol::MAX_FRAME_BYTES)
        .on_upgrade(move |socket| handle_socket(socket, state, room, guard))
}

fn normalise_code(raw: &str) -> Option<String> {
    if raw.len() != protocol::ROOM_CODE_LEN || !raw.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    Some(raw.to_ascii_uppercase())
}

/// An absent Origin is allowed for CLI and tests. A browser Origin must be on
/// the explicit list; the default list is the Vite dev and preview ports.
pub fn origin_allowed(origin: Option<&str>, allowed: &[String]) -> bool {
    match origin {
        None => true,
        Some(value) => allowed.iter().any(|candidate| candidate == value),
    }
}

async fn handle_socket(socket: WebSocket, state: AppState, room: String, guard: ConnectionGuard) {
    let (mut sink, mut stream) = socket.split();

    // T9: no Welcome, no room work, and no seat until the client identifies
    // itself. A socket that stays silent is closed after five seconds.
    let first = tokio::time::timeout(HELLO_TIMEOUT, stream.next()).await;
    let Ok(Some(Ok(Message::Binary(bytes)))) = first else {
        let _ = sink.close().await;
        return;
    };
    let hello = match protocol::codec::decode_checked(&bytes, protocol::codec::Bounds::STANDARD) {
        Ok(C2s::Hello(hello)) => hello,
        _ => {
            let _ = sink.close().await;
            return;
        }
    };
    if !state.switches.protocol_allowed(u32::from(hello.protocol)) {
        let _ = sink.close().await;
        return;
    }
    if hello.room_code.as_slice() != room.as_bytes() {
        let _ = sink.close().await;
        return;
    }
    if !state.e2e && !state.hub.exists(&room).await && !state.creations.allow(&guard.ip) {
        telemetry::emit(
            "room_creation_rate_limited",
            serde_json::json!({"room": room, "ip": guard.ip}),
        );
        let _ = sink.close().await;
        return;
    }

    let (to_client, mut from_room) = tokio::sync::mpsc::channel::<Vec<u8>>(16);
    let room_tx = state.hub.sender(&room).await;
    let client_id = state.hub.next_client_id();
    telemetry::emit(
        "ws_connected",
        serde_json::json!({"room": room, "client": client_id}),
    );
    if room_tx
        .send(RoomCommand::Join {
            id: client_id,
            to_client: to_client.clone(),
            token: hello.token.map(|token| *token.as_array()),
        })
        .await
        .is_err()
    {
        return;
    }

    let writer = tokio::spawn(async move {
        while let Some(bytes) = from_room.recv().await {
            if sink.send(Message::Binary(bytes.into())).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    let mut rate = MessageRate::new();
    let mut chat = ChatRate::new();
    while let Some(message) = stream.next().await {
        let Ok(message) = message else {
            break;
        };
        match message {
            Message::Binary(bytes) => {
                if !rate.allow() {
                    telemetry::emit(
                        "message_rate_limited",
                        serde_json::json!({"room": room, "client": client_id}),
                    );
                    break;
                }
                match protocol::codec::decode_checked(&bytes, protocol::codec::Bounds::STANDARD) {
                    Ok(C2s::Input(packet)) => {
                        if room_tx
                            .send(RoomCommand::Input {
                                id: client_id,
                                packet,
                            })
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Ok(C2s::Edit(edit)) => {
                        if room_tx
                            .send(RoomCommand::Edit {
                                id: client_id,
                                edit,
                            })
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    // Exactly one Hello per connection (§11.3).
                    Ok(C2s::Hello(_)) => break,
                    Ok(C2s::Chat(_)) => {
                        // Dropped silently above 0.5/s; there is no chat
                        // broadcast in v1, but the channel is still limited.
                        if !chat.allow() {
                            continue;
                        }
                    }
                    Ok(C2s::Ping(ping)) => {
                        let _ =
                            to_client.try_send(S2c::Pong(protocol::Pong { t: ping.t }).encode());
                    }
                    Ok(_) => {}
                    Err(error) => {
                        if error.close_code().is_some() {
                            break;
                        }
                    }
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    let _ = room_tx.send(RoomCommand::Leave { id: client_id }).await;
    telemetry::emit(
        "ws_disconnected",
        serde_json::json!({"room": room, "client": client_id}),
    );
    writer.abort();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app, default_origins};
    use futures_util::StreamExt;
    use std::net::SocketAddr;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::oneshot;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    #[test]
    fn chat_rate_drops_after_its_burst() {
        let mut rate = ChatRate::new();
        assert!(rate.allow());
        assert!(rate.allow());
        assert!(!rate.allow(), "the third immediate chat is dropped");
    }

    #[test]
    fn origin_policy_accepts_local_and_rejects_other() {
        let allowed = vec![
            "http://127.0.0.1:5173".to_owned(),
            "http://localhost:4173".to_owned(),
        ];
        assert!(origin_allowed(None, &allowed));
        assert!(origin_allowed(Some("http://127.0.0.1:5173"), &allowed));
        assert!(!origin_allowed(Some("https://evil.example"), &allowed));
    }

    #[test]
    fn room_codes_are_normalised_and_bounded() {
        assert_eq!(normalise_code("dev001"), Some("DEV001".to_owned()));
        assert_eq!(normalise_code("DEV01"), None);
        assert_eq!(normalise_code("DEV00!"), None);
    }

    async fn spawn(
        name: &str,
    ) -> (
        SocketAddr,
        AppState,
        oneshot::Sender<()>,
        tokio::task::JoinHandle<()>,
    ) {
        let dir = std::env::temp_dir().join(format!("cb-host-{name}-{}", std::process::id()));
        let state = AppState::new(default_origins(), dir);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (stop, stopped) = oneshot::channel();
        let server_state = state.clone();
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                app(server_state).into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async move {
                let _ = stopped.await;
            })
            .await
            .expect("serve");
        });
        (addr, state, stop, task)
    }

    fn hello_frame(room: &str, token: Option<[u8; protocol::TOKEN_LEN]>) -> Vec<u8> {
        protocol::C2s::Hello(protocol::Hello {
            protocol: protocol::PROTOCOL_VERSION,
            name: protocol::FixedBytes::from_str_lossy("tester"),
            room_code: protocol::FixedBytes::from_str_lossy(room),
            token: token.map(protocol::FixedBytes),
        })
        .encode()
    }

    async fn send_hello(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        room: &str,
    ) {
        socket
            .send(tokio_tungstenite::tungstenite::Message::Binary(
                hello_frame(room, None).into(),
            ))
            .await
            .expect("hello");
    }

    #[tokio::test]
    async fn healthz_is_200_with_room_count_and_uptime() {
        let (addr, state, stop, task) = spawn("health").await;
        let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
        stream
            .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .expect("write");
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.expect("read");
        let text = String::from_utf8_lossy(&response);
        assert!(text.starts_with("HTTP/1.1 200"), "{text}");
        assert!(text.contains("\"status\":\"ok\""), "{text}");
        assert!(text.contains("\"rooms\":0"), "{text}");
        let _ = stop.send(());
        let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
        state.hub.shutdown().await;
    }

    #[tokio::test]
    async fn websocket_rejects_a_foreign_origin() {
        let (addr, state, stop, task) = spawn("origin").await;
        let mut request = format!("ws://{addr}/ws?room=DEV001")
            .into_client_request()
            .expect("request");
        request
            .headers_mut()
            .insert("origin", "https://evil.example".parse().expect("header"));
        let error = tokio_tungstenite::connect_async(request)
            .await
            .expect_err("a foreign Origin must not upgrade");
        assert!(
            error.to_string().contains("403"),
            "expected an HTTP 403, got {error}"
        );
        let _ = stop.send(());
        let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
        state.hub.shutdown().await;
    }

    #[tokio::test]
    async fn websocket_upgrades_and_receives_welcome() {
        let (addr, state, stop, task) = spawn("ws").await;
        let mut request = format!("ws://{addr}/ws?room=dev001")
            .into_client_request()
            .expect("request");
        request
            .headers_mut()
            .insert("origin", "http://127.0.0.1:5173".parse().expect("header"));
        let (mut socket, _) = tokio_tungstenite::connect_async(request)
            .await
            .expect("upgrade");
        send_hello(&mut socket, "DEV001").await;
        let message = tokio::time::timeout(Duration::from_secs(3), socket.next())
            .await
            .expect("welcome timeout")
            .expect("message")
            .expect("frame");
        let tokio_tungstenite::tungstenite::Message::Binary(bytes) = message else {
            panic!("expected a binary welcome");
        };
        match protocol::decode_s2c(&bytes).expect("decode") {
            S2c::Welcome(welcome) => {
                assert_eq!(welcome.player_id, 0);
                assert_eq!(welcome.tick.0, 0);
            }
            other => panic!("expected welcome, got {other:?}"),
        }
        drop(socket);
        let _ = stop.send(());
        let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
        state.hub.shutdown().await;
    }

    type WsClient = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    async fn connect(addr: SocketAddr, room: &str) -> WsClient {
        let mut request = format!("ws://{addr}/ws?room={room}")
            .into_client_request()
            .expect("request");
        request
            .headers_mut()
            .insert("origin", "http://127.0.0.1:5173".parse().expect("header"));
        tokio_tungstenite::connect_async(request)
            .await
            .expect("upgrade")
            .0
    }

    async fn next_s2c(socket: &mut WsClient) -> S2c {
        let message = tokio::time::timeout(Duration::from_secs(3), socket.next())
            .await
            .expect("frame timeout")
            .expect("message")
            .expect("frame");
        let tokio_tungstenite::tungstenite::Message::Binary(bytes) = message else {
            panic!("expected a binary frame");
        };
        protocol::decode_s2c(&bytes).expect("decode")
    }

    #[tokio::test]
    async fn four_connections_from_one_ip_are_capped() {
        let (addr, state, stop, task) = spawn("cap").await;
        let mut held = Vec::new();
        for _ in 0..3 {
            held.push(connect(addr, "CAP001").await);
        }
        let request = format!("ws://{addr}/ws?room=CAP001")
            .into_client_request()
            .expect("request");
        let error = tokio_tungstenite::connect_async(request)
            .await
            .expect_err("the fourth socket must be refused");
        assert!(error.to_string().contains("429"), "{error}");
        drop(held);
        let _ = stop.send(());
        let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
        state.hub.shutdown().await;
    }

    #[tokio::test]
    async fn a_reconnect_token_resumes_the_same_seat() {
        let (addr, state, stop, task) = spawn("token").await;
        let mut first = connect(addr, "TOK001").await;
        send_hello(&mut first, "TOK001").await;
        let (player, token) = match next_s2c(&mut first).await {
            S2c::Welcome(welcome) => (welcome.player_id, *welcome.your_token.as_array()),
            other => panic!("expected welcome, got {other:?}"),
        };
        drop(first);
        tokio::time::sleep(Duration::from_millis(200)).await;

        let mut second = connect(addr, "TOK001").await;
        second
            .send(tokio_tungstenite::tungstenite::Message::Binary(
                hello_frame("TOK001", Some(token)).into(),
            ))
            .await
            .expect("hello");
        match next_s2c(&mut second).await {
            S2c::Welcome(welcome) => assert_eq!(welcome.player_id, player),
            other => panic!("expected welcome, got {other:?}"),
        }
        drop(second);
        let _ = stop.send(());
        let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
        state.hub.shutdown().await;
    }

    #[tokio::test]
    async fn two_connections_get_distinct_seats_and_a_third_is_rejected() {
        let (addr, state, stop, task) = spawn("seats").await;
        let mut first = connect(addr, "dev001").await;
        send_hello(&mut first, "DEV001").await;
        match next_s2c(&mut first).await {
            S2c::Welcome(welcome) => assert_eq!(welcome.player_id, 0),
            other => panic!("expected welcome, got {other:?}"),
        }
        let mut second = connect(addr, "dev001").await;
        send_hello(&mut second, "DEV001").await;
        match next_s2c(&mut second).await {
            S2c::Welcome(welcome) => assert_eq!(welcome.player_id, 1),
            other => panic!("expected welcome, got {other:?}"),
        }
        let mut third = connect(addr, "dev001").await;
        send_hello(&mut third, "DEV001").await;
        match next_s2c(&mut third).await {
            S2c::Error(error) => assert_eq!(error.code, protocol::error_code::ROOM_FULL),
            other => panic!("expected room-full error, got {other:?}"),
        }
        drop((first, second, third));
        let _ = stop.send(());
        let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
        state.hub.shutdown().await;
    }
}
