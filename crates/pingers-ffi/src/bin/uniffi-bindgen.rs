// SPDX-License-Identifier: AGPL-3.0-only
//! Generates the Kotlin bindings from the built library (feature `bindgen`).

fn main() {
    uniffi::uniffi_bindgen_main();
}
