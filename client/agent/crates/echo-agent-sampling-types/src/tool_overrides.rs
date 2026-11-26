//! The `toolOverrides` wire contract for backend-hosted `x_search` / `web_search`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A content-date window for `x_search`: `fromDate` inclusive, `toDate` exclusive at 00:00 UTC of
/// the named day. Both are canonical `YYYY-MM-DD` (camelCase on the wire), validated in
/// [`SearchDateBound::new`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", try_from = "SearchDateBoundWire")]
#[schemars(deny_unknown_fields)]
pub struct SearchDateBound {
    #[serde(skip_serializing_if = "Option::is_none")]
    from_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    to_date: Option<String>,
}

impl SearchDateBound {
    pub fn new(
        from_date: Option<String>,
        to_date: Option<String>,
    ) -> Result<Self, SearchDateBoundError> {
        validate_bound_pair(from_date.as_deref(), to_date.as_deref())?;
        Ok(Self { from_date, to_date })
    }

    pub fn from_date(&self) -> Option<&str> {
        self.from_date.as_deref()
    }

    pub fn to_date(&self) -> Option<&str> {
        self.to_date.as_deref()
    }

    pub fn is_empty(&self) -> bool {
        let SearchDateBound { from_date, to_date } = self;
        from_date.is_none() && to_date.is_none()
    }

    /// Deserialize and validate (via `try_from`), returning the structured `serde_json::Error`.
    pub fn parse(value: &serde_json::Value) -> Result<Self, serde_json::Error> {
        Self::deserialize(value)
    }
}

// Deserialize routes through `SearchDateBound::new` via `try_from`, so every ingress is validated.
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SearchDateBoundWire {
    #[serde(default)]
    from_date: Option<String>,
    #[serde(default)]
    to_date: Option<String>,
}

impl TryFrom<SearchDateBoundWire> for SearchDateBound {
    type Error = SearchDateBoundError;

    fn try_from(wire: SearchDateBoundWire) -> Result<Self, Self::Error> {
        Self::new(wire.from_date, wire.to_date)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SearchDateBoundError {
    #[error("{field} {value:?} is not a valid YYYY-MM-DD date")]
    InvalidDate { field: &'static str, value: String },
    #[error("{field} {value:?} is not zero-padded YYYY-MM-DD")]
    NotZeroPadded { field: &'static str, value: String },
    #[error("fromDate must be on or before toDate (got {from} > {to})")]
    InvertedWindow { from: String, to: String },
}

fn validate_bound_date(
    field: &'static str,
    s: &str,
) -> Result<chrono::NaiveDate, SearchDateBoundError> {
    let parsed = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| {
        SearchDateBoundError::InvalidDate {
            field,
            value: s.to_owned(),
        }
    })?;
    // chrono's proleptic calendar admits year 0; reject it so the minimum year is 1.
    if chrono::Datelike::year(&parsed) < 1 {
        return Err(SearchDateBoundError::InvalidDate {
            field,
            value: s.to_owned(),
        });
    }
    if s.len() != 10 || parsed.format("%Y-%m-%d").to_string() != s {
        return Err(SearchDateBoundError::NotZeroPadded {
            field,
            value: s.to_owned(),
        });
    }
    Ok(parsed)
}

fn validate_bound_pair(from: Option<&str>, to: Option<&str>) -> Result<(), SearchDateBoundError> {
    let from_date = from
        .map(|s| validate_bound_date("fromDate", s))
        .transpose()?;
    let to_date = to.map(|s| validate_bound_date("toDate", s)).transpose()?;
    if let (Some(from), Some(to), Some(from_date), Some(to_date)) = (from, to, from_date, to_date)
        && from_date > to_date
    {
        return Err(SearchDateBoundError::InvertedWindow {
            from: from.to_owned(),
            to: to.to_owned(),
        });
    }
    Ok(())
}

/// `x_search` override: the content-date [`SearchDateBound`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct XSearchOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_bound: Option<SearchDateBound>,
}

impl XSearchOptions {
    pub fn is_empty(&self) -> bool {
        let XSearchOptions { date_bound } = self;
        date_bound.as_ref().is_none_or(SearchDateBound::is_empty)
    }

    pub fn to_tool_entry(&self) -> serde_json::Value {
        #[derive(Serialize)]
        #[serde(rename_all = "snake_case")]
        struct XSearchToolEntry<'a> {
            r#type: &'static str,
            #[serde(skip_serializing_if = "Option::is_none")]
            from_date: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            to_date: Option<&'a str>,
        }
        // Destructure so a new field forces a compile error rather than a dropped wire field.
        let XSearchOptions { date_bound } = self;
        let bound = date_bound.as_ref();
        serde_json::to_value(XSearchToolEntry {
            r#type: "x_search",
            from_date: bound.and_then(SearchDateBound::from_date),
            to_date: bound.and_then(SearchDateBound::to_date),
        })
        .expect("XSearchToolEntry is always serializable")
    }
}

/// The public web-search API caps each domain list at 5 entries.
pub const MAX_WEB_SEARCH_DOMAINS: usize = 5;

/// `web_search` override: a domain allowlist and/or blocklist (empty or absent is unbounded).
///
/// The two lists are mutually exclusive and each is capped at [`MAX_WEB_SEARCH_DOMAINS`];
/// both rules are enforced by [`WebSearchOptions::validate`] on every deserialize ingress.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", try_from = "WebSearchOptionsWire")]
#[schemars(deny_unknown_fields)]
pub struct WebSearchOptions {
    /// Restrict search results to these domains. Cannot be combined with `excluded_domains`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_domains: Option<Vec<String>>,
    /// Exclude these domains from search results. Cannot be combined with `allowed_domains`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excluded_domains: Option<Vec<String>>,
}

impl WebSearchOptions {
    pub fn is_empty(&self) -> bool {
        let WebSearchOptions {
            allowed_domains,
            excluded_domains,
        } = self;
        allowed_domains.as_ref().is_none_or(|d| d.is_empty())
            && excluded_domains.as_ref().is_none_or(|d| d.is_empty())
    }

