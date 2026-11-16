// SPDX-License-Identifier: AGPL-3.0-only
//! The two ways to reach a map host (design B.12): [`DirectFetch`] over HTTPS, and
//! [`RelayFetch`] through the OHTTP relay and the Pingers server to the map host's own
//! gateway.

use std::sync::{Arc, Mutex, PoisonError};

use pingers_ohttp::{BinaryRequest, BinaryResponse, Header, KeyConfig, encapsulate_request_padded};
use pingers_proto::{KeyConfigHash, MapHostUrl};
use pingers_transport::{PrivacyClass, PublicClient, Transport};
use rand_core::CryptoRng;

use crate::{MapHost, MapsError};

/// How many bytes one request asks for, and whether answers and request counts are padded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PieceSize {
    /// The most body bytes one ranged request asks for.
    pub bytes: u64,
    /// Whether every answer is padded to one size and the number of requests per download to
    /// a size class ([`padded_count`]).
    pub padding: Padding,
}

/// Whether a route pads what observers on the path can measure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Padding {
    /// Direct: the map host sees the file anyway.
    Off,
    /// Relayed: the relay and the server see only sizes, so sizes must not name a region.
    SizeClasses,
}

/// One way to reach a map host.
#[async_trait::async_trait]
pub trait MapFetch: Send + Sync {
    /// `GET path` (with query, starting with `/`) from `host`, with `headers`.
    ///
    /// # Errors
    ///
    /// [`MapsError::NotThroughRelay`] if this route cannot reach `host`; otherwise any
    /// [`MapsError`] about the exchange.
    async fn get(
        &self,
        host: &MapHost,
        path: &str,
        headers: Vec<Header>,
    ) -> Result<BinaryResponse, MapsError>;

    /// The piece size and padding of this route.
    fn piece(&self) -> PieceSize;
}

/// The piece size for direct downloads: few requests, bounded memory.
pub const DIRECT_PIECE: u64 = 4 * 1024 * 1024;
/// The piece size through the relay: one piece and its headers fit [`RELAY_PADDED`].
pub const RELAY_PIECE: u64 = 255 * 1024;
/// What every relayed answer is padded to, before encapsulation (the map host's
/// `pingers-pad`).
pub const RELAY_PADDED: usize = 256 * 1024;
/// What every relayed request is padded to, before encapsulation: paths name regions, so
/// their length must not show.
pub const RELAY_REQUEST_PADDED: usize = 1024;

/// Straight to the map host over HTTPS.
pub struct DirectFetch {
    client: PublicClient,
}

impl DirectFetch {
    /// Over `client`, which must accept bodies of [`DIRECT_PIECE`] and a little more.
    #[must_use]
    pub const fn new(client: PublicClient) -> Self {
        Self { client }
    }
}

#[async_trait::async_trait]
impl MapFetch for DirectFetch {
    async fn get(
        &self,
        host: &MapHost,
        path: &str,
        headers: Vec<Header>,
    ) -> Result<BinaryResponse, MapsError> {
        Ok(self.client.get(&host.file_url(path), &headers).await?)
    }

    fn piece(&self) -> PieceSize {
        PieceSize {
            bytes: DIRECT_PIECE,
            padding: Padding::Off,
        }
    }
}

/// Where the map host gateway keys seen on first use are kept, one per host (design B.12:
/// the server forwards the key config, so a key that changes later fails closed).
#[async_trait::async_trait]
pub trait KeyPinStore: Send + Sync {
    /// The pinned key config hash for `host`, if any.
    ///
    /// # Errors
    ///
    /// [`MapsError::PinStore`] if the store cannot be read.
    async fn pinned(&self, host: &MapHostUrl) -> Result<Option<KeyConfigHash>, MapsError>;

    /// Pins `hash` for `host`.
    ///
    /// # Errors
    ///
    /// [`MapsError::PinStore`] if the store cannot be written.
    async fn pin(&self, host: &MapHostUrl, hash: KeyConfigHash) -> Result<(), MapsError>;
}

/// Through the OHTTP relay and the Pingers server, sealed to the map host's own gateway.
///
/// Only the host the server advertises can be reached this way: the server forwards to its
/// map host's gateway and nowhere else.
pub struct RelayFetch {
    transport: Arc<dyn Transport>,
    server_host: Option<MapHostUrl>,
    pins: Arc<dyn KeyPinStore>,
    rng: Mutex<Box<dyn CryptoRng + Send>>,
    key: tokio::sync::Mutex<Option<KeyConfig>>,
}

/// What [`RelayFetch::new`] is built from.
pub struct RelayParts {
    /// The router: map requests go as [`PrivacyClass::Anonymous`].
    pub transport: Arc<dyn Transport>,
    /// The map host the server advertises, if it has said.
    pub server_host: Option<MapHostUrl>,
    /// Where gateway keys are pinned.
    pub pins: Arc<dyn KeyPinStore>,
    /// Randomness for sealing.
    pub rng: Box<dyn CryptoRng + Send>,
}

impl RelayFetch {
    /// A relayed route.
    #[must_use]
    pub fn new(parts: RelayParts) -> Self {
        Self {
            transport: parts.transport,
            server_host: parts.server_host.map(|url| {
                MapHost::new(&url, crate::MapHostSource::Server)
                    .url()
                    .clone()
            }),
            pins: parts.pins,
            rng: Mutex::new(parts.rng),
            key: tokio::sync::Mutex::new(None),
        }
    }

