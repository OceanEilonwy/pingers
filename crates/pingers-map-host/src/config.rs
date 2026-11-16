// SPDX-License-Identifier: AGPL-3.0-only
//! The map host's configuration file, read once at start.
//!
//! ```toml
//! listen = "127.0.0.1:8090"
//! directory = "/srv/pingers-maps"   # pingers-tiles output (or the cache, in cache mode)
//! max_disk = "60GB"                 # B, KB, MB, GB, TB (decimal) or KiB, MiB, GiB, TiB
//! hosted = ["oceania", "europe/germany"]   # whole-segment prefixes of region ids
//! logging = "off"                   # or "counts": aggregate totals every counts_interval_s
//! counts_interval_s = 3600
//! ohttp_key_file = "/var/lib/pingers-map-host/ohttp-key"   # optional; default <directory>/.ohttp-key
//!
//! [max_zoom]                        # optional; the longest matching prefix wins
//! "europe/germany" = 13
//!
//! [cache]                           # optional; its presence turns on cache mode
//! regions = "regions.toml"          # pingers-tiles region list, relative to this file
//! upstream = "https://build.protomaps.com/20261112.pmtiles"
//! pmtiles = "/usr/local/bin/pmtiles"
//! max_concurrent_extracts = 1
//! retry_after_seconds = 30
//! ```
//!
//! `listen`, `directory`, `max_disk` and `hosted` are required. `hosted` may also be one
//! string of prefixes separated by spaces (`hosted = "oceania europe/germany"`), for template
//! systems whose settings have no list type; an empty list or string hosts nothing. The
//! directory and the region list are read when the host starts ([`crate::serve`]), not when
//! the file is parsed.
//!
//! `ohttp_key_file` holds the OHTTP gateway's private key (32 bytes, hex) that clients fetching
//! through a relay encapsulate to (`crate::gateway`). The host makes one there on first start
//! if the file does not exist. A relative path is resolved against the configuration file's
//! directory.

use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::error::ConfigError;
use crate::region::{AllowList, RegionId};

/// The deepest zoom a pack may be cut to: the deepest the Protomaps basemap has, and what
/// pingers-map-packs accepts in a region list.
pub const MAX_ZOOM_LIMIT: u8 = 15;

/// Whether the host reports anything: off by default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Logging {
    /// Nothing is written.
    Off,
    /// Requests per status class, body bytes and disk usage, once per interval. Never
    /// addresses, paths, regions or single requests.
    Counts {
        /// How often.
        interval: Duration,
    },
}

/// Cache mode: cut allow-listed regions from an upstream planet pack on first request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheConfig {
    /// The pingers-tiles region list, resolved against the configuration file's directory.
    pub regions: PathBuf,
    /// The upstream PMTiles archive (URL or path) that cuts read from.
    pub upstream: String,
    /// The `pmtiles` command-line tool.
    pub pmtiles: PathBuf,
    /// How many cuts may run at once; the rest queue.
    pub max_concurrent_extracts: NonZeroUsize,
    /// `Retry-After` while a region is being prepared, in seconds.
    pub retry_after_seconds: u32,
}

/// The validated configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapHostConfig {
    /// Address to listen on.
    pub listen: SocketAddr,
    /// The directory whose region files are served.
    pub directory: PathBuf,
    /// The disk budget for region files, in bytes.
    pub max_disk: u64,
    /// Which regions are hosted, and zoom overrides for cuts.
    pub allow: AllowList,
    /// Reporting.
    pub logging: Logging,
    /// Cache mode, if configured; otherwise the directory is served as it is.
    pub cache: Option<CacheConfig>,
    /// The OHTTP gateway key file: `ohttp_key_file`, or `.ohttp-key` in the directory (never
    /// served: the host serves no dotfiles).
    pub ohttp_key_file: PathBuf,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum FileLogging {
    Off,
    Counts,
}

const fn default_logging() -> FileLogging {
    FileLogging::Off
}
const fn default_counts_interval() -> u64 {
    3600
}
const fn default_concurrent_extracts() -> usize {
    1
}
const fn default_retry_after() -> u32 {
    30
}
fn default_pmtiles() -> PathBuf {
    PathBuf::from("pmtiles")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    listen: SocketAddr,
    directory: PathBuf,
    max_disk: String,
    hosted: FileHosted,
    #[serde(default)]
    max_zoom: std::collections::BTreeMap<String, u8>,
    #[serde(default = "default_logging")]
    logging: FileLogging,
    #[serde(default = "default_counts_interval")]
    counts_interval_s: u64,
    cache: Option<FileCache>,
    ohttp_key_file: Option<PathBuf>,
}

/// `hosted`: a TOML array, or one space-separated string.
#[derive(Deserialize)]
#[serde(untagged)]
enum FileHosted {
    List(Vec<String>),
    Spaced(String),
}

