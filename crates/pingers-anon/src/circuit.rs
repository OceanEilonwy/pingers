// SPDX-License-Identifier: AGPL-3.0-only
//! The RLN-v2 circuit parameters both sides use.
//!
//! The proving key is the one compiled into `rln` 3.1.0 (`resources/tree_depth_20/rln_single`):
//! `RLN(20, 16)` from circom-rln, 5,820 constraints, five public inputs. Its verifying key is
//! identical, point for point, to the output of PSE's RLN Trusted Setup Ceremony (rln-20,
//! 60 contributions and a beacon, finalised 2023-09-04); the test below pins it. The bytes
//! are embedded in the binary, so nothing is read from the file system.

use std::sync::LazyLock;

use rln::circuit::{ArkGroth16Backend, default_graph_single, default_zkey_single};
use rln::hashers::PoseidonHash;

/// The Groth16 backend over the embedded key and witness graph. The key is decoded and the
/// verifying key prepared on first use (tens of milliseconds, about 30 MB).
pub(crate) static BACKEND: LazyLock<ArkGroth16Backend<PoseidonHash>> = LazyLock::new(|| {
    ArkGroth16Backend::new(
        std::sync::Arc::clone(default_zkey_single()),
        std::sync::Arc::clone(default_graph_single()),
    )
});

#[cfg(test)]
mod tests {
    use rln::circuit::default_zkey_single;

    /// `vk_delta_2` of PSE's RLN ceremony output (`rln-20_vkey.json`, sha256
    /// `75b035607a8c42fda93f7ce24011a25ac129737aaaa6271d511a98e3dd556964`). Delta is the
    /// element every phase-2 contribution changes, so equality ties the embedded key to the
    /// ceremony's final contribution.
    const CEREMONY_DELTA: [&str; 4] = [
        "17077735495685170943380938230836408503627170115414840315502244846025577589191",
        "14030085636943255545683322474441991939484590437387381169642530788494152024614",
        "11568745146423307387256571230823432454624378106569286849514884592874522611163",
        "1838524899938769516485895655063198583192139511330418290063560641219523306282",
    ];

    #[test]
    fn embedded_key_is_the_pse_ceremony_key() {
        let zkey = default_zkey_single();
        let delta = zkey.0.vk.delta_g2;
        let actual = [
            delta.x.c0.to_string(),
            delta.x.c1.to_string(),
            delta.y.c0.to_string(),
            delta.y.c1.to_string(),
        ];
        assert_eq!(actual, CEREMONY_DELTA);
        assert_eq!(zkey.0.vk.gamma_abc_g1.len(), 6, "five public inputs");
        assert_eq!(zkey.1.num_constraints, 5820);
    }
}
