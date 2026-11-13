// SPDX-License-Identifier: AGPL-3.0-only
//! The direct transport, for the `Identified` class only: registration, membership leaf
//! registration and token issuance, which name the account anyway (design B.12).

use std::sync::Arc;
use std::time::Duration;

use hyper::header::{HeaderName, HeaderValue};
use hyper::{Method, Request};
use pingers_ohttp::{BinaryRequest, BinaryResponse, Header, StatusCode};
use pingers_time::Timer;

use crate::endpoint::{Endpoint, Security};
use crate::http::{HttpClient, OutBody, read_body};
use crate::ohttp::Timeouts;
use crate::timer::within;
use crate::tls::{TlsRoots, client_config};
use crate::{DuplexStream, PrivacyClass, Transport, TransportError, TransportKind};

/// Everything a [`DirectTransport`] is configured with.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct DirectConfig {
    /// The server's origin, `https://host[:port]`; request paths are appended to it.
    pub server: String,
    /// Trusted roots.
    pub tls: TlsRoots,
    /// Per-step timeouts.
    pub timeouts: Timeouts,
    /// The largest response accepted.
    pub max_response_bytes: usize,
}

impl DirectConfig {
    /// A configuration with default timeouts and a 1 MiB response limit.
    #[must_use]
    pub fn new(server: String, tls: TlsRoots) -> Self {
        Self {
            server,
            tls,
            timeouts: Timeouts::default(),
            max_response_bytes: 1024 * 1024,
        }
    }
}

/// HTTPS straight to the server, over its own connection: never shared with anonymous
/// traffic. Refuses every class but [`PrivacyClass::Identified`].
pub struct DirectTransport {
    server: Endpoint,
    http: HttpClient,
    timer: Arc<dyn Timer>,
    request_timeout: Duration,
    max_response_bytes: usize,
}

impl DirectTransport {
    /// A transport to `config.server`. Must be used inside a tokio runtime.
    ///
    /// # Errors
    ///
    /// [`TransportError::InvalidUrl`] unless the server is an `https://` origin with no path;
    /// [`TransportError::InvalidCertificate`] if TLS cannot be set up.
    pub fn new(config: &DirectConfig, timer: Arc<dyn Timer>) -> Result<Self, TransportError> {
        let server = Endpoint::parse(&config.server)?;
        // Identified requests carry account credentials: never without TLS.
        if server.security() != Security::Tls || !server.is_origin_only() {
            return Err(TransportError::InvalidUrl);
        }
        let tls = client_config(&config.tls)?;
        Ok(Self {
            server,
            http: HttpClient::new(tls, Arc::clone(&timer), config.timeouts.connect),
            timer,
            request_timeout: config.timeouts.request,
            max_response_bytes: config.max_response_bytes,
        })
    }

