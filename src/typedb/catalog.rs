//! GH #18: the version catalog — which Openplanet typedbs are available
//! (installed install + recorded snapshots), and which one(s) a check
//! targets. Version LOGIC lives here; the checker only renders strings
//! this module hands it.

use std::fmt;

/// A parsed Openplanet version — the ordering key of every catalog entry.
/// Derived `Ord` on field order gives numeric ordering (`1.29.5 < 1.29.14`,
/// which lexical comparison inverts).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OpVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl OpVersion {
    /// Parse `"1.29.14"` or a full Core stamp `"1.29.14 (next, Public, …)"`.
    /// Exactly three numeric dot-separated components; anything else is
    /// `None`.
    pub fn parse(stamp: &str) -> Option<Self> {
        Self::parse_meta(stamp).map(|(v, _)| v)
    }

    /// [`Self::parse`] plus the parenthesized build metadata
    /// (`"next, Public, 6fd4200f"`), when the stamp carries it.
    pub fn parse_meta(stamp: &str) -> Option<(Self, Option<String>)> {
        let stamp = stamp.trim();
        let (head, meta) = match stamp.split_once(" (") {
            Some((h, rest)) => (h, Some(rest.strip_suffix(')')?.to_string())),
            None => (stamp, None),
        };
        let mut parts = head.split('.');
        let (a, b, c) = (parts.next()?, parts.next()?, parts.next()?);
        if parts.next().is_some() {
            return None;
        }
        let v = OpVersion {
            major: a.parse().ok()?,
            minor: b.parse().ok()?,
            patch: c.parse().ok()?,
        };
        Some((v, meta))
    }
}

impl fmt::Display for OpVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_version_parses_bare_and_full_stamps() {
        assert_eq!(
            OpVersion::parse("1.29.14"),
            Some(OpVersion {
                major: 1,
                minor: 29,
                patch: 14
            })
        );
        assert_eq!(
            OpVersion::parse("1.29.5 (next, Public, 1234ad9e)"),
            Some(OpVersion {
                major: 1,
                minor: 29,
                patch: 5
            })
        );
    }

    #[test]
    fn op_version_rejects_malformed() {
        for bad in ["", "1.29", "next", "1.29.x", "1.29.5.2", "1.29.5 next"] {
            assert_eq!(OpVersion::parse(bad), None, "should reject {bad:?}");
        }
    }

    #[test]
    fn op_version_orders_numerically_not_lexically() {
        let v5 = OpVersion::parse("1.29.5").unwrap();
        let v14 = OpVersion::parse("1.29.14").unwrap();
        assert!(v5 < v14, "1.29.5 < 1.29.14 (lexical order would invert this)");
        assert_eq!(v14.to_string(), "1.29.14");
    }

    #[test]
    fn op_version_meta_extracts_parenthetical() {
        let (v, meta) = OpVersion::parse_meta("1.29.14 (next, Public, 6fd4200f)").unwrap();
        assert_eq!(v.to_string(), "1.29.14");
        assert_eq!(meta.as_deref(), Some("next, Public, 6fd4200f"));
        let (_, none) = OpVersion::parse_meta("1.29.14").unwrap();
        assert_eq!(none, None);
    }
}
