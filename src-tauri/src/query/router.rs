use crate::query::ast::{Filter, Query, TermField};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryRoute {
    NameCode,
    ContentPhrase,
    FilteredScope,
    SemanticSearch,
    VisualSearch,
    ScopeNavigation,
    General,
}

const NL_STOPWORDS: &[&str] = &[
    "how", "to", "what", "is", "where", "the", "a", "an", "in", "of", "for", "with", "on", "at",
    "by", "from", "and", "or", "why", "can", "i", "find", "which", "who", "show", "me", "all",
];

pub fn route_query(query: &Query) -> QueryRoute {
    let raw_trimmed = query.raw.trim();

    // 1. Ends with '/' -> pure scope navigation
    if raw_trimmed.ends_with('/') || raw_trimmed.ends_with('\\') {
        return QueryRoute::ScopeNavigation;
    }

    // 2. type: or in: present -> filtered, scope-first
    let has_type_filter = query.filters.iter().any(|f| match f {
        Filter::Type(_) | Filter::Ext(_) => true,
        Filter::Not(inner) => matches!(**inner, Filter::Type(_) | Filter::Ext(_)),
        _ => false,
    });
    let has_scope = !query.scopes.is_empty();
    if has_type_filter || has_scope {
        let has_image_filter = query.filters.iter().any(|f| match f {
            Filter::Type(t) => t == "image",
            Filter::Ext(ext) => matches!(
                ext.as_str(),
                "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "tiff" | "tif"
            ),
            _ => false,
        });
        if has_image_filter && !query.terms.is_empty() {
            return QueryRoute::VisualSearch;
        }
        return QueryRoute::FilteredScope;
    }

    // 3. quoted or re: -> content + phrase/regex
    let has_quote_or_regex = query
        .terms
        .iter()
        .any(|t| t.phrase || t.is_regex || t.field == TermField::Content);
    if has_quote_or_regex {
        return QueryRoute::ContentPhrase;
    }

    // Check tokens and spaces
    let tokens_count = query.terms.len();
    let has_space = raw_trimmed.chars().any(|c| c.is_whitespace());

    // 4. <=2 tokens, no spaces, has camelCase/_/. -> name-heavy + code tokenizer
    if tokens_count <= 2 && !has_space && is_code_identifier(raw_trimmed) {
        return QueryRoute::NameCode;
    }

    // 5. >=4 tokens or contains NL stopwords -> semantic search
    if tokens_count >= 4 || contains_nl_stopword(&query.terms) {
        return QueryRoute::SemanticSearch;
    }

    QueryRoute::General
}

fn is_code_identifier(s: &str) -> bool {
    if s.contains('_') || s.contains('.') || s.contains('-') {
        return true;
    }
    let mut prev_lower = false;
    for ch in s.chars() {
        if ch.is_lowercase() {
            prev_lower = true;
        } else if ch.is_uppercase() && prev_lower {
            return true;
        } else {
            prev_lower = false;
        }
    }
    false
}

