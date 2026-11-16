// SPDX-License-Identifier: AGPL-3.0-only
//! The first databases a process opens, opened from several threads at once.
//!
//! SQLite marks itself initialised before it runs SQLCipher's own initialisation, so a second
//! thread that opens a database in that window keys it before SQLCipher is ready and fails with
//! "An error occurred with PRAGMA key". In the app this is a push wake and the UI opening the
//! store at the same moment as the process starts. SQLite initialises once per process, so this
//! test is alone in its own test binary: it must be the first SQLite use in the process.

#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::thread;

use pingers_store::{Database, DatabaseKey, StoreError};
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// The window is narrow: without the fix about one run in 700 fails under heavy load, so this
/// catches a regression over repeated CI runs rather than on every run.
const THREADS: usize = 16;

/// A fresh directory under the system temporary directory, removed on drop.
struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Result<Self, std::io::Error> {
        let path =
            std::env::temp_dir().join(format!("pingers-store-first-open-{}", std::process::id()));
        drop(std::fs::remove_dir_all(&path));
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.0));
    }
}

#[test]
fn the_first_opens_in_a_process_can_run_at_the_same_moment() -> Result<(), TestError> {
    let directory = TestDir::new()?;
    let start = Arc::new(Barrier::new(THREADS));
    let openers: Vec<_> = (0..THREADS)
        .map(|index| {
            let path = directory.0.join(format!("store-{index}.db"));
            let start = Arc::clone(&start);
            thread::spawn(move || -> Result<(), StoreError> {
                start.wait();
                let database =
                    Database::open(&path, DatabaseKey::from_bytes(Zeroizing::new([7; 32])))?;
                database.session().map(drop)
            })
        })
        .collect();
    for opener in openers {
        let outcome = opener.join();
        assert!(
            matches!(outcome, Ok(Ok(()))),
            "an opener failed: {outcome:?}"
        );
    }
    Ok(())
}
