// SPDX-License-Identifier: AGPL-3.0-only
//! The binary's commands as an operator runs them: `pins`, `keygen ohttp`, and the failures.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::path::PathBuf;
use std::process::{Command, Output};

use pingers_proto::KeyConfigHash;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const SECRET_A: &str = "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a";
const SECRET_B: &str = "1111111111111111111111111111111111111111111111111111111111111111";

fn server(arguments: &[&str]) -> Result<Output, std::io::Error> {
    Command::new(env!("CARGO_BIN_EXE_pingers-server"))
        .args(arguments)
        .output()
}

fn config_file(name: &str) -> Result<PathBuf, std::io::Error> {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::write(
        &path,
        format!(
            "listen = \"127.0.0.1:0\"\nserver_id = \"https://pingers.test\"\n\
             [ohttp]\nlisten = \"127.0.0.1:0\"\nrelays = [\"127.0.0.1\"]\nserved_key_id = 2\n\
             [[ohttp.keys]]\nkey_id = 1\nsecret = \"{SECRET_A}\"\n\
             [[ohttp.keys]]\nkey_id = 2\nsecret = \"{SECRET_B}\"\n"
        ),
    )?;
    Ok(path)
}

#[test]
fn pins_prints_one_pin_per_configured_key_and_no_secret() -> TestResult {
    let path = config_file("cli-pins.toml")?;
    let output = server(&["pins", path.to_str().ok_or("path")?])?;
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout)?;
    let config = pingers_server::load_config(&path)?;
    let expected: Vec<String> = pingers_server::pin_set(&config.ohttp_keys)
        .iter()
        .map(KeyConfigHash::to_pin)
        .collect();
    assert_eq!(text.lines().collect::<Vec<_>>(), expected);
    assert_eq!(expected.len(), 2);
    assert!(!text.contains(SECRET_A) && !text.contains(SECRET_B));
    Ok(())
}

#[test]
fn keygen_prints_a_new_secret_each_time() -> TestResult {
    let first = server(&["keygen", "ohttp"])?;
    let second = server(&["keygen", "ohttp"])?;
    assert!(first.status.success() && second.status.success());
    let first = String::from_utf8(first.stdout)?;
    assert_eq!(first.trim_end().len(), 64);
    assert!(first.ends_with('\n'));
    assert_ne!(first, String::from_utf8(second.stdout)?);
    Ok(())
}

#[test]
fn bad_arguments_and_unreadable_configs_fail_with_a_message() -> TestResult {
    for arguments in [
        &[][..],
        &["keygen", "push"],
        &["/nonexistent/server.toml"],
        &["pins", "/nonexistent/server.toml"],
    ] {
        let output = server(arguments)?;
        assert!(!output.status.success(), "{arguments:?}");
        assert!(output.stdout.is_empty(), "{arguments:?}");
        assert!(
            String::from_utf8(output.stderr)?.starts_with("pingers-server: "),
            "{arguments:?}"
        );
    }
    Ok(())
}