    /// Reject a config that violates the public web-search API's rules: the allow/block lists
    /// are mutually exclusive, and each is capped at [`MAX_WEB_SEARCH_DOMAINS`].
    pub fn validate(&self) -> Result<(), WebSearchOptionsError> {
        let WebSearchOptions {
            allowed_domains,
            excluded_domains,
        } = self;
        let has_allowed = allowed_domains.as_ref().is_some_and(|d| !d.is_empty());
        let has_excluded = excluded_domains.as_ref().is_some_and(|d| !d.is_empty());
        if has_allowed && has_excluded {
            return Err(WebSearchOptionsError::BothAllowedAndExcluded);
        }
        for (field, list) in [
            ("allowed_domains", allowed_domains),
            ("excluded_domains", excluded_domains),
        ] {
            if let Some(domains) = list
                && domains.len() > MAX_WEB_SEARCH_DOMAINS
            {
                return Err(WebSearchOptionsError::TooManyDomains {
                    field,
                    count: domains.len(),
                });
            }
        }
        Ok(())
    }

    /// The raw `web_search` tool entry spliced into the Responses `tools` array. An empty
    /// allowlist/blocklist is unbounded, so it emits no `filters` (matching a bare tool).
    pub fn to_tool_entry(&self) -> serde_json::Value {
        #[derive(Serialize)]
        struct WebSearchToolFilters<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            allowed_domains: Option<&'a [String]>,
            #[serde(skip_serializing_if = "Option::is_none")]
            excluded_domains: Option<&'a [String]>,
        }
        #[derive(Serialize)]
        #[serde(rename_all = "snake_case")]
        struct WebSearchToolEntry<'a> {
            r#type: &'static str,
            #[serde(skip_serializing_if = "Option::is_none")]
            filters: Option<WebSearchToolFilters<'a>>,
        }
        // Destructure so a new field forces a compile error rather than a dropped wire field.
        let WebSearchOptions {
            allowed_domains,
            excluded_domains,
        } = self;
        let allowed = allowed_domains.as_deref().filter(|d| !d.is_empty());
        let excluded = excluded_domains.as_deref().filter(|d| !d.is_empty());
        let filters = (allowed.is_some() || excluded.is_some()).then_some(WebSearchToolFilters {
            allowed_domains: allowed,
            excluded_domains: excluded,
        });
        #[expect(clippy::expect_used)]
        serde_json::to_value(WebSearchToolEntry {
            r#type: "web_search",
            filters,
        })
        .expect("WebSearchToolEntry holds only &str and String lists, so serialization cannot fail")
    }
}

