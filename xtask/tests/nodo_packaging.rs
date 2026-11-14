// SPDX-License-Identifier: AGPL-3.0-only
//! Checks `packaging/nodo` the way a monokulo-nodo device would: each manifest through
//! `nodo_manifest::Manifest::parse` (the feed's install review), each `settings.lua` through
//! `nodo_settings::load_settings`, and each config template through `Installed::render_toml` with
//! all four apps installed together, as the helper renders them. The tier-1 dev configs must have
//! the same shape as what the templates render.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use nodo_manifest::{Manifest, UpstreamProtocol};
use nodo_settings::{
    Grant, Installed, InstalledPackage, Limits, LoadedSettings, SecretString, SettingValue, Value,
    load_settings,
};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// The four apps: id, config file name, and the dev config that mirrors its template.
const APPS: [(&str, &str); 4] = [
    ("pingers-server", "server"),
    ("pingers-push-relay", "push-relay"),
    ("pingers-ohttp-relay", "relay"),
    ("pingers-map-host", "map-host"),
];

/// A version of the form `cargo xtask nodo-dist` writes.
const VERSION: &str = "0.1.0pre20261113.1930";

fn packaging() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("packaging")
        .join("nodo")
}

fn app_dir(id: &str) -> PathBuf {
    packaging()
        .join("srcpkgs")
        .join("pingers-server")
        .join("files")
        .join(id)
}

fn read(path: &Path) -> TestResult<String> {
    fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()).into())
}

/// The manifest as the recipe installs it.
fn manifest_text(id: &str) -> TestResult<String> {
    Ok(read(&app_dir(id).join("manifest.toml"))?.replace("@VERSION@", VERSION))
}

/// Parses a manifest with nodo's feed checks, `upstream_protocol` (N1) included.
fn parse_manifest(text: &str) -> TestResult<Manifest> {
    Ok(Manifest::parse(text)?)
}

fn settings(id: &str) -> TestResult<LoadedSettings> {
    Ok(load_settings(
        &read(&app_dir(id).join("settings.lua"))?,
        Limits::default(),
    )?)
}

#[test]
fn every_manifest_passes_the_feeds_install_review() -> TestResult {
    for (id, name) in APPS {
        let manifest = parse_manifest(&manifest_text(id)?)?;
        assert_eq!(manifest.id, id);
        assert_eq!(manifest.version, VERSION);
        // The launcher, which keeps the binary's name for control/t's pkill -x.
        assert_eq!(manifest.exec, vec![format!("bin/{id}")]);
        let launcher = read(&app_dir(id).join("launcher"))?;
        assert!(
            launcher.contains(&format!("config=/etc/apps/{id}/{name}.toml\n")),
            "{id}'s launcher names another config file"
        );
        assert!(launcher.contains(&format!("/../libexec/{id}\" \"$config\"")));
        let config = manifest.config.as_ref().ok_or("no [config]")?;
        assert_eq!(config.name, format!("{name}.toml"));
        assert!(app_dir(id).join(&config.template).is_file());
        assert_eq!(manifest.settings.as_deref(), Some("settings.lua"));
        // Every service listens on loopback only, behind Caddy, and its health check is its
        // own web upstream.
        let web = manifest.web.first().ok_or("no [[web]]")?;
        assert_eq!(manifest.web.len(), 1);
        let Some(nodo_manifest::HealthCheck::Tcp { port }) = manifest.health else {
            return Err(format!("{id} has no tcp health check").into());
        };
        assert_eq!(web.upstream, format!("127.0.0.1:{port}"));
    }
    Ok(())
}

#[test]
fn the_http2_only_services_ask_caddy_for_h2c() -> TestResult {
    for (id, _) in APPS {
        let expected = match id {
            "pingers-server" | "pingers-ohttp-relay" => UpstreamProtocol::H2c,
            _ => UpstreamProtocol::H1,
        };
        let protocols: Vec<UpstreamProtocol> = parse_manifest(&manifest_text(id)?)?
            .web
            .iter()
            .map(|web| web.upstream_protocol)
            .collect();
        assert_eq!(protocols, vec![expected], "{id}");
    }
    Ok(())
}

#[test]
fn only_the_push_relay_reaches_the_internet() -> TestResult {
    for (id, _) in APPS {
        let manifest = parse_manifest(&manifest_text(id)?)?;
        let internet = manifest.network.outbound == nodo_manifest::Outbound::Internet;
        assert_eq!(internet, id == "pingers-push-relay", "{id}");
    }
    Ok(())
}

