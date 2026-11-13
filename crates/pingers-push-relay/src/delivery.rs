// SPDX-License-Identifier: AGPL-3.0-only
//! Sending the content-free push: `{"v":1,"h":"<handle hex>"}` (design B.7), by UnifiedPush
//! (an HTTP POST to the distributor endpoint in the token) or FCM HTTP v1. No APNs for now.

use std::future::Future;
use std::net::IpAddr;
use std::time::Duration;

use pingers_proto::{PushClass, PushHandle, PushPlatform, PushToken, Urgency};
use pingers_time::Timer;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use reqwest::{StatusCode, Url};
use zeroize::Zeroizing;

use crate::address::ip_is_public;

/// The longest a push service should hold an undelivered push. A wake that arrives an hour
/// late has missed the knock's lease; later ones do nothing useful.
pub const MAX_PUSH_TTL: Duration = Duration::from_hours(1);

/// The three kinds of wake the relay sends (design B.4.9, "Push classes and what runs").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeClass {
    /// A knock arrived: check liveness. High priority.
    Location,
    /// Something the user should see. High priority.
    Alert,
    /// Housekeeping. Normal priority.
    Quiet,
}

impl WakeClass {
    /// The sealed class decides Location; for Standard the server's urgency decides.
    #[must_use]
    pub const fn of(class: PushClass, urgency: Urgency) -> Self {
        match (class, urgency) {
            (PushClass::Location, _) => Self::Location,
            (PushClass::Standard, Urgency::Alert) => Self::Alert,
            (PushClass::Standard, Urgency::Quiet) => Self::Quiet,
        }
    }

    /// Delivery priority on Android.
    #[must_use]
    pub const fn priority(self) -> Priority {
        match self {
            Self::Location | Self::Alert => Priority::High,
            Self::Quiet => Priority::Normal,
        }
    }
}

/// Delivery priority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priority {
    /// FCM `HIGH`, UnifiedPush `Urgency: high`.
    High,
    /// FCM `NORMAL`, UnifiedPush `Urgency: normal`.
    Normal,
}

/// Everything a push carries: the device's handle, nothing else, plus delivery hints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PushMessage {
    /// Tells the device which mailbox or group to read.
    pub handle: PushHandle,
    /// Delivery priority.
    pub priority: Priority,
    /// How long the push service may hold it.
    pub time_to_live: Duration,
}

impl PushMessage {
    /// The body every push carries: `{"v":1,"h":"<16 hex digits>"}`.
    #[must_use]
    pub fn payload(&self) -> String {
        format!(r#"{{"v":1,"h":"{}"}}"#, hex::encode(self.handle.as_bytes()))
    }
}

/// What happened to one push.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// The push service accepted it.
    Delivered,
    /// The push service says the token is invalid or unregistered, or the token cannot be
    /// used at all. The server should delete the blob.
    TokenGone,
    /// The push service failed or refused for another reason; the blob may work later.
    Failed,
    /// No answer within the timeout.
    TimedOut,
}

/// Sends pushes on the platforms it supports.
pub trait PushSender: Send + Sync + 'static {
    /// Whether this sender can deliver on `platform`.
    fn supports(&self, platform: PushPlatform) -> bool;

    /// Sends one push to `token`.
    fn send(
        &self,
        platform: PushPlatform,
        token: &PushToken,
        message: &PushMessage,
    ) -> impl Future<Output = DeliveryOutcome> + Send;
}

/// Which UnifiedPush endpoints the relay will POST to. The endpoint comes from the device, so
/// without a policy the relay would make requests into its own network for anyone who can
/// seal a token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointPolicy {
    /// `https://` only, and only to globally routable addresses: literal IPs are checked when
    /// the endpoint is parsed, host names after resolution at connect time
    /// ([`crate::address::PublicOnlyResolver`], installed by [`crate::public_http_client`]).
    PublicHttpsOnly,
    /// Also `http://` and local hosts: for testing against a local distributor only.
    AllowInsecureLocal,
}

/// An OAuth 2 access token for FCM HTTP v1. No `Debug` or `Clone`: it is a secret.
pub struct FcmAccessToken(Zeroizing<String>);

