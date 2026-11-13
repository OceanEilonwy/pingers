// SPDX-License-Identifier: AGPL-3.0-only
//! Operator tool: cuts region map packs from the Protomaps daily build.
//!
//! - `plan`: prints the `pmtiles extract` command for each region, and with `--estimate` (or a
//!   host config, whose `max_disk` it checks) the size of each pack from a dry run.
//! - `cut`: runs the extracts, then writes each region's search index and manifest and a
//!   `catalog.json`. Refuses up front if the packs would not fit the host's `max_disk`.
//! - `publish`: writes indexes, manifests and the catalog for packs already in the directory
//!   (cut elsewhere with the commands `plan` prints).
//!
//! The operator workflow is in this crate's README.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, bail};
use pingers_map_packs::extract::{Extractor, Publication, RegionFiles, publish};
use pingers_map_packs::manifest::{Catalog, FORMAT, build_date_of};
use pingers_map_packs::regions::{Region, RegionId, RegionList, Selection};
use pingers_map_packs::size::{BudgetLine, BudgetReport, ByteSize};
use serde::Deserialize;

const USAGE: &str = "\
pingers-tiles: cut region map packs from the Protomaps build

Usage:
  pingers-tiles plan REGIONS.toml --build URL [--out DIR] [--host-config HOST.toml]
                [--pmtiles PATH] [--estimate]
  pingers-tiles cut REGIONS.toml --build URL --out DIR [--host-config HOST.toml]
                [--pmtiles PATH] [--build-date YYYY-MM-DD]
  pingers-tiles publish REGIONS.toml --build URL --out DIR [--host-config HOST.toml]
                [--build-date YYYY-MM-DD]
  pingers-tiles help

--build is a Protomaps daily build, e.g. https://build.protomaps.com/20261112.pmtiles
(list: https://maps.protomaps.com/builds/). --host-config reads `hosted`, `[max_zoom]` and
`max_disk` from a pingers-map-host config, so only what that host carries is cut, at its
zooms, and only if it fits. --pmtiles defaults to `pmtiles` on the PATH.
";

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Plan { estimate: bool },
    Cut,
    Publish,
}

#[derive(Debug, PartialEq, Eq)]
struct Invocation {
    action: Action,
    regions: PathBuf,
    build: String,
    out: PathBuf,
    host_config: Option<PathBuf>,
    pmtiles: PathBuf,
    build_date: Option<String>,
}

fn parse(arguments: &[String]) -> anyhow::Result<Option<Invocation>> {
    let mut words = arguments.iter();
    let action = match words.next().map(String::as_str) {
        None | Some("help" | "--help" | "-h") => return Ok(None),
        Some("plan") => Action::Plan { estimate: false },
        Some("cut") => Action::Cut,
        Some("publish") => Action::Publish,
        Some(other) => bail!("unknown command `{other}`"),
    };
    let mut invocation = Invocation {
        action,
        regions: PathBuf::new(),
        build: String::new(),
        out: PathBuf::from("."),
        host_config: None,
        pmtiles: PathBuf::from("pmtiles"),
        build_date: None,
    };
    let mut out_given = false;
    while let Some(word) = words.next() {
        let mut value = |flag: &str| {
            words
                .next()
                .cloned()
                .with_context(|| format!("{flag} needs a value"))
        };
        match word.as_str() {
            "--build" => invocation.build = value("--build")?,
            "--out" => {
                invocation.out = value("--out")?.into();
                out_given = true;
            }
            "--host-config" => invocation.host_config = Some(value("--host-config")?.into()),
            "--pmtiles" => invocation.pmtiles = value("--pmtiles")?.into(),
            "--build-date" => invocation.build_date = Some(value("--build-date")?),
            "--estimate" if matches!(invocation.action, Action::Plan { .. }) => {
                invocation.action = Action::Plan { estimate: true };
            }
            flag if flag.starts_with('-') => bail!("unexpected flag `{flag}`"),
            path if invocation.regions.as_os_str().is_empty() => invocation.regions = path.into(),
            other => bail!("unexpected argument `{other}`"),
        }
    }
    if invocation.regions.as_os_str().is_empty() {
        bail!("missing REGIONS.toml");
    }
    if invocation.build.is_empty() {
        bail!("missing --build URL");
    }
    if invocation.action != (Action::Plan { estimate: false })
        && invocation.action != (Action::Plan { estimate: true })
        && !out_given
    {
        bail!("missing --out DIR");
    }
    Ok(Some(invocation))
}