#[test]
fn keys_and_databases_are_never_on_the_disk_in_clear() -> TestResult {
    let server = parse_manifest(&manifest_text("pingers-server")?)?;
    assert!(server.storage.encrypted);
    // The push relay has no volume; its sealed secrets keep the rendered config in memory.
    let relay = settings("pingers-push-relay")?;
    assert!(
        relay
            .schema()
            .settings
            .iter()
            .any(|setting| setting.name == "hash_key"
                && matches!(setting.ty, nodo_settings::SettingType::Secret))
    );
    Ok(())
}

#[test]
fn the_registration_code_generates_itself_and_passes_its_own_check() -> TestResult {
    let server = settings("pingers-server")?;
    let setting = server
        .schema()
        .settings
        .iter()
        .find(|setting| setting.name == "registration_code")
        .ok_or("no registration_code setting")?;
    assert!(matches!(setting.ty, nodo_settings::SettingType::Secret));
    let generate = setting
        .generate
        .ok_or("the registration code is not generated on install")?;
    let code = generate.encode(&vec![0xa5; generate.byte_len()])?;
    assert!(
        server
            .validate_value("registration_code", &Value::Secret(code))
            .is_ok()
    );
    Ok(())
}

fn text(value: &str) -> SettingValue {
    SettingValue::Literal(Value::String(value.to_owned()))
}

fn url(value: &str) -> SettingValue {
    SettingValue::Literal(Value::Url(value.to_owned()))
}

fn secret(value: &str) -> SettingValue {
    SettingValue::Literal(Value::Secret(SecretString::new(value)))
}

fn hex(byte: char) -> String {
    std::iter::repeat_n(byte, 64).collect()
}

/// What an owner sets, plus what the device generates on install, for each app.
/// A service account of the shape Google issues, with a throwaway RSA key: the relay checks the
/// key when it reads its configuration. On one line, as an owner pastes it.
fn service_account_json() -> TestResult<String> {
    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_RSA_SHA256)?;
    let pem = key.serialize_pem().replace('\n', "\\n");
    Ok(format!(
        r#"{{"type": "service_account", "project_id": "example-project", "private_key_id": "test", "private_key": "{pem}", "client_email": "relay@example-project.iam.gserviceaccount.com", "token_uri": "https://oauth2.googleapis.com/token"}}"#
    ))
}

fn example_values() -> TestResult<BTreeMap<&'static str, BTreeMap<String, SettingValue>>> {
    let server = BTreeMap::from([
        ("server_id".to_owned(), url("https://pingers.example.org")),
        (
            "ohttp_relay_url".to_owned(),
            url("https://ohttp.example.org/ohttp"),
        ),
        ("map_host".to_owned(), url("https://maps.example.org")),
        (
            "push_relay_public_url".to_owned(),
            url("https://push.example.org"),
        ),
        (
            "push_relay_key_config_hash".to_owned(),
            text("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
        ),
        ("ohttp_key_a".to_owned(), secret(&hex('1'))),
        ("ohttp_key_b".to_owned(), secret(&hex('2'))),
        (
            "registration_code".to_owned(),
            secret("0123456789abcdef0123456789abcdef"),
        ),
    ]);
    let push = BTreeMap::from([
        ("operator".to_owned(), text("Example operator")),
        ("fcm_project_id".to_owned(), text("example-project")),
        (
            "fcm_service_account".to_owned(),
            secret(&service_account_json()?),
        ),
        ("hash_key".to_owned(), secret(&hex('4'))),
        ("key_a".to_owned(), secret(&hex('5'))),
        ("key_b".to_owned(), secret(&hex('6'))),
        ("server_credential".to_owned(), secret(&hex('3'))),
    ]);
    Ok(BTreeMap::from([
        ("pingers-server", server),
        ("pingers-push-relay", push),
        ("pingers-ohttp-relay", BTreeMap::new()),
        ("pingers-map-host", BTreeMap::new()),
    ]))
}

/// All four apps installed together, with the one grant the owner approves.
fn installed() -> TestResult<Installed> {
    let mut values = example_values()?;
    let mut installed = Installed::default();
    for (id, _) in APPS {
        installed.packages.insert(
            id.to_owned(),
            InstalledPackage {
                manifest: parse_manifest(&manifest_text(id)?)?,
                schema: settings(id)?.schema().clone(),
                values: values.remove(id).unwrap_or_default(),
            },
        );
    }
    installed.grants.insert(Grant {
        consumer: "pingers-server".to_owned(),
        provider: "pingers-push-relay".to_owned(),
        setting: "server_credential".to_owned(),
    });
    Ok(installed)
}

