// SPDX-License-Identifier: AGPL-3.0-only
#![expect(
    clippy::panic_in_result_fn,
    clippy::wildcard_enum_match_arm,
    reason = "assertions are how tests fail; each lookup picks out one kind of item"
)]

use pingers_proto::{KeyConfigHash, SetupPayload};

use super::*;
use crate::checks::PushRelayFacts;
use crate::config::Config;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn config() -> Result<Config, crate::ConfigError> {
    Config::parse(&crate::config::tests::file())
}

fn pins(first: u8) -> Vec<KeyConfigHash> {
    vec![
        KeyConfigHash::from_bytes([first; 32]),
        KeyConfigHash::from_bytes([first.wrapping_add(1); 32]),
    ]
}

fn running() -> Facts {
    Facts {
        gateway_pins: Ok(pins(1)),
        push_relay: Ok(PushRelayFacts {
            pins: pins(3),
            fcm: true,
        }),
        map_regions: Ok(2),
        public: Vec::new(),
    }
}

fn down() -> Facts {
    Facts {
        gateway_pins: Err("pingers-server's gateway is not answering".to_owned()),
        push_relay: Err("pingers-push-relay is not answering".to_owned()),
        map_regions: Err("pingers-map-host is not answering".to_owned()),
        public: Vec::new(),
    }
}

/// Every value item of the page, by label.
fn values(page: &Page) -> Vec<(String, String)> {
    page.steps
        .iter()
        .flat_map(|step| &step.items)
        .filter_map(|item| match item {
            Item::Value { label, value } => Some((label.clone(), value.clone())),
            _ => None,
        })
        .collect()
}

fn value_of(page: &Page, label: &str) -> Option<String> {
    values(page)
        .into_iter()
        .find(|(name, _)| name == label)
        .map(|(_, value)| value)
}

#[test]
fn the_checklist_runs_from_the_domain_to_the_backup() -> TestResult {
    let page = build(&config()?, &running());
    let titles: Vec<&str> = page.steps.iter().map(|step| step.title.as_str()).collect();
    assert_eq!(
        titles,
        [
            "Connect your domain",
            "Put the four sites online",
            "Forward port 443 to this appliance",
            "Check the services answer",
            "Firebase wake-ups",
            "Upload the map packs",
            "Copy the hosts and pins into GitHub",
            "Make a release",
            "Set up your first phone",
            "Make a backup",
        ]
    );
    // What the appliance cannot see is never claimed done.
    for step in &page.steps {
        let blind = matches!(
            step.title.as_str(),
            "Copy the hosts and pins into GitHub" | "Make a release" | "Set up your first phone"
        );
        assert_eq!(step.state == StepState::Unknown, blind, "{}", step.title);
    }
    Ok(())
}

#[test]
fn the_github_variables_carry_this_appliances_values() -> TestResult {
    let page = build(&config()?, &running());
    assert_eq!(
        value_of(&page, "PINGERS_SERVER_HOST").as_deref(),
        Some("https://pingers.example.org")
    );
    assert_eq!(
        value_of(&page, "PINGERS_OHTTP_RELAY").as_deref(),
        Some("https://ohttp.example.org/ohttp")
    );
    assert_eq!(
        value_of(&page, "PINGERS_PUSH_RELAY").as_deref(),
        Some("https://push.example.org")
    );
    assert_eq!(
        value_of(&page, "PINGERS_MAP_HOST").as_deref(),
        Some("https://maps.example.org")
    );
    let gateway = pin_list(&pins(1));
    assert_eq!(gateway.split(',').count(), 2);
    assert!(
        gateway
            .split(',')
            .all(|pin| pin.len() == KeyConfigHash::PIN_LENGTH)
    );
    assert_eq!(value_of(&page, "PINGERS_GATEWAY_KEY_PINS"), Some(gateway));
    assert_eq!(
        value_of(&page, "PINGERS_PUSH_RELAY_KEY_PINS"),
        Some(pin_list(&pins(3)))
    );
    assert_eq!(
        value_of(&page, "Tag").as_deref(),
        Some("git tag -a v1.0.0-rc.1 -m \"Pingers v1.0.0-rc.1\"")
    );
    assert_eq!(
        value_of(&page, "Push").as_deref(),
        Some("git push origin v1.0.0-rc.1")
    );
    Ok(())
}

