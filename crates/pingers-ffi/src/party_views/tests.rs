// SPDX-License-Identifier: AGPL-3.0-only
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::sync::Arc;

use pingers_screens::fixtures;

use super::*;
use crate::views::{PushRelayChoice, Reachability, ServerChoice, ServiceDraft};

fn snapshot() -> Result<Arc<Snapshot>, CoreError> {
    let snapshot = fixtures::snapshot().map_err(|_| CoreError::invalid("fixture"))?;
    Ok(Snapshot::new(Arc::new(snapshot)))
}

fn crew() -> PartyId {
    fixtures::climbing_crew().into()
}

#[test]
fn party_settings_convert_the_canvas_example() -> Result<(), CoreError> {
    let view = snapshot()?.party_settings(&crew(), Some(9_880), 10_000)?;
    assert_eq!(view.privacy, PrivacyLevel::Full);
    assert_eq!(view.privacy_title, "Full privacy");
    assert_eq!(view.consequence_before, None);
    assert_eq!(
        view.relay_check.as_deref(),
        Some("Your server answered through it · 2 min ago")
    );
    assert!(view.relay_check_ok);
    assert_eq!(view.push_relay.operator, "Pingers");
    assert!(!view.push_relay.shared);
    let unchecked = snapshot()?.party_settings(&crew(), None, 10_000)?;
    assert_eq!(unchecked.relay_check, None);
    assert!(!unchecked.relay_check_ok);
    Ok(())
}

#[test]
fn safety_codes_show_the_code_for_the_first_member_not_checked() -> Result<(), CoreError> {
    let snapshot = snapshot()?;
    let first = snapshot
        .first_to_check(&crew())?
        .ok_or_else(|| CoreError::invalid("nobody"))?;
    let view = snapshot.safety_codes(&crew(), Some(first), Some("48210 93755".into()))?;
    assert_eq!(view.shown, Some(first));
    assert_eq!(view.code_groups, ["48210", "93755"]);
    assert_eq!(
        view.mark.as_deref(),
        Some("Codes match · Mark Ana as checked")
    );
    assert_eq!(view.members.iter().filter(|row| row.selected).count(), 1);
    assert_eq!(view.members.iter().filter(|row| row.checked).count(), 2);
    let none = snapshot.safety_codes(&crew(), Some(first), None)?;
    assert_eq!(none.shown, None);
    assert!(matches!(
        snapshot.safety_codes(&crew(), Some(first), Some("not digits".into())),
        Err(CoreError::Invalid { .. })
    ));
    Ok(())
}

#[test]
fn leaving_and_the_code_screen_convert() -> Result<(), CoreError> {
    let snapshot = snapshot()?;
    let leave = snapshot.leave_party(&crew())?;
    assert_eq!(leave.title, "Leave Climbing crew?");
    assert!(leave.meetup_warning.is_some());
    let draft = PartyDraft {
        name: "Climbing crew".into(),
        colour: crate::views::PartyColour::Blue,
        server_choice: ServerChoice::BuiltIn,
        built_in: "server.pingers.org".into(),
        server: ServiceDraft {
            host: "server.pingers.org".into(),
            check: Reachability::Reachable {
                operator: "Pingers".into(),
            },
        },
        server_keys: crate::views::ServerKeysDraft::NotLookedUp,
        relay: ServiceDraft {
            host: "server.pingers.org".into(),
            check: Reachability::Checking,
        },
        push_relay: PushRelayChoice::Official,
        push_relay_operator: None,
        post_quantum: false,
    };
    let code = snapshot.party_code(&draft, String::new(), CodeAnswer::Refused)?;
    assert_eq!(code.server, "server.pingers.org");
    assert_eq!(code.operator, "Run by Pingers");
    assert_eq!(code.create, "Try again");
    assert!(!code.can_create);
    Ok(())
}