    /// The gateway's key config, fetched through the relay once and checked against the pin.
    async fn key_config(&self, host: &MapHostUrl) -> Result<KeyConfig, MapsError> {
        let mut key = self.key.lock().await;
        if let Some(config) = key.as_ref() {
            return Ok(config.clone());
        }
        let answer = self
            .transport
            .request(
                PrivacyClass::Anonymous,
                BinaryRequest {
                    method: "GET".to_owned(),
                    path: KEYS_PATH.to_owned(),
                    headers: Vec::new(),
                    body: Vec::new(),
                },
            )
            .await?;
        check_forwarded(&answer)?;
        let config = KeyConfig::decode(&answer.body)?;
        match self.pins.pinned(host).await? {
            Some(pinned) if pinned != config.hash() => return Err(MapsError::KeyChanged),
            Some(_) => {}
            None => self.pins.pin(host, config.hash()).await?,
        }
        *key = Some(config.clone());
        Ok(config)
    }
}

/// The server's route to the map host gateway's key config.
const KEYS_PATH: &str = "/v1/maps/ohttp-keys";
/// The server's route that forwards sealed map requests.
const REQUEST_PATH: &str = "/v1/maps/ohttp";

/// The server's own answer to a forwarded request: 200, or why not.
fn check_forwarded(answer: &BinaryResponse) -> Result<(), MapsError> {
    match answer.status.get() {
        200 => Ok(()),
        // The server forwards to no map host at all.
        404 => Err(MapsError::NotThroughRelay),
        status => Err(MapsError::Status { status }),
    }
}

#[async_trait::async_trait]
impl MapFetch for RelayFetch {
    async fn get(
        &self,
        host: &MapHost,
        path: &str,
        mut headers: Vec<Header>,
    ) -> Result<BinaryResponse, MapsError> {
        if self.server_host.as_ref() != Some(host.url()) {
            return Err(MapsError::NotThroughRelay);
        }
        let key = self.key_config(host.url()).await?;
        headers.push(Header {
            name: "pingers-pad".to_owned(),
            value: RELAY_PADDED.to_string().into_bytes(),
        });
        let inner = BinaryRequest {
            method: "GET".to_owned(),
            path: path.to_owned(),
            headers,
            body: Vec::new(),
        };
        let sealed = {
            let mut rng = self.rng.lock().unwrap_or_else(PoisonError::into_inner);
            encapsulate_request_padded(&key, &inner, RELAY_REQUEST_PADDED, &mut *rng)?
        };
        let answer = self
            .transport
            .request(
                PrivacyClass::Anonymous,
                BinaryRequest {
                    method: "POST".to_owned(),
                    path: REQUEST_PATH.to_owned(),
                    headers: vec![Header {
                        name: "content-type".to_owned(),
                        value: b"message/ohttp-req".to_vec(),
                    }],
                    body: sealed.message,
                },
            )
            .await?;
        check_forwarded(&answer)?;
        Ok(sealed.response_context.open(&answer.body)?)
    }

    fn piece(&self) -> PieceSize {
        PieceSize {
            bytes: RELAY_PIECE,
            padding: Padding::SizeClasses,
        }
    }
}

/// `count` rounded up to the next size class: 1, 2, 3, 4, 6, 8, 12, 16, 24, ... (powers of two
/// and one and a half times them), so a relayed download's number of requests says only which
/// class its size is in, at a cost of at most half again.
#[must_use]
pub fn padded_count(count: u32) -> u32 {
    if count <= 4 {
        return count;
    }
    let Some(power) = count.checked_next_power_of_two() else {
        return count;
    };
    let between = power.checked_div(4).unwrap_or(0).saturating_mul(3);
    if count <= between { between } else { power }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    #[test]
    fn a_relayed_piece_fits_its_padding_with_little_to_spare() -> Result<(), MapsError> {
        let header = |name: &str, value: &str| Header {
            name: name.to_owned(),
            value: value.as_bytes().to_vec(),
        };
        let piece = BinaryResponse {
            status: pingers_ohttp::StatusCode::new(206)?,
            headers: vec![
                header("content-type", "application/vnd.pmtiles"),
                header("content-range", "bytes 9999738880-9999999999/10000000000"),
                header("etag", &format!("\"{}\"", "ab".repeat(32))),
                header("accept-ranges", "bytes"),
            ],
            body: vec![0; usize::try_from(RELAY_PIECE).map_err(|_| MapsError::Internal)?],
        };
        let encoded = piece.encode()?.len();
        assert!(encoded <= RELAY_PADDED, "{encoded}");
        assert!(RELAY_PADDED - encoded < 2048, "pieces waste the padding");
        // The map host's largest padded answer holds a relayed piece.
        const { assert!(RELAY_PADDED < 1_044_480) };
        Ok(())
    }

    #[test]
    fn direct_pieces_are_four_mebibytes_unpadded() -> Result<(), MapsError> {
        let client = PublicClient::new(
            &pingers_transport::PublicConfig::new(pingers_transport::TlsRoots::from_der(&[])?, 1),
            Arc::new(pingers_time::ManualClock::new(
                pingers_time::UnixSeconds::EPOCH,
            )),
        )?;
        assert_eq!(
            DirectFetch::new(client).piece(),
            PieceSize {
                bytes: 4_194_304,
                padding: Padding::Off
            }
        );
        Ok(())
    }

    #[test]
    fn request_counts_round_up_to_size_classes() {
        let classes: Vec<u32> = (0..=17).map(padded_count).collect();
        assert_eq!(
            classes,
            [
                0, 1, 2, 3, 4, 6, 6, 8, 8, 12, 12, 12, 12, 16, 16, 16, 16, 24
            ]
        );
        assert_eq!(padded_count(25), 32);
        assert_eq!(padded_count(1000), 1024);
        assert_eq!(padded_count(769), 1024);
        assert_eq!(padded_count(768), 768);
        assert_eq!(padded_count(u32::MAX), u32::MAX);
    }
}