impl FcmAccessToken {
    /// Wraps a token (surrounding whitespace is removed).
    #[must_use]
    pub fn new(token: &str) -> Self {
        Self(Zeroizing::new(token.trim().to_owned()))
    }
}

/// FCM HTTP v1 settings.
pub struct FcmSettings {
    send_url: Url,
    access_token: FcmAccessToken,
}

impl FcmSettings {
    /// FCM for `project_id`, at `endpoint` (normally `https://fcm.googleapis.com/`).
    ///
    /// Minting the access token from a service account needs an RS256 JWT, which this crate
    /// cannot sign (only pingers-crypto calls primitives). Until then the operator supplies a
    /// token, for example from `gcloud auth print-access-token`.
    ///
    /// # Errors
    ///
    /// `None` if the project id is not a valid FCM project id or the URL cannot be built.
    #[must_use]
    pub fn new(project_id: &str, endpoint: &Url, access_token: FcmAccessToken) -> Option<Self> {
        if !is_valid_fcm_project_id(project_id) {
            return None;
        }
        let send_url = endpoint
            .join(&format!("v1/projects/{project_id}/messages:send"))
            .ok()?;
        Some(Self {
            send_url,
            access_token,
        })
    }
}

/// Letters, digits and hyphens only, so the id cannot change the request path.
pub(crate) fn is_valid_fcm_project_id(project_id: &str) -> bool {
    !project_id.is_empty()
        && project_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// The production sender over HTTP.
pub struct Dispatcher<T> {
    client: reqwest::Client,
    timer: T,
    timeout: Duration,
    unifiedpush: Option<EndpointPolicy>,
    fcm: Option<FcmSettings>,
}

/// What [`Dispatcher::new`] needs.
pub struct DispatcherSettings {
    /// The longest to wait for a push service.
    pub timeout: Duration,
    /// UnifiedPush, if enabled.
    pub unifiedpush: Option<EndpointPolicy>,
    /// FCM, if enabled.
    pub fcm: Option<FcmSettings>,
}

impl<T: Timer + 'static> Dispatcher<T> {
    /// A dispatcher using `client`, which must not follow redirects (a redirect would get
    /// around the endpoint policy).
    #[must_use]
    pub fn new(client: reqwest::Client, timer: T, settings: DispatcherSettings) -> Self {
        Self {
            client,
            timer,
            timeout: settings.timeout,
            unifiedpush: settings.unifiedpush,
            fcm: settings.fcm,
        }
    }

    async fn with_timeout<F: Future>(&self, future: F) -> Option<F::Output> {
        tokio::select! {
            biased;
            output = future => Some(output),
            () = self.timer.sleep(self.timeout) => None,
        }
    }

    async fn send_unifiedpush(
        &self,
        policy: EndpointPolicy,
        token: &PushToken,
        message: &PushMessage,
    ) -> DeliveryOutcome {
        let Some(endpoint) = unifiedpush_endpoint(token, policy) else {
            return DeliveryOutcome::TokenGone;
        };
        let request = self
            .client
            .post(endpoint)
            .header("TTL", message.time_to_live.as_secs().to_string())
            .header(
                "Urgency",
                match message.priority {
                    Priority::High => "high",
                    Priority::Normal => "normal",
                },
            )
            .header(CONTENT_TYPE, "application/json")
            .body(message.payload())
            .send();
        match self.with_timeout(request).await {
            None => DeliveryOutcome::TimedOut,
            Some(Err(_)) => DeliveryOutcome::Failed,
            Some(Ok(response)) => unifiedpush_outcome(response.status()),
        }
    }

    async fn send_fcm(
        &self,
        settings: &FcmSettings,
        token: &PushToken,
        message: &PushMessage,
    ) -> DeliveryOutcome {
        let Ok(token) = std::str::from_utf8(token.as_bytes()) else {
            return DeliveryOutcome::TokenGone;
        };
        let exchange = async {
            let response = self
                .client
                .post(settings.send_url.clone())
                .header(
                    AUTHORIZATION,
                    format!("Bearer {}", settings.access_token.0.as_str()),
                )
                .header(CONTENT_TYPE, "application/json")
                .body(fcm_body(token, message))
                .send()
                .await?;
            let status = response.status();
            let body = response.bytes().await?;
            Ok::<_, reqwest::Error>(fcm_outcome(status, &body))
        };
        match self.with_timeout(exchange).await {
            None => DeliveryOutcome::TimedOut,
            Some(Err(_)) => DeliveryOutcome::Failed,
            Some(Ok(outcome)) => outcome,
        }
    }
}

