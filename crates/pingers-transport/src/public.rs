// SPDX-License-Identifier: AGPL-3.0-only
//! Plain HTTPS `GET`s to a public static host the user chose to contact directly: the map host,
//! when "Download maps through privacy relay" is off (design B.12, "Map host").
//!
//! Never the Pingers server, and never a request that carries an account, a Party or a
//! location: the host learns the device's address and which public file it fetched, as the
//! user agreed by leaving the relay setting off. Over its own connections, shared with
//! nothing else.

use std::sync::Arc;
use std::time::Duration;

use hyper::header::{HeaderName, HeaderValue};
use hyper::{Method, Request};
use pingers_ohttp::{BinaryResponse, Header, StatusCode};
use pingers_time::Timer;

use crate::TransportError;
use crate::endpoint::{Endpoint, Security};
use crate::http::{HttpClient, OutBody, read_body};
use crate::ohttp::Timeouts;
use crate::timer::within;
use crate::tls::{TlsRoots, client_config};

/// Everything a [`PublicClient`] is configured with.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct PublicConfig {
    /// Trusted roots.
    pub tls: TlsRoots,
    /// Per-step timeouts.
    pub timeouts: Timeouts,
    /// The largest response body accepted.
    pub max_response_bytes: usize,
}

impl PublicConfig {
    /// A configuration with default timeouts and a body limit of `max_response_bytes`.
    #[must_use]
    pub fn new(tls: TlsRoots, max_response_bytes: usize) -> Self {
        Self {
            tls,
            timeouts: Timeouts::default(),
            max_response_bytes,
        }
    }
}

/// `GET`s over HTTPS (HTTP/2) to public hosts.
pub struct PublicClient {
    http: HttpClient,
    timer: Arc<dyn Timer>,
    request_timeout: Duration,
    max_response_bytes: usize,
}

impl PublicClient {
    /// A client. Must be used inside a tokio runtime.
    ///
    /// # Errors
    ///
    /// [`TransportError::InvalidCertificate`] if TLS cannot be set up.
    pub fn new(config: &PublicConfig, timer: Arc<dyn Timer>) -> Result<Self, TransportError> {
        let tls = client_config(&config.tls)?;
        Ok(Self {
            http: HttpClient::new(tls, Arc::clone(&timer), config.timeouts.connect),
            timer,
            request_timeout: config.timeouts.request,
            max_response_bytes: config.max_response_bytes,
        })
    }

    /// `GET url` with `headers`: the status, the response headers and the whole body.
    ///
    /// # Errors
    ///
    /// [`TransportError::InvalidUrl`] unless `url` is an absolute `https://` URL;
    /// [`TransportError::InvalidRequest`] for a malformed header;
    /// [`TransportError::Connection`] or [`TransportError::Timeout`];
    /// [`TransportError::TooLarge`] if the body is over the limit.
    pub async fn get(
        &self,
        url: &str,
        headers: &[Header],
    ) -> Result<BinaryResponse, TransportError> {
        let endpoint = Endpoint::parse(url)?;
        // The host sees the address anyway; TLS keeps the network from seeing the file.
        if endpoint.security() != Security::Tls {
            return Err(TransportError::InvalidUrl);
        }
        let mut builder = Request::builder().method(Method::GET).uri(endpoint.uri()?);
        for header in headers {
            let name = HeaderName::from_bytes(header.name.as_bytes())
                .map_err(|_| TransportError::InvalidRequest)?;
            let value = HeaderValue::from_bytes(&header.value)
                .map_err(|_| TransportError::InvalidRequest)?;
            builder = builder.header(name, value);
        }
        let request = builder
            .body(OutBody::empty())
            .map_err(|_| TransportError::InvalidRequest)?;
        let deadline = self.timer.now().saturating_add(self.request_timeout);
        let response = self
            .http
            .send(&endpoint.origin(), request, deadline)
            .await
            .map_err(crate::http::SendError::into_error)?;
        let code = response.status().as_u16();
        let status = StatusCode::new(code).map_err(|_| TransportError::Status { status: code })?;
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| Header {
                name: name.as_str().to_owned(),
                value: value.as_bytes().to_vec(),
            })
            .collect();
        let mut body = response.into_body();
        let body = within(
            &*self.timer,
            deadline,
            read_body(&mut body, self.max_response_bytes),
        )
        .await??;
        Ok(BinaryResponse {
            status,
            headers,
            body,
        })
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_time::{ManualClock, UnixSeconds};

    use super::*;

    #[tokio::test]
    async fn only_https_urls_are_fetched() -> Result<(), TransportError> {
        let client = PublicClient::new(
            &PublicConfig::new(TlsRoots::from_der(&[])?, 1024),
            Arc::new(ManualClock::new(UnixSeconds::EPOCH)),
        )?;
        for url in [
            "http://maps.example/catalog.json",
            "ftp://maps.example/x",
            "/x",
        ] {
            assert!(
                matches!(client.get(url, &[]).await, Err(TransportError::InvalidUrl)),
                "{url}"
            );
        }
        let bad_header = Header {
            name: "bad name".to_owned(),
            value: Vec::new(),
        };
        assert!(matches!(
            client
                .get("https://maps.invalid:9/catalog.json", &[bad_header])
                .await,
            Err(TransportError::InvalidRequest)
        ));
        Ok(())
    }
}