/// The parts of a pingers-map-host config this tool reads; the rest is the host's business.
#[derive(Deserialize)]
struct HostConfig {
    hosted: Vec<String>,
    #[serde(default)]
    max_zoom: BTreeMap<String, u8>,
    max_disk: String,
}

struct Host {
    selection: Selection,
    max_disk: Option<ByteSize>,
}

fn read_host(path: Option<&Path>) -> anyhow::Result<Host> {
    let Some(path) = path else {
        // No host config: every region, at its own zoom, no budget.
        return Ok(Host {
            selection: Selection::default(),
            max_disk: None,
        });
    };
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let config: HostConfig =
        toml::from_str(&text).with_context(|| format!("in {}", path.display()))?;
    let hosted = config
        .hosted
        .iter()
        .map(|id| RegionId::new(id))
        .collect::<Result<Vec<_>, _>>()?;
    let max_zoom = config
        .max_zoom
        .iter()
        .map(|(id, zoom)| Ok((RegionId::new(id)?, *zoom)))
        .collect::<Result<BTreeMap<_, _>, pingers_map_packs::regions::RegionError>>()?;
    Ok(Host {
        selection: Selection { hosted, max_zoom },
        max_disk: Some(ByteSize::parse(&config.max_disk)?),
    })
}

fn chosen_regions(invocation: &Invocation, host: &Host) -> anyhow::Result<Vec<Region>> {
    let text = fs::read_to_string(&invocation.regions)
        .with_context(|| format!("reading {}", invocation.regions.display()))?;
    let base = invocation
        .regions
        .parent()
        .unwrap_or_else(|| Path::new("."));
    let list = RegionList::parse(&text, base)
        .with_context(|| format!("in {}", invocation.regions.display()))?;
    if host.max_disk.is_none() {
        return Ok(list.regions);
    }
    let chosen = host.selection.select(&list);
    if chosen.is_empty() {
        bail!(
            "the host config's `hosted` names no region of {}",
            invocation.regions.display()
        );
    }
    Ok(chosen)
}

fn budget(
    out: &mut impl io::Write,
    extractor: &Extractor,
    regions: &[Region],
    max_disk: Option<ByteSize>,
) -> anyhow::Result<BudgetReport> {
    let mut lines = Vec::new();
    for region in regions {
        writeln!(out, "estimating {} (dry run) ...", region.id)?;
        let size = extractor.estimate(region)?;
        lines.push(BudgetLine {
            id: region.id.to_string(),
            max_zoom: region.max_zoom,
            size,
        });
    }
    let report = BudgetReport::new(lines, max_disk.unwrap_or(ByteSize::new(u64::MAX)));
    write!(out, "{report}")?;
    Ok(report)
}

fn write_catalog(
    directory: &Path,
    manifests: Vec<pingers_map_packs::manifest::Manifest>,
) -> anyhow::Result<()> {
    let catalog = Catalog {
        format: FORMAT,
        regions: manifests,
    };
    let path = directory.join("catalog.json");
    let mut json = serde_json::to_string_pretty(&catalog)?;
    json.push('\n');
    fs::write(&path, json).with_context(|| format!("writing {}", path.display()))
}

