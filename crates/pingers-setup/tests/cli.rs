// SPDX-License-Identifier: AGPL-3.0-only
//! The `pingers` binary: its usage, a bad configuration, and serving the page.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::io::Read as _;
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::Duration;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const BINARY: &str = env!("CARGO_BIN_EXE_pingers");

#[test]
fn without_one_configuration_file_it_prints_its_usage_and_fails() -> TestResult {
    for arguments in [&[][..], &["a.toml", "b.toml"][..]] {
        let output = Command::new(BINARY).args(arguments).output()?;
        assert!(!output.status.success());
        assert!(String::from_utf8(output.stderr)?.contains("usage: pingers <setup.toml>"));
    }
    Ok(())
}

#[test]
fn a_missing_or_bad_configuration_fails_by_name() -> TestResult {
    let dir = std::env::temp_dir().join(format!("pingers-setup-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let bad = dir.join("bad.toml");
    std::fs::write(&bad, "listen = \"0.0.0.0:1\"\n")?;
    let missing = Command::new(BINARY).arg(dir.join("none.toml")).output()?;
    let refused = Command::new(BINARY).arg(&bad).output()?;
    std::fs::remove_dir_all(&dir)?;
    assert!(!missing.status.success());
    assert!(String::from_utf8(missing.stderr)?.contains("reading the configuration file"));
    assert!(!refused.status.success());
    assert!(String::from_utf8(refused.stderr)?.contains("setup.toml"));
    Ok(())
}

#[test]
fn with_a_configuration_it_serves_the_page() -> TestResult {
    let port = TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    let dev = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/nodo/dev");
    let text = std::fs::read_to_string(dev.join("setup.toml"))?
        .replace("127.0.0.1:9479", &format!("127.0.0.1:{port}"))
        // Nothing listens for these here: the page says so instead of failing.
        .replace("127.0.0.1:9471", "127.0.0.1:1")
        .replace("127.0.0.1:9472", "127.0.0.1:1")
        .replace("127.0.0.1:9474", "127.0.0.1:1");
    let dir = std::env::temp_dir().join(format!("pingers-setup-serve-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let config = dir.join("setup.toml");
    std::fs::write(&config, text)?;
    let mut child = Command::new(BINARY)
        .arg(&config)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut page = None;
    for _ in 0..200 {
        if let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", port)) {
            use std::io::Write as _;
            stream.set_read_timeout(Some(Duration::from_secs(30)))?;
            stream.write_all(b"GET /owner HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")?;
            let mut answer = String::new();
            stream.read_to_string(&mut answer)?;
            page = Some(answer);
            break;
        }
        std::thread::park_timeout(Duration::from_millis(50));
    }
    child.kill()?;
    child.wait()?;
    std::fs::remove_dir_all(&dir)?;
    let page = page.ok_or("the page never answered")?;
    assert!(page.starts_with("HTTP/1.1 200"));
    assert!(page.contains("\"title\":\"Pingers setup\""));
    Ok(())
}