fn render(installed: &Installed, id: &str, name: &str) -> TestResult<nodo_settings::Rendered> {
    let template = read(&app_dir(id).join(format!("{name}.toml.in")))?;
    installed.render_toml(id, &template).map_err(|errors| {
        let shown: Vec<String> = errors.iter().map(ToString::to_string).collect();
        format!("{id}: {}", shown.join("; ")).into()
    })
}

/// Every leaf of a TOML document as `path: type`, arrays of tables as `name[]`.
fn shape(prefix: &str, value: &toml::Value, out: &mut BTreeSet<String>) {
    match value {
        toml::Value::Table(table) => {
            for (key, child) in table {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                shape(&path, child, out);
            }
        }
        toml::Value::Array(items) if items.iter().all(toml::Value::is_table) => {
            for item in items {
                shape(&format!("{prefix}[]"), item, out);
            }
        }
        other @ (toml::Value::String(_)
        | toml::Value::Integer(_)
        | toml::Value::Float(_)
        | toml::Value::Boolean(_)
        | toml::Value::Datetime(_)
        | toml::Value::Array(_)) => {
            out.insert(format!("{prefix}: {}", other.type_str()));
        }
    }
}

fn shape_of(text: &str) -> TestResult<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    shape("", &toml::Value::Table(toml::from_str(text)?), &mut out);
    Ok(out)
}

#[test]
fn templates_render_on_a_device_and_match_the_dev_configs() -> TestResult {
    let installed = installed()?;
    for (id, name) in APPS {
        let rendered = shape_of(&render(&installed, id, name)?.text)?;
        let mut dev = shape_of(&read(
            &packaging().join("dev").join(format!("{name}.toml")),
        )?)?;
        // The dev stack reports counts every minute and has no FCM service account.
        dev.remove("counts_interval_s: integer");
        let rendered: BTreeSet<String> = rendered
            .into_iter()
            .filter(|leaf| !(id == "pingers-push-relay" && leaf.starts_with("fcm.")))
            .collect();
        assert_eq!(rendered, dev, "{id}: template and dev config differ");
    }
    Ok(())
}

#[test]
fn references_open_exactly_the_loopback_ports_of_the_design() -> TestResult {
    let installed = installed()?;
    let ports = |id: &str, name: &str| -> TestResult<Vec<(String, String)>> {
        Ok(render(&installed, id, name)?
            .network
            .into_iter()
            .map(|grant| (grant.provider, format!("{:?}", grant.access)))
            .collect())
    };
    // The relay reaches only the gateway listener, never the public API.
    assert_eq!(
        ports("pingers-ohttp-relay", "relay")?,
        vec![(
            "pingers-server".to_owned(),
            "Port { protocol: Tcp, port: 9471 }".to_owned()
        )]
    );
    // The server reaches only the push relay and the map host's gateway (map downloads
    // through the privacy relay), never the internet.
    let mut server = ports("pingers-server", "server")?;
    server.sort();
    assert_eq!(
        server,
        vec![
            (
                "pingers-map-host".to_owned(),
                "Port { protocol: Tcp, port: 9474 }".to_owned()
            ),
            (
                "pingers-push-relay".to_owned(),
                "Port { protocol: Tcp, port: 9472 }".to_owned()
            ),
        ]
    );
    assert!(ports("pingers-map-host", "map-host")?.is_empty());
    assert!(ports("pingers-push-relay", "push-relay")?.is_empty());
    Ok(())
}

#[test]
fn the_server_cannot_read_the_relays_credential_without_the_owners_grant() -> TestResult {
    let mut installed = installed()?;
    installed.grants.clear();
    assert!(render(&installed, "pingers-server", "server").is_err());
    Ok(())
}

#[test]
fn both_operators_say_the_same_unless_the_owner_changes_one() -> TestResult {
    let installed = installed()?;
    let server: toml::Table =
        toml::from_str(&render(&installed, "pingers-server", "server")?.text)?;
    let relay: toml::Table =
        toml::from_str(&render(&installed, "pingers-push-relay", "push-relay")?.text)?;
    assert_eq!(server.get("server_operator"), relay.get("operator"));
    Ok(())
}