    async fn identified(&self, request: BinaryRequest) -> Result<BinaryResponse, TransportError> {
        let outgoing = to_http(&self.server, request)?;
        let deadline = self.timer.now().saturating_add(self.request_timeout);
        let response = self
            .http
            .send(&self.server.origin(), outgoing, deadline)
            .await
            .map_err(crate::http::SendError::into_error)?;
        let status =
            StatusCode::new(response.status().as_u16()).map_err(|_| TransportError::Status {
                status: response.status().as_u16(),
            })?;
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

/// The HTTP request for a Binary HTTP request: the same method, path, headers and body.
fn to_http(server: &Endpoint, request: BinaryRequest) -> Result<Request<OutBody>, TransportError> {
    let method = Method::from_bytes(request.method.as_bytes())
        .map_err(|_| TransportError::InvalidRequest)?;
    let mut builder = Request::builder()
        .method(method)
        .uri(server.uri_with_path(&request.path)?);
    for header in request.headers {
        let name = HeaderName::from_bytes(header.name.as_bytes())
            .map_err(|_| TransportError::InvalidRequest)?;
        let value =
            HeaderValue::from_bytes(&header.value).map_err(|_| TransportError::InvalidRequest)?;
        builder = builder.header(name, value);
    }
    builder
        .body(OutBody::full(request.body))
        .map_err(|_| TransportError::InvalidRequest)
}

#[async_trait::async_trait]
impl Transport for DirectTransport {
    async fn request(
        &self,
        class: PrivacyClass,
        request: BinaryRequest,
    ) -> Result<BinaryResponse, TransportError> {
        match class {
            PrivacyClass::Identified => self.identified(request).await,
            // Anonymous and stream traffic never goes direct (design B.12).
            PrivacyClass::Anonymous | PrivacyClass::Stream => Err(TransportError::ClassNotAllowed),
        }
    }

    async fn stream(
        &self,
        _request: BinaryRequest,
    ) -> Result<Box<dyn DuplexStream>, TransportError> {
        Err(TransportError::ClassNotAllowed)
    }

    fn kind(&self) -> TransportKind {
        TransportKind::Direct
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_time::{ManualClock, UnixSeconds};

    use super::*;

    fn transport(server: &str) -> Result<DirectTransport, TransportError> {
        DirectTransport::new(
            &DirectConfig::new(server.to_owned(), TlsRoots::from_der(&[])?),
            Arc::new(ManualClock::new(UnixSeconds::EPOCH)),
        )
    }

    #[test]
    fn the_server_must_be_an_https_origin() {
        assert!(matches!(
            transport("http://server.example"),
            Err(TransportError::InvalidUrl)
        ));
        assert!(matches!(
            transport("https://server.example/api"),
            Err(TransportError::InvalidUrl)
        ));
    }

    #[tokio::test]
    async fn anonymous_and_stream_traffic_is_refused_without_a_connection()
    -> Result<(), TransportError> {
        // Port 9 on a reserved name: any attempt to connect would fail differently.
        let direct = transport("https://server.invalid:9")?;
        let request = BinaryRequest {
            method: "POST".to_owned(),
            path: "/v1/board/sync".to_owned(),
            headers: Vec::new(),
            body: Vec::new(),
        };
        for class in [PrivacyClass::Anonymous, PrivacyClass::Stream] {
            assert!(matches!(
                direct.request(class, request.clone()).await,
                Err(TransportError::ClassNotAllowed)
            ));
        }
        assert!(matches!(
            direct.stream(request).await,
            Err(TransportError::ClassNotAllowed)
        ));
        assert_eq!(direct.kind(), TransportKind::Direct);
        Ok(())
    }

    #[test]
    fn requests_keep_method_path_headers_and_body() -> Result<(), TransportError> {
        let server = Endpoint::parse("https://server.example:8443")?;
        let request = to_http(
            &server,
            BinaryRequest {
                method: "PUT".to_owned(),
                path: "/v1/accounts?x=1".to_owned(),
                headers: vec![Header {
                    name: "authorization".to_owned(),
                    value: b"Bearer t".to_vec(),
                }],
                body: vec![1, 2],
            },
        )?;
        assert_eq!(request.method(), Method::PUT);
        assert_eq!(
            request.uri().to_string(),
            "https://server.example:8443/v1/accounts?x=1"
        );
        assert_eq!(
            request
                .headers()
                .get("authorization")
                .map(HeaderValue::as_bytes),
            Some(b"Bearer t".as_slice())
        );
        assert!(matches!(request.body(), OutBody::Full(Some(bytes)) if bytes.as_ref() == [1, 2]));
        Ok(())
    }

    #[test]
    fn malformed_methods_and_headers_are_refused() -> Result<(), TransportError> {
        let server = Endpoint::parse("https://server.example")?;
        let base = BinaryRequest {
            method: "GET".to_owned(),
            path: "/".to_owned(),
            headers: Vec::new(),
            body: Vec::new(),
        };
        let bad_method = BinaryRequest {
            method: "G T".to_owned(),
            ..base.clone()
        };
        let bad_name = BinaryRequest {
            headers: vec![Header {
                name: "bad name".to_owned(),
                value: Vec::new(),
            }],
            ..base.clone()
        };
        let bad_value = BinaryRequest {
            headers: vec![Header {
                name: "x".to_owned(),
                value: b"line\nbreak".to_vec(),
            }],
            ..base
        };
        for request in [bad_method, bad_name, bad_value] {
            assert!(matches!(
                to_http(&server, request),
                Err(TransportError::InvalidRequest)
            ));
        }
        Ok(())
    }
}
