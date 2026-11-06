use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TermField {
    Any,
    Name,
    Content,
    Path,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Term {
    pub text: String,
    pub field: TermField,
    pub phrase: bool,
    pub is_regex: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileKind {
    File,
    Dir,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RangeOp {
    Lt(u64),
    Lte(u64),
    Gt(u64),
    Gte(u64),
    Between(u64, u64),
}

impl RangeOp {
    pub fn matches(&self, value: u64) -> bool {
        match *self {
            RangeOp::Lt(target) => value < target,
            RangeOp::Lte(target) => value <= target,
            RangeOp::Gt(target) => value > target,
            RangeOp::Gte(target) => value >= target,
            RangeOp::Between(low, high) => value >= low && value <= high,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Filter {
    Ext(String),
    Type(String),
    Modified(RangeOp),
    Size(RangeOp),
    Kind(FileKind),
    Lang(String),
    Name(String),
    Path(String),
    Not(Box<Filter>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub dir_id: u64,
    pub include: bool,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    Ranked,
    Exhaustive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveFilterDto {
    pub raw: String,
    pub filter_type: String,
    pub value: String,
    pub negated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Query {
    pub terms: Vec<Term>,
    pub filters: Vec<Filter>,
    pub scopes: Vec<Scope>,
    pub mode: Mode,
    pub raw: String,
    pub hints: Vec<String>,
    pub active_filters_dto: Vec<ActiveFilterDto>,
    /// Pairs of term indices that must appear within `distance` words of each
    /// other (the `near[x]` operator). Indices refer to `terms`.
    #[serde(default)]
    pub proximity: Vec<(usize, usize, u32)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_matches_covers_every_operator_boundary() {
        // Lt / Lte at the boundary.
        assert!(RangeOp::Lt(5).matches(4));
        assert!(!RangeOp::Lt(5).matches(5));
        assert!(!RangeOp::Lt(5).matches(6));
        assert!(RangeOp::Lte(5).matches(5));
        assert!(!RangeOp::Lte(5).matches(6));
        assert!(!RangeOp::Gt(5).matches(5));
        assert!(RangeOp::Gt(5).matches(6));
        assert!(RangeOp::Gte(5).matches(5));
        assert!(!RangeOp::Gte(5).matches(4));
        // Inclusive on both ends.
        assert!(RangeOp::Between(2, 9).matches(2));
        assert!(RangeOp::Between(2, 9).matches(9));
        assert!(!RangeOp::Between(2, 9).matches(1));
        assert!(!RangeOp::Between(2, 9).matches(10));
        // Zero and u64::MAX edges (file size 0 / enormous sizes must not
        // wrap or overflow).
        assert!(RangeOp::Gte(0).matches(0));
        assert!(RangeOp::Lt(u64::MAX).matches(u64::MAX - 1));
        assert!(!RangeOp::Between(0, u64::MAX - 1).matches(u64::MAX));
    }

    #[test]
    fn query_json_round_trip_preserves_everything() {
        let q = Query {
            terms: vec![Term {
                text: "parse".to_string(),
                field: TermField::Name,
                phrase: false,
                is_regex: true,
            }],
            filters: vec![
                Filter::Size(RangeOp::Gt(1024)),
                Filter::Not(Box::new(Filter::Kind(FileKind::Dir))),
            ],
            scopes: vec![Scope {
                dir_id: 7,
                include: false,
                path: PathBuf::from("/tmp/excluded"),
            }],
            mode: Mode::Exhaustive,
            raw: "re:parse size:>1kb".to_string(),
            hints: vec!["hint".to_string()],
            active_filters_dto: vec![ActiveFilterDto {
                raw: "size:>1kb".to_string(),
                filter_type: "size".to_string(),
                value: "1kb".to_string(),
                negated: false,
            }],
            proximity: vec![(0, 1, 5)],
        };

        let json = serde_json::to_string(&q).unwrap();
        let back: Query = serde_json::from_str(&json).unwrap();
        assert_eq!(back, q);
    }

    #[test]
    fn saved_query_without_proximity_field_still_loads() {
        // Queries saved by 1.0.11 have no `proximity` key; the
        // #[serde(default)] keeps those loading as empty.
        let legacy = serde_json::json!({
            "terms": [],
            "filters": [],
            "scopes": [],
            "mode": "Ranked",
            "raw": "mouse button",
            "hints": [],
            "active_filters_dto": []
        });
        let q: Query = serde_json::from_value(legacy).expect("legacy query must load");
        assert!(q.proximity.is_empty());
        assert_eq!(q.mode, Mode::Ranked);
        assert_eq!(q.raw, "mouse button");
    }

    #[test]
    fn term_field_file_kind_enums_round_trip_texts() {
        for (field, name) in [
            (TermField::Any, r#""Any""#),
            (TermField::Name, r#""Name""#),
            (TermField::Content, r#""Content""#),
            (TermField::Path, r#""Path""#),
        ] {
            let json = serde_json::to_string(&field).unwrap();
            assert_eq!(json, name);
            let back: TermField = serde_json::from_str(&json).unwrap();
            assert_eq!(back, field);
        }
        let kinds: Vec<FileKind> = serde_json::from_str(r#"["File", "Dir"]"#).unwrap();
        assert_eq!(kinds, vec![FileKind::File, FileKind::Dir]);
    }
}
