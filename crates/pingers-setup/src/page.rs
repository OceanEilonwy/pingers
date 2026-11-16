// SPDX-License-Identifier: AGPL-3.0-only
//! The owner page (nodo's owner page format, version 1): the ordered checklist of everything
//! the owner does by hand, with this appliance's own values filled in. nodo fills the items it
//! knows better (`site`, `service`, `domain`, `backup`, and the `{device_address}`,
//! `{public_address}` and `{data_dir:<app>}` tokens in texts and values) and derives `auto`
//! step states from the checks.

use pingers_proto::{KeyConfigHash, SetupPayload};
use serde::Serialize;

use crate::checks::{Check, Facts, State};
use crate::config::Config;

/// The page as nodo reads it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Page {
    /// Always 1.
    pub version: u32,
    /// The page's title.
    pub title: String,
    /// A short paragraph under the title.
    pub intro: String,
    /// The checklist, in order.
    pub steps: Vec<Step>,
}

/// A step's state. `Auto` lets nodo derive it from the step's checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StepState {
    /// Done.
    Done,
    /// Still to do.
    Todo,
    /// Something to fix.
    Problem,
    /// The appliance cannot tell.
    Unknown,
    /// From the checks.
    Auto,
}

/// One step of the checklist.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Step {
    /// What to do, as a short imperative.
    pub title: String,
    /// Done or not, where the appliance can tell.
    pub state: StepState,
    /// What it takes, and the checks.
    pub items: Vec<Item>,
}

/// One item of a step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Item {
    /// A paragraph.
    Text {
        /// The words.
        text: String,
    },
    /// A value to copy, with what it is.
    Value {
        /// What it is, such as a variable's name.
        label: String,
        /// The value.
        value: String,
    },
    /// A check and its outcome.
    Check {
        /// What was checked.
        label: String,
        /// How it went.
        state: CheckState,
        /// More about it.
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// A QR code of `data`.
    Qr {
        /// What to do with it.
        label: String,
        /// What it encodes.
        data: String,
    },
    /// A site of an app, filled in by nodo: its address, DNS record, certificate, reachability.
    Site {
        /// The app.
        package: String,
        /// The site's name in its manifest.
        site: String,
    },
    /// An app's service, filled in by nodo: running and healthy.
    Service {
        /// The app.
        package: String,
    },
    /// Whether a domain provider holding the domain is connected, filled in by nodo.
    Domain {
        /// The domain.
        domain: String,
    },
    /// The newest backup, filled in by nodo.
    Backup {},
}

/// A check's outcome in the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckState {
    /// Passed.
    Ok,
    /// Failed.
    Failed,
    /// Not yet.
    Waiting,
    /// Not tried.
    Unknown,
}

impl From<State> for CheckState {
    fn from(state: State) -> Self {
        match state {
            State::Ok => Self::Ok,
            State::Failed => Self::Failed,
            State::Waiting => Self::Waiting,
            State::Unknown => Self::Unknown,
        }
    }
}

fn text(text: impl Into<String>) -> Item {
    Item::Text { text: text.into() }
}

fn value(label: impl Into<String>, value: impl Into<String>) -> Item {
    Item::Value {
        label: label.into(),
        value: value.into(),
    }
}

fn check(check: &Check) -> Item {
    Item::Check {
        label: check.label.clone(),
        state: check.state.into(),
        detail: check.detail.clone(),
    }
}

fn check_of(label: &str, state: State, detail: impl Into<String>) -> Item {
    let detail: String = detail.into();
    Item::Check {
        label: label.to_owned(),
        state: state.into(),
        detail: Some(detail).filter(|detail| !detail.is_empty()),
    }
}

fn step(title: &str, state: StepState, items: Vec<Item>) -> Step {
    Step {
        title: title.to_owned(),
        state,
        items,
    }
}

/// The nodo apps Pingers installs, the setup app last.
const APPS: [&str; 5] = [
    "pingers-push-relay",
    "pingers-map-host",
    "pingers-server",
    "pingers-ohttp-relay",
    "pingers",
];

/// Pins as an app build's variable takes them: comma-separated, served key first.
#[must_use]
pub fn pin_list(pins: &[KeyConfigHash]) -> String {
    pins.iter()
        .map(KeyConfigHash::to_pin)
        .collect::<Vec<_>>()
        .join(",")
}

/// Builds the page from the configuration and what the checks found.
#[must_use]
pub fn build(config: &Config, facts: &Facts) -> Page {
    Page {
        version: 1,
        title: "Pingers setup".to_owned(),
        intro: format!(
            "Pingers made its keys and codes when you installed it. These are the steps left \
             for you, with the values from this appliance. A step turns done when the \
             appliance can see it is; it cannot see GitHub or your phone, so tick those off \
             yourself. Your domain: {}.",
            config.domain
        ),
        steps: vec![
            domain_step(config),
            sites_step(config),
            router_step(facts),
            services_step(facts),
            firebase_step(facts),
            maps_step(facts),
            github_step(config, facts),
            release_step(config),
            phone_step(config, facts),
            backup_step(),
        ],
    }
}

