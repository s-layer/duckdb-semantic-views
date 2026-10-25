//! The items of a query request: what `semantic_view()`'s `dimensions`,
//! `metrics` and `facts` lists and the `SEMANTIC_VIEW(...)` clauses name, and
//! the order their columns come out in.
//!
//! An item is a member reference or an expression with an optional output
//! name, as Snowflake's `SEMANTIC_VIEW` construct writes it:
//! `<member> [[AS] <alias>]`, or `<table>.*` for every public member of a
//! table. The member half goes to the expansion, an expression once it is
//! lowered to a member (`super::expr_items`); the alias and the column order
//! only rename and reorder the expansion's output (see [`OutputColumns`]), so
//! no emission path needs to know about them.

use crate::ident::parse_qualified_identifier_with_quoting;

/// One parsed item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The member reference as written (`o.region`, `"Region"`, `o.*`).
    pub member: String,
    /// The output column's name, unquoted (`r` for `AS r`, `My R` for
    /// `AS "My R"`), or `None` to keep the member's own name.
    pub alias: Option<String>,
}

/// Parse one item: `<member> [AS <alias>]`.
///
/// The member is the text before the last top-level `AS`, or all of it, and is
/// resolved as before: a member whose name has a space can still be written
/// without quotes (`Total Revenue`). So, unlike the `SEMANTIC_VIEW(...)`
/// construct, a list item names its column only with `AS`; the construct
/// writes its bare aliases with one. An expression is kept as written for
/// `super::expr_items` to lower.
///
/// # Errors
///
/// A message for a malformed item: an empty one, an alias that is not one
/// identifier, or an alias on a wildcard.
pub fn parse_item(raw: &str) -> Result<Item, String> {
    let text = raw.trim();
    if text.is_empty() {
        return Err("an item is empty".to_string());
    }
    let (member, alias) = match split_alias(text) {
        Some((member, alias)) => (member.trim_end(), Some(alias.trim_start())),
        None => (text, None),
    };
    let alias = alias
        .map(|a| {
            parse_alias(a).ok_or_else(|| {
                format!("'{a}' in '{text}' is not one identifier, so it cannot name the column")
            })
        })
        .transpose()?;
    if member.ends_with('*') {
        if alias.is_some() {
            return Err(format!(
                "'{text}' gives a wildcard an alias; a wildcard stands for several columns, \
                 so name each member you want to rename"
            ));
        }
        return Ok(Item {
            member: member.to_string(),
            alias,
        });
    }
    Ok(Item {
        member: member.to_string(),
        alias,
    })
}

/// Split `text` into the member and its alias, at the last top-level `AS`.
fn split_alias(text: &str) -> Option<(&str, &str)> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut last_as = None;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' => {
                i = crate::expr_tokens::skip_single_quoted(bytes, i);
                continue;
            }
            b'"' => {
                i = skip_quoted_identifier(bytes, i);
                continue;
            }
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth = depth.saturating_sub(1),
            b if b.is_ascii_whitespace() && depth == 0 => {
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                let word_end = word_end(bytes, i);
                // A quote ends the keyword as a space does (`AS"R"`, `AS'R'`).
                let followed_by_gap = bytes
                    .get(word_end)
                    .is_some_and(|b| b.is_ascii_whitespace() || matches!(b, b'"' | b'\''));
                if text[i..word_end].eq_ignore_ascii_case("as") && followed_by_gap {
                    last_as = Some((start, word_end));
                }
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    let (start, end) = last_as?;
    Some((&text[..start], &text[end..]))
}

/// Whether `text` is one dotted name (`a`, `o.region`, `"My T".x`) and nothing
/// else, by the scanner every member reference goes through.
fn is_name(text: &str) -> bool {
    let refs = crate::expr_tokens::scan_references(text);
    matches!(refs.as_slice(), [r] if r.start == 0 && r.end == text.len())
}

fn word_end(bytes: &[u8], start: usize) -> usize {
    let mut j = start;
    while j < bytes.len() && crate::util::is_ident_byte(bytes[j]) {
        j += 1;
    }
    j
}

