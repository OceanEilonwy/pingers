// SPDX-License-Identifier: AGPL-3.0-only
//! The client's own records, kept in the store's encrypted key-value table.
//!
//! Interim, as pingers-parties does (its `records.rs`): pingers-store has no table for a
//! Party's colour, the device settings the gate reads, exclusion-zone schedules, the display
//! name or pingers-wake's `WakeSnapshot`. Until it does, they are written through the store's
//! `MlsStorage` implementation on `Session` under `pingers-client/v1/`, disjoint from
//! `openmls/v1/` and `pingers-parties/v1/`, so they join the session's transaction like any
//! other row.

use pingers_geo::{Circle, Coordinate, Fence, Meters, Polygon};
use pingers_mls::MlsStorage as _;
use pingers_proto::PartyId;
use pingers_store::{Session, StoreError};

use crate::ClientError;

const PREFIX: &[u8] = b"pingers-client/v1/";

/// A record's key.
pub(crate) enum Key<'id> {
    /// A Party's colour.
    Colour(&'id PartyId),
    /// The device settings the gate reads.
    DeviceSettings,
    /// The exclusion zones with their schedules.
    Zones,
    /// pingers-wake's state between wakes.
    WakeSnapshot,
    /// The name this device's user goes by in new Parties and joins.
    DisplayName,
}

impl Key<'_> {
    fn bytes(&self) -> Vec<u8> {
        let mut key = PREFIX.to_vec();
        match self {
            Self::Colour(party) => {
                key.extend_from_slice(b"colour/");
                key.extend_from_slice(party.as_bytes());
            }
            Self::DeviceSettings => key.extend_from_slice(b"device-settings"),
            Self::Zones => key.extend_from_slice(b"zones"),
            Self::WakeSnapshot => key.extend_from_slice(b"wake-snapshot"),
            Self::DisplayName => key.extend_from_slice(b"display-name"),
        }
        key
    }
}

/// The record under `key`, decoded, or `None`.
pub(crate) fn read<T>(
    session: &Session,
    key: &Key<'_>,
    decode: impl FnOnce(&mut Reader<'_>) -> Result<T, ClientError>,
) -> Result<Option<T>, ClientError> {
    let Some(bytes) = session.read(&key.bytes())? else {
        return Ok(None);
    };
    let mut reader = Reader { bytes: &bytes };
    let value = decode(&mut reader)?;
    reader.end()?;
    Ok(Some(value))
}

/// Writes the record under `key`.
pub(crate) fn write(
    session: &Session,
    key: &Key<'_>,
    encode: impl FnOnce(&mut Writer),
) -> Result<(), ClientError> {
    let mut writer = Writer(Vec::new());
    encode(&mut writer);
    Ok(session.write(&key.bytes(), &writer.0)?)
}

/// Deletes the record under `key`.
pub(crate) fn delete(session: &Session, key: &Key<'_>) -> Result<(), ClientError> {
    Ok(session.delete(&key.bytes())?)
}

/// A corrupt client record.
pub(crate) const fn corrupt() -> ClientError {
    ClientError::Store(StoreError::Corrupt {
        field: "pingers-client record",
    })
}

/// Encodes a record: fixed-width big-endian integers, length-prefixed lists.
pub(crate) struct Writer(Vec<u8>);

impl Writer {
    pub(crate) fn byte(&mut self, value: u8) {
        self.0.push(value);
    }

    pub(crate) fn u16(&mut self, value: u16) {
        self.0.extend_from_slice(&value.to_be_bytes());
    }

    pub(crate) fn u32(&mut self, value: u32) {
        self.0.extend_from_slice(&value.to_be_bytes());
    }

    pub(crate) fn u64(&mut self, value: u64) {
        self.0.extend_from_slice(&value.to_be_bytes());
    }

    pub(crate) fn f64(&mut self, value: f64) {
        self.u64(value.to_bits());
    }

    pub(crate) fn fixed(&mut self, bytes: &[u8]) {
        self.0.extend_from_slice(bytes);
    }

    /// A list length; lists here are short (Spots, zones, leases), so `u32` is plenty and a
    /// longer one is cut, which the reader then reports as corrupt rather than misreading.
    pub(crate) fn len(&mut self, len: usize) {
        self.u32(u32::try_from(len).unwrap_or(u32::MAX));
    }

    pub(crate) fn text(&mut self, text: &str) {
        self.len(text.len());
        self.fixed(text.as_bytes());
    }

    pub(crate) fn option<T>(&mut self, value: Option<T>, encode: impl FnOnce(&mut Self, T)) {
        match value {
            None => self.byte(0),
            Some(value) => {
                self.byte(1);
                encode(self, value);
            }
        }
    }

    pub(crate) fn fence(&mut self, fence: &Fence) {
        match fence {
            Fence::Circle(circle) => {
                self.byte(0);
                self.coordinate(circle.center());
                self.f64(circle.radius().get());
            }
            Fence::Polygon(polygon) => {
                self.byte(1);
                self.len(polygon.vertices().len());
                for vertex in polygon.vertices() {
                    self.coordinate(*vertex);
                }
            }
        }
    }

    fn coordinate(&mut self, coordinate: Coordinate) {
        self.f64(coordinate.latitude());
        self.f64(coordinate.longitude());
    }
}

/// Decodes what [`Writer`] wrote. Every failure is [`corrupt`].
pub(crate) struct Reader<'bytes> {
    bytes: &'bytes [u8],
}

