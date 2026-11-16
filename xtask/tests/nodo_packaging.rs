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

/// The five apps: id, config file name, and the dev config that mirrors its template. pingers,
/// the bundle the owner installs, is last, as in the feed.
const APPS: [(&str, &str); 5] = [
    ("pingers-server", "server"),
    ("pingers-push-relay", "push-relay"),
    ("pingers-ohttp-relay", "relay"),
    ("pingers-map-host", "map-host"),
    ("pingers", "setup"),
];

/// The four services, which are web sites; pingers is not.
const SERVICES: [&str; 4] = [
    "pingers-server",
    "pingers-push-relay",
    "pingers-ohttp-relay",
    "pingers-map-host",
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
        let Some(nodo_manifest::HealthCheck::Tcp { port }) = manifest.health else {
            return Err(format!("{id} has no tcp health check").into());
        };
        if id == "pingers" {
            // The setup page is no web site: only nodo's helper reads it, on loopback.
            assert!(manifest.web.is_empty());
            assert_eq!(port, 9479);
            continue;
        }
        // Every service listens on loopback only, behind Caddy, and its health check is its
        // own web upstream.
        let web = manifest.web.first().ok_or("no [[web]]")?;
        assert_eq!(manifest.web.len(), 1);
        assert_eq!(web.upstream, format!("127.0.0.1:{port}"));
    }
    Ok(())
}