#[test]
fn the_first_phone_link_carries_the_server_code_and_pins() -> TestResult {
    let config = config()?;
    let page = build(&config, &running());
    let qr = page
        .steps
        .iter()
        .flat_map(|step| &step.items)
        .find_map(|item| match item {
            Item::Qr { data, .. } => Some(data.clone()),
            _ => None,
        })
        .ok_or("no QR code")?;
    let payload = SetupPayload::from_link(&qr)?;
    assert_eq!(payload.server, config.server_id);
    assert_eq!(
        payload.registration_code.expose(),
        config.registration_code.expose()
    );
    assert_eq!(payload.gateway_pins, pins(1));
    assert_eq!(payload.push_relay_pins, pins(3));
    assert_eq!(value_of(&page, "Setup link"), Some(qr));
    assert_eq!(
        value_of(&page, "Registration code").as_deref(),
        Some(config.registration_code.expose())
    );
    Ok(())
}

#[test]
fn with_the_services_down_the_page_says_why_and_shows_no_link() -> TestResult {
    let page = build(&config()?, &down());
    let items: Vec<&Item> = page.steps.iter().flat_map(|step| &step.items).collect();
    assert!(!items.iter().any(|item| matches!(item, Item::Qr { .. })));
    assert!(value_of(&page, "PINGERS_GATEWAY_KEY_PINS").is_none());
    let failed: Vec<&str> = items
        .iter()
        .filter_map(|item| match item {
            Item::Check {
                label,
                state: CheckState::Failed,
                ..
            } => Some(label.as_str()),
            _ => None,
        })
        .collect();
    for label in [
        "PINGERS_GATEWAY_KEY_PINS",
        "PINGERS_PUSH_RELAY_KEY_PINS",
        "The map host serves map packs",
        "pingers-server's gateway answers on the appliance",
    ] {
        assert!(failed.contains(&label), "{label}");
    }
    // The hosts and the code do not depend on the services.
    assert!(value_of(&page, "PINGERS_SERVER_HOST").is_some());
    assert!(value_of(&page, "Registration code").is_some());
    Ok(())
}