impl FileHosted {
    fn prefixes(&self) -> Vec<&str> {
        match self {
            Self::List(list) => list.iter().map(String::as_str).collect(),
            Self::Spaced(text) => text.split_whitespace().collect(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileCache {
    regions: PathBuf,
    upstream: String,
    #[serde(default = "default_pmtiles")]
    pmtiles: PathBuf,
    #[serde(default = "default_concurrent_extracts")]
    max_concurrent_extracts: usize,
    #[serde(default = "default_retry_after")]
    retry_after_seconds: u32,
}

impl MapHostConfig {
    /// Parses and checks a configuration file's text. `base` is the file's directory: a
    /// relative `cache.regions` is resolved against it.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Syntax`] for TOML or schema errors, [`ConfigError::BadSize`] for
    /// `max_disk`, [`ConfigError::BadRegionId`] for an id in `hosted` or `max_zoom`, and
    /// [`ConfigError::OutOfRange`] for other values.
    pub fn parse(text: &str, base: &Path) -> Result<Self, ConfigError> {
        let file: FileConfig = toml::from_str(text).map_err(|error| ConfigError::Syntax {
            line: error.span().map_or(0, |span| line_of(text, span.start)),
        })?;
        if file.directory.as_os_str().is_empty() {
            return Err(ConfigError::OutOfRange { field: "directory" });
        }
        let logging = match file.logging {
            FileLogging::Off => Logging::Off,
            FileLogging::Counts => match file.counts_interval_s {
                0 => {
                    return Err(ConfigError::OutOfRange {
                        field: "counts_interval_s",
                    });
                }
                seconds => Logging::Counts {
                    interval: Duration::from_secs(seconds),
                },
            },
        };
        let max_disk = parse_size(&file.max_disk).ok_or(ConfigError::BadSize)?;
        let region = |text: &str| {
            RegionId::parse(text).ok_or_else(|| ConfigError::BadRegionId {
                id: text.to_owned(),
            })
        };
        let hosted = file
            .hosted
            .prefixes()
            .into_iter()
            .map(region)
            .collect::<Result<Vec<_>, _>>()?;
        let max_zoom = file
            .max_zoom
            .iter()
            .map(|(prefix, zoom)| {
                if *zoom > MAX_ZOOM_LIMIT {
                    return Err(ConfigError::OutOfRange { field: "max_zoom" });
                }
                Ok((region(prefix)?, *zoom))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let cache = file
            .cache
            .map(|cache| {
                if cache.upstream.is_empty() {
                    return Err(ConfigError::OutOfRange {
                        field: "cache.upstream",
                    });
                }
                Ok(CacheConfig {
                    regions: base.join(cache.regions),
                    upstream: cache.upstream,
                    pmtiles: cache.pmtiles,
                    max_concurrent_extracts: NonZeroUsize::new(cache.max_concurrent_extracts)
                        .ok_or(ConfigError::OutOfRange {
                            field: "cache.max_concurrent_extracts",
                        })?,
                    retry_after_seconds: match cache.retry_after_seconds {
                        0 => {
                            return Err(ConfigError::OutOfRange {
                                field: "cache.retry_after_seconds",
                            });
                        }
                        seconds => seconds,
                    },
                })
            })
            .transpose()?;
        let ohttp_key_file = file
            .ohttp_key_file
            .map_or_else(|| file.directory.join(".ohttp-key"), |path| base.join(path));
        Ok(Self {
            listen: file.listen,
            directory: file.directory,
            max_disk,
            allow: AllowList { hosted, max_zoom },
            logging,
            cache,
            ohttp_key_file,
        })
    }
}

/// Parses a size such as `60GB`, `500 MiB` or `1024`: a whole number and an optional unit,
/// B, KB, MB, GB, TB (powers of 1000) or KiB, MiB, GiB, TiB (powers of 1024), any case.
#[must_use]
pub fn parse_size(text: &str) -> Option<u64> {
    let text = text.trim();
    let split = text
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(text.len());
    let (number, unit) = text.split_at_checked(split)?;
    if number.is_empty() {
        return None;
    }
    let number: u64 = number.parse().ok()?;
    let multiplier: u64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "kb" => 1_000,
        "mb" => 1_000_000,
        "gb" => 1_000_000_000,
        "tb" => 1_000_000_000_000,
        "kib" => 1 << 10,
        "mib" => 1 << 20,
        "gib" => 1 << 30,
        "tib" => 1 << 40,
        _ => return None,
    };
    number.checked_mul(multiplier)
}

pub(crate) fn line_of(text: &str, offset: usize) -> usize {
    text.bytes()
        .take(offset)
        .filter(|byte| *byte == b'\n')
        .count()
        .saturating_add(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = "listen = \"127.0.0.1:8090\"\ndirectory = \"/srv/pingers-maps\"\n\
                           max_disk = \"60GB\"\nhosted = [\"oceania\"]\n";

    fn parse(text: &str) -> Result<MapHostConfig, ConfigError> {
        MapHostConfig::parse(text, Path::new("/etc/pingers"))
    }

    #[test]
    fn minimal_file_is_pre_extracted_with_logging_off() {
        assert_eq!(
            parse(MINIMAL),
            Ok(MapHostConfig {
                listen: SocketAddr::from(([127, 0, 0, 1], 8090)),
                directory: PathBuf::from("/srv/pingers-maps"),
                max_disk: 60_000_000_000,
                allow: AllowList {
                    hosted: RegionId::parse("oceania").into_iter().collect(),
                    max_zoom: Vec::new(),
                },
                logging: Logging::Off,
                cache: None,
                ohttp_key_file: PathBuf::from("/srv/pingers-maps/.ohttp-key"),
            })
        );
    }

    #[test]
    fn full_file_parses() {
        let text = format!(
            "{MINIMAL}logging = \"counts\"\ncounts_interval_s = 60\nohttp_key_file = \"keys/map\"\n\
             [max_zoom]\n\"europe/germany\" = 13\n\
             [cache]\nregions = \"regions.toml\"\nupstream = \"https://example.org/p.pmtiles\"\n\
             max_concurrent_extracts = 2\nretry_after_seconds = 10\n"
        );
        let config = parse(&text);
        assert_eq!(
            config.as_ref().map(|config| config.logging),
            Ok(Logging::Counts {
                interval: Duration::from_secs(60)
            })
        );
        assert_eq!(
            config.as_ref().map(|config| config.allow.max_zoom.len()),
            Ok(1)
        );
        assert_eq!(
            config.as_ref().map(|config| config.ohttp_key_file.clone()),
            Ok(PathBuf::from("/etc/pingers/keys/map")),
            "a relative key file is beside the configuration"
        );
        let cache = config.ok().and_then(|config| config.cache);
        assert_eq!(
            cache,
            Some(CacheConfig {
                regions: PathBuf::from("/etc/pingers/regions.toml"),
                upstream: "https://example.org/p.pmtiles".to_owned(),
                pmtiles: PathBuf::from("pmtiles"),
                max_concurrent_extracts: NonZeroUsize::MIN.saturating_add(1),
                retry_after_seconds: 10,
            })
        );
    }

    #[test]
    fn bad_values_are_refused() {
        assert_eq!(
            parse(&format!(
                "{MINIMAL}logging = \"counts\"\ncounts_interval_s = 0\n"
            )),
            Err(ConfigError::OutOfRange {
                field: "counts_interval_s"
            })
        );
        assert_eq!(
            parse(&MINIMAL.replace("60GB", "60 parsecs")),
            Err(ConfigError::BadSize)
        );
        assert_eq!(
            parse(&MINIMAL.replace("oceania", "Oceania")),
            Err(ConfigError::BadRegionId {
                id: "Oceania".to_owned()
            })
        );
        assert_eq!(
            parse(&format!("{MINIMAL}[max_zoom]\n\"oceania\" = 30\n")),
            Err(ConfigError::OutOfRange { field: "max_zoom" })
        );
        assert_eq!(
            parse(&format!("{MINIMAL}extra = 1\n")),
            Err(ConfigError::Syntax { line: 5 })
        );
        assert!(parse("listen = \"127.0.0.1:8090\"\ndirectory = \"/d\"\n").is_err());
    }

    fn hosted_ids(text: &str) -> Result<Vec<String>, ConfigError> {
        let config = parse(&MINIMAL.replace("hosted = [\"oceania\"]", text))?;
        Ok(config
            .allow
            .hosted
            .iter()
            .map(|id| id.as_str().to_owned())
            .collect())
    }

    #[test]
    fn hosted_may_be_one_space_separated_string() {
        let both = Ok(vec!["oceania".to_owned(), "europe/germany".to_owned()]);
        assert_eq!(hosted_ids("hosted = \"oceania europe/germany\""), both);
        assert_eq!(hosted_ids("hosted = \"  oceania\teurope/germany \""), both);
        assert_eq!(
            hosted_ids("hosted = [\"oceania\", \"europe/germany\"]"),
            both
        );
        assert_eq!(hosted_ids("hosted = \"\""), Ok(Vec::new()));
    }

    #[test]
    fn a_bad_prefix_in_a_hosted_string_is_named() {
        assert_eq!(
            hosted_ids("hosted = \"oceania Europe\""),
            Err(ConfigError::BadRegionId {
                id: "Europe".to_owned()
            })
        );
        assert!(matches!(
            hosted_ids("hosted = 3"),
            Err(ConfigError::Syntax { .. })
        ));
    }

    #[test]
    fn sizes_take_decimal_and_binary_units() {
        assert_eq!(parse_size("60GB"), Some(60_000_000_000));
        assert_eq!(parse_size("60 gb"), Some(60_000_000_000));
        assert_eq!(parse_size("2KiB"), Some(2048));
        assert_eq!(parse_size("1TiB"), Some(1 << 40));
        assert_eq!(parse_size("512"), Some(512));
        assert_eq!(parse_size("7B"), Some(7));
        for bad in ["", "GB", "1.5GB", "-1GB", "1 XB", "99999999999TB"] {
            assert_eq!(parse_size(bad), None, "{bad}");
        }
    }
}
