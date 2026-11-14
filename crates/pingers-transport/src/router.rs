// SPDX-License-Identifier: AGPL-3.0-only
//! The router: one transport per privacy class (design B.12).

use std::sync::Arc;

use pingers_ohttp::{BinaryRequest, BinaryResponse};

use crate::{DuplexStream, PrivacyClass, Transport, TransportError, TransportKind};

/// The transport for each class.
#[derive(Clone)]
pub struct Routes {
    /// Board, mailboxes, groups, key configs, token redemption. Never [`TransportKind::Direct`].
    pub anonymous: Arc<dyn Transport>,
    /// Registration, leaf registration, token issuance.
    pub identified: Arc<dyn Transport>,
    /// The Meetup live channel. Never [`TransportKind::Direct`].
    pub stream: Arc<dyn Transport>,
}

/// Routes each request by its [`PrivacyClass`]. Anonymous and stream traffic can only be
/// routed to a relayed transport; the router refuses to be built otherwise, and the
/// transports refuse classes they must not carry, so nothing anonymous reaches the server
/// directly even through a misconfiguration.
pub struct Router {
    routes: Routes,
}

impl Router {
    /// A router over `routes`.
    ///
    /// # Errors
    ///
    /// [`TransportError::ClassNotAllowed`] if the anonymous or stream route is direct.
    pub fn new(routes: Routes) -> Result<Self, TransportError> {
        if routes.anonymous.kind() == TransportKind::Direct
            || routes.stream.kind() == TransportKind::Direct
        {
            return Err(TransportError::ClassNotAllowed);
        }
        Ok(Self { routes })
    }

    /// The transport `class` travels over.
    fn route(&self, class: PrivacyClass) -> &Arc<dyn Transport> {
        match class {
            PrivacyClass::Anonymous => &self.routes.anonymous,
            PrivacyClass::Identified => &self.routes.identified,
            PrivacyClass::Stream => &self.routes.stream,
        }
    }

    /// What `class` travels over, for "What your server sees".
    #[must_use]
    pub fn kind_for(&self, class: PrivacyClass) -> TransportKind {
        self.route(class).kind()
    }
}

#[async_trait::async_trait]
impl Transport for Router {
    async fn request(
        &self,
        class: PrivacyClass,
        request: BinaryRequest,
    ) -> Result<BinaryResponse, TransportError> {
        self.route(class).request(class, request).await
    }

    async fn stream(
        &self,
        request: BinaryRequest,
    ) -> Result<Box<dyn DuplexStream>, TransportError> {
        self.routes.stream.stream(request).await
    }

    /// The anonymous route's check: that is the relay anonymous requests take.
    async fn check_gateway_key(&self) -> Result<(), TransportError> {
        self.routes.anonymous.check_gateway_key().await
    }

    /// The anonymous route's gateway key, as for the check.
    fn gateway_key_hash(&self) -> Option<pingers_proto::KeyConfigHash> {
        self.routes.anonymous.gateway_key_hash()
    }

