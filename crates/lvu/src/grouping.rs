//! Stable wire interpretation for display-only multiline grouping.
//!
//! The persisted/query field predates modes and stores a custom regex verbatim.
//! Auto therefore uses a token that the legacy regex parser rejects, so no
//! formerly valid custom rule can silently change meaning.

pub const AUTO_GROUPING_TOKEN: &str = "(?lvu:auto:v1)";
const AUTO_NAMESPACE: &str = "(?lvu:auto:";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GroupingSpec<'a> {
    Auto,
    Custom(&'a str),
}

pub fn parse_grouping(source: &str) -> Result<GroupingSpec<'_>, String> {
    if source == AUTO_GROUPING_TOKEN {
        return Ok(GroupingSpec::Auto);
    }
    if source.starts_with(AUTO_NAMESPACE) {
        let version = source
            .strip_prefix(AUTO_NAMESPACE)
            .and_then(|suffix| suffix.strip_suffix(')'))
            .unwrap_or("unknown");
        return Err(format!(
            "unsupported automatic grouping version {version}; reopen Grouping and choose Auto"
        ));
    }
    Ok(GroupingSpec::Custom(source))
}

pub fn grouping_label(source: &str) -> Result<&str, String> {
    match parse_grouping(source)? {
        GroupingSpec::Auto => Ok("Auto"),
        GroupingSpec::Custom(_) => Ok("Custom"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_namespace_is_versioned_and_custom_text_is_untouched() {
        assert_eq!(parse_grouping(AUTO_GROUPING_TOKEN), Ok(GroupingSpec::Auto));
        let unknown = parse_grouping("(?lvu:auto:v2)").unwrap_err();
        assert!(unknown.contains("version v2"));
        assert!(!unknown.contains("(?lvu:auto:"));
        for custom in ["", r"^\s+", " ", "\t", "auto", r"(?P<auto>.*)"] {
            assert_eq!(parse_grouping(custom), Ok(GroupingSpec::Custom(custom)));
        }
    }
}