fn skip_quoted_identifier(bytes: &[u8], i: usize) -> usize {
    let mut j = i + 1;
    while j < bytes.len() {
        if bytes[j] == b'"' {
            if bytes.get(j + 1) == Some(&b'"') {
                j += 2;
                continue;
            }
            return j + 1;
        }
        j += 1;
    }
    bytes.len()
}

/// An alias: one identifier, bare or double-quoted, or a string as `DuckDB`
/// allows (`AS 'x'`), returned unquoted.
fn parse_alias(text: &str) -> Option<String> {
    if text.starts_with('\'') {
        return match crate::util::extract_single_quoted_prefix(text) {
            Ok((name, consumed)) if consumed == text.len() && !name.is_empty() => Some(name),
            _ => None,
        };
    }
    if !is_name(text) {
        return None;
    }
    match parse_qualified_identifier_with_quoting(text) {
        Ok(mut parts) if parts.len() == 1 => parts.pop().map(|(name, _)| name),
        _ => None,
    }
}

/// The three item lists of a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Dimensions,
    Metrics,
    Facts,
}

impl Group {
    /// The order columns come out in when no order is given: the order the
    /// expansion emits them in.
    pub const DEFAULT_ORDER: [Self; 3] = [Self::Dimensions, Self::Metrics, Self::Facts];

    fn name(self) -> &'static str {
        match self {
            Self::Dimensions => "dimensions",
            Self::Metrics => "metrics",
            Self::Facts => "facts",
        }
    }
}

/// Parse `column_order := [...]`: the item lists in the order their columns
/// should come out. A list it leaves out follows in [`Group::DEFAULT_ORDER`].
///
/// # Errors
///
/// A message for an entry that names no list, or names one twice.
pub fn parse_column_order(entries: &[String]) -> Result<[Group; 3], String> {
    let mut order: Vec<Group> = Vec::with_capacity(3);
    for entry in entries {
        let group = Group::DEFAULT_ORDER
            .into_iter()
            .find(|g| g.name().eq_ignore_ascii_case(entry.trim()))
            .ok_or_else(|| {
                format!(
                    "column_order entry '{entry}' must be one of 'dimensions', 'metrics' \
                     or 'facts'"
                )
            })?;
        if order.contains(&group) {
            return Err(format!("column_order names '{}' twice", group.name()));
        }
        order.push(group);
    }
    for group in Group::DEFAULT_ORDER {
        if !order.contains(&group) {
            order.push(group);
        }
    }
    Ok([order[0], order[1], order[2]])
}

/// The aliases of one item list, once its members are expanded: the alias of
/// each expanded member, `None` where an item has none or the member came from
/// a wildcard.
///
/// `expanded` is the list `expand_wildcards` returned for these items' members.
/// It keeps one spelling of each member, whichever item named it first, and
/// matches names as `DuckDB` does (case-insensitively, quoted or not), so an
/// alias finds its member again by the same key, `ident::normalize_ident_part`.
///
/// # Errors
///
/// A message when one member is written twice with different aliases, since
/// the expansion would keep only one of them.
pub fn aliases_for(items: &[Item], expanded: &[String]) -> Result<Vec<Option<String>>, String> {
    let key = |member: &str| crate::ident::normalize_ident_part(member);
    let keys: Vec<String> = items.iter().map(|item| key(&item.member)).collect();
    for (i, a) in items.iter().enumerate() {
        if let Some(b) = items[i + 1..]
            .iter()
            .zip(&keys[i + 1..])
            .find_map(|(b, k)| (*k == keys[i]).then_some(b))
        {
            if a.alias != b.alias {
                return Err(format!(
                    "'{}' is requested twice with different names; request each member once",
                    a.member
                ));
            }
        }
    }
    Ok(expanded
        .iter()
        .map(|m| {
            let m = key(m);
            items
                .iter()
                .zip(&keys)
                .find(|(_, k)| **k == m)
                .and_then(|(item, _)| item.alias.clone())
        })
        .collect())
}

/// A request's three item lists and its column order, parsed.
#[derive(Debug, Clone)]
pub struct RequestItems {
    pub dimensions: Vec<Item>,
    pub metrics: Vec<Item>,
    pub facts: Vec<Item>,
    pub order: [Group; 3],
}