#[test]
fn the_http2_only_services_ask_caddy_for_h2c() -> TestResult {
    for id in SERVICES {
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
fn only_the_push_relay_and_the_setup_page_reach_the_internet() -> TestResult {
    for (id, _) in APPS {
        let manifest = parse_manifest(&manifest_text(id)?)?;
        let internet = manifest.network.outbound == nodo_manifest::Outbound::Internet;
        // The setup page checks the public names from the appliance.
        let expected = id == "pingers-push-relay" || id == "pingers";
        assert_eq!(internet, expected, "{id}");
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
    // The addresses and the operator all follow from the domain; the keys and the code are
    // what the device generates.
    let server = BTreeMap::from([
        ("ohttp_key_a".to_owned(), secret(&hex('1'))),
        ("ohttp_key_b".to_owned(), secret(&hex('2'))),
        (
            "registration_code".to_owned(),
            secret("0123456789abcdef0123456789abcdef"),
        ),
    ]);
    // The two answers the bundle asks for, and the generated keys.
    let push = BTreeMap::from([
        ("domain".to_owned(), text("example.org")),
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
        ("pingers", BTreeMap::new()),
    ]))
}

/// The bundle's grants, as the pingers manifest declares them.
fn bundle_grants() -> TestResult<Vec<Grant>> {
    let manifest = parse_manifest(&manifest_text("pingers")?)?;
    let bundle = manifest.bundle.ok_or("pingers has no [bundle]")?;
    Ok(bundle
        .grants
        .into_iter()
        .map(|grant| Grant {
            consumer: grant.consumer,
            provider: grant.provider,
            setting: grant.setting,
        })
        .collect())
}

/// All five apps installed together, with the grants the owner approves with the bundle.
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
    installed.grants.extend(bundle_grants()?);
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
    // The setup page reads the gateway's key configs, the push relay's published keys and the
    // map catalog, on loopback; never the public API.
    let mut setup = ports("pingers", "setup")?;
    setup.sort();
    assert_eq!(
        setup,
        vec![
            (
                "pingers-map-host".to_owned(),
                "Port { protocol: Tcp, port: 9474 }".to_owned()
            ),
            (
                "pingers-push-relay".to_owned(),
                "Port { protocol: Tcp, port: 9472 }".to_owned()
            ),
            (
                "pingers-server".to_owned(),
                "Port { protocol: Tcp, port: 9471 }".to_owned()
            ),
        ]
    );
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
        // The push relay's key is learned from the relay itself: nothing pasted.
        assert!(config.advertised_push_relays.is_empty(), "{what}");
        assert_eq!(config.learned_push_relays.len(), 1, "{what}");
        assert!(
            config
                .learned_push_relays
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
    let setup = |text: &str, what: &str| -> TestResult {
        pingers_setup::Config::parse(text)
            .map(drop)
            .map_err(|error| format!("{what} setup.toml: {error}").into())
    };
    setup(&render(&installed, "pingers", "setup")?.text, "rendered")?;
    setup(&dev("setup")?, "dev")?;
    Ok(())
}

#[test]
fn every_address_follows_from_the_domain() -> TestResult {
    let installed = installed()?;
    let server: toml::Table =
        toml::from_str(&render(&installed, "pingers-server", "server")?.text)?;
    let field = |name: &str| {
        server
            .get(name)
            .and_then(toml::Value::as_str)
            .map(str::to_owned)
    };
    assert_eq!(
        field("server_id").as_deref(),
        Some("https://pingers.example.org")
    );
    assert_eq!(
        field("map_host").as_deref(),
        Some("https://maps.example.org")
    );
    assert_eq!(
        server.get("ohttp_relay_urls"),
        Some(&toml::Value::Array(vec![
            "https://ohttp.example.org/ohttp".into()
        ]))
    );
    let relay = server
        .get("push_relays")
        .and_then(toml::Value::as_array)
        .and_then(|relays| relays.first())
        .ok_or("no push relay")?;
    assert_eq!(
        relay.get("public_url").and_then(toml::Value::as_str),
        Some("https://push.example.org")
    );
    // Both operators default to the domain.
    assert_eq!(field("server_operator").as_deref(), Some("example.org"));
    // The setup page gets the same addresses and the server's code.
    let setup = pingers_setup::Config::parse(&render(&installed, "pingers", "setup")?.text)?;
    assert_eq!(setup.server_id.as_str(), "https://pingers.example.org");
    assert_eq!(setup.push_relay_url, "https://push.example.org");
    assert_eq!(
        setup.registration_code.expose(),
        "0123456789abcdef0123456789abcdef"
    );
    Ok(())
}

#[test]
fn the_relay_takes_its_firebase_project_from_the_pasted_key() -> TestResult {
    let installed = installed()?;
    let rendered = render(&installed, "pingers-push-relay", "push-relay")?.text;
    let table: toml::Table = toml::from_str(&rendered)?;
    assert!(
        table
            .get("fcm")
            .and_then(|fcm| fcm.get("project_id"))
            .is_none()
    );
    let config = pingers_push_relay::config::RelayConfig::parse(&rendered)?;
    assert_eq!(
        config.fcm.map(|fcm| fcm.project_id).as_deref(),
        Some("example-project")
    );
    Ok(())
}

#[test]
fn the_bundle_installs_the_services_in_reference_order_and_asks_only_two_things() -> TestResult {
    let manifest = parse_manifest(&manifest_text("pingers")?)?;
    let bundle = manifest.bundle.ok_or("pingers has no [bundle]")?;
    // Each member before whatever references it: the push relay and the map host first, the
    // server (which references both), then the relay (which references the server).
    assert_eq!(
        bundle.members,
        [
            "pingers-push-relay",
            "pingers-map-host",
            "pingers-server",
            "pingers-ohttp-relay"
        ]
    );
    let installed = installed()?;
    for (index, member) in bundle.members.iter().enumerate() {
        let resolved = installed
            .resolve(member)
            .map_err(|errors| format!("{member}: {errors:?}"))?;
        for dependency in resolved.dependencies {
            let position = bundle
                .members
                .iter()
                .position(|other| *other == dependency.package)
                .ok_or("a dependency outside the bundle")?;
            assert!(
                position < index,
                "{member} needs {} first",
                dependency.package
            );
        }
    }
    assert_eq!(bundle.data_drive, ["pingers-map-host"]);
    // Every grant is needed: without any one of them, its consumer does not render.
    for grant in bundle_grants()? {
        let mut without = installed.clone();
        without.grants.remove(&grant);
        let name = APPS
            .iter()
            .find(|(id, _)| *id == grant.consumer)
            .map(|(_, name)| *name)
            .ok_or("a grant for an app outside the bundle")?;
        assert!(
            render(&without, &grant.consumer, name).is_err(),
            "{grant:?} is not needed"
        );
    }
    // The owner answers two questions; the device passphrase is nodo's own. Each answer is a
    // real setting of a member, and the secret one is a secret there.
    let asked: Vec<(String, String, bool)> = bundle
        .ask
        .iter()
        .map(|ask| (ask.package.clone(), ask.setting.clone(), ask.secret))
        .collect();
    assert_eq!(
        asked,
        [
            ("pingers-push-relay".to_owned(), "domain".to_owned(), false),
            (
                "pingers-push-relay".to_owned(),
                "fcm_service_account".to_owned(),
                true
            ),
        ]
    );
    for (package, setting, is_secret) in asked {
        let schema = settings(&package)?;
        let def = schema
            .schema()
            .settings
            .iter()
            .find(|def| def.name == setting)
            .ok_or("an ask for no setting")?;
        assert_eq!(
            matches!(def.ty, nodo_settings::SettingType::Secret),
            is_secret,
            "{setting}"
        );
    }
    Ok(())
}

#[test]
fn the_owner_page_is_the_setup_apps_own_loopback_port() -> TestResult {
    let manifest = parse_manifest(&manifest_text("pingers")?)?;
    let page = manifest.owner_page.ok_or("pingers has no [owner_page]")?;
    assert_eq!(
        page.url,
        format!("http://127.0.0.1:9479{}", pingers_setup::OWNER_PATH)
    );
    assert!(
        manifest
            .network
            .inbound
            .iter()
            .any(|inbound| inbound.port == 9479)
    );
    // Its config, holding the code, is rendered into memory: the app has a secret of its own.
    assert!(
        settings("pingers")?
            .schema()
            .settings
            .iter()
            .any(|setting| matches!(setting.ty, nodo_settings::SettingType::Secret))
    );
    Ok(())
}

#[test]
fn the_domain_is_checked() -> TestResult {
    let relay = settings("pingers-push-relay")?;
    for good in ["example.org", "a-b.example.co.uk"] {
        assert!(
            relay
                .validate_value("domain", &Value::String(good.to_owned()))
                .is_ok(),
            "{good}"
        );
    }
    for bad in [
        "Example.org",
        "example",
        "-a.org",
        "a..org",
        "https://example.org",
        "example.org/",
    ] {
        assert!(
            relay
                .validate_value("domain", &Value::String(bad.to_owned()))
                .is_err(),
            "{bad}"
        );
    }
    Ok(())
}

#[test]
fn the_setup_page_passes_nodos_own_owner_page_check() -> TestResult {
    use pingers_setup::checks::{Check, Facts, PushRelayFacts, State};
    let installed = installed()?;
    let config = pingers_setup::Config::parse(&render(&installed, "pingers", "setup")?.text)?;
    let pins = |byte: u8| {
        vec![
            pingers_proto::KeyConfigHash::from_bytes([byte; 32]),
            pingers_proto::KeyConfigHash::from_bytes([byte.wrapping_add(1); 32]),
        ]
    };
    // Every check failing with the longest detail, and the services up (so the QR code is
    // there); then the services down.
    let failing: Vec<Check> = (0..10)
        .map(|_| Check::new("A check", State::Failed, "d".repeat(4000)))
        .collect();
    let up = Facts {
        gateway_pins: Ok(pins(1)),
        push_relay: Ok(PushRelayFacts {
            pins: pins(3),
            fcm: true,
        }),
        map_regions: Ok(1),
        map_host_pins: Ok(pins(5)),
        public: failing.clone(),
    };
    let down = Facts {
        gateway_pins: Err("pingers-server's gateway is not answering".to_owned()),
        push_relay: Err("pingers-push-relay is not answering".to_owned()),
        map_regions: Err("pingers-map-host is not answering".to_owned()),
        map_host_pins: Err("pingers-map-host's gateway is not answering".to_owned()),
        public: failing,
    };
    for facts in [up, down] {
        let json = serde_json::to_string(&pingers_setup::page::build(&config, &facts))?;
        assert!(json.len() <= nodo_manifest::owner_page::MAX_BODY_BYTES);
        let page: nodo_manifest::owner_page::PageDoc = serde_json::from_str(&json)?;
        page.check()?;
    }
    Ok(())
}
