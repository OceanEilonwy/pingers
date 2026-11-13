// SPDX-License-Identifier: AGPL-3.0-only
//! A wrapper that keeps values out of logs.

use core::fmt;

/// A value that could identify someone if logged: its `Debug` prints only the type name
/// (style guide §9).
///
/// Wrap fields of this kind in structs that derive `Debug`, so the derive stays safe.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Redacted<T>(T);

impl<T> Redacted<T> {
    /// Wraps a value.
    #[must_use]
    pub const fn new(value: T) -> Self {
        Self(value)
    }

    /// The wrapped value.
    #[must_use]
    pub const fn get(&self) -> &T {
        &self.0
    }

    /// Unwraps the value.
    #[must_use]
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> fmt::Debug for Redacted<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Redacted<{}>", core::any::type_name::<T>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_hides_the_value() {
        let printed = format!("{:?}", Redacted::new(String::from("The Anchor")));
        assert!(!printed.contains("Anchor"));
    }
}
