//! Self-hosted EasySSH sync server (AGPL-3.0-only).
//!
//! It stores OPAQUE registration records, public keys, the wrapped account
//! key blob, sealed vault keys and signed ciphertext envelopes. It never
//! sees a passphrase, an account key, a vault key or a plaintext record.

#![forbid(unsafe_code)]

mod accounts;
mod app;
mod config;
mod db;
mod devices;
mod error;
mod ratelimit;
mod records;
mod vaults;

pub use app::{router, AppState};
pub use config::{Config, LogFormat};
pub use error::ApiError;

use std::net::SocketAddr;
use tokio::net::TcpListener;

/// Open the database and build the application.
pub async fn build(config: Config) -> Result<axum::Router, ApiError> {
    Ok(router(AppState::new(config).await?))
}

/// Serve until `shutdown` resolves.
pub async fn serve(
    listener: TcpListener,
    config: Config,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let app = build(config)
        .await
        .map_err(|e| format!("startup failed: {}", e.message))?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await?;
    Ok(())
}