#[test]
fn validators_accept_the_examples_and_refuse_near_misses() -> TestResult {
    let examples = example_values()?;
    for (id, _) in APPS {
        let loaded = settings(id)?;
        let values: BTreeMap<String, Value> = examples
            .get(id)
            .into_iter()
            .flatten()
            .filter_map(|(name, value)| match value {
                SettingValue::Literal(value) => Some((name.clone(), value.clone())),
                SettingValue::Template(_) => None,
            })
            .collect();
        assert!(loaded.validate_values(&values).is_empty(), "{id}");
    }
    let server = settings("pingers-server")?;
    for bad in [
        "https://pingers.example.org/",
        "http://pingers.example.org",
        "https://pingers.example.org/api",
    ] {
        assert!(
            server
                .validate_value("server_id", &Value::Url(bad.to_owned()))
                .is_err(),
            "{bad}"
        );
    }
    assert!(
        server
            .validate_value("ohttp_key_a", &Value::Secret(SecretString::new(hex('A'))))
            .is_err(),
        "uppercase hex"
    );
    for bad in [
        "0123456789abcde",
        "0123456789abcdef 0123",
        "code/with/slashes0123",
    ] {
        assert!(
            server
                .validate_value("registration_code", &Value::Secret(SecretString::new(bad)))
                .is_err(),
            "{bad}"
        );
    }
    let maps = settings("pingers-map-host")?;
    for (setting, bad) in [
        ("hosted", "oceania/australia,europe"),
        ("hosted", "oceania//australia"),
        ("max_disk", "40 GB"),
    ] {
        assert!(
            maps.validate_value(setting, &Value::String(bad.to_owned()))
                .is_err(),
            "{setting} {bad}"
        );
    }
    Ok(())
}

#[test]
fn the_feed_publishes_the_recipe_and_every_subpackage() -> TestResult {
    let listed: Vec<String> = read(&packaging().join("feed-packages"))?
        .lines()
        .map(|line| line.split('#').next().unwrap_or_default().trim().to_owned())
        .filter(|line| !line.is_empty())
        .collect();
    let apps: Vec<String> = APPS.iter().map(|(id, _)| (*id).to_owned()).collect();
    assert_eq!(listed, apps);
    let template = read(&packaging().join("srcpkgs/pingers-server/template"))?;
    for (id, _) in APPS.iter().skip(1) {
        assert!(template.contains(&format!("{id}_package() {{")), "{id}");
        assert!(
            packaging().join("srcpkgs").join(id).is_symlink(),
            "{id} needs its srcpkgs symlink"
        );
    }
    Ok(())
}

/// The server, the push relay and the map host read what nodo renders, and the tier-1 dev
/// configs, with their own parsers.
#[test]
fn the_services_accept_the_rendered_and_dev_configs() -> TestResult {
    let installed = installed()?;
    let dev = |name: &str| read(&packaging().join("dev").join(format!("{name}.toml")));
    let server = |text: &str, what: &str| -> TestResult {
        let config = pingers_server::ServerConfig::parse(text)
            .map_err(|error| format!("{what} server.toml: {error}"))?;
        // The gateway is on its own loopback listener, and wakes go to the push relay on
        // loopback while devices are told its public address.
        assert_eq!(
            config.gateway_listen,
            "127.0.0.1:9471".parse::<std::net::SocketAddr>()?,
            "{what}"
        );
        assert_eq!(config.ohttp_keys.configs().len(), 2, "{what}");
        // Signups need the operator's code: the packaged server is never open to anyone.
        assert_eq!(config.registration_codes.len(), 1, "{what}");
        assert!(
            config
                .advertised_push_relays
                .iter()
                .all(|relay| relay.url.as_str().starts_with("https://")),
            "{what}"
        );
        Ok(())
    };
    server(
        &render(&installed, "pingers-server", "server")?.text,
        "rendered",
    )?;
    server(&dev("server")?, "dev")?;
    let push = |text: &str, what: &str| -> TestResult {
        pingers_push_relay::config::RelayConfig::parse(text)
            .map(drop)
            .map_err(|error| format!("{what} push-relay.toml: {error}").into())
    };
    push(
        &render(&installed, "pingers-push-relay", "push-relay")?.text,
        "rendered",
    )?;
    push(&dev("push-relay")?, "dev")?;
    let maps = |text: &str, what: &str| -> TestResult {
        pingers_map_host::config::MapHostConfig::parse(text, Path::new("/"))
            .map(drop)
            .map_err(|error| format!("{what} map-host.toml: {error}").into())
    };
    maps(
        &render(&installed, "pingers-map-host", "map-host")?.text,
        "rendered",
    )?;
    maps(&dev("map-host")?, "dev")?;
    Ok(())
}
