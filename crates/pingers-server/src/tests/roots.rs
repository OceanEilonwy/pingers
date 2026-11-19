// SPDX-License-Identifier: AGPL-3.0-only
//! The server starts where the system has no CA certificates, as in its sandbox on
//! monokulo-nodo (no outbound internet, so no `/etc/ssl`): its HTTP clients trust bundled roots
//! and never read the system's store.

use std::process::Command;

use super::{LOOPBACK, Setup, TestResult, configured, run};
use crate::{DeliveryLive, MapHostGateway};

/// Set in the child process this test starts, which runs the test's body.
const CHILD: &str = "PINGERS_SERVER_TEST_NO_SYSTEM_ROOTS";

/// The system's store is read from `SSL_CERT_FILE` and `SSL_CERT_DIR` alone when either is set
/// (rustls-native-certs), so the child gets an empty file and an empty directory. A child
/// process, because the environment is the whole process's and other tests run alongside.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_server_starts_without_a_system_ca_store() -> TestResult {
    if std::env::var_os(CHILD).is_some() {
        // No push_relay_ca, so nothing is trusted but what the server brings itself; a map host
        // gateway, so its client is built too.
        let mut config = configured(Setup::default())?;
        config.map_host_gateway = MapHostGateway::from_settings(
            Some("https://maps.example"),
            Some(format!("http://{LOOPBACK}:9474")),
        )?;
        return run(config, DeliveryLive).await?.stop().await;
    }
    let empty =
        std::env::temp_dir().join(format!("pingers-server-no-roots-{}", std::process::id()));
    std::fs::create_dir_all(&empty)?;
    let file = empty.join("none.pem");
    std::fs::write(&file, "")?;
    let output = Command::new(std::env::current_exe()?)
        .args([
            "tests::roots::the_server_starts_without_a_system_ca_store",
            "--exact",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env("SSL_CERT_FILE", &file)
        .env("SSL_CERT_DIR", &empty)
        .output()?;
    std::fs::remove_dir_all(&empty)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "the server did not start without a system CA store:\n{stdout}\n{stderr}"
    );
    // The child ran this very test, not nothing.
    assert!(stdout.contains("1 passed"), "{stdout}");
    Ok(())
}
