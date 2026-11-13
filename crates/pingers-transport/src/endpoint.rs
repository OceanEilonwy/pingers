// SPDX-License-Identifier: AGPL-3.0-only
//! Parsed URLs: where a relay, its key config or the server's direct API is.

use hyper::Uri;
use hyper::http::uri::{Authority, PathAndQuery, Scheme};

use crate::TransportError;

/// How a connection to an [`Endpoint`] is secured.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Security {
    /// TLS 1.3 with ALPN `h2`.
    Tls,
    /// HTTP/2 with prior knowledge and no TLS: only for a relay reached over loopback or a
    /// private tunnel (the reference relay speaks only this; design B.12 puts TLS in front).
    Cleartext,
}

/// One parsed absolute URL: `https://host[:port]/path` or `http://host[:port]/path`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Endpoint {
    security: Security,
    authority: Authority,
    path: PathAndQuery,
}

/// Which connection an endpoint uses: the same host, port and security share one.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Origin {
    pub(crate) security: Security,
    pub(crate) host: String,
    pub(crate) port: u16,
}

impl Endpoint {
    /// Parses an absolute `https://` or `http://` URL.
    pub(crate) fn parse(url: &str) -> Result<Self, TransportError> {
        let uri: Uri = url.parse().map_err(|_| TransportError::InvalidUrl)?;
        let security = match uri.scheme_str() {
            Some("https") => Security::Tls,
            Some("http") => Security::Cleartext,
            _ => return Err(TransportError::InvalidUrl),
        };
        let authority = uri.authority().cloned().ok_or(TransportError::InvalidUrl)?;
        // A user name or password in a URL would be sent nowhere and only leak into config.
        if authority.as_str().contains('@') || authority.host().is_empty() {
            return Err(TransportError::InvalidUrl);
        }
        let path = uri
            .path_and_query()
            .cloned()
            .unwrap_or_else(|| PathAndQuery::from_static("/"));
        Ok(Self {
            security,
            authority,
            path,
        })
    }

    /// The connection this endpoint is reached over.
    pub(crate) fn origin(&self) -> Origin {
        let default_port = match self.security {
            Security::Tls => 443,
            Security::Cleartext => 80,
        };
        Origin {
            security: self.security,
            host: self.host().to_owned(),
            port: self.authority.port_u16().unwrap_or(default_port),
        }
    }

    /// The host without brackets, as TLS server names and socket addresses want it.
    fn host(&self) -> &str {
        let host = self.authority.host();
        host.strip_prefix('[')
            .and_then(|inner| inner.strip_suffix(']'))
            .unwrap_or(host)
    }

    pub(crate) fn security(&self) -> Security {
        self.security
    }

    /// The URL itself, for a request line.
    pub(crate) fn uri(&self) -> Result<Uri, TransportError> {
        self.uri_with_path(self.path.as_str())
    }

    /// The same scheme and authority with another path, for direct requests whose paths come
    /// from the caller.
    pub(crate) fn uri_with_path(&self, path: &str) -> Result<Uri, TransportError> {
        let scheme = match self.security {
            Security::Tls => Scheme::HTTPS,
            Security::Cleartext => Scheme::HTTP,
        };
        Uri::builder()
            .scheme(scheme)
            .authority(self.authority.clone())
            .path_and_query(path)
            .build()
            .map_err(|_| TransportError::InvalidRequest)
    }

    /// Whether the URL names only an origin (no path beyond `/`, no query).
    pub(crate) fn is_origin_only(&self) -> bool {
        self.path.as_str() == "/"
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    #[test]
    fn https_defaults_to_port_443_and_tls() -> Result<(), TransportError> {
        let endpoint = Endpoint::parse("https://relay.example/ohttp")?;
        assert_eq!(
            endpoint.origin(),
            Origin {
                security: Security::Tls,
                host: "relay.example".to_owned(),
                port: 443
            }
        );
        assert_eq!(endpoint.uri()?.to_string(), "https://relay.example/ohttp");
        Ok(())
    }

    #[test]
    fn http_is_cleartext_on_port_80_unless_given() -> Result<(), TransportError> {
        assert_eq!(Endpoint::parse("http://relay/")?.origin().port, 80);
        let endpoint = Endpoint::parse("http://127.0.0.1:8443/ohttp")?;
        assert_eq!(endpoint.security(), Security::Cleartext);
        assert_eq!(endpoint.origin().port, 8443);
        Ok(())
    }

    #[test]
    fn ipv6_hosts_lose_their_brackets() -> Result<(), TransportError> {
        assert_eq!(Endpoint::parse("https://[::1]:9/x")?.origin().host, "::1");
        Ok(())
    }

    #[test]
    fn other_schemes_relative_urls_and_credentials_are_refused() {
        for url in [
            "ftp://relay/x",
            "/ohttp",
            "relay.example",
            "https://user:secret@relay/x",
            "not a url",
        ] {
            assert!(
                matches!(Endpoint::parse(url), Err(TransportError::InvalidUrl)),
                "{url}"
            );
        }
    }

    #[test]
    fn a_missing_path_is_the_root_and_counts_as_origin_only() -> Result<(), TransportError> {
        assert!(Endpoint::parse("https://server.example")?.is_origin_only());
        assert!(Endpoint::parse("https://server.example/")?.is_origin_only());
        assert!(!Endpoint::parse("https://server.example/api")?.is_origin_only());
        assert!(!Endpoint::parse("https://server.example/?q")?.is_origin_only());
        Ok(())
    }

    #[test]
    fn a_new_path_keeps_scheme_and_authority() -> Result<(), TransportError> {
        let endpoint = Endpoint::parse("https://server.example:8443/")?;
        assert_eq!(
            endpoint.uri_with_path("/v1/register")?.to_string(),
            "https://server.example:8443/v1/register"
        );
        assert!(matches!(
            endpoint.uri_with_path("no slash"),
            Err(TransportError::InvalidRequest)
        ));
        Ok(())
    }
}