    /// The anonymous route's kind; [`Router::kind_for`] answers per class.
    fn kind(&self) -> TransportKind {
        self.routes.anonymous.kind()
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use std::sync::Mutex;

    use pingers_ohttp::StatusCode;

    use super::*;

    /// Records which classes reached it and answers with its own status code.
    struct Fake {
        kind: TransportKind,
        status: u16,
        seen: Mutex<Vec<PrivacyClass>>,
        streams: Mutex<usize>,
    }

    impl Fake {
        fn new(kind: TransportKind, status: u16) -> Arc<Self> {
            Arc::new(Self {
                kind,
                status,
                seen: Mutex::new(Vec::new()),
                streams: Mutex::new(0),
            })
        }

        fn seen(&self) -> Vec<PrivacyClass> {
            self.seen
                .lock()
                .map(|seen| seen.clone())
                .unwrap_or_default()
        }

        fn streams(&self) -> usize {
            self.streams.lock().map(|count| *count).unwrap_or_default()
        }
    }

    #[async_trait::async_trait]
    impl Transport for Fake {
        async fn request(
            &self,
            class: PrivacyClass,
            _request: BinaryRequest,
        ) -> Result<BinaryResponse, TransportError> {
            if let Ok(mut seen) = self.seen.lock() {
                seen.push(class);
            }
            Ok(BinaryResponse {
                status: StatusCode::new(self.status)?,
                headers: Vec::new(),
                body: Vec::new(),
            })
        }

        async fn stream(
            &self,
            _request: BinaryRequest,
        ) -> Result<Box<dyn DuplexStream>, TransportError> {
            if let Ok(mut count) = self.streams.lock() {
                *count = count.saturating_add(1);
            }
            Err(TransportError::StreamClosed)
        }

        async fn check_gateway_key(&self) -> Result<(), TransportError> {
            Err(TransportError::Status {
                status: self.status,
            })
        }

        fn gateway_key_hash(&self) -> Option<pingers_proto::KeyConfigHash> {
            let fill = u8::try_from(self.status % 256).ok()?;
            Some(pingers_proto::KeyConfigHash::from_bytes([fill; 32]))
        }

        fn kind(&self) -> TransportKind {
            self.kind
        }
    }

    fn request() -> BinaryRequest {
        BinaryRequest {
            method: "GET".to_owned(),
            path: "/".to_owned(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    #[test]
    fn anonymous_or_stream_over_direct_is_refused() {
        let direct = Fake::new(TransportKind::Direct, 200);
        let relayed = Fake::new(TransportKind::Ohttp, 200);
        for (anonymous, stream) in [
            (Arc::clone(&direct), Arc::clone(&relayed)),
            (Arc::clone(&relayed), Arc::clone(&direct)),
        ] {
            let routes = Routes {
                anonymous,
                identified: direct.clone(),
                stream,
            };
            assert!(matches!(
                Router::new(routes),
                Err(TransportError::ClassNotAllowed)
            ));
        }
    }

    #[tokio::test]
    async fn each_class_goes_to_its_own_transport() -> Result<(), TransportError> {
        let anonymous = Fake::new(TransportKind::Ohttp, 201);
        let identified = Fake::new(TransportKind::Direct, 202);
        let stream = Fake::new(TransportKind::Ohttp, 203);
        let router = Router::new(Routes {
            anonymous: anonymous.clone(),
            identified: identified.clone(),
            stream: stream.clone(),
        })?;

        let answered = router.request(PrivacyClass::Anonymous, request()).await?;
        assert_eq!(answered.status.get(), 201);
        let answered = router.request(PrivacyClass::Identified, request()).await?;
        assert_eq!(answered.status.get(), 202);
        let answered = router.request(PrivacyClass::Stream, request()).await?;
        assert_eq!(answered.status.get(), 203);
        assert!(router.stream(request()).await.is_err());
        // The check goes to the relay anonymous requests take.
        assert!(matches!(
            router.check_gateway_key().await,
            Err(TransportError::Status { status: 201 })
        ));
        assert_eq!(
            router.gateway_key_hash(),
            Some(pingers_proto::KeyConfigHash::from_bytes([201; 32]))
        );

        assert_eq!(anonymous.seen(), [PrivacyClass::Anonymous]);
        assert_eq!(identified.seen(), [PrivacyClass::Identified]);
        assert_eq!(stream.seen(), [PrivacyClass::Stream]);
        assert_eq!(stream.streams(), 1);
        assert_eq!(anonymous.streams(), 0);

        assert_eq!(
            router.kind_for(PrivacyClass::Anonymous),
            TransportKind::Ohttp
        );
        assert_eq!(
            router.kind_for(PrivacyClass::Identified),
            TransportKind::Direct
        );
        assert_eq!(router.kind_for(PrivacyClass::Stream), TransportKind::Ohttp);
        assert_eq!(router.kind(), TransportKind::Ohttp);
        Ok(())
    }
}
