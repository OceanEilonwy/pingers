// SPDX-License-Identifier: AGPL-3.0-only
//! The binary's stop signals. In a test binary of its own: the signal goes to this whole
//! process, which survives it only because the handler is installed first.
#![cfg(unix)]
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::time::Duration;

use pingers_server::StopSignals;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const STEP: Duration = Duration::from_secs(10);

fn send_to_self(signal: &str) -> TestResult {
    let status = std::process::Command::new("kill")
        .args([signal, &std::process::id().to_string()])
        .status()?;
    assert!(status.success());
    Ok(())
}

#[tokio::test]
async fn sigterm_and_ctrl_c_each_stop_the_server() -> TestResult {
    let mut signals = StopSignals::install();
    for signal in ["-TERM", "-INT"] {
        let mut waiting = std::pin::pin!(signals.received());
        // Nothing has arrived yet.
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut waiting)
                .await
                .is_err()
        );
        send_to_self(signal)?;
        tokio::time::timeout(STEP, waiting).await?;
    }
    Ok(())
}