#[test]
fn the_json_is_nodos_owner_page_format() -> TestResult {
    let json = serde_json::to_value(build(&config()?, &running()))?;
    assert_eq!(json.get("version"), Some(&serde_json::json!(1)));
    let steps = json
        .get("steps")
        .and_then(serde_json::Value::as_array)
        .ok_or("steps")?;
    let kinds: std::collections::BTreeSet<&str> = steps
        .iter()
        .filter_map(|step| step.get("items")?.as_array())
        .flatten()
        .filter_map(|item| item.get("kind")?.as_str())
        .collect();
    assert_eq!(
        kinds.into_iter().collect::<Vec<_>>(),
        [
            "backup", "check", "domain", "qr", "service", "site", "text", "value"
        ]
    );
    let first = steps.first().ok_or("no step")?;
    assert_eq!(first.get("state"), Some(&serde_json::json!("auto")));
    assert!(
        json.to_string()
            .contains(r#"{"kind":"site","package":"pingers-server","site":"api"}"#)
    );
    assert!(json.to_string().contains(r#"{"kind":"backup"}"#));
    // Within nodo's limits: at most 32 steps of 32 items, texts and values short enough.
    assert!(steps.len() <= 32);
    for step in steps {
        let items = step
            .get("items")
            .and_then(serde_json::Value::as_array)
            .ok_or("items")?;
        assert!(items.len() <= 32);
        for item in items {
            for field in ["text", "value"] {
                if let Some(text) = item.get(field).and_then(serde_json::Value::as_str) {
                    assert!(text.len() <= 2048, "{text}");
                }
            }
            if let Some(data) = item.get("data").and_then(serde_json::Value::as_str) {
                assert!(data.len() <= 1024);
            }
        }
    }
    Ok(())
}

#[test]
fn a_long_detail_is_cut_to_nodos_limit() {
    let long = crate::checks::Check::new("TLS", State::Failed, "x".repeat(2000));
    let detail = long.detail.unwrap_or_default();
    assert_eq!(detail.chars().count(), crate::checks::MAX_DETAIL_CHARS);
    assert!(detail.ends_with('\u{2026}'));
    let short = crate::checks::Check::new("TLS", State::Ok, "");
    assert_eq!(short.detail, None);
    let exact = crate::checks::Check::new("TLS", State::Ok, "y".repeat(512));
    assert_eq!(exact.detail.map(|detail| detail.len()), Some(512));
}

#[test]
fn firebase_is_done_only_when_the_relay_serves_fcm() -> TestResult {
    let config = config()?;
    let state = |fcm: bool| {
        let facts = Facts {
            push_relay: Ok(PushRelayFacts { pins: pins(3), fcm }),
            ..running()
        };
        build(&config, &facts)
            .steps
            .into_iter()
            .find(|step| step.title == "Firebase wake-ups")
            .and_then(|step| {
                step.items.into_iter().find_map(|item| match item {
                    Item::Check { state, detail, .. } => Some((state, detail)),
                    _ => None,
                })
            })
    };
    // A passing check has no detail at all, not an empty one.
    assert_eq!(state(true), Some((CheckState::Ok, None)));
    let unipush = state(false).ok_or("no check")?;
    assert_eq!(unipush.0, CheckState::Failed);
    assert!(unipush.1.is_some());
    Ok(())
}

/// Every text a page step shows, values and labels included.
fn step_text(page: &Page, title: &str) -> String {
    page.steps
        .iter()
        .filter(|step| step.title == title)
        .flat_map(|step| &step.items)
        .map(|item| match item {
            Item::Text { text } => text.clone(),
            Item::Value { label, value } => format!("{label} {value}"),
            Item::Check { label, .. } => label.clone(),
            _ => String::new(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The names after `prefix` (`vars.` or `secrets.`) in a workflow file.
fn workflow_names(workflow: &str, prefix: &str) -> Vec<String> {
    let mut names: Vec<String> = workflow
        .split(prefix)
        .skip(1)
        .map(|rest| {
            rest.chars()
                .take_while(|character| character.is_ascii_uppercase() || *character == '_')
                .collect::<String>()
        })
        .filter(|name| !name.is_empty())
        .collect();
    names.sort();
    names.dedup();
    names
}

const RELEASE_WORKFLOW: &str = include_str!("../../../../.github/workflows/release.yml");
const CI_WORKFLOW: &str = include_str!("../../../../.github/workflows/ci.yml");

#[test]
fn the_page_names_every_variable_the_release_reads_with_a_value() -> TestResult {
    let page = build(&config()?, &running());
    let variables = workflow_names(RELEASE_WORKFLOW, "vars.");
    assert_eq!(variables.len(), 6, "{variables:?}");
    let commands = value_of(&page, "The same with the gh command line").ok_or("no gh lines")?;
    for name in &variables {
        let shown = value_of(&page, name).ok_or_else(|| format!("{name} not on the page"))?;
        let line = format!("gh variable set {name} --repo OceanEilonwy/pingers --body={shown}");
        assert!(commands.lines().any(|command| command == line), "{name}");
    }
    assert_eq!(commands.lines().count(), variables.len());
    Ok(())
}

#[test]
fn the_page_names_every_secret_the_workflows_read() -> TestResult {
    let page = build(&config()?, &running());
    let github = step_text(&page, "Copy the hosts and pins into GitHub");
    let variables = workflow_names(RELEASE_WORKFLOW, "vars.");
    for name in workflow_names(RELEASE_WORKFLOW, "secrets.")
        .into_iter()
        .chain(workflow_names(CI_WORKFLOW, "secrets."))
    {
        // A host may be a secret instead of a variable; the page sets it as a variable.
        if variables.contains(&name) {
            continue;
        }
        assert!(github.contains(&name), "{name}");
    }
    // CI reads GOOGLE_SERVICES_JSON outside the release environment.
    assert!(github.contains("GOOGLE_SERVICES_JSON as a repository secret"));
    assert!(github.contains("environment release"));
    Ok(())
}

#[test]
fn each_site_shows_its_dns_record() -> TestResult {
    let page = build(&config()?, &running());
    for (package, name) in [
        ("pingers-server", "pingers"),
        ("pingers-ohttp-relay", "ohttp"),
        ("pingers-push-relay", "push"),
        ("pingers-map-host", "maps"),
    ] {
        assert_eq!(
            value_of(&page, &format!("{package} DNS record")),
            Some(format!(
                "Type A, name {name} ({name}.example.org), value {{public_address}}"
            )),
        );
    }
    assert_eq!(
        dns_record("example.org", "example.org"),
        "Type A, name @ (example.org), value {public_address}"
    );
    assert_eq!(
        dns_record("pingers.other.net", "example.org"),
        "Type A, name pingers.other.net (pingers.other.net), value {public_address}"
    );
    Ok(())
}
