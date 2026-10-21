//! `cargo run -p host-axum` starts the local WebSocket host.

use std::env;
use std::error::Error;
use std::net::SocketAddr;
use std::path::PathBuf;

use host_axum::{app, default_origins, AppState};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let addr: SocketAddr = env::var("CB_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8080".to_owned())
        .parse()?;
    let origins = match env::var("CB_ALLOWED_ORIGINS") {
        Ok(value) if !value.trim().is_empty() => {
            value.split(',').map(str::trim).map(str::to_owned).collect()
        }
        _ => default_origins(),
    };
    let summaries = PathBuf::from(
        env::var("CB_SUMMARY_DIR").unwrap_or_else(|_| "target/room-summaries".to_owned()),
    );
    let state = AppState::new(origins, summaries);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("host-axum listening on http://{addr}");
    println!("  GET /healthz");
    println!("  GET /ws?room=DEV001");

    let shutdown = {
        let hub = state.hub.clone();
        async move {
            shutdown_signal().await;
            println!("shutdown: draining rooms");
            hub.shutdown().await;
        }
    };
    axum::serve(listener, app(state.clone()))
        .with_graceful_shutdown(shutdown)
        .await?;
    state.hub.shutdown().await;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}