fn domain_step(config: &Config) -> Step {
    step(
        "Connect your domain",
        StepState::Auto,
        vec![
            text(format!(
                "At Porkbun: Account, API Access, create an API key (you get an API key and a \
                 secret key), then open {domain} under Domain Management, Details, and turn on \
                 API Access. On this appliance: Web, Domain providers, Add, Porkbun, and paste \
                 both keys. The appliance then creates the DNS records for the four sites and \
                 keeps them pointed at your public address, and gets their certificates \
                 through DNS (DNS-01), so port 80 need not be forwarded.",
                domain = config.domain
            )),
            Item::Domain {
                domain: config.domain.clone(),
            },
        ],
    )
}

fn sites_step(config: &Config) -> Step {
    let mut items = vec![text(format!(
        "For each app below: Apps, the app, Hosting, Internet; pick {domain}, keep the address \
         part it suggests, check, then Put it online. On the confirm screen turn the rate limit \
         off for ohttp.{domain} (phones behind a carrier share addresses) and keep it on for \
         the other three, unless your router terminates TLS (next step), then off for all four. \
         The records below are the ones the appliance publishes; with another DNS provider, \
         create them yourself.",
        domain = config.domain
    ))];
    for site in config.origins() {
        items.push(value(format!("{} address", site.package), site.host()));
        items.push(Item::Site {
            package: site.package.to_owned(),
            site: site.site.to_owned(),
        });
    }
    step("Put the four sites online", StepState::Auto, items)
}

fn router_step(facts: &Facts) -> Step {
    let mut items = vec![
        text(
            "On your router, forward TCP port 443 to this appliance, {device_address}, port \
             443. Your public address is {public_address}.",
        ),
        text(
            "If your router runs its own reverse proxy in front of the appliance, set it to \
             pass TLS through (TCP or SNI forwarding) for the four names to \
             {device_address}:443. That is the recommended setup: the phone then talks \
             post-quantum TLS to the appliance itself.",
        ),
        text(
            "If the proxy can only terminate TLS: it must offer X25519MLKEM768, connect to \
             {device_address}:443 over HTTPS with the site's name as SNI and HTTP/2, must not \
             buffer request or response bodies for the server and the relay, and must keep idle \
             connections open for more than 60 seconds. Then turn the rate limit off for all \
             four sites, since the appliance sees only the router's address.",
        ),
    ];
    items.extend(facts.public.iter().take(4).map(check));
    step("Forward port 443 to this appliance", StepState::Auto, items)
}

fn services_step(facts: &Facts) -> Step {
    let mut items: Vec<Item> = APPS
        .iter()
        .map(|package| Item::Service {
            package: (*package).to_owned(),
        })
        .collect();
    items.push(match &facts.gateway_pins {
        Ok(_) => check_of(
            "pingers-server's gateway answers on the appliance",
            State::Ok,
            "",
        ),
        Err(why) => check_of(
            "pingers-server's gateway answers on the appliance",
            State::Failed,
            why.clone(),
        ),
    });
    items.push(match &facts.push_relay {
        Ok(_) => check_of("pingers-push-relay answers on the appliance", State::Ok, ""),
        Err(why) => check_of(
            "pingers-push-relay answers on the appliance",
            State::Failed,
            why.clone(),
        ),
    });
    items.extend(facts.public.iter().skip(4).map(check));
    step("Check the services answer", StepState::Auto, items)
}

fn firebase_step(facts: &Facts) -> Step {
    let state = match &facts.push_relay {
        Ok(relay) if relay.fcm => check_of(
            "The push relay wakes phones through Firebase",
            State::Ok,
            "",
        ),
        Ok(_) => check_of(
            "The push relay wakes phones through Firebase",
            State::Failed,
            "It serves UnifiedPush only: paste the service account (below).",
        ),
        Err(why) => check_of(
            "The push relay wakes phones through Firebase",
            State::Waiting,
            why.clone(),
        ),
    };
    step(
        "Firebase wake-ups",
        StepState::Auto,
        vec![
            text(
                "You pasted the Firebase service account's key file when you installed \
                 Pingers; it is sealed on this appliance. To replace it: Apps, \
                 pingers-push-relay, Settings, Firebase service account, and paste the new key \
                 file, then delete the old key in the Google Cloud console.",
            ),
            state,
        ],
    )
}

