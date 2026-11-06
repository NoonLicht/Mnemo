use crate::indexer::walker::FileEntry;
use crate::query::ast::{
    ActiveFilterDto, FileKind, Filter, Mode, Query, RangeOp, Scope, Term, TermField,
};
use crate::query::lexer::{Token, tokenize};
use crate::query::scope_trie::{DirectoryTrie, TrieMatchResult};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

pub fn parse_query(
    raw: &str,
    entries: &[FileEntry],
    path_index: &HashMap<PathBuf, usize>,
    trie: Option<&DirectoryTrie>,
    generation: u64,
) -> Query {
    let now_sec = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let tokens = tokenize(raw);
    let mut terms = Vec::new();
    let mut filters = Vec::new();
    let mut scopes = Vec::new();
    let mut hints = Vec::new();
    let mut active_filters_dto = Vec::new();
    let mut mode = Mode::Ranked;
    let mut proximity: Vec<(usize, usize, u32)> = Vec::new();
    let mut pending_near: Option<u32> = None;

    macro_rules! push_term {
        ($term:expr) => {
            terms.push($term);
            if let Some(d) = pending_near.take() {
                if terms.len() >= 2 {
                    proximity.push((terms.len() - 2, terms.len() - 1, d));
                }
            }
        };
    }

    let mut is_first_token = true;

    for token in tokens {
        // `near[x]` is an infix proximity operator, never a search term.
        if let Token::Word(text) = &token
            && let Some(distance) = parse_near_token(text)
        {
            let clamped = distance.min(NEAR_MAX_DISTANCE);
            if clamped != distance {
                hints.push(format!("near[{distance}] clamped to {NEAR_MAX_DISTANCE}"));
            }
            pending_near = Some(clamped);
            is_first_token = false;
            continue;
        }
        match token {
            Token::Word(ref text)
                if is_first_token && is_directory_like(text) && trie.is_some() =>
            {
                // Leading directory-looking token (e.g. src/ or ~/Projects/)
                if let Some(dir_trie) = trie {
                    match dir_trie.resolve(text, entries, path_index) {
                        TrieMatchResult::Single(resolved) => {
                            scopes.push(Scope {
                                dir_id: resolved.dir_idx as u64,
                                include: true,
                                path: resolved.path.clone(),
                            });
                            active_filters_dto.push(ActiveFilterDto {
                                raw: text.clone(),
                                filter_type: "in".to_string(),
                                value: resolved.path.to_string_lossy().to_string(),
                                negated: false,
                            });
                        }
                        TrieMatchResult::Ambiguous(candidates) => {
                            push_term!(Term {
                                text: text.clone(),
                                field: TermField::Any,
                                phrase: false,
                                is_regex: false,
                            });
                            for cand in candidates.iter().take(2) {
                                hints.push(format!(
                                    "did you mean 📁 {}?",
                                    cand.path.to_string_lossy()
                                ));
                            }
                        }
                        TrieMatchResult::None => {
                            push_term!(Term {
                                text: text.clone(),
                                field: TermField::Any,
                                phrase: false,
                                is_regex: false,
                            });
                        }
                    }
                }
            }
            Token::Word(text) => {
                push_term!(Term {
                    text,
                    field: TermField::Any,
                    phrase: false,
                    is_regex: false,
                });
            }
            Token::Quoted(phrase) => {
                push_term!(Term {
                    text: phrase,
                    field: TermField::Any,
                    phrase: true,
                    is_regex: false,
                });
            }
            Token::Regex(pattern) => {
                push_term!(Term {
                    text: pattern,
                    field: TermField::Any,
                    phrase: false,
                    is_regex: true,
                });
            }
            Token::NegatedWord(word) => {
                filters.push(Filter::Not(Box::new(Filter::Path(word.clone()))));
                active_filters_dto.push(ActiveFilterDto {
                    raw: format!("-{}", word),
                    filter_type: "exclude".to_string(),
                    value: word,
                    negated: true,
                });
            }
            Token::NegatedQuoted(phrase) => {
                filters.push(Filter::Not(Box::new(Filter::Name(phrase.clone()))));
                active_filters_dto.push(ActiveFilterDto {
                    raw: format!("-\"{}\"", phrase),
                    filter_type: "exclude".to_string(),
                    value: phrase,
                    negated: true,
                });
            }
            Token::KeyValue {
                negated,
                key,
                value,
                quoted_value,
            } => {
                let key_lower = key.to_lowercase();
                let raw_token = if negated {
                    if quoted_value {
                        format!("-{}:\"{}\"", key, value)
                    } else {
                        format!("-{}:{}", key, value)
                    }
                } else if quoted_value {
                    format!("{}:\"{}\"", key, value)
                } else {
                    format!("{}:{}", key, value)
                };

                match key_lower.as_str() {
                    "near" => {
                        if let Ok(distance) = value.parse::<u32>() {
                            let clamped = distance.min(NEAR_MAX_DISTANCE);
                            if clamped != distance {
                                hints.push(format!(
                                    "near:{distance} clamped to {NEAR_MAX_DISTANCE}"
                                ));
                            }
                            pending_near = Some(clamped);
                            continue;
                        }
                        hints.push(format!(
                            "Invalid near filter '{}'; use near:5 (or a near[5] b)",
                            value
                        ));
                        push_term!(Term {
                            text: raw_token,
                            field: TermField::Any,
                            phrase: false,
                            is_regex: false,
                        });
                    }
                    "visual" => {
                        let filter = Filter::Type("image".to_string());
                        if negated {
                            filters.push(Filter::Not(Box::new(filter)));
                        } else {
                            filters.push(filter);
                        }
                        push_term!(Term {
                            text: value.clone(),
                            field: TermField::Any,
                            phrase: quoted_value,
                            is_regex: false,
                        });
                        active_filters_dto.push(ActiveFilterDto {
                            raw: raw_token,
                            filter_type: "visual".to_string(),
                            value,
                            negated,
                        });
                    }
                    "type" => {
                        let filter = Filter::Type(value.to_lowercase());
                        if negated {
                            filters.push(Filter::Not(Box::new(filter)));
                        } else {
                            filters.push(filter);
                        }
                        active_filters_dto.push(ActiveFilterDto {
                            raw: raw_token,
                            filter_type: "type".to_string(),
                            value,
                            negated,
                        });
                    }
                    "ext" => {
                        let ext_clean = value.trim_start_matches('.').to_lowercase();
                        let filter = Filter::Ext(ext_clean.clone());
                        if negated {
                            filters.push(Filter::Not(Box::new(filter)));
                        } else {
                            filters.push(filter);
                        }
                        active_filters_dto.push(ActiveFilterDto {
                            raw: raw_token,
                            filter_type: "ext".to_string(),
                            value: ext_clean,
                            negated,
                        });
                    }
                    "in" => {
                        let mut resolved_scope = false;
                        if let Some(dir_trie) = trie {
                            match dir_trie.resolve(&value, entries, path_index) {
                                TrieMatchResult::Single(resolved) => {
                                    scopes.push(Scope {
                                        dir_id: resolved.dir_idx as u64,
                                        include: !negated,
                                        path: resolved.path.clone(),
                                    });
                                    active_filters_dto.push(ActiveFilterDto {
                                        raw: raw_token.clone(),
                                        filter_type: "in".to_string(),
                                        value: resolved.path.to_string_lossy().to_string(),
                                        negated,
                                    });
                                    resolved_scope = true;
                                }
                                TrieMatchResult::Ambiguous(candidates) => {
                                    push_term!(Term {
                                        text: value.clone(),
                                        field: TermField::Path,
                                        phrase: false,
                                        is_regex: false,
                                    });
                                    for cand in candidates.iter().take(2) {
                                        hints.push(format!(
                                            "did you mean 📁 {}?",
                                            cand.path.to_string_lossy()
                                        ));
                                    }
                                    resolved_scope = true;
                                }
                                TrieMatchResult::None => {}
                            }
                        }
                        if !resolved_scope {
                            // If trie could not resolve, match path as filter
                            let filter = Filter::Path(value.clone());
                            if negated {
                                filters.push(Filter::Not(Box::new(filter)));
                            } else {
                                filters.push(filter);
                            }
                            active_filters_dto.push(ActiveFilterDto {
                                raw: raw_token,
                                filter_type: "in".to_string(),
                                value,
                                negated,
                            });
                        }
                    }
                    "path" => {
                        push_term!(Term {
                            text: value.clone(),
                            field: TermField::Path,
                            phrase: quoted_value,
                            is_regex: false,
                        });
                        active_filters_dto.push(ActiveFilterDto {
                            raw: raw_token,
                            filter_type: "path".to_string(),
                            value,
                            negated,
                        });
                    }
                    "modified" => {
                        if let Some(range_op) = parse_modified_range(&value, now_sec) {
                            let filter = Filter::Modified(range_op);
                            if negated {
                                filters.push(Filter::Not(Box::new(filter)));
                            } else {
                                filters.push(filter);
                            }
                            active_filters_dto.push(ActiveFilterDto {
                                raw: raw_token,
                                filter_type: "modified".to_string(),
                                value,
                                negated,
                            });
                        } else {
                            hints.push(format!(
                                "Invalid modified filter '{}'; use <7d, >30d, 7d..30d",
                                value
                            ));
                            push_term!(Term {
                                text: raw_token,
                                field: TermField::Any,
                                phrase: false,
                                is_regex: false,
                            });
                        }
                    }
                    "size" => {
                        if let Some(range_op) = parse_size_range(&value) {
                            let filter = Filter::Size(range_op);
                            if negated {
                                filters.push(Filter::Not(Box::new(filter)));
                            } else {
                                filters.push(filter);
                            }
                            active_filters_dto.push(ActiveFilterDto {
                                raw: raw_token,
                                filter_type: "size".to_string(),
                                value,
                                negated,
                            });
                        } else {
                            hints.push(format!(
                                "Invalid size filter '{}'; use >2mb, <100kb, 1mb..10mb",
                                value
                            ));
                            push_term!(Term {
                                text: raw_token,
                                field: TermField::Any,
                                phrase: false,
                                is_regex: false,
                            });
                        }
                    }
                    "is" => {
                        let filter = match value.to_lowercase().as_str() {
                            "dir" | "directory" | "folder" => Some(Filter::Kind(FileKind::Dir)),
                            "file" => Some(Filter::Kind(FileKind::File)),
                            "image" | "video" | "audio" | "doc" | "code" | "archive" => {
                                Some(Filter::Type(value.to_lowercase()))
                            }
                            _ => None,
                        };

                        if let Some(f) = filter {
                            if negated {
                                filters.push(Filter::Not(Box::new(f)));
                            } else {
                                filters.push(f);
                            }
                            active_filters_dto.push(ActiveFilterDto {
                                raw: raw_token,
                                filter_type: "is".to_string(),
                                value,
                                negated,
                            });
                        } else {
                            hints.push(format!(
                                "Unknown 'is:{}' filter; use is:image, is:dir, is:file",
                                value
                            ));
                            push_term!(Term {
                                text: raw_token,
                                field: TermField::Any,
                                phrase: false,
                                is_regex: false,
                            });
                        }
                    }
                    "lang" => {
                        let filter = Filter::Lang(value.to_lowercase());
                        if negated {
                            filters.push(Filter::Not(Box::new(filter)));
                        } else {
                            filters.push(filter);
                        }
                        active_filters_dto.push(ActiveFilterDto {
                            raw: raw_token,
                            filter_type: "lang".to_string(),
                            value,
                            negated,
                        });
                    }
                    "name" => {
                        push_term!(Term {
                            text: value.clone(),
                            field: TermField::Name,
                            phrase: quoted_value,
                            is_regex: false,
                        });
                    }
                    "content" => {
                        push_term!(Term {
                            text: value.clone(),
                            field: TermField::Content,
                            phrase: quoted_value,
                            is_regex: false,
                        });
                    }
                    "mode" => {
                        if value.eq_ignore_ascii_case("exhaustive") {
                            mode = Mode::Exhaustive;
                        } else {
                            mode = Mode::Ranked;
                        }
                    }
                    _ => {
                        // Unknown key -> treat as literal text (never zero results)
                        push_term!(Term {
                            text: raw_token,
                            field: TermField::Any,
                            phrase: false,
                            is_regex: false,
                        });
                    }
                }
            }
        }
        is_first_token = false;
    }

    let _ = generation; // Used for reference

    Query {
        terms,
        filters,
        scopes,
        mode,
        raw: raw.to_string(),
        hints,
        active_filters_dto,
        proximity,
    }
}

