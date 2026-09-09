//! Stable wire interpretation for display-only multiline grouping.
//!
//! The persisted/query field predates modes and stores a custom regex verbatim.
//! Auto therefore uses a token that the legacy regex parser rejects, so no
//! formerly valid custom rule can silently change meaning. The configured
//! Run and Filter modes use a second reserved namespace naming one accepted
//! enrichment output column; every enrichment output name is an ASCII
//! identifier, so the plain `name` form round-trips without escaping and any
//! other text stays a legacy custom regex with its legacy meaning.

pub const AUTO_GROUPING_TOKEN: &str = "(?lvu:auto:v1)";
const AUTO_NAMESPACE: &str = "(?lvu:auto:";
pub const RUN_NAMESPACE: &str = "(?lvu:run:v1:column:";
pub const FILTER_NAMESPACE: &str = "(?lvu:filter:v1:column:";
const START_NAMESPACE: &str = "(?lvu:start:";

/// Longest enrichment output column a grouping rule may name: enrichment
/// output names are ASCII identifiers of at most 64 bytes, so this bound
/// accepts every producible column while rejecting pasted accidents.
pub const MAX_GROUPING_COLUMN_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GroupingSpec<'a> {
    Auto,
    Custom(&'a str),
    /// Consecutive records with equal values in this enrichment column form
    /// one run. Null values never equal, not even each other.
    Run {
        column: &'a str,
    },
    /// Every non-null value in this enrichment column opens an event; every
    /// record until the next non-null value continues it, whatever it looks
    /// like. Any produced value opens, including a produced `false`; only a
    /// produced null continues.
    Filter {
        column: &'a str,
    },
}

/// Build the persisted Run rule for an enrichment output column.
pub fn run_rule(column: &str) -> String {
    format!("{RUN_NAMESPACE}{column})")
}

/// Build the persisted Filter rule for an enrichment output column.
pub fn filter_rule(column: &str) -> String {
    format!("{FILTER_NAMESPACE}{column})")
}

/// Whether a name can appear in a Run/Filter rule. Plain enrichment outputs
/// are ASCII identifiers and command outputs add one dotted field; anything
/// else (including the `)` that closes the rule) is rejected with guidance
/// to pick the column again rather than stored as unparsable text.
pub fn is_grouping_column_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_GROUPING_COLUMN_BYTES
        && name.chars().all(|character| {
            character == '_' || character == '.' || character.is_ascii_alphanumeric()
        })
}

fn parse_column<'a>(namespace: &str, source: &'a str) -> Result<&'a str, String> {
    let Some(name) = source
        .strip_prefix(namespace)
        .and_then(|suffix| suffix.strip_suffix(')'))
    else {
        return Err(format!(
            "grouping rule {source:?} is malformed; pick the enrichment column again"
        ));
    };
    if !is_grouping_column_name(name) {
        return Err(
            "grouping rule names no enrichment column; pick the column that marks event starts"
                .to_owned(),
        );
    }
    Ok(name)
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
    if source.starts_with(RUN_NAMESPACE) {
        return Ok(GroupingSpec::Run {
            column: parse_column(RUN_NAMESPACE, source)?,
        });
    }
    if source.starts_with(FILTER_NAMESPACE) {
        return Ok(GroupingSpec::Filter {
            column: parse_column(FILTER_NAMESPACE, source)?,
        });
    }
    if source.starts_with(START_NAMESPACE)
        || source.starts_with("(?lvu:run:")
        || source.starts_with("(?lvu:filter:")
    {
        return Err(
            "unsupported grouping rule version; reopen Grouping and pick the column again"
                .to_owned(),
        );
    }
    Ok(GroupingSpec::Custom(source))
}

pub fn grouping_label(source: &str) -> Result<&str, String> {
    match parse_grouping(source)? {
        GroupingSpec::Auto => Ok("Auto"),
        GroupingSpec::Custom(_) => Ok("Custom"),
        GroupingSpec::Run { .. } => Ok("Run"),
        GroupingSpec::Filter { .. } => Ok("Filter"),
    }
}

/// The enrichment column of an applied Run/Filter rule, if the view is
/// grouped by a configured rule. Legacy rules yield `None`: their folding
/// and display behavior is unchanged.
pub fn configured_grouping_column(applied: &str) -> Option<String> {
    match parse_grouping(applied) {
        Ok(GroupingSpec::Run { column }) | Ok(GroupingSpec::Filter { column }) => {
            Some(column.to_owned())
        }
        _ => None,
    }
}

/// Whether the persisted rule is one of the legacy lexical modes (Auto or a
/// custom continuation regex) rather than a configured enrichment rule.
pub fn is_legacy(source: &str) -> bool {
    matches!(
        parse_grouping(source),
        Ok(GroupingSpec::Auto) | Ok(GroupingSpec::Custom(_))
    )
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

    #[test]
    fn run_and_filter_rules_round_trip_their_column() {
        for (rule, column) in [
            (run_rule("is_start"), "is_start"),
            (filter_rule("is_start"), "is_start"),
            (run_rule("svc_2"), "svc_2"),
        ] {
            let parsed = parse_grouping(&rule).expect("rule parses");
            match parsed {
                GroupingSpec::Run { column: found } | GroupingSpec::Filter { column: found } => {
                    assert_eq!(found, column)
                }
                other => panic!("unexpected spec {other:?}"),
            }
        }
        // Legacy text never becomes a configured rule, however similar.
        assert!(parse_grouping("(?lvu:run:v1:column:)").is_err());
        assert!(parse_grouping("(?lvu:filter:v1:column:has space)").is_err());
        assert!(parse_grouping("(?lvu:filter:v1:column:bad)paren)").is_err());
        assert_eq!(
            parse_grouping(&filter_rule("geo.city")),
            Ok(GroupingSpec::Filter { column: "geo.city" })
        );
        assert!(parse_grouping("(?lvu:start:v1:column:is_start)").is_err());
        assert!(parse_grouping("(?lvu:run:v2:column:is_start)").is_err());
        assert_eq!(
            parse_grouping("is_start"),
            Ok(GroupingSpec::Custom("is_start"))
        );
    }
}