impl RequestItems {
    /// Parse the raw lists and `column_order` entries.
    ///
    /// # Errors
    ///
    /// The first malformed item's message, naming its list.
    pub fn parse(
        dimensions: &[String],
        metrics: &[String],
        facts: &[String],
        column_order: &[String],
    ) -> Result<Self, String> {
        let list = |group: Group, raw: &[String]| -> Result<Vec<Item>, String> {
            raw.iter()
                .map(|r| parse_item(r).map_err(|e| format!("{}: {e}", group.name())))
                .collect()
        };
        Ok(Self {
            dimensions: list(Group::Dimensions, dimensions)?,
            metrics: list(Group::Metrics, metrics)?,
            facts: list(Group::Facts, facts)?,
            order: parse_column_order(column_order)?,
        })
    }

    /// The member references of one list, as written, for wildcard expansion.
    #[must_use]
    pub fn members(items: &[Item]) -> Vec<String> {
        items.iter().map(|i| i.member.clone()).collect()
    }
}

/// How the expansion's output columns become the query's: which expansion
/// column each output column takes, and its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputColumns {
    /// `(index into the expansion's columns, output name)`, in output order.
    pub columns: Vec<(usize, String)>,
}

impl OutputColumns {
    /// Lay out the output of an expansion whose columns are `names`: the
    /// dimensions, then the metrics, then the facts, each in request order,
    /// as the expansion emits them. `aliases` holds each list's aliases
    /// ([`aliases_for`]) in [`Group::DEFAULT_ORDER`].
    ///
    /// # Errors
    ///
    /// A message when two output columns would have the same name, or when
    /// the expansion's column count is not the requested one (an internal
    /// error).
    pub fn new(
        names: &[String],
        aliases: [&[Option<String>]; 3],
        order: [Group; 3],
    ) -> Result<Self, String> {
        let total: usize = aliases.iter().map(|a| a.len()).sum();
        if total != names.len() {
            return Err(format!(
                "internal error: the query has {} columns for {total} requested members \
                 (please report this bug)",
                names.len()
            ));
        }
        let start_of = |group: Group| -> usize {
            Group::DEFAULT_ORDER
                .iter()
                .take_while(|g| **g != group)
                .map(|g| aliases[*g as usize].len())
                .sum()
        };
        let mut columns = Vec::with_capacity(total);
        for group in order {
            let start = start_of(group);
            for (k, alias) in aliases[group as usize].iter().enumerate() {
                let name = alias.clone().unwrap_or_else(|| names[start + k].clone());
                if let Some((_, other)) = columns
                    .iter()
                    .find(|(_, other): &&(usize, String)| other.eq_ignore_ascii_case(&name))
                {
                    return Err(format!(
                        "two columns of the query are named '{other}'; give one of them \
                         another name with AS"
                    ));
                }
                columns.push((start + k, name));
            }
        }
        Ok(Self { columns })
    }

