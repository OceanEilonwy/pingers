// SPDX-License-Identifier: AGPL-3.0-only
//! Catalog ranges and the OHTTP gateway that downloads through a privacy relay use.

use pingers_ohttp::{BinaryRequest, BinaryResponse, Header, KeyConfig, encapsulate_request_padded};

use super::*;

#[tokio::test]
async fn catalog_answers_ranges_pinned_by_if_range() -> TestResult {
    let directory = TempDir::new("catalog-range")?;
    write_pre_extracted(directory.path())?;
    let host = start(
        config(directory.path(), &["oceania"], 1 << 30),
        None,
        clock(),
    )
    .await?;
    let client = reqwest::Client::new();
    let url = host.url("/catalog.json");
    let whole = client.get(&url).send().await?;
    let etag = whole
        .headers()
        .get(ETAG)
        .cloned()
        .ok_or("catalog without an etag")?;
    let whole = whole.bytes().await?;

    let first = client.get(&url).header(RANGE, "bytes=0-9").send().await?;
    assert_eq!(first.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        first
            .headers()
            .get(CONTENT_RANGE)
            .map(HeaderValue::as_bytes),
        Some(format!("bytes 0-9/{}", whole.len()).as_bytes())
    );
    assert_eq!(
        first.bytes().await?.as_ref(),
        whole.get(..10).unwrap_or_default()
    );

    let rest = client
        .get(&url)
        .header(RANGE, "bytes=10-")
        .header(IF_RANGE, etag.clone())
        .send()
        .await?;
    assert_eq!(rest.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        rest.bytes().await?.as_ref(),
        whole.get(10..).unwrap_or_default()
    );

    // A different catalog version gets the whole new catalog, so pieces never mix.
    let changed = client
        .get(&url)
        .header(RANGE, "bytes=10-")
        .header(IF_RANGE, "\"older\"")
        .send()
        .await?;
    assert_eq!(changed.status(), StatusCode::OK);
    assert_eq!(changed.bytes().await?, whole);

    let beyond = client
        .get(&url)
        .header(RANGE, "bytes=999999-")
        .send()
        .await?;
    assert_eq!(beyond.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    Ok(())
}

/// What came back through the gateway: the sealed answer's length and the opened answer.
struct Relayed {
    sealed_length: usize,
    answer: BinaryResponse,
}

/// One request through the host's OHTTP gateway, as the Pingers server forwards it.
async fn through_gateway(
    client: &reqwest::Client,
    host: &Running,
    request: &BinaryRequest,
    seed: u64,
) -> Result<Relayed, Box<dyn std::error::Error>> {
    let config = KeyConfig::decode(
        &client
            .get(host.url("/ohttp-keys"))
            .send()
            .await?
            .bytes()
            .await?,
    )?;
    let mut rng = pingers_testkit::seeded_rng(seed);
    let sealed = encapsulate_request_padded(&config, request, 1024, &mut rng)?;
    let answer = client
        .post(host.url("/ohttp"))
        .header(CONTENT_TYPE, "message/ohttp-req")
        .body(sealed.message)
        .send()
        .await?;
    assert_eq!(answer.status(), StatusCode::OK);
    assert_eq!(
        answer
            .headers()
            .get(CONTENT_TYPE)
            .map(HeaderValue::as_bytes),
        Some(&b"message/ohttp-res"[..])
    );
    let body = answer.bytes().await?;
    Ok(Relayed {
        sealed_length: body.len(),
        answer: sealed.response_context.open(&body)?,
    })
}

fn get(path: &str, headers: &[(&str, &str)]) -> BinaryRequest {
    BinaryRequest {
        method: "GET".to_owned(),
        path: path.to_owned(),
        headers: headers
            .iter()
            .map(|(name, value)| Header {
                name: (*name).to_owned(),
                value: value.as_bytes().to_vec(),
            })
            .collect(),
        body: Vec::new(),
    }
}

fn header<'response>(response: &'response BinaryResponse, name: &str) -> Option<&'response [u8]> {
    response
        .headers
        .iter()
        .find(|header| header.name == name)
        .map(|header| header.value.as_slice())
}