impl<T: Timer + 'static> PushSender for Dispatcher<T> {
    fn supports(&self, platform: PushPlatform) -> bool {
        match platform {
            PushPlatform::UnifiedPush => self.unifiedpush.is_some(),
            PushPlatform::Fcm => self.fcm.is_some(),
            _ => false,
        }
    }

    async fn send(
        &self,
        platform: PushPlatform,
        token: &PushToken,
        message: &PushMessage,
    ) -> DeliveryOutcome {
        match (platform, self.unifiedpush, &self.fcm) {
            (PushPlatform::UnifiedPush, Some(policy), _) => {
                self.send_unifiedpush(policy, token, message).await
            }
            (PushPlatform::Fcm, _, Some(settings)) => self.send_fcm(settings, token, message).await,
            _ => DeliveryOutcome::Failed,
        }
    }
}

/// The distributor endpoint in a UnifiedPush token, if the policy allows it.
fn unifiedpush_endpoint(token: &PushToken, policy: EndpointPolicy) -> Option<Url> {
    let text = std::str::from_utf8(token.as_bytes()).ok()?;
    let url = Url::parse(text).ok()?;
    let allowed = match policy {
        EndpointPolicy::AllowInsecureLocal => matches!(url.scheme(), "https" | "http"),
        EndpointPolicy::PublicHttpsOnly => url.scheme() == "https" && host_is_public(&url),
    };
    allowed.then_some(url)
}

fn host_is_public(url: &Url) -> bool {
    match url.host() {
        // Names are checked again after resolution, at connect time, by the client's
        // PublicOnlyResolver (see `crate::address`).
        Some(url::Host::Domain(domain)) => {
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            domain != "localhost" && !domain.ends_with(".localhost")
        }
        Some(url::Host::Ipv4(address)) => ip_is_public(IpAddr::V4(address)),
        Some(url::Host::Ipv6(address)) => ip_is_public(IpAddr::V6(address)),
        None => false,
    }
}

fn unifiedpush_outcome(status: StatusCode) -> DeliveryOutcome {
    if status.is_success() {
        DeliveryOutcome::Delivered
    } else if status == StatusCode::NOT_FOUND || status == StatusCode::GONE {
        DeliveryOutcome::TokenGone
    } else {
        DeliveryOutcome::Failed
    }
}

fn fcm_body(token: &str, message: &PushMessage) -> Vec<u8> {
    let body = serde_json::json!({
        "message": {
            "token": token,
            "data": { "v": "1", "h": hex::encode(message.handle.as_bytes()) },
            "android": {
                "priority": match message.priority {
                    Priority::High => "HIGH",
                    Priority::Normal => "NORMAL",
                },
                "ttl": format!("{}s", message.time_to_live.as_secs()),
            },
        },
    });
    serde_json::to_vec(&body).unwrap_or_default()
}

/// FCM error codes that mean the token will never work again.
const FCM_GONE_CODES: [&str; 2] = ["UNREGISTERED", "SENDER_ID_MISMATCH"];

fn fcm_outcome(status: StatusCode, body: &[u8]) -> DeliveryOutcome {
    if status.is_success() {
        return DeliveryOutcome::Delivered;
    }
    if status == StatusCode::NOT_FOUND {
        return DeliveryOutcome::TokenGone;
    }
    let error_code = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/details")
                .and_then(serde_json::Value::as_array)
                .and_then(|details| {
                    details
                        .iter()
                        .find_map(|detail| detail.get("errorCode")?.as_str().map(str::to_owned))
                })
        });
    match error_code {
        Some(code) if FCM_GONE_CODES.contains(&code.as_str()) => DeliveryOutcome::TokenGone,
        _ => DeliveryOutcome::Failed,
    }
}

#[cfg(test)]
mod tests;