fn contains_nl_stopword(terms: &[crate::query::ast::Term]) -> bool {
    for term in terms {
        let text_lower = term.text.to_lowercase();
        if NL_STOPWORDS.contains(&text_lower.as_str()) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::ast::{Filter, Query, Scope, Term, TermField};
    use std::path::PathBuf;

    fn query(raw: &str) -> Query {
        Query {
            terms: Vec::new(),
            filters: Vec::new(),
            scopes: Vec::new(),
            mode: crate::query::ast::Mode::Ranked,
            raw: raw.to_string(),
            hints: Vec::new(),
            active_filters_dto: Vec::new(),
            proximity: Vec::new(),
        }
    }

    fn with_terms(mut q: Query, texts: &[&str]) -> Query {
        q.terms = texts
            .iter()
            .map(|t| Term {
                text: (*t).to_string(),
                field: TermField::Any,
                phrase: false,
                is_regex: false,
            })
            .collect();
        q
    }

    #[test]
    fn trailing_slash_is_pure_scope_navigation() {
        assert_eq!(
            route_query(&query("projects/")),
            QueryRoute::ScopeNavigation
        );
        assert_eq!(
            route_query(&query("~/Downloads/")),
            QueryRoute::ScopeNavigation
        );
        // Backslash form too (Windows users).
        assert_eq!(
            route_query(&query("Downloads\\")),
            QueryRoute::ScopeNavigation
        );
        // Even with terms: the trailing slash wins.
        assert_eq!(
            route_query(&with_terms(query("rpg projects/"), &["rpg"])),
            QueryRoute::ScopeNavigation
        );
    }

    #[test]
    fn whitespace_surrounding_does_not_change_classification() {
        let mut q = query("  deed/  ");
        q.raw = "  deed/  ".to_string();
        assert_eq!(route_query(&q), QueryRoute::ScopeNavigation);
    }

    #[test]
    fn plain_slash_only_query_is_scope_navigation() {
        // "/" alone: the intent is "show me the current scope tree".
        assert_eq!(route_query(&query("/")), QueryRoute::ScopeNavigation);
    }

    #[test]
    fn type_or_ext_filter_routes_to_filtered_scope() {
        let mut q = query("report");
        q.terms = with_terms(query("").clone(), &["report"]).terms;
        q.filters.push(Filter::Ext("pdf".to_string()));
        assert_eq!(route_query(&q), QueryRoute::FilteredScope);

        let mut q2 = query("report");
        q2.terms = q.terms.clone();
        q2.filters.push(Filter::Type("doc".to_string()));
        assert_eq!(route_query(&q2), QueryRoute::FilteredScope);
    }

    #[test]
    fn negated_type_filter_still_counts_as_filtered() {
        let mut q = query("notes");
        q.terms = with_terms(query("").clone(), &["notes"]).terms;
        q.filters
            .push(Filter::Not(Box::new(Filter::Type("image".to_string()))));
        assert_eq!(route_query(&q), QueryRoute::FilteredScope);
    }

    #[test]
    fn scope_without_filter_is_filtered_scope() {
        let mut q = query("notes");
        q.terms = with_terms(query("").clone(), &["notes"]).terms;
        q.scopes.push(Scope {
            dir_id: 3,
            include: true,
            path: PathBuf::from("/home/user/notes"),
        });
        assert_eq!(route_query(&q), QueryRoute::FilteredScope);
    }

    #[test]
    fn image_filter_with_terms_is_visual_search() {
        let mut q = query("mountains");
        q.terms = with_terms(query("").clone(), &["mountains"]).terms;
        q.filters.push(Filter::Type("image".to_string()));
        assert_eq!(route_query(&q), QueryRoute::VisualSearch);

        let mut q2 = query("mountains");
        q2.terms = q.terms.clone();
        q2.filters.push(Filter::Ext("png".to_string()));
        assert_eq!(route_query(&q2), QueryRoute::VisualSearch);
    }

    #[test]
    fn image_filter_without_terms_falls_back_to_filtered_scope() {
        let mut q = query("");
        q.filters.push(Filter::Type("image".to_string()));
        assert_eq!(route_query(&q), QueryRoute::FilteredScope);
    }

    #[test]
    fn quoted_phrase_or_regex_routes_to_content_phrase() {
        let mut q = query("\"exact phrase\"");
        q.terms = with_terms(query("").clone(), &["exact phrase"]).terms;
        q.terms[0].phrase = true;
        assert_eq!(route_query(&q), QueryRoute::ContentPhrase);

        let mut q2 = query("re:parse.*tree");
        q2.terms = with_terms(query("").clone(), &["parse.*tree"]).terms;
        q2.terms[0].is_regex = true;
        assert_eq!(route_query(&q), QueryRoute::ContentPhrase);

        let mut q3 = query("content:wiki");
        q3.terms = with_terms(query("").clone(), &["wiki"]).terms;
        q3.terms[0].field = TermField::Content;
        assert_eq!(route_query(&q3), QueryRoute::ContentPhrase);
    }

    #[test]
    fn camel_snake_kebab_dotted_single_tokens_route_to_name_code() {
        for raw in ["parse_xlim", "PascalCaseToken", "kebab-case", "config.json"] {
            assert_eq!(
                route_query(&with_terms(query(raw), &[raw])),
                QueryRoute::NameCode,
                "{raw} must classify as name/code"
            );
        }
    }

    #[test]
    fn spaced_or_plain_lowercase_words_do_not_route_to_name_code() {
        // Contains a space: never NameCode even if it looks code-ish.
        assert_eq!(
            route_query(&with_terms(
                query("parse_xlim notes"),
                &["parse_xlim", "notes"]
            )),
            QueryRoute::General
        );
        // Plain lowercase word(s): General.
        assert_eq!(
            route_query(&with_terms(query("ketchup"), &["ketchup"])),
            QueryRoute::General
        );
    }

    #[test]
    fn three_plain_words_stay_general() {
        assert_eq!(
            route_query(&with_terms(
                query("red running shoes"),
                &["red", "running", "shoes"]
            )),
            QueryRoute::General
        );
    }

    #[test]
    fn four_or_more_tokens_route_to_semantic_search() {
        assert_eq!(
            route_query(&with_terms(
                query("best lightweight running shoes for trail"),
                &["best", "lightweight", "running", "shoes", "for", "trail"]
            )),
            QueryRoute::SemanticSearch
        );
    }

    #[test]
    fn nl_stopword_lowers_the_semantic_threshold() {
        // 3 tokens but one is a stopword: natural-language question.
        assert_eq!(
            route_query(&with_terms(
                query("how rust async works"),
                &["rust", "async", "how"]
            )),
            QueryRoute::SemanticSearch
        );
    }

    #[test]
    fn stopword_only_check_uses_case_folding() {
        assert_eq!(
            route_query(&with_terms(
                query("Explain THE build"),
                &["Explain", "THE", "build"]
            )),
            QueryRoute::SemanticSearch
        );
    }

    #[test]
    fn empty_query_is_general() {
        assert_eq!(route_query(&query("")), QueryRoute::General);
    }
}