fn run(invocation: &Invocation) -> anyhow::Result<ExitCode> {
    let mut out = io::stdout().lock();
    let host = read_host(invocation.host_config.as_deref())?;
    let regions = chosen_regions(invocation, &host)?;
    let extractor = Extractor {
        pmtiles: invocation.pmtiles.clone(),
        build: invocation.build.clone(),
    };
    let build_date = match &invocation.build_date {
        Some(date) => date.clone(),
        None => build_date_of(&invocation.build)
            .context("cannot tell the build date from --build; pass --build-date")?,
    };
    match invocation.action {
        Action::Plan { estimate } => {
            for region in &regions {
                writeln!(
                    out,
                    "{}",
                    extractor.command_line(region, &RegionFiles::of(region, &invocation.out).pack)
                )?;
            }
            if estimate || host.max_disk.is_some() {
                let report = budget(&mut out, &extractor, &regions, host.max_disk)?;
                if !report.fits() {
                    return Ok(ExitCode::FAILURE);
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Action::Cut | Action::Publish => {
            fs::create_dir_all(&invocation.out)
                .with_context(|| format!("creating {}", invocation.out.display()))?;
            if invocation.action == Action::Cut {
                if host.max_disk.is_some() {
                    let report = budget(&mut out, &extractor, &regions, host.max_disk)?;
                    if !report.fits() {
                        bail!(
                            "these packs would not fit max_disk; host fewer regions or lower their max_zoom"
                        );
                    }
                }
                for region in &regions {
                    writeln!(out, "cutting {} at zoom {} ...", region.id, region.max_zoom)?;
                    extractor.extract(region, &RegionFiles::of(region, &invocation.out).pack)?;
                }
            }
            let publication = Publication {
                directory: &invocation.out,
                source: &invocation.build,
                build_date: &build_date,
            };
            let mut manifests = Vec::new();
            for region in &regions {
                let manifest = publish(region, &publication)
                    .with_context(|| format!("publishing {}", region.id))?;
                writeln!(
                    out,
                    "{}: pack {} ({}), {} places",
                    region.id,
                    ByteSize::new(manifest.pack.size),
                    manifest.pack.sha256,
                    manifest.index.entries.unwrap_or(0)
                )?;
                manifests.push(manifest);
            }
            write_catalog(&invocation.out, manifests)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let result = parse(&arguments).and_then(|invocation| {
        if let Some(invocation) = invocation {
            run(&invocation)
        } else {
            io::stdout().write_all(USAGE.as_bytes())?;
            Ok(ExitCode::SUCCESS)
        }
    });
    result.unwrap_or_else(|error| {
        let _ignored = writeln!(io::stderr(), "pingers-tiles: {error:#}");
        ExitCode::FAILURE
    })
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn command_lines_parse() -> anyhow::Result<()> {
        let plan = parse(&words(
            "plan regions.toml --build https://b/20261112.pmtiles --estimate",
        ))?;
        assert_eq!(
            plan.map(|invocation| invocation.action),
            Some(Action::Plan { estimate: true })
        );
        let cut = parse(&words(
            "cut r.toml --build x --out /srv/packs --host-config host.toml",
        ))?;
        assert_eq!(
            cut.as_ref().map(|invocation| invocation.out.clone()),
            Some(PathBuf::from("/srv/packs"))
        );
        assert_eq!(
            cut.and_then(|invocation| invocation.host_config),
            Some(PathBuf::from("host.toml"))
        );
        assert!(parse(&words("cut r.toml --build x")).is_err());
        assert!(parse(&words("plan --build x")).is_err());
        assert!(parse(&words("cut r.toml --build x --out d --estimate")).is_err());
        assert_eq!(parse(&[])?, None);
        Ok(())
    }

    #[test]
    fn host_configs_select_regions_and_set_the_budget() -> anyhow::Result<()> {
        let directory =
            std::env::temp_dir().join(format!("pingers-tiles-test-{}", std::process::id()));
        fs::create_dir_all(&directory)?;
        let host = directory.join("host.toml");
        fs::write(
            &host,
            "listen = \"127.0.0.1:8090\"\nmax_disk = \"60GB\"\nhosted = [\"oceania\"]\n[max_zoom]\n\"oceania\" = 13\n",
        )?;
        let regions = directory.join("regions.toml");
        fs::write(
            &regions,
            "[[region]]\nid = \"oceania/australia\"\nname = \"Australia\"\npolygon = \"au.geojson\"\nmax_zoom = 15\n\
             [[region]]\nid = \"europe/germany\"\nname = \"Germany\"\npolygon = \"de.geojson\"\nmax_zoom = 15\n",
        )?;
        let read = read_host(Some(&host))?;
        assert_eq!(read.max_disk, Some(ByteSize::new(60_000_000_000)));
        let invocation = Invocation {
            action: Action::Plan { estimate: false },
            regions,
            build: "x".into(),
            out: directory.clone(),
            host_config: Some(host),
            pmtiles: "pmtiles".into(),
            build_date: None,
        };
        let chosen = chosen_regions(&invocation, &read)?;
        let ids: Vec<(&str, u8)> = chosen
            .iter()
            .map(|region| (region.id.as_str(), region.max_zoom))
            .collect();
        assert_eq!(ids, vec![("oceania/australia", 13)]);
        fs::remove_dir_all(&directory)?;
        Ok(())
    }

    /// A stand-in for `pmtiles` that answers every dry run with `size` and fails anything else.
    #[cfg(unix)]
    fn fake_pmtiles(directory: &Path, size: &str) -> anyhow::Result<PathBuf> {
        use std::os::unix::fs::PermissionsExt as _;
        let path = directory.join("pmtiles");
        fs::write(
            &path,
            format!(
                "#!/bin/sh\ncase \"$*\" in *--dry-run*) echo \"Extract transferred x for an archive size of {size}\" >&2; exit 0;; esac\nexit 7\n"
            ),
        )?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        Ok(path)
    }

    #[cfg(unix)]
    #[test]
    fn cuts_over_the_disk_budget_are_refused_before_extracting() -> anyhow::Result<()> {
        let directory =
            std::env::temp_dir().join(format!("pingers-tiles-budget-{}", std::process::id()));
        fs::create_dir_all(&directory)?;
        let host = directory.join("host.toml");
        fs::write(&host, "max_disk = \"60GB\"\nhosted = [\"europe\"]\n")?;
        let regions = directory.join("regions.toml");
        fs::write(
            &regions,
            "[[region]]\nid = \"europe/germany\"\nname = \"Germany\"\npolygon = \"de.geojson\"\nmax_zoom = 15\n\
             [[region]]\nid = \"europe/france\"\nname = \"France\"\npolygon = \"fr.geojson\"\nmax_zoom = 15\n",
        )?;
        let invocation = |action: Action, pmtiles: PathBuf| Invocation {
            action,
            regions: regions.clone(),
            build: "https://build.protomaps.com/20261112.pmtiles".into(),
            out: directory.join("out"),
            host_config: Some(host.clone()),
            pmtiles,
            build_date: None,
        };
        // 2 x 40 GB > 60 GB: refused, and the (failing) extract never runs.
        let big = fake_pmtiles(&directory, "40 GB")?;
        let refused = run(&invocation(Action::Cut, big.clone()));
        assert!(refused.is_err_and(|error| error.to_string().contains("max_disk")));
        assert_eq!(
            run(&invocation(Action::Plan { estimate: true }, big))?,
            ExitCode::FAILURE
        );
        // 2 x 20 GB fits, so the cut goes on to extract, which the stand-in fails.
        let small = fake_pmtiles(&directory, "20 GB")?;
        assert_eq!(
            run(&invocation(Action::Plan { estimate: true }, small.clone()))?,
            ExitCode::SUCCESS
        );
        assert!(
            run(&invocation(Action::Cut, small))
                .is_err_and(|error| error.to_string().contains("pmtiles failed"))
        );
        fs::remove_dir_all(&directory)?;
        Ok(())
    }
}