    /// Whether the output is the expansion's, unchanged.
    #[must_use]
    pub fn is_identity(&self, names: &[String]) -> bool {
        self.columns.len() == names.len()
            && self
                .columns
                .iter()
                .enumerate()
                .all(|(i, (src, name))| *src == i && *name == names[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(member: &str, alias: Option<&str>) -> Item {
        Item {
            member: member.to_string(),
            alias: alias.map(str::to_string),
        }
    }

    #[test]
    fn a_name_alone_is_an_item_without_an_alias() {
        assert_eq!(parse_item(" o.region ").unwrap(), item("o.region", None));
        assert_eq!(parse_item("\"Region\"").unwrap(), item("\"Region\"", None));
    }

    #[test]
    fn as_names_the_column() {
        assert_eq!(
            parse_item("o.region AS r").unwrap(),
            item("o.region", Some("r"))
        );
        assert_eq!(
            parse_item("o.region as \"My R\"").unwrap(),
            item("o.region", Some("My R"))
        );
        assert_eq!(
            parse_item("o.region\tAS\nr").unwrap(),
            item("o.region", Some("r"))
        );
    }

    /// `AS` needs no space before a quoted alias, as in DuckDB.
    #[test]
    fn as_directly_before_a_quoted_alias_names_the_column() {
        assert_eq!(
            parse_item("o.region AS\"R\"").unwrap(),
            item("o.region", Some("R"))
        );
    }

    #[test]
    fn as_directly_before_a_string_alias_names_the_column() {
        assert_eq!(
            parse_item("o.region AS'R'").unwrap(),
            item("o.region", Some("R"))
        );
    }

    /// A string names a column too, as in DuckDB (`SELECT 1 AS 'x'`).
    #[test]
    fn a_string_alias_names_the_column() {
        assert_eq!(
            parse_item("o.region AS 'R'").unwrap(),
            item("o.region", Some("R"))
        );
        assert_eq!(
            parse_item("o.region AS 'it''s'").unwrap(),
            item("o.region", Some("it's"))
        );
    }

    #[test]
    fn an_empty_or_unclosed_string_alias_is_refused() {
        for raw in ["o.region AS ''", "o.region AS 'ab", "o.region AS 'ab''"] {
            let err = parse_item(raw).unwrap_err();
            assert!(err.contains("not one identifier"), "{raw}: {err}");
        }
    }

    /// A member whose name has a space can be written without quotes, as it
    /// could before items took an alias (`cr20260716c_reference_scanners.test`).
    #[test]
    fn a_member_name_with_a_space_is_one_member() {
        assert_eq!(
            parse_item("Total Revenue").unwrap(),
            item("Total Revenue", None)
        );
        assert_eq!(
            parse_item("Total Revenue AS t").unwrap(),
            item("Total Revenue", Some("t"))
        );
    }

    /// Without `AS` the whole text is the member, so a name with a space
    /// stays one name (the construct adds the `AS` to a bare alias).
    #[test]
    fn an_alias_without_as_is_part_of_the_member() {
        assert_eq!(parse_item("o.region r").unwrap(), item("o.region r", None));
    }

    /// A member named `as` is still a member: the word is an alias keyword only
    /// between a member and an alias.
    #[test]
    fn a_member_named_as_is_a_member() {
        assert_eq!(parse_item("o.as").unwrap(), item("o.as", None));
        assert_eq!(
            parse_item("\"as\" AS a").unwrap(),
            item("\"as\"", Some("a"))
        );
    }

    #[test]
    fn an_alias_is_one_identifier() {
        let err = parse_item("o.region AS r s").unwrap_err();
        assert!(err.contains("not one identifier"), "{err}");
        let err = parse_item("o.region AS a.b").unwrap_err();
        assert!(err.contains("not one identifier"), "{err}");
    }

    #[test]
    fn a_wildcard_has_no_alias() {
        assert_eq!(parse_item("o.*").unwrap(), item("o.*", None));
        let err = parse_item("o.* AS x").unwrap_err();
        assert!(err.contains("wildcard"), "{err}");
    }

    /// An expression is kept as written, for `expr_items` to lower.
    #[test]
    fn an_expression_is_kept_as_written() {
        assert_eq!(
            parse_item("upper(o.region) AS r").unwrap(),
            item("upper(o.region)", Some("r"))
        );
    }

    #[test]
    fn an_empty_item_is_refused() {
        assert!(parse_item("  ").is_err());
    }

    #[test]
    fn column_order_fills_in_the_default_for_the_rest() {
        let order = |v: &[&str]| {
            parse_column_order(&v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>())
        };
        assert_eq!(order(&[]).unwrap(), Group::DEFAULT_ORDER);
        assert_eq!(
            order(&["METRICS"]).unwrap(),
            [Group::Metrics, Group::Dimensions, Group::Facts]
        );
        assert_eq!(
            order(&["facts", "dimensions"]).unwrap(),
            [Group::Facts, Group::Dimensions, Group::Metrics]
        );
        assert!(order(&["metrics", "metrics"])
            .unwrap_err()
            .contains("twice"));
        assert!(order(&["where"]).unwrap_err().contains("must be one of"));
    }

    #[test]
    fn aliases_follow_their_members_through_wildcard_expansion() {
        let items = [item("o.*", None), item("c.name", Some("customer"))];
        let expanded = ["o.region".to_string(), "c.name".to_string()];
        assert_eq!(
            aliases_for(&items, &expanded).unwrap(),
            vec![None, Some("customer".to_string())]
        );
    }

    #[test]
    fn a_member_written_twice_needs_one_name() {
        let items = [item("o.region", Some("a")), item("o.region", Some("b"))];
        assert!(aliases_for(&items, &[]).unwrap_err().contains("twice"));
        let items = [item("o.region", None), item("o.region", None)];
        assert!(
            aliases_for(&items, &[]).is_ok(),
            "the expansion reports this one"
        );
    }

    /// `expand_wildcards` keeps the spelling of whichever item named a member
    /// first, matching names as DuckDB does (case-insensitively, quoted or
    /// not), so an alias finds its member the same way.
    #[test]
    fn an_alias_finds_its_member_in_another_case_after_a_wildcard() {
        let items = [item("c.*", None), item("REGION", Some("r"))];
        assert_eq!(
            aliases_for(&items, &["region".to_string()]).unwrap(),
            vec![Some("r".to_string())]
        );
    }

    #[test]
    fn an_alias_finds_its_quoted_member_after_a_wildcard() {
        let items = [item("c.*", None), item("\"region\"", Some("r"))];
        assert_eq!(
            aliases_for(&items, &["region".to_string()]).unwrap(),
            vec![Some("r".to_string())]
        );
    }

    #[test]
    fn an_alias_finds_a_member_declared_in_another_case() {
        let items = [item("c.*", None), item("region", Some("r"))];
        assert_eq!(
            aliases_for(&items, &["Region".to_string()]).unwrap(),
            vec![Some("r".to_string())]
        );
    }

    #[test]
    fn a_member_written_twice_in_another_case_needs_one_name() {
        let items = [item("region", Some("a")), item("REGION", Some("b"))];
        assert!(aliases_for(&items, &[]).unwrap_err().contains("twice"));
    }

    #[test]
    fn a_member_written_twice_quoted_and_bare_needs_one_name() {
        let items = [item("region", Some("a")), item("\"region\"", Some("b"))];
        assert!(aliases_for(&items, &[]).unwrap_err().contains("twice"));
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn the_default_layout_is_the_expansion_unchanged() {
        let n = names(&["region", "revenue"]);
        let out = OutputColumns::new(&n, [&[None], &[None], &[]], Group::DEFAULT_ORDER).unwrap();
        assert!(out.is_identity(&n));
    }

    #[test]
    fn metrics_first_moves_the_metric_columns_ahead() {
        let n = names(&["region", "year", "revenue"]);
        let order = [Group::Metrics, Group::Dimensions, Group::Facts];
        let out = OutputColumns::new(&n, [&[None, None], &[None], &[]], order).unwrap();
        assert_eq!(
            out.columns,
            vec![
                (2, "revenue".to_string()),
                (0, "region".to_string()),
                (1, "year".to_string())
            ]
        );
        assert!(!out.is_identity(&n));
    }

    #[test]
    fn an_alias_renames_its_column() {
        let n = names(&["region", "revenue"]);
        let out = OutputColumns::new(
            &n,
            [&[Some("r".to_string())], &[None], &[]],
            Group::DEFAULT_ORDER,
        )
        .unwrap();
        assert_eq!(
            out.columns,
            vec![(0, "r".to_string()), (1, "revenue".to_string())]
        );
    }

    #[test]
    fn two_columns_with_one_name_are_refused() {
        let n = names(&["region", "revenue"]);
        let err = OutputColumns::new(
            &n,
            [&[Some("REVENUE".to_string())], &[None], &[]],
            Group::DEFAULT_ORDER,
        )
        .unwrap_err();
        assert!(err.contains("two columns"), "{err}");
    }

    #[test]
    fn a_column_count_that_does_not_match_is_an_internal_error() {
        let n = names(&["region"]);
        let err =
            OutputColumns::new(&n, [&[None], &[None], &[]], Group::DEFAULT_ORDER).unwrap_err();
        assert!(err.contains("internal error"), "{err}");
    }
}