fn maps_step(facts: &Facts) -> Step {
    let state = match facts.map_regions {
        Ok(0) => check_of(
            "The map host serves map packs",
            State::Waiting,
            "No packs uploaded yet.",
        ),
        Ok(count) => check_of(
            "The map host serves map packs",
            State::Ok,
            format!("{count} regions."),
        ),
        Err(ref why) => check_of("The map host serves map packs", State::Failed, why.clone()),
    };
    step(
        "Upload the map packs",
        StepState::Auto,
        vec![
            text(
                "On your computer, cut the packs with pingers-tiles from the newest daily \
                 build at build.protomaps.com, for the regions in your regions.toml, within the \
                 map host's disk budget (crates/pingers-tiles/README.md has the commands). Then \
                 upload them to the appliance. Packs are public map data; uploading while the \
                 host runs is safe. Do not add --delete: the directory also holds the host's own \
                 key.",
            ),
            value(
                "Upload command",
                "rsync -a --partial --chown=_pingers-map-host:_pingers-map-host ./packs/ \
                 root@{device_address}:{data_dir:pingers-map-host}/",
            ),
            state,
        ],
    )
}

fn github_step(config: &Config, facts: &Facts) -> Step {
    let mut items = vec![text(format!(
        "At github.com/{repo}: Settings, Secrets and variables, Actions, the Variables tab, New \
         repository variable, once for each name below. The release builds read them; the pins \
         make the app accept only this appliance's keys. The release workflow also needs its \
         signing secrets in the release environment (PINGERS_KEYSTORE_BASE64, \
         PINGERS_KEYSTORE_PASSWORD, PINGERS_KEY_ALIAS, GOOGLE_SERVICES_JSON; \
         android/docs/release.md).",
        repo = config.github_repository
    ))];
    let [server, _, push, maps] = config.origins();
    items.push(value("PINGERS_SERVER_HOST", server.origin));
    items.push(value("PINGERS_OHTTP_RELAY", config.ohttp_relay_url.clone()));
    items.push(value("PINGERS_PUSH_RELAY", push.origin));
    items.push(value("PINGERS_MAP_HOST", maps.origin));
    match &facts.gateway_pins {
        Ok(pins) => items.push(value("PINGERS_GATEWAY_KEY_PINS", pin_list(pins))),
        Err(why) => items.push(check_of(
            "PINGERS_GATEWAY_KEY_PINS",
            State::Failed,
            why.clone(),
        )),
    }
    match &facts.push_relay {
        Ok(relay) => items.push(value("PINGERS_PUSH_RELAY_KEY_PINS", pin_list(&relay.pins))),
        Err(why) => items.push(check_of(
            "PINGERS_PUSH_RELAY_KEY_PINS",
            State::Failed,
            why.clone(),
        )),
    }
    step(
        "Copy the hosts and pins into GitHub",
        StepState::Unknown,
        items,
    )
}

fn release_step(config: &Config) -> Step {
    let tag = &config.release_tag;
    step(
        "Make a release",
        StepState::Unknown,
        vec![
            text(format!(
                "In your checkout of {repo}, on the commit to release, tag it and push the tag. \
                 The release workflow then builds both APKs with the variables above and \
                 publishes them at github.com/{repo}/releases (a -rc tag makes a \
                 pre-release). Install it on your phone with Obtainium, with pre-releases on \
                 (docs/install.md).",
                repo = config.github_repository
            )),
            value("Tag", format!("git tag -a {tag} -m \"Pingers {tag}\"")),
            value("Push", format!("git push origin {tag}")),
        ],
    )
}

fn phone_step(config: &Config, facts: &Facts) -> Step {
    let mut items = vec![text(
        "Once the release is on your phone, scan this code with the phone's camera. Pingers \
         opens, asks to set up, and makes your phone's account with this server's registration \
         code. Friends then join through your invite links, which carry the code; they never \
         need it. Keep the code to yourself like an invite link.",
    )];
    let link = match (&facts.gateway_pins, &facts.push_relay) {
        (Ok(gateway), Ok(relay)) => SetupPayload {
            server: config.server_id.clone(),
            registration_code: config.registration_code.clone(),
            gateway_pins: gateway.clone(),
            push_relay_pins: relay.pins.clone(),
        }
        .to_link()
        .map_err(|_| "the setup link could not be made".to_owned()),
        (Err(why), _) | (_, Err(why)) => Err(why.clone()),
    };
    match link {
        Ok(link) => {
            items.push(Item::Qr {
                label: "First-phone setup link".to_owned(),
                data: link.to_string(),
            });
            items.push(value("Setup link", link.to_string()));
        }
        Err(why) => items.push(check_of(
            "The setup link is ready",
            State::Waiting,
            format!("It needs both keys from the running services: {why}."),
        )),
    }
    items.push(value(
        "Registration code",
        config.registration_code.expose(),
    ));
    items.push(text(
        "To stop new accounts from old invite links, Reset the registration code (Apps, \
         pingers-server, Settings); phones that already have an account are not affected, and \
         this page then shows a new link.",
    ));
    step("Set up your first phone", StepState::Unknown, items)
}

fn backup_step() -> Step {
    step(
        "Make a backup",
        StepState::Auto,
        vec![
            text(
                "System, Backups, back up to a USB drive, with the device passphrase so the \
                 backup includes the device key; without it the sealed keys cannot be \
                 restored. Store the drive away from the appliance.",
            ),
            Item::Backup {},
        ],
    )
}

#[cfg(test)]
mod tests;
