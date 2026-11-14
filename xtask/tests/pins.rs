// SPDX-License-Identifier: AGPL-3.0-only
//! `cargo xtask pins` end to end: the binary reads both files and prints both variables.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::path::PathBuf;
use std::process::Command;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const KEY_A: &str = "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a";
const KEY_B: &str = "1111111111111111111111111111111111111111111111111111111111111111";

fn write(name: &str, text: &str) -> Result<PathBuf, std::io::Error> {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::write(&path, text)?;
    Ok(path)
}

#[test]
fn pins_prints_both_build_variables() -> TestResult {
    let server = write(
        "xtask-pins-server.toml",
        &format!(
            "listen = \"0.0.0.0:1\"\nserver_id = \"https://pingers.test\"\n\
             [ohttp]\nrelays = [\"127.0.0.1\"]\n[[ohttp.keys]]\nkey_id = 1\nsecret = \"{KEY_A}\"\n"
        ),
    )?;
    let relay = write(
        "xtask-pins-relay.toml",
        &format!(
            "[[keys]]\nkey_id = 1\nprivate_key = \"{KEY_A}\"\nactivated_at = 0\n\
             [[keys]]\nkey_id = 2\nprivate_key = \"{KEY_B}\"\nactivated_at = 4102444800\n"
        ),
    )?;
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("pins")
        .arg(&server)
        .arg(&relay)
        .output()?;
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout)?;
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    let gateway = lines
        .first()
        .and_then(|line| line.strip_prefix("PINGERS_GATEWAY_KEY_PINS="))
        .ok_or("no gateway line")?;
    let push = lines
        .get(1)
        .and_then(|line| line.strip_prefix("PINGERS_PUSH_RELAY_KEY_PINS="))
        .ok_or("no push relay line")?;
    assert_eq!(gateway.split(',').count(), 1);
    assert_eq!(push.split(',').count(), 2);
    assert!(
        gateway
            .split(',')
            .chain(push.split(','))
            .all(|pin| pin.len() == 43)
    );
    assert!(!text.contains(KEY_A) && !text.contains(KEY_B));
    Ok(())
}

#[test]
fn pins_fails_on_a_missing_file() -> TestResult {
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "pins",
            "/nonexistent/server.toml",
            "/nonexistent/relay.toml",
        ])
        .output()?;
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)?.contains("cannot read"));
    Ok(())
}
