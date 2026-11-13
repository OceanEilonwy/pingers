// SPDX-License-Identifier: AGPL-3.0-only
//! Byte sizes as operators write them (`60GB`, `512 MiB`) and the disk budget check.

use std::fmt;

/// A size in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ByteSize(u64);

/// A size that did not parse.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("`{0}` is not a size (for example 60GB, 512MB or 2TiB)")]
pub struct SizeError(String);

impl ByteSize {
    /// A size of `bytes` bytes.
    #[must_use]
    pub const fn new(bytes: u64) -> Self {
        Self(bytes)
    }

    /// The number of bytes.
    #[must_use]
    pub const fn bytes(self) -> u64 {
        self.0
    }

    /// The sum, saturating.
    #[must_use]
    pub const fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }

    /// Parses `60GB`, `60 GB`, `3.2 MB`, `512MiB` or a plain number of bytes. KB, MB, GB and
    /// TB are decimal (the units `pmtiles` prints); KiB, MiB, GiB and TiB are binary.
    ///
    /// # Errors
    ///
    /// [`SizeError`] if it does not parse or overflows.
    pub fn parse(text: &str) -> Result<Self, SizeError> {
        let error = || SizeError(text.to_owned());
        let trimmed = text.trim();
        let split = trimmed
            .find(|character: char| !(character.is_ascii_digit() || character == '.'))
            .unwrap_or(trimmed.len());
        let (number, unit) = trimmed.split_at_checked(split).ok_or_else(error)?;
        let multiplier: u64 = match unit.trim().to_ascii_lowercase().as_str() {
            "" | "b" => 1,
            "kb" => 1_000,
            "mb" => 1_000_000,
            "gb" => 1_000_000_000,
            "tb" => 1_000_000_000_000,
            "kib" => 1 << 10,
            "mib" => 1 << 20,
            "gib" => 1 << 30,
            "tib" => 1 << 40,
            _ => return Err(error()),
        };
        // Whole and fractional parts separately, so `3.2 MB` is exact and nothing goes
        // through floating point.
        let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
        if whole.is_empty() && fraction.is_empty() {
            return Err(error());
        }
        let whole: u64 = if whole.is_empty() {
            0
        } else {
            whole.parse().map_err(|_| error())?
        };
        let mut bytes = whole.checked_mul(multiplier).ok_or_else(error)?;
        let mut scale = multiplier;
        for digit in fraction.chars() {
            let digit = u64::from(digit.to_digit(10).ok_or_else(error)?);
            scale /= 10;
            bytes = bytes
                .checked_add(digit.checked_mul(scale).ok_or_else(error)?)
                .ok_or_else(error)?;
        }
        Ok(Self(bytes))
    }
}

impl fmt::Display for ByteSize {
    /// Decimal units with one decimal: `3.2 MB`, `58.0 GB`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let units = [
            ("TB", 1_000_000_000_000_u64),
            ("GB", 1_000_000_000),
            ("MB", 1_000_000),
            ("kB", 1_000),
        ];
        for (unit, size) in units {
            if self.0 >= size {
                let tenths = self.0.checked_div(size / 10).unwrap_or(0);
                return write!(formatter, "{}.{} {unit}", tenths / 10, tenths % 10);
            }
        }
        write!(formatter, "{} B", self.0)
    }
}

/// One line of a budget table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetLine {
    /// The region id.
    pub id: String,
    /// The zoom it would be cut at.
    pub max_zoom: u8,
    /// Its estimated pack size.
    pub size: ByteSize,
}

/// Whether a set of packs fits a disk budget.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetReport {
    /// Each region's estimate.
    pub lines: Vec<BudgetLine>,
    /// Their sum.
    pub total: ByteSize,
    /// The budget.
    pub budget: ByteSize,
}

impl BudgetReport {
    /// Sums `lines` against `budget`.
    #[must_use]
    pub fn new(lines: Vec<BudgetLine>, budget: ByteSize) -> Self {
        let total = lines.iter().fold(ByteSize::default(), |sum, line| {
            sum.saturating_add(line.size)
        });
        Self {
            lines,
            total,
            budget,
        }
    }

    /// Whether the total is within the budget.
    #[must_use]
    pub fn fits(&self) -> bool {
        self.total <= self.budget
    }
}

impl fmt::Display for BudgetReport {
    /// A plain table: region, zoom, size; then the total against the budget.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let width = self
            .lines
            .iter()
            .map(|line| line.id.len())
            .max()
            .unwrap_or(6)
            .max(6);
        writeln!(formatter, "{:<width$}  zoom  {:>10}", "region", "size")?;
        for line in &self.lines {
            writeln!(
                formatter,
                "{:<width$}  {:>4}  {:>10}",
                line.id,
                line.max_zoom,
                line.size.to_string()
            )?;
        }
        writeln!(
            formatter,
            "{:<width$}        {:>10}",
            "total",
            self.total.to_string()
        )?;
        let verdict = if self.fits() { "fits" } else { "OVER BUDGET" };
        writeln!(
            formatter,
            "{:<width$}        {:>10}  {verdict}",
            "budget",
            self.budget.to_string()
        )
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    #[test]
    fn sizes_parse_in_decimal_and_binary_units() -> Result<(), SizeError> {
        assert_eq!(ByteSize::parse("60GB")?, ByteSize::new(60_000_000_000));
        assert_eq!(ByteSize::parse("3.2 MB")?, ByteSize::new(3_200_000));
        assert_eq!(ByteSize::parse("999 kB")?, ByteSize::new(999_000));
        assert_eq!(ByteSize::parse("1GiB")?, ByteSize::new(1 << 30));
        assert_eq!(ByteSize::parse("12")?, ByteSize::new(12));
        assert!(ByteSize::parse("GB").is_err());
        assert!(ByteSize::parse("1 PB").is_err());
        assert!(ByteSize::parse("99999999999999 TB").is_err());
        Ok(())
    }

    #[test]
    fn sizes_print_with_one_decimal() {
        assert_eq!(ByteSize::new(3_174_276).to_string(), "3.1 MB");
        assert_eq!(ByteSize::new(60_000_000_000).to_string(), "60.0 GB");
        assert_eq!(ByteSize::new(512).to_string(), "512 B");
    }

    #[test]
    fn budgets_refuse_totals_over_the_limit() {
        let line = |id: &str, gigabytes: u64| BudgetLine {
            id: id.into(),
            max_zoom: 15,
            size: ByteSize::new(gigabytes * 1_000_000_000),
        };
        let over = BudgetReport::new(
            vec![line("europe/germany", 45), line("europe/france", 30)],
            ByteSize::new(60_000_000_000),
        );
        assert!(!over.fits());
        assert_eq!(over.total, ByteSize::new(75_000_000_000));
        let table = over.to_string();
        assert!(table.contains("europe/germany"));
        assert!(table.contains("45.0 GB"));
        assert!(table.contains("OVER BUDGET"));
        let within = BudgetReport::new(vec![line("oceania", 20)], ByteSize::new(60_000_000_000));
        assert!(within.fits());
    }
}
