// SPDX-License-Identifier: AGPL-3.0-only
//! The binary itself: each subcommand prints what it says, and failures exit non-zero.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const BINARY: &str = env!("CARGO_BIN_EXE_pingers-push-relay");
const HASH_KEY: &str = "4242424242424242424242424242424242424242424242424242424242424242";
const CREDENTIAL: &str = "0123456789abcdef0123456789abcdef";
/// `credential_hash` of [`CREDENTIAL`] under [`HASH_KEY`].
const CREDENTIAL_HASH: &str = "11e91a598f0cdd95086a51f58961207c05755742f65f5e2a4885bbe8721c3985";

fn run(arguments: &[&str], input: &str) -> Result<Output, std::io::Error> {
    let mut child = Command::new(BINARY)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input.as_bytes())?;
    }
    child.wait_with_output()
}

#[test]
fn keygen_prints_64_hex_digits() -> TestResult {
    let output = run(&["keygen"], "")?;
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout)?;
    let key = text.strip_suffix('\n').ok_or("no newline")?;
    assert_eq!(key.len(), 64);
    assert!(key.bytes().all(|byte| byte.is_ascii_hexdigit()));
    Ok(())
}

#[test]
fn credential_hash_reads_standard_input() -> TestResult {
    let output = run(&["credential-hash"], &format!("{HASH_KEY}\n{CREDENTIAL}\n"))?;
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout)?,
        format!("{CREDENTIAL_HASH}\n")
    );
    Ok(())
}

#[test]
fn pins_prints_one_pin_per_key() -> TestResult {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cli-pins-relay.toml");
    std::fs::write(
        &path,
        format!(
            "listen = \"127.0.0.1:0\"\noperator = \"x\"\nhash_key = \"{HASH_KEY}\"\n\
             [[keys]]\nkey_id = 1\nprivate_key = \"{HASH_KEY}\"\nactivated_at = 0\n\
             [[keys]]\nkey_id = 2\nprivate_key = \"{CREDENTIAL}{CREDENTIAL}\"\n\
             activated_at = 4102444800\n\
             [[servers]]\nserver_id = \"a\"\ncredential = \"{CREDENTIAL}\"\npushes_per_minute = 1\n\
             [unifiedpush]\n"
        ),
    )?;
    let output = run(&["pins", &path.to_string_lossy()], "")?;
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout)?;
    assert_eq!(text.lines().count(), 2);
    assert!(text.lines().all(|line| line.len() == 43));
    Ok(())
}

#[test]
fn failures_exit_non_zero_with_a_reason() -> TestResult {
    let missing = run(&["/nonexistent/pingers-push-relay.toml"], "")?;
    assert!(!missing.status.success());
    assert!(String::from_utf8(missing.stderr)?.contains("cannot read"));
    let usage = run(&["pins"], "")?;
    assert!(!usage.status.success());
    assert!(String::from_utf8(usage.stderr)?.contains("usage:"));
    Ok(())
}