fn is_directory_like(s: &str) -> bool {
    s.ends_with('/') || s.ends_with('\\') || s.starts_with("~/") || s.starts_with("./")
}

fn parse_duration_seconds(s: &str) -> Option<u64> {
    let s = s.trim().to_lowercase();
    let num_str: String = s
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    if num_str.is_empty() {
        return None;
    }
    let num: f64 = num_str.parse().ok()?;
    let suffix = s[num_str.len()..].trim();

    let secs = match suffix {
        "" | "s" | "sec" | "secs" => num,
        "m" | "min" | "mins" => num * 60.0,
        "h" | "hr" | "hrs" | "hour" | "hours" => num * 3600.0,
        "d" | "day" | "days" => num * 86400.0,
        "w" | "wk" | "wks" | "week" | "weeks" => num * 7.0 * 86400.0,
        "y" | "yr" | "yrs" | "year" | "years" => num * 365.0 * 86400.0,
        _ => return None,
    };
    Some(secs as u64)
}

fn parse_modified_range(val: &str, now_sec: u64) -> Option<RangeOp> {
    let s = val.trim();
    if let Some((first, second)) = s.split_once("..") {
        let dur1 = parse_duration_seconds(first)?;
        let dur2 = parse_duration_seconds(second)?;
        let (min_dur, max_dur) = if dur1 <= dur2 {
            (dur1, dur2)
        } else {
            (dur2, dur1)
        };
        let low_time = now_sec.saturating_sub(max_dur);
        let high_time = now_sec.saturating_sub(min_dur);
        return Some(RangeOp::Between(low_time, high_time));
    }
    if let Some(rest) = s.strip_prefix('<') {
        let dur = parse_duration_seconds(rest.trim_start_matches('='))?;
        return Some(RangeOp::Gte(now_sec.saturating_sub(dur)));
    }
    if let Some(rest) = s.strip_prefix('>') {
        let dur = parse_duration_seconds(rest.trim_start_matches('='))?;
        return Some(RangeOp::Lte(now_sec.saturating_sub(dur)));
    }
    let dur = parse_duration_seconds(s)?;
    Some(RangeOp::Gte(now_sec.saturating_sub(dur)))
}

fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim().to_lowercase();
    let num_str: String = s
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    if num_str.is_empty() {
        return None;
    }
    let num: f64 = num_str.parse().ok()?;
    let suffix = s[num_str.len()..].trim();

    let bytes = match suffix {
        "" | "b" => num,
        "k" | "kb" | "kib" => num * 1024.0,
        "m" | "mb" | "mib" => num * 1024.0 * 1024.0,
        "g" | "gb" | "gib" => num * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some(bytes as u64)
}

fn parse_size_range(val: &str) -> Option<RangeOp> {
    let s = val.trim();
    if let Some((low_s, high_s)) = s.split_once("..") {
        let low = parse_size(low_s)?;
        let high = parse_size(high_s)?;
        return Some(RangeOp::Between(low, high));
    }
    if let Some(rest) = s.strip_prefix(">=") {
        return Some(RangeOp::Gte(parse_size(rest)?));
    }
    if let Some(rest) = s.strip_prefix('>') {
        return Some(RangeOp::Gt(parse_size(rest)?));
    }
    if let Some(rest) = s.strip_prefix("<=") {
        return Some(RangeOp::Lte(parse_size(rest)?));
    }
    if let Some(rest) = s.strip_prefix('<') {
        return Some(RangeOp::Lt(parse_size(rest)?));
    }
    let size = parse_size(s)?;
    Some(RangeOp::Between(size.saturating_sub(1024), size + 1024))
}

