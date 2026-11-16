// SPDX-License-Identifier: AGPL-3.0-only
//! The `pingers` app on a monokulo-nodo appliance: the head of the Pingers bundle, which nodo
//! installs together with pingers-push-relay, pingers-map-host, pingers-server and
//! pingers-ohttp-relay, and which serves the owner's setup page.
//!
//! It listens on loopback only and is not a web site: nodo's helper fetches `GET /owner` for
//! the owner, behind the device passphrase, and shows it in nodo-tui (the app's Setup row).
//! The page is the ordered checklist of everything the owner still does by hand, with this
//! appliance's values filled in: the domain and DNS, the four sites, the router, the services'
//! own checks, Firebase, the map packs, the GitHub variables (both pin lines), the release, the
//! first phone's QR code, and the backup. See [`page`] for the steps and [`checks`] for what
//! is checked and how.
//!
//! What it holds: the server's registration code (a sealed secret it reads through a grant the
//! owner approved with the install) and public addresses. It reads the gateway's and the push
//! relay's public key configs on loopback, through references that open exactly those ports,
//! never a private key.

pub mod checks;
pub mod config;
pub mod page;

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse as _, Response};
use axum::routing::get;

pub use crate::checks::{Clients, Facts};
pub use crate::config::{Config, ConfigError};
pub use crate::page::Page;

/// The path nodo's manifest names (`[owner_page] url`).
pub const OWNER_PATH: &str = "/owner";

/// The running page: its configuration and clients. One page is built at a time.
pub struct App {
    config: Config,
    clients: Clients,
    building: tokio::sync::Mutex<()>,
}

impl App {
    /// The app over `config`.
    ///
    /// # Errors
    ///
    /// As [`Clients::new`].
    pub fn new(config: Config) -> Result<Self, String> {
        Ok(Self {
            config,
            clients: Clients::new()?,
            building: tokio::sync::Mutex::new(()),
        })
    }

    /// Gathers the facts and builds the page.
    pub async fn page(&self) -> Page {
        let _one_at_a_time = self.building.lock().await;
        let facts = checks::gather(&self.config, &self.clients).await;
        page::build(&self.config, &facts)
    }
}

/// `GET /owner`, and nothing else.
pub fn router(app: Arc<App>) -> Router {
    Router::new().route(OWNER_PATH, get(owner)).with_state(app)
}

async fn owner(State(app): State<Arc<App>>) -> Response {
    match serde_json::to_vec(&app.page().await) {
        Ok(body) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            body,
        )
            .into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Serves `app` on `listener` until `shutdown` completes.
///
/// # Errors
///
/// Accepting connections failed.
pub async fn serve(
    listener: tokio::net::TcpListener,
    app: Arc<App>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, router(app))
        .with_graceful_shutdown(shutdown)
        .await
}

#[cfg(test)]
mod tests;
