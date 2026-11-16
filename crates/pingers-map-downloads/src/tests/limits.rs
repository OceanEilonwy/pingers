// SPDX-License-Identifier: AGPL-3.0-only
//! Hosts that answer wrongly, limits, retries, and maps directories that misbehave.

use super::*;

#[tokio::test]
async fn pieces_that_are_not_what_was_asked_for_are_refused() -> TestResult {
    for lie in [Lie::WrongStart, Lie::ShortBody, Lie::EndPastTotal] {
        let directory = TempDir::new("lies");
        let host = scripted(Padding::Off);
        host.lie(lie);
        assert!(
            matches!(
                run(&host, &ready_offer()?, directory.path()).await,
                Err(MapsError::BadAnswer)
            ),
            "{lie:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_catalog_that_changes_between_pieces_comes_back_whole_in_its_new_version() -> TestResult {
    let host = scripted(Padding::Off);
    let old = format!(r#"{{"format":1,"regions":[],"note":"{}"}}"#, "o".repeat(40));
    let new = format!(
        r#"{{"format":1,"regions":[{}]}}"#,
        manifest_json(ID, PACK, INDEX)
    );
    host.put("catalog.json", old.as_bytes());
    host.change_after(2, "catalog.json", new.as_bytes());
    let url = MapHostUrl::new("https://maps.example".to_owned())?;
    let result = fetch_catalog(&host, &HostChain::new(None, Some(&url), None)).await;
    assert_eq!(result.offers.len(), 1, "the new version, not a mix");
    // Two pieces of the old version, then the new one whole.
    assert_eq!(host.requests().len(), 3);
    Ok(())
}

#[tokio::test]
async fn small_files_are_limited_to_their_size() -> TestResult {
    // A catalog of exactly 16 MiB comes through; one byte more does not.
    let host = ScriptedHost::new(8 * 1024 * 1024, Padding::Off);
    let place = map_host()?;
    host.put("catalog.json", &vec![b' '; 16_777_216]);
    let mut fetching = crate::pieces::Fetching::new(&host, &place);
    let body =
        crate::pieces::fetch_small(&mut fetching, "/catalog.json", crate::catalog::MAX_CATALOG)
            .await?;
    assert_eq!(body.len(), 16_777_216);
    host.put("catalog.json", &vec![b' '; 16_777_217]);
    let mut fetching = crate::pieces::Fetching::new(&host, &place);
    assert!(matches!(
        crate::pieces::fetch_small(&mut fetching, "/catalog.json", crate::catalog::MAX_CATALOG)
            .await,
        Err(MapsError::BadAnswer)
    ));

    // A manifest may be 64 KiB, not more.
    let offer = RegionOffer {
        entry: CatalogEntry::OnDemand {
            id: ID.to_owned(),
            stem: StemName::of_region(ID)?,
            name: "Test".to_owned(),
            group: None,
        },
        host: map_host()?,
    };
    let json = manifest_json(ID, PACK, INDEX);
    let padded = |length: usize| {
        let mut text = json.clone();
        text.push_str(&" ".repeat(length.saturating_sub(json.len())));
        text
    };
    let host = scripted(Padding::Off);
    host.put("oceania.test.json", padded(65_536).as_bytes());
    let directory = TempDir::new("manifest-limit");
    run(&host, &offer, directory.path()).await?;
    host.put("oceania.test.json", padded(65_537).as_bytes());
    let other = TempDir::new("manifest-over");
    assert!(matches!(
        run(&host, &offer, other.path()).await,
        Err(MapsError::BadAnswer)
    ));
    Ok(())
}

#[tokio::test]
async fn a_host_that_never_finishes_preparing_is_asked_forty_times() -> TestResult {
    let host = scripted(Padding::Off);
    host.preparing.store(usize::MAX, Ordering::SeqCst);
    let directory = TempDir::new("forty");
    let reports = Reports::default();
    let timer = InstantTimer::default();
    let offer = ready_offer()?;
    let outcome = bounded(download(&Download {
        fetch: &host,
        timer: &timer,
        offer: &offer,
        directory: directory.path(),
        progress: &reports,
    }))
    .await?;
    assert!(matches!(outcome, Err(MapsError::Preparing { .. })));
    assert_eq!(
        reports
            .preparing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len(),
        40
    );
    assert_eq!(host.requests().len(), 41);
    Ok(())
}

#[tokio::test]
async fn a_complete_partial_is_installed_without_asking_again() -> TestResult {
    let directory = TempDir::new("complete-partial");
    let host = scripted(Padding::Off);
    let offer = ready_offer()?;
    let CatalogEntry::Ready(manifest) = &offer.entry else {
        return Err("ready".into());
    };
    std::fs::create_dir_all(directory.path())?;
    std::fs::write(
        crate::pieces::partial_path(directory.path(), &manifest.pack),
        PACK,
    )?;
    run(&host, &offer, directory.path()).await?;
    assert!(
        host.requests()
            .iter()
            .all(|request| !request.starts_with("/oceania.test.pmtiles")),
        "{:?}",
        host.requests()
    );
    assert_eq!(installed(directory.path()).await?.len(), 1);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_partial_that_cannot_be_read_is_an_error_not_a_fresh_start() -> TestResult {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = TempDir::new("unreadable-partial");
    let host = scripted(Padding::Off);
    let offer = ready_offer()?;
    let CatalogEntry::Ready(manifest) = &offer.entry else {
        return Err("ready".into());
    };
    std::fs::create_dir_all(directory.path())?;
    let partial = crate::pieces::partial_path(directory.path(), &manifest.pack);
    std::fs::write(&partial, b"junk")?;
    std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o200))?;
    assert!(matches!(
        run(&host, &offer, directory.path()).await,
        Err(MapsError::Io(_))
    ));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_maps_directory_that_cannot_be_listed_is_an_error() -> TestResult {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = TempDir::new("unlistable");
    std::fs::create_dir_all(directory.path())?;
    // Write and search, not read: files can go, but the directory cannot be listed.
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o300))?;
    let listed = installed(directory.path()).await;
    let deleted = delete(directory.path(), ID).await;
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
    assert!(matches!(listed, Err(MapsError::Io(_))));
    assert!(matches!(deleted, Err(MapsError::Io(_))));
    // A directory that is not there yet holds nothing to delete.
    let missing = TempDir::new("missing");
    delete(missing.path(), ID).await?;
    Ok(())
}

#[tokio::test]
async fn deleting_keeps_other_regions_partials_and_reports_what_cannot_go() -> TestResult {
    let directory = TempDir::new("delete-others");
    std::fs::create_dir_all(directory.path())?;
    let theirs = directory
        .path()
        .join("oceania.other.pmtiles.0123456789abcdef.partial");
    std::fs::write(&theirs, b"theirs")?;
    delete(directory.path(), ID).await?;
    assert!(theirs.exists());
    // A pack that is a directory cannot be removed as a file: an error, not success.
    std::fs::create_dir_all(directory.path().join("oceania.test.pmtiles"))?;
    assert!(matches!(
        delete(directory.path(), ID).await,
        Err(MapsError::Io(_))
    ));
    Ok(())
}