/// Validation error for [`WebSearchOptions`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WebSearchOptionsError {
    #[error("web_search cannot set both allowed_domains and excluded_domains")]
    BothAllowedAndExcluded,
    #[error(
        "web_search {field} has {count} domains; the web-search API allows at most {MAX_WEB_SEARCH_DOMAINS}"
    )]
    TooManyDomains { field: &'static str, count: usize },
}

/// Deserialize target for [`WebSearchOptions`]: every ingress (agent frontmatter, per-turn
/// `ToolOverridesUpdate`) routes through `try_from`, so both-set and over-limit configs fail
/// to parse instead of surfacing as a request-level API error later.
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WebSearchOptionsWire {
    #[serde(default)]
    allowed_domains: Option<Vec<String>>,
    #[serde(default)]
    excluded_domains: Option<Vec<String>>,
}

impl TryFrom<WebSearchOptionsWire> for WebSearchOptions {
    type Error = WebSearchOptionsError;

    fn try_from(wire: WebSearchOptionsWire) -> Result<Self, Self::Error> {
        let opts = WebSearchOptions {
            allowed_domains: wire.allowed_domains,
            excluded_domains: wire.excluded_domains,
        };
        opts.validate()?;
        Ok(opts)
    }
}

/// The resolved per-tool overrides, and the shape echoed back for attestation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct ToolOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x_search: Option<XSearchOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search: Option<WebSearchOptions>,
}

impl ToolOverrides {
    pub fn parse(value: &serde_json::Value) -> Result<Self, serde_json::Error> {
        Self::deserialize(value)
    }

    pub fn is_empty(&self) -> bool {
        let ToolOverrides {
            x_search,
            web_search,
        } = self;
        x_search.as_ref().is_none_or(XSearchOptions::is_empty)
            && web_search.as_ref().is_none_or(WebSearchOptions::is_empty)
    }
}

/// A tri-state per-turn patch field: absent leaves, `null` clears, a value sets. Pair with
/// [`crate::serde_helpers::double_option`].
pub type ClearableField<T> = Option<Option<T>>;

/// The ingress-only per-turn patch: each tool is a tri-state [`ClearableField`] applied by
/// [`Self::apply`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct ToolOverridesUpdate {
    #[serde(default, deserialize_with = "crate::serde_helpers::double_option")]
    pub x_search: ClearableField<XSearchOptions>,
    #[serde(default, deserialize_with = "crate::serde_helpers::double_option")]
    pub web_search: ClearableField<WebSearchOptions>,
}

impl ToolOverridesUpdate {
    pub fn parse(value: &serde_json::Value) -> Result<Self, serde_json::Error> {
        Self::deserialize(value)
    }

    /// Fold a per-turn update onto `base`: an object sets, `null` clears, absent or empty leaves.
    pub fn apply(self, base: Option<ToolOverrides>) -> Option<ToolOverrides> {
        let ToolOverridesUpdate {
            x_search,
            web_search,
        } = self;
        let base = base.unwrap_or_default();
        let next = ToolOverrides {
            x_search: merge_field(x_search, base.x_search, XSearchOptions::is_empty),
            web_search: merge_field(web_search, base.web_search, WebSearchOptions::is_empty),
        };
        (!next.is_empty()).then_some(next)
    }
}

/// Normalize an override option: empty carries no constraint, so it reads as absent. The one home
/// for this rule, shared by `merge_field` and `apply_tool_overrides`.
pub(crate) fn drop_empty<T>(opt: Option<T>, is_empty: impl Fn(&T) -> bool) -> Option<T> {
    opt.filter(|value| !is_empty(value))
}