#[tokio::test]
async fn the_gateway_answers_as_the_host_does_padded_to_one_size() -> TestResult {
    let directory = TempDir::new("gateway")?;
    write_pre_extracted(directory.path())?;
    let host = start(
        config(directory.path(), &["oceania"], 1 << 30),
        None,
        clock(),
    )
    .await?;
    let client = reqwest::Client::new();
    let pack = pack_bytes();

    let part = through_gateway(
        &client,
        &host,
        &get("/oceania.pmtiles", &[("range", "bytes=10-19")]),
        1,
    )
    .await?;
    // The key file was made on first start, and the host never serves it.
    assert!(directory.path().join(".ohttp-key").exists());
    assert_eq!(
        client.get(host.url("/.ohttp-key")).send().await?.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(part.answer.status.get(), 206);
    assert_eq!(
        part.answer.body.as_slice(),
        pack.get(10..20).unwrap_or_default()
    );
    assert_eq!(
        header(&part.answer, "content-range"),
        Some(format!("bytes 10-19/{}", pack.len()).as_bytes())
    );
    // Only the headers a download needs come back.
    assert!(header(&part.answer, "cache-control").is_none());

    // Pieces asked to be padded to 64 KiB are that size whatever they hold; so is a 404.
    let pad = ("pingers-pad", "65536");
    let one_byte = through_gateway(
        &client,
        &host,
        &get("/oceania.pmtiles", &[("range", "bytes=0-0"), pad]),
        2,
    )
    .await?;
    let many_bytes = through_gateway(
        &client,
        &host,
        &get("/oceania.pmtiles", &[("range", "bytes=0-499"), pad]),
        3,
    )
    .await?;
    let missing = through_gateway(&client, &host, &get("/europe.pmtiles", &[pad]), 4).await?;
    assert_eq!(missing.answer.status.get(), 404);
    assert_eq!(one_byte.sealed_length, many_bytes.sealed_length);
    assert_eq!(one_byte.sealed_length, missing.sealed_length);
    assert!(one_byte.sealed_length > 65_536);
    assert!(part.sealed_length < one_byte.sealed_length);

    let catalog = through_gateway(&client, &host, &get("/catalog.json", &[]), 5).await?;
    assert_eq!(catalog.answer.status.get(), 200);
    let direct = client.get(host.url("/catalog.json")).send().await?;
    assert_eq!(catalog.answer.body, direct.bytes().await?.to_vec());

    let mut post = get("/catalog.json", &[]);
    post.method = "POST".to_owned();
    let refused = through_gateway(&client, &host, &post, 6).await?;
    let mut head = get("/oceania.pmtiles", &[]);
    head.method = "HEAD".to_owned();
    let headed = through_gateway(&client, &host, &head, 7).await?;
    assert_eq!(headed.answer.status.get(), 200);
    assert!(headed.answer.body.is_empty());
    assert_eq!(refused.answer.status.get(), 405);
    Ok(())
}

#[tokio::test]
async fn the_gateway_refuses_what_it_cannot_open() -> TestResult {
    let directory = TempDir::new("gateway-refused")?;
    write_pre_extracted(directory.path())?;
    let host = start(
        config(directory.path(), &["oceania"], 1 << 30),
        None,
        clock(),
    )
    .await?;
    let client = reqwest::Client::new();
    let ohttp = |body: Vec<u8>| {
        client
            .post(host.url("/ohttp"))
            .header(CONTENT_TYPE, "message/ohttp-req")
            .body(body)
    };
    assert_eq!(
        ohttp(vec![1_u8; 100]).send().await?.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        ohttp(vec![1_u8; 64 * 1024]).send().await?.status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let untyped = client
        .post(host.url("/ohttp"))
        .body(vec![1_u8; 100])
        .send()
        .await?;
    assert_eq!(untyped.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(
        client.get(host.url("/ohttp")).send().await?.status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    Ok(())
}

#[tokio::test]
async fn the_binarys_serve_reports_a_directory_it_cannot_use() -> TestResult {
    let directory = TempDir::new("serve-missing")?;
    let missing = directory.path().join("not-there");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let served = pingers_map_host::serve(
        config(&missing, &["oceania"], 1 << 30),
        listener,
        clock(),
        std::future::pending(),
    )
    .await;
    assert!(matches!(
        served,
        Err(pingers_map_host::error::MapHostError::Directory { .. })
    ));
    Ok(())
}