impl Reader<'_> {
    pub(crate) fn fixed<const N: usize>(&mut self) -> Result<[u8; N], ClientError> {
        let (head, rest) = self.bytes.split_first_chunk::<N>().ok_or_else(corrupt)?;
        self.bytes = rest;
        Ok(*head)
    }

    pub(crate) fn byte(&mut self) -> Result<u8, ClientError> {
        Ok(u8::from_be_bytes(self.fixed()?))
    }

    pub(crate) fn u16(&mut self) -> Result<u16, ClientError> {
        Ok(u16::from_be_bytes(self.fixed()?))
    }

    pub(crate) fn u32(&mut self) -> Result<u32, ClientError> {
        Ok(u32::from_be_bytes(self.fixed()?))
    }

    pub(crate) fn u64(&mut self) -> Result<u64, ClientError> {
        Ok(u64::from_be_bytes(self.fixed()?))
    }

    pub(crate) fn f64(&mut self) -> Result<f64, ClientError> {
        Ok(f64::from_bits(self.u64()?))
    }

    pub(crate) fn len(&mut self) -> Result<usize, ClientError> {
        usize::try_from(self.u32()?).map_err(|_too_long| corrupt())
    }

    pub(crate) fn text(&mut self) -> Result<String, ClientError> {
        let len = self.len()?;
        let (head, rest) = self.bytes.split_at_checked(len).ok_or_else(corrupt)?;
        self.bytes = rest;
        String::from_utf8(head.to_vec()).map_err(|_not_utf8| corrupt())
    }

    pub(crate) fn list<T>(
        &mut self,
        mut decode: impl FnMut(&mut Self) -> Result<T, ClientError>,
    ) -> Result<Vec<T>, ClientError> {
        let len = self.len()?;
        // Each item takes at least one byte, so a corrupt length cannot reserve much.
        let mut items = Vec::with_capacity(len.min(self.bytes.len()));
        for _ in 0..len {
            items.push(decode(self)?);
        }
        Ok(items)
    }

    pub(crate) fn option<T>(
        &mut self,
        decode: impl FnOnce(&mut Self) -> Result<T, ClientError>,
    ) -> Result<Option<T>, ClientError> {
        match self.byte()? {
            0 => Ok(None),
            1 => decode(self).map(Some),
            _ => Err(corrupt()),
        }
    }

    pub(crate) fn fence(&mut self) -> Result<Fence, ClientError> {
        let fence = match self.byte()? {
            0 => {
                let center = self.coordinate()?;
                let radius = Meters::new(self.f64()?).map_err(|_bad| corrupt())?;
                Fence::Circle(Circle::new(center, radius).map_err(|_bad| corrupt())?)
            }
            1 => {
                let vertices = self.list(Self::coordinate)?;
                Fence::Polygon(Polygon::new(vertices).map_err(|_bad| corrupt())?)
            }
            _ => return Err(corrupt()),
        };
        Ok(fence)
    }

    fn coordinate(&mut self) -> Result<Coordinate, ClientError> {
        let latitude = self.f64()?;
        let longitude = self.f64()?;
        Coordinate::new(latitude, longitude).map_err(|_bad| corrupt())
    }

    fn end(&self) -> Result<(), ClientError> {
        if self.bytes.is_empty() {
            Ok(())
        } else {
            Err(corrupt())
        }
    }
}