/// Fold one tri-state field onto its base: an object sets, `null` clears, absent or empty leaves.
fn merge_field<T>(
    update: ClearableField<T>,
    base: Option<T>,
    is_empty: impl Fn(&T) -> bool,
) -> Option<T> {
    match update {
        // An object sets, but an empty one carries no instruction, so it falls back to the base.
        Some(Some(value)) => drop_empty(Some(value), is_empty).or(base),
        Some(None) => None,
        None => base,
    }
}

#[cfg(test)]
mod web_search_options_tests {
    use super::*;
    use serde_json::json;

    fn opts(allowed: Option<&[&str]>, excluded: Option<&[&str]>) -> WebSearchOptions {
        let to_vec = |s: &[&str]| s.iter().map(|d| d.to_string()).collect::<Vec<_>>();
        WebSearchOptions {
            allowed_domains: allowed.map(to_vec),
            excluded_domains: excluded.map(to_vec),
        }
    }

    #[test]
    fn is_empty_covers_both_fields() {
        assert!(WebSearchOptions::default().is_empty());
        assert!(opts(Some(&[]), Some(&[])).is_empty());
        assert!(!opts(Some(&["a.com"]), None).is_empty());
        assert!(!opts(None, Some(&["b.com"])).is_empty());
    }

    #[test]
    fn validate_rejects_both_non_empty() {
        assert!(opts(Some(&["a.com"]), Some(&["b.com"])).validate().is_err());
        // Either alone (or an empty counterpart) is fine.
        assert!(opts(Some(&["a.com"]), None).validate().is_ok());
        assert!(opts(None, Some(&["b.com"])).validate().is_ok());
        assert!(opts(Some(&["a.com"]), Some(&[])).validate().is_ok());
    }

    #[test]
    fn validate_rejects_over_the_max() {
        let six = ["1", "2", "3", "4", "5", "6"];
        assert_eq!(
            opts(Some(&six), None).validate(),
            Err(WebSearchOptionsError::TooManyDomains {
                field: "allowed_domains",
                count: 6
            })
        );
        assert_eq!(
            opts(None, Some(&six)).validate(),
            Err(WebSearchOptionsError::TooManyDomains {
                field: "excluded_domains",
                count: 6
            })
        );
        // Exactly the max is allowed.
        assert!(
            opts(Some(&["1", "2", "3", "4", "5"]), None)
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn deserialize_hard_errors_on_both_set() {
        let json = serde_json::json!({
            "allowedDomains": ["a.com"],
            "excludedDomains": ["b.com"]
        });
        assert!(WebSearchOptions::deserialize(&json).is_err());
    }

    #[test]
    fn deserialize_hard_errors_over_the_max() {
        let json = serde_json::json!({
            "allowedDomains": ["1", "2", "3", "4", "5", "6"]
        });
        assert!(WebSearchOptions::deserialize(&json).is_err());
    }

    #[test]
    fn deserialize_accepts_valid_config() {
        let json = serde_json::json!({ "excludedDomains": ["reddit.com"] });
        let parsed = WebSearchOptions::deserialize(&json).unwrap();
        assert_eq!(
            parsed.excluded_domains,
            Some(vec!["reddit.com".to_string()])
        );
        assert!(parsed.allowed_domains.is_none());
    }

    #[test]
    fn to_tool_entry_no_filters_matches_bare_tool() {
        assert_eq!(
            WebSearchOptions::default().to_tool_entry(),
            json!({ "type": "web_search" })
        );
        // An explicitly empty list is unbounded, so it also emits no filters.
        assert_eq!(
            opts(Some(&[]), None).to_tool_entry(),
            json!({ "type": "web_search" })
        );
    }

    #[test]
    fn to_tool_entry_allowlist_only() {
        assert_eq!(
            opts(Some(&["docs.echo-agent.invalid", "arxiv.org"]), None).to_tool_entry(),
            json!({
                "type": "web_search",
                "filters": { "allowed_domains": ["docs.echo-agent.invalid", "arxiv.org"] }
            })
        );
    }

    #[test]
    fn to_tool_entry_blocklist_only() {
        assert_eq!(
            opts(None, Some(&["reddit.com"])).to_tool_entry(),
            json!({
                "type": "web_search",
                "filters": { "excluded_domains": ["reddit.com"] }
            })
        );
    }
}
