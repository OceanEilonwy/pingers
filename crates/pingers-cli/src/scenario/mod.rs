// SPDX-License-Identifier: AGPL-3.0-only
//! Scripted timelines: the file format, the runner, and the bundled G.3 to G.7 scenarios.

mod format;
mod runner;
#[cfg(test)]
mod tests;

pub(crate) use format::parse_with_seed;
pub(crate) use runner::{Checks, DryRunDriver, run};

/// A scenario shipped with the binary: the design's sequences G.3 to G.7.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Builtin {
    /// The name on the command line, for example `g3`.
    pub(crate) name: &'static str,
    /// The scenario file.
    pub(crate) text: &'static str,
}

/// The bundled scenarios, in design order.
pub(crate) const BUILTINS: [Builtin; 5] = [
    Builtin {
        name: "g3",
        text: include_str!("../../scenarios/g3-enter-spot-alone.toml"),
    },
    Builtin {
        name: "g4",
        text: include_str!("../../scenarios/g4-second-person-meetup.toml"),
    },
    Builtin {
        name: "g5",
        text: include_str!("../../scenarios/g5-third-person-joins.toml"),
    },
    Builtin {
        name: "g6",
        text: include_str!("../../scenarios/g6-someone-leaves.toml"),
    },
    Builtin {
        name: "g7",
        text: include_str!("../../scenarios/g7-everyone-leaves.toml"),
    },
];

/// The bundled scenario called `name`.
pub(crate) fn builtin(name: &str) -> Option<Builtin> {
    BUILTINS
        .iter()
        .find(|builtin| builtin.name.eq_ignore_ascii_case(name))
        .copied()
}