/// Maximum bound for a `near[x]` distance. Sloppier phrases beyond this are
/// closer to "anywhere in the document" than "within N words"; clamping keeps
/// phrase queries meaningful and matches the documented behavior.
pub const NEAR_MAX_DISTANCE: u32 = 100;

fn parse_near_token(text: &str) -> Option<u32> {
    let lower = text.to_ascii_lowercase();
    let rest = lower.strip_prefix("near")?;
    let rest = rest.trim();
    // Only the canonical bracket form: near[5]
    let inner = rest.strip_prefix('[')?;
    let digits: String = inner.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let after = &inner[digits.len()..];
    let after = after.trim();
    if after != "]" {
        return None;
    }
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;
    const WEEK: u64 = 7 * DAY;
    const MONTH: u64 = 30 * DAY;

    struct Fixture {
        entries: Vec<FileEntry>,
        path_index: HashMap<PathBuf, usize>,
        trie: DirectoryTrie,
        projects_idx: usize,
        src_idx: usize,
    }

    fn file_entry(path: PathBuf, is_dir: bool) -> FileEntry {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let extension = if is_dir {
            None
        } else {
            crate::indexer::walker::intern_extension(path.extension().and_then(|e| e.to_str()))
        };
        FileEntry {
            id: 0,
            char_bloom: crate::search::fuzzy::compute_char_mask(&name),
            path,
            name,
            extension,
            size: 0,
            modified: 0,
            is_dir,
            dev: 0,
            ino: 0,
            content_pending: false,
        }
    }

    /// Synthetic tree rooted at the user's home directory so that `~/Projects`
    /// resolves through the same code path as production.
    fn fixture() -> Fixture {
        let projects = match dirs::home_dir() {
            Some(home) => home.join("Projects"),
            None => PathBuf::from("~/Projects"),
        };
        let mnemo = projects.join("mnemo");
        let src = mnemo.join("src");

        let specs = [
            (projects.clone(), true),
            (mnemo.clone(), true),
            (src.clone(), true),
            (mnemo.join("Cargo.toml"), false),
            (src.join("main.rs"), false),
        ];

        let entries: Vec<FileEntry> = specs
            .into_iter()
            .map(|(path, is_dir)| file_entry(path, is_dir))
            .collect();
        let path_index: HashMap<PathBuf, usize> = entries
            .iter()
            .enumerate()
            .map(|(idx, entry)| (entry.path.clone(), idx))
            .collect();
        let trie = DirectoryTrie::build(&entries, &path_index, 1);

        Fixture {
            projects_idx: path_index[&projects],
            src_idx: path_index[&src],
            entries,
            path_index,
            trie,
        }
    }

    fn parse(raw: &str) -> Query {
        let fixture = fixture();
        parse_query(
            raw,
            &fixture.entries,
            &fixture.path_index,
            Some(&fixture.trie),
            1,
        )
    }

    fn term(text: &str) -> Term {
        Term {
            text: text.to_string(),
            field: TermField::Any,
            phrase: false,
            is_regex: false,
        }
    }

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    fn bound(op: &RangeOp) -> u64 {
        match *op {
            RangeOp::Gte(value) | RangeOp::Lte(value) | RangeOp::Gt(value) | RangeOp::Lt(value) => {
                value
            }
            RangeOp::Between(low, high) => {
                panic!("expected a single bound, got Between({low}, {high})")
            }
        }
    }

    fn modified_range(query: &Query) -> &RangeOp {
        match query.filters.as_slice() {
            [Filter::Modified(op)] => op,
            other => panic!("expected exactly one modified filter, got {other:?}"),
        }
    }

    #[test]
    fn parses_plain_terms() {
        let query = parse("alpha beta");
        assert_eq!(query.terms, vec![term("alpha"), term("beta")]);
        assert!(query.filters.is_empty());
        assert!(query.scopes.is_empty());
        assert_eq!(query.mode, Mode::Ranked);
        assert_eq!(query.raw, "alpha beta");
    }

    #[test]
    fn parses_near_operator_pair() {
        let query = parse("mouse near[5] button");
        assert_eq!(query.terms, vec![term("mouse"), term("button")]);
        assert_eq!(query.proximity, vec![(0, 1, 5)]);
    }

    #[test]
    fn parses_near_operator_chain() {
        let query = parse("mouse near[3] button near[2] click");
        assert_eq!(query.proximity, vec![(0, 1, 3), (1, 2, 2)]);
    }

    #[test]
    fn doubles_near_operator_last_wins() {
        let query = parse("mouse near[5] near[2] button");
        assert_eq!(query.proximity, vec![(0, 1, 2)]);
    }

    #[test]
    fn drops_dangling_near_operator() {
        let query = parse("mouse near[5]");
        assert_eq!(query.terms, vec![term("mouse")]);
        assert!(query.proximity.is_empty());
    }

    #[test]
    fn leading_near_operator_is_dropped_not_a_term() {
        let query = parse("near[5] mouse");
        assert_eq!(query.terms, vec![term("mouse")]);
        assert!(query.proximity.is_empty());
    }

    #[test]
    fn parses_near_filter_form() {
        let query = parse("mouse near:5 button");
        assert_eq!(query.terms, vec![term("mouse"), term("button")]);
        assert_eq!(query.proximity, vec![(0, 1, 5)]);
    }

    #[test]
    fn near_distance_above_max_is_clamped_with_a_hint() {
        let query = parse("mouse near[5000] button");
        assert_eq!(query.proximity, vec![(0, 1, 100)]);
        assert!(query.hints.iter().any(|h| h.contains("clamped to 100")));

        let query = parse("mouse near:5000 button");
        assert_eq!(query.proximity, vec![(0, 1, 100)]);
        assert!(query.hints.iter().any(|h| h.contains("clamped to 100")));
    }

    #[test]
    fn near_distance_at_max_is_not_clamped() {
        let query = parse("mouse near[100] button");
        assert_eq!(query.proximity, vec![(0, 1, 100)]);
        assert!(
            !query.hints.iter().any(|h| h.contains("clamped")),
            "boundary value must not emit a clamp hint, got {:?}",
            query.hints
        );
    }

    #[test]
    fn duplicate_near_hint_text_differs_for_filter_form() {
        let query = parse("near:900 mouse");
        assert!(
            query
                .hints
                .iter()
                .any(|h| h.contains("near:900 clamped to 100"))
        );
    }

    #[test]
    fn invalid_near_filter_falls_back_to_literal_term() {
        let query = parse("near:xyz mouse");
        assert!(
            query
                .hints
                .iter()
                .any(|h| h.contains("Invalid near filter")),
            "expected a malformed-near hint, got {:?}",
            query.hints
        );
        assert_eq!(query.terms, vec![term("near:xyz"), term("mouse")]);
    }

    #[test]
    fn parses_quoted_phrases() {
        let query = parse("say \"hello world\"");
        assert_eq!(query.terms.len(), 2);
        assert_eq!(query.terms[0], term("say"));
        assert_eq!(query.terms[1].text, "hello world");
        assert!(query.terms[1].phrase);
        assert!(!query.terms[1].is_regex);
    }

    #[test]
    fn parses_negation() {
        let query = parse("keep -draft");
        assert_eq!(query.terms, vec![term("keep")]);
        assert_eq!(
            query.filters,
            vec![Filter::Not(Box::new(Filter::Path("draft".to_string())))]
        );
        assert_eq!(query.active_filters_dto.len(), 1);
        assert!(query.active_filters_dto[0].negated);
    }

    #[test]
    fn parses_keyword_filters() {
        let query = parse("type:PDF ext:.rs is:image lang:rust");
        assert!(query.terms.is_empty());
        assert_eq!(
            query.filters,
            vec![
                Filter::Type("pdf".to_string()),
                Filter::Ext("rs".to_string()),
                Filter::Type("image".to_string()),
                Filter::Lang("rust".to_string()),
            ]
        );
        assert_eq!(query.active_filters_dto.len(), 4);
    }

    #[test]
    fn parses_modified_less_than() {
        let before = now_secs();
        let query = parse("modified:<7d");
        let after = now_secs();

        let op = modified_range(&query);
        assert!(matches!(op, RangeOp::Gte(_)));
        let target = bound(op);
        let low = before.saturating_sub(WEEK);
        let high = after.saturating_sub(WEEK);
        assert!(
            (low..=high).contains(&target),
            "target {target} not in {low}..={high}"
        );
    }

    #[test]
    fn parses_modified_greater_than() {
        let before = now_secs();
        let query = parse("modified:>30d");
        let after = now_secs();

        let op = modified_range(&query);
        assert!(matches!(op, RangeOp::Lte(_)));
        let target = bound(op);
        let low = before.saturating_sub(MONTH);
        let high = after.saturating_sub(MONTH);
        assert!(
            (low..=high).contains(&target),
            "target {target} not in {low}..={high}"
        );
    }

    #[test]
    fn parses_modified_between() {
        let before = now_secs();
        let query = parse("modified:7d..30d");
        let after = now_secs();

        match modified_range(&query) {
            RangeOp::Between(low, high) => {
                assert!(
                    (before.saturating_sub(MONTH)..=after.saturating_sub(MONTH)).contains(low),
                    "low {low} out of range"
                );
                assert!(
                    (before.saturating_sub(WEEK)..=after.saturating_sub(WEEK)).contains(high),
                    "high {high} out of range"
                );
            }
            other => panic!("expected Between, got {other:?}"),
        }
    }

    #[test]
    fn parses_size_ranges() {
        let query = parse("size:>2mb");
        assert_eq!(
            query.filters,
            vec![Filter::Size(RangeOp::Gt(2 * 1024 * 1024))]
        );

        let query = parse("size:<100kb");
        assert_eq!(query.filters, vec![Filter::Size(RangeOp::Lt(100 * 1024))]);
    }

    #[test]
    fn resolves_home_scope() {
        let fixture = fixture();
        let query = parse_query(
            "in:~/Projects",
            &fixture.entries,
            &fixture.path_index,
            Some(&fixture.trie),
            1,
        );

        assert!(query.terms.is_empty());
        assert!(query.filters.is_empty());
        assert_eq!(query.scopes.len(), 1);
        assert!(query.scopes[0].include);
        assert_eq!(query.scopes[0].dir_id, fixture.projects_idx as u64);
        assert_eq!(
            query.scopes[0].path,
            fixture.entries[fixture.projects_idx].path
        );
    }

    /// #59: a quoted absolute path, typed raw or escaped the way the UI
    /// inserts it, must resolve to the folder. On Windows the fixture paths
    /// are real `C:\Users\...` paths, so this runs the reported case.
    #[test]
    fn resolves_quoted_absolute_path_with_native_separators() {
        let fixture = fixture();
        let src = fixture.entries[fixture.src_idx]
            .path
            .to_string_lossy()
            .into_owned();
        let escaped = src.replace('\\', "\\\\").replace('"', "\\\"");
        for raw in [
            format!("in:\"{src}\""),
            format!("in:\"{escaped}\""),
            format!("in:{src}"),
        ] {
            let query = parse_query(
                &raw,
                &fixture.entries,
                &fixture.path_index,
                Some(&fixture.trie),
                1,
            );
            assert!(
                query.filters.is_empty(),
                "{raw}: fell back to a path filter"
            );
            assert_eq!(query.scopes.len(), 1, "{raw}: did not resolve a scope");
            assert_eq!(query.scopes[0].dir_id, fixture.src_idx as u64, "{raw}");
        }
    }

    /// Same case with a hard-coded Windows path, so Linux CI covers it too.
    #[test]
    fn resolves_quoted_windows_path_with_spaces() {
        let dir = PathBuf::from(r"C:\Users\me\My Docs");
        let entries = vec![
            file_entry(dir.clone(), true),
            file_entry(dir.join("a.txt"), false),
        ];
        let path_index: HashMap<PathBuf, usize> = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.path.clone(), i))
            .collect();
        let trie = DirectoryTrie::build(&entries, &path_index, 1);
        for raw in [
            r#"in:"C:\Users\me\My Docs""#,
            r#"in:"C:\\Users\\me\\My Docs""#,
        ] {
            let query = parse_query(raw, &entries, &path_index, Some(&trie), 1);
            assert!(
                query.filters.is_empty(),
                "{raw}: fell back to a path filter"
            );
            assert_eq!(query.scopes.len(), 1, "{raw}");
            assert_eq!(query.scopes[0].path, dir, "{raw}");
        }
    }

    #[test]
    fn resolves_leading_directory_token() {
        let fixture = fixture();
        let query = parse_query(
            "src/ fuzzy",
            &fixture.entries,
            &fixture.path_index,
            Some(&fixture.trie),
            1,
        );

        assert_eq!(query.scopes.len(), 1);
        assert!(query.scopes[0].include);
        assert_eq!(query.scopes[0].path, fixture.entries[fixture.src_idx].path);
        assert_eq!(query.terms, vec![term("fuzzy")]);
    }

    #[test]
    fn routes_field_terms() {
        let query = parse("name:\"cargo\" content:needle path:src/main.rs");
        assert_eq!(
            query.terms,
            vec![
                Term {
                    text: "cargo".to_string(),
                    field: TermField::Name,
                    phrase: true,
                    is_regex: false,
                },
                Term {
                    text: "needle".to_string(),
                    field: TermField::Content,
                    phrase: false,
                    is_regex: false,
                },
                Term {
                    text: "src/main.rs".to_string(),
                    field: TermField::Path,
                    phrase: false,
                    is_regex: false,
                },
            ]
        );
    }

    #[test]
    fn parses_regex_terms() {
        let query = parse(r#"re:"fn \\w+""#);
        assert_eq!(query.terms.len(), 1);
        assert!(query.terms[0].is_regex);
        assert_eq!(query.terms[0].text, r"fn \w+");
    }

    #[test]
    fn malformed_regex_does_not_panic() {
        let query = parse("re:");
        assert_eq!(query.terms.len(), 1);
        assert!(query.terms[0].is_regex);
        assert_eq!(query.terms[0].text, "");
    }

    #[test]
    fn unknown_key_falls_back_to_literal_text() {
        let query = parse("foo:bar baz");
        assert_eq!(query.terms, vec![term("foo:bar"), term("baz")]);
        assert!(query.filters.is_empty());
        assert!(query.hints.is_empty());
    }

    #[test]
    fn parses_empty_and_whitespace_only_queries() {
        for raw in ["", "   ", "\t\n  "] {
            let query = parse(raw);
            assert!(query.terms.is_empty(), "raw: {raw:?}");
            assert!(query.filters.is_empty(), "raw: {raw:?}");
            assert!(query.scopes.is_empty(), "raw: {raw:?}");
            assert_eq!(query.mode, Mode::Ranked);
        }
    }

    #[test]
    fn parses_filter_only_query() {
        let query = parse("type:pdf size:>1kb");
        assert!(query.terms.is_empty());
        assert_eq!(
            query.filters,
            vec![
                Filter::Type("pdf".to_string()),
                Filter::Size(RangeOp::Gt(1024)),
            ]
        );
    }

    #[test]
    fn parses_visual_filter_and_routes_to_visual_search() {
        let query = parse("visual:diagram");
        assert_eq!(query.filters, vec![Filter::Type("image".to_string())]);
        assert_eq!(query.terms, vec![term("diagram")]);
        assert!(
            query
                .active_filters_dto
                .iter()
                .any(|f| f.filter_type == "visual" && f.value == "diagram")
        );

        let route = crate::query::router::route_query(&query);
        assert_eq!(route, crate::query::router::QueryRoute::VisualSearch);
    }
}
