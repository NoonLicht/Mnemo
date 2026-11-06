//! `.gitignore` / `.cursorignore` parity for the index.
//!
//! Agents assume the index matches what a developer sees: files a repo
//! ignores should not surface as search results. This module parses the
//! gitignore-format files (`.gitignore`, `.cursorignore`, and
//! `.mnemoignore`) found under the watched roots and answers one question:
//! is this path ignored?
//!
//! Scope and honesty notes:
//!
//! - This is a **matching-parity subset**, not a full gitignore engine: the
//!   common syntax is supported (`*`, `?`, `**`, character ranges, anchored
//!   patterns, directory-only patterns, negation, comments). Exotic edge
//!   cases (escaped trailing spaces, nested attributes files) are not.
//! - Rules are applied **after** the walk, not during traversal: ignored
//!   directories are still traversed (the default `excluded_dirs` list
//!   already keeps the big ones like `node_modules` out), but their files
//!   never reach the index.
//! - Precedence follows git: once a parent directory is excluded, nothing
//!   under it can be re-included; for the path itself, the deepest
//!   applicable rule wins, later lines win within a file, and `!` negates.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const IGNORE_FILE_NAMES: &[&str] = &[".gitignore", ".cursorignore", ".mnemoignore"];

#[derive(Debug, Clone, PartialEq, Eq)]
struct Rule {
    /// Raw pattern text without leading `!` or trailing `/`.
    pattern: String,
    negated: bool,
    /// Pattern ended with `/`: matches directories only.
    dir_only: bool,
    /// Pattern contains a `/` (other than a trailing one): match relative to
    /// the defining directory instead of any basename.
    anchored: bool,
}

/// Parsed ignore rules grouped by the directory that defines them.
#[derive(Debug, Default)]
pub struct IgnoreSet {
    rules: BTreeMap<PathBuf, Vec<Rule>>,
    case_insensitive: bool,
}

impl IgnoreSet {
    /// Empty set: nothing is ignored.
    pub fn empty() -> Self {
        Self {
            rules: BTreeMap::new(),
            case_insensitive: cfg!(windows) || cfg!(target_os = "macos"),
        }
    }

    /// Scans `dirs` (candidate directories, usually the walker's output,
    /// which is already in memory) for ignore files. The ignore files
    /// themselves are hidden entries, so they are probed on disk rather than
    /// read from the entry list.
    pub fn from_directories(dirs: &[PathBuf]) -> Self {
        let mut set = Self::empty();
        for dir in dirs {
            for name in IGNORE_FILE_NAMES {
                let file = dir.join(name);
                if !file.is_file() {
                    continue;
                }
                if let Ok(raw) = std::fs::read_to_string(&file) {
                    let rules = parse(&raw);
                    if !rules.is_empty() {
                        set.rules.entry(dir.clone()).or_default().extend(rules);
                    }
                }
            }
        }
        set
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// True when `path` is ignored. `is_dir` reflects the entry's type.
    pub fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        if self.rules.is_empty() {
            return false;
        }
        // Applicable rule groups, ordered shallow → deep so deeper files
        // override shallower ones.
        let mut applicable: Vec<(&Path, &[Rule])> = self
            .rules
            .iter()
            .filter(|(dir, _)| path.starts_with(*dir))
            .map(|(dir, rules)| (dir.as_path(), rules.as_slice()))
            .collect();
        if applicable.is_empty() {
            return false;
        }
        applicable.sort_by_key(|(dir, _)| dir.as_os_str().len());

        let mut ignored = false;
        for (dir, rules) in applicable {
            let Ok(relative) = path.strip_prefix(dir) else {
                continue;
            };
            let components: Vec<String> = relative
                .components()
                .map(|component| {
                    normalize_component(
                        component.as_os_str().to_string_lossy().as_ref(),
                        self.case_insensitive,
                    )
                })
                .collect();
            if components.is_empty() {
                continue;
            }

            // Parent directories: if any parent prefix is ignored (as a
            // directory), the whole subtree is ignored and cannot be
            // re-included (git semantics).
            for depth in 1..components.len() {
                let prefix = components[..depth].join("/");
                if self.evaluate(rules, &prefix, true) {
                    return true;
                }
            }

            // The path itself: last matching rule wins, and deeper groups
            // override shallower ones.
            let full = components.join("/");
            if let Some(decision) = self.evaluate_last(rules, &full, is_dir) {
                ignored = decision;
            }
        }
        ignored
    }

    /// Returns the decision of the last matching rule, or `None` when no rule
    /// matches.
    fn evaluate_last(&self, rules: &[Rule], relative: &str, is_dir: bool) -> Option<bool> {
        let mut decision = None;
        for rule in rules {
            if self.rule_matches(rule, relative, is_dir) {
                decision = Some(!rule.negated);
            }
        }
        decision
    }

    /// True when any rule matches (used for parent prefixes, where git says
    /// an excluded directory cannot be re-included from below).
    fn evaluate(&self, rules: &[Rule], relative: &str, is_dir: bool) -> bool {
        rules
            .iter()
            .any(|rule| self.rule_matches(rule, relative, is_dir))
    }

    fn rule_matches(&self, rule: &Rule, relative: &str, is_dir: bool) -> bool {
        if rule.dir_only && !is_dir && !relative.contains('/') {
            // A dir-only pattern never matches a file by its own basename…
            return false;
        }
        let candidate: Cow<'_, str> = if rule.anchored {
            Cow::Borrowed(relative)
        } else {
            // Unanchored patterns match the basename at any depth.
            Cow::Owned(relative.rsplit('/').next().unwrap_or(relative).to_string())
        };
        glob_match(&rule.pattern, &candidate)
    }
}

fn normalize_component(component: &str, case_insensitive: bool) -> String {
    if case_insensitive {
        component.to_lowercase()
    } else {
        component.to_string()
    }
}

fn parse(raw: &str) -> Vec<Rule> {
    let mut rules = Vec::new();
    for line in raw.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Unescaped trailing whitespace is not significant.
        let line = line.trim_end_matches([' ', '\t']);
        if line.is_empty() {
            continue;
        }
        let (pattern, negated) = match line.strip_prefix('!') {
            Some(rest) => (rest.to_string(), true),
            None => (line.to_string(), false),
        };
        if pattern.is_empty() {
            continue;
        }
        let (pattern, dir_only) = match pattern.strip_suffix('/') {
            Some(rest) => (rest.to_string(), true),
            None => (pattern, false),
        };
        // A leading slash anchors the pattern and is not part of the match.
        // A pattern like `foo/**/` keeps its interior slash after the
        // trailing one is stripped, so anchoring stays correct.
        let anchored = pattern.contains('/');
        let pattern = pattern.strip_prefix('/').unwrap_or(&pattern).to_string();
        rules.push(Rule {
            pattern,
            negated,
            dir_only,
            anchored,
        });
    }
    rules
}

/// gitignore-flavored glob match over one candidate string.
///
/// `*` matches any run without `/`, `?` one non-`/` character, `**` any run
/// including separators, and `[a-z]` / `[!a-z]` are character classes.
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    glob_impl(&p, &t)
}

fn glob_impl(p: &[char], t: &[char]) -> bool {
    let Some((&head, rest)) = p.split_first() else {
        return t.is_empty();
    };
    match head {
        '*' => {
            if rest.first() == Some(&'*') {
                // `**`: crosses separators. A following `/` is swallowed so
                // `**/x` also matches `x`.
                let after = if rest.get(1) == Some(&'/') {
                    &rest[2..]
                } else {
                    &rest[1..]
                };
                (0..=t.len()).any(|i| glob_impl(after, &t[i..]))
            } else {
                // `*`: any run without `/`.
                (0..=t.len())
                    .take_while(|&i| !t[..i].contains(&'/'))
                    .any(|i| glob_impl(rest, &t[i..]))
            }
        }
        '?' => !t.is_empty() && t[0] != '/' && glob_impl(rest, &t[1..]),
        '[' => {
            let Some((&first, tail)) = t.split_first() else {
                return false;
            };
            if first == '/' {
                return false;
            }
            match match_class(p, 0, first) {
                Some((matched, next)) if matched => glob_impl(&p[next..], tail),
                Some((_, _)) => false,
                // Unterminated class: `[` is a literal.
                None => t[0] == '[' && glob_impl(rest, tail),
            }
        }
        ch => t.first() == Some(&ch) && glob_impl(rest, &t[1..]),
    }
}

/// Matches a `[...]` class starting at `p[start]`. Returns whether the
/// character matched and the index just past the class, or `None` when the
/// class is unterminated.
fn match_class(p: &[char], start: usize, ch: char) -> Option<(bool, usize)> {
    let mut i = start + 1;
    let negated = matches!(p.get(i), Some('!') | Some('^'));
    if negated {
        i += 1;
    }
    let mut matched = false;
    let mut first = true;
    while i < p.len() && (p[i] != ']' || first) {
        first = false;
        if i + 2 < p.len() && p[i + 1] == '-' && p[i + 2] != ']' {
            if ch >= p[i] && ch <= p[i + 2] {
                matched = true;
            }
            i += 3;
        } else {
            if p[i] == ch {
                matched = true;
            }
            i += 1;
        }
    }
    if i >= p.len() {
        return None;
    }
    Some((matched != negated, i + 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_ignore(dir: &Path, name: &str, content: &str) {
        fs::write(dir.join(name), content).unwrap();
    }

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mnemo-ignore-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn basename_patterns_match_any_depth() {
        let dir = temp("basename");
        write_ignore(&dir, ".gitignore", "*.log\nbuild\n");
        let set = IgnoreSet::from_directories(std::slice::from_ref(&dir));

        assert!(set.is_ignored(&dir.join("a/b/error.log"), false));
        assert!(set.is_ignored(&dir.join("build"), true));
        assert!(set.is_ignored(&dir.join("x/build"), true));
        assert!(!set.is_ignored(&dir.join("builder"), true));
        assert!(!set.is_ignored(&dir.join("a/keep.txt"), false));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn anchored_patterns_match_only_at_root() {
        let dir = temp("anchored");
        write_ignore(&dir, ".gitignore", "/root-only.txt\nsub/**/deep\n");
        let set = IgnoreSet::from_directories(std::slice::from_ref(&dir));

        assert!(set.is_ignored(&dir.join("root-only.txt"), false));
        assert!(!set.is_ignored(&dir.join("a/root-only.txt"), false));
        assert!(set.is_ignored(&dir.join("sub/x/y/deep"), false));
        assert!(set.is_ignored(&dir.join("sub/deep"), false));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dir_only_patterns_ignore_contents_too() {
        let dir = temp("dironly");
        write_ignore(&dir, ".gitignore", "logs/\n");
        let set = IgnoreSet::from_directories(std::slice::from_ref(&dir));

        assert!(set.is_ignored(&dir.join("logs"), true));
        assert!(!set.is_ignored(&dir.join("logs.txt"), false));
        // Files under an ignored directory are ignored via the parent rule;
        // they cannot be re-included.
        assert!(set.is_ignored(&dir.join("logs/x.txt"), false));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn negation_and_last_match_wins() {
        let dir = temp("negation");
        write_ignore(&dir, ".gitignore", "*.log\n!important.log\n!keep/\nkeep/\n");
        let set = IgnoreSet::from_directories(std::slice::from_ref(&dir));

        assert!(set.is_ignored(&dir.join("debug.log"), false));
        assert!(!set.is_ignored(&dir.join("important.log"), false));
        // The later line re-ignores what an earlier line un-ignored.
        assert!(set.is_ignored(&dir.join("keep"), true));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cursorignore_and_deeper_files_override() {
        let dir = temp("cursor");
        write_ignore(&dir, ".cursorignore", "secrets/\n");
        let subdir = dir.join("sub");
        fs::create_dir_all(&subdir).unwrap();
        write_ignore(&subdir, ".gitignore", "!secrets/\n");
        // The walker probes every visited directory, so both are passed in.
        let set = IgnoreSet::from_directories(&[dir.clone(), subdir.clone()]);

        assert!(set.is_ignored(&dir.join("secrets"), true));
        // The deeper un-ignore wins for the nested path itself.
        assert!(!set.is_ignored(&subdir.join("secrets"), true));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn comments_blank_and_classes() {
        let dir = temp("syntax");
        write_ignore(
            &dir,
            ".gitignore",
            "# comment\n\n   \nfile?.txt\nfoo[0-9].dat\n[!a]special\n",
        );
        let set = IgnoreSet::from_directories(std::slice::from_ref(&dir));

        assert!(set.is_ignored(&dir.join("file1.txt"), false));
        assert!(!set.is_ignored(&dir.join("file10.txt"), false));
        assert!(set.is_ignored(&dir.join("foo7.dat"), false));
        assert!(!set.is_ignored(&dir.join("foox.dat"), false));
        assert!(set.is_ignored(&dir.join("bspecial"), false));
        assert!(!set.is_ignored(&dir.join("aspecial"), false));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn empty_set_never_ignores() {
        let set = IgnoreSet::empty();
        assert!(set.is_empty());
        assert!(!set.is_ignored(Path::new("/anything/at/all"), false));
    }

    #[test]
    fn paths_outside_rule_dirs_are_not_ignored() {
        let dir = temp("outside");
        write_ignore(&dir, ".gitignore", "*\n");
        let set = IgnoreSet::from_directories(std::slice::from_ref(&dir));
        assert!(set.is_ignored(&dir.join("x.txt"), false));
        assert!(!set.is_ignored(Path::new("/elsewhere/x.txt"), false));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn double_star_forms() {
        let dir = temp("doublestar");
        write_ignore(&dir, ".gitignore", "a/**/b\n**/vendor/\ndocs/**\n");
        let set = IgnoreSet::from_directories(std::slice::from_ref(&dir));

        assert!(set.is_ignored(&dir.join("a/b"), true));
        assert!(set.is_ignored(&dir.join("a/x/y/b"), true));
        assert!(set.is_ignored(&dir.join("x/vendor"), true));
        assert!(set.is_ignored(&dir.join("vendor"), true));
        assert!(set.is_ignored(&dir.join("docs/a/b.txt"), false));
        assert!(!set.is_ignored(&dir.join("docsalot/a.txt"), false));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn mnemoignore_and_multiple_ignore_files_coexist() {
        let dir = temp("coexist");
        write_ignore(
            &dir,
            ".gitignore",
            "git_ignored.txt
",
        );
        write_ignore(
            &dir,
            ".mnemoignore",
            "mnemo_ignored.txt
",
        );
        let set = IgnoreSet::from_directories(std::slice::from_ref(&dir));

        assert!(set.is_ignored(&dir.join("git_ignored.txt"), false));
        assert!(set.is_ignored(&dir.join("mnemo_ignored.txt"), false));
        assert!(!set.is_ignored(&dir.join("allowed.txt"), false));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parent_directory_exclusion_cannot_be_reincluded() {
        let dir = temp("parent_reinclude");
        // Git rule: if a parent directory is excluded, child files cannot re-include themselves
        write_ignore(
            &dir,
            ".gitignore",
            "ignored_dir/
!ignored_dir/keep.txt
",
        );
        let set = IgnoreSet::from_directories(std::slice::from_ref(&dir));

        assert!(set.is_ignored(&dir.join("ignored_dir"), true));
        assert!(
            set.is_ignored(&dir.join("ignored_dir/keep.txt"), false),
            "file inside ignored parent dir must remain ignored"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pattern_trailing_whitespace_is_trimmed() {
        let dir = temp("whitespace_trim");
        // Lines with spaces or tabs after pattern
        write_ignore(
            &dir,
            ".gitignore",
            "build   
*.cache		
",
        );
        let set = IgnoreSet::from_directories(std::slice::from_ref(&dir));

        assert!(set.is_ignored(&dir.join("build"), true));
        assert!(set.is_ignored(&dir.join("foo.cache"), false));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn glob_character_classes_and_ranges() {
        let dir = temp("char_classes");
        write_ignore(
            &dir,
            ".gitignore",
            "img_[0-9][0-9].png
alpha_[a-z].txt
non_num_[^0-9].bin
unterminated_[abc
",
        );
        let set = IgnoreSet::from_directories(std::slice::from_ref(&dir));

        assert!(set.is_ignored(&dir.join("img_42.png"), false));
        assert!(!set.is_ignored(&dir.join("img_4a.png"), false));
        assert!(set.is_ignored(&dir.join("alpha_q.txt"), false));
        assert!(!set.is_ignored(&dir.join("alpha_1.txt"), false));
        assert!(set.is_ignored(&dir.join("non_num_x.bin"), false));
        assert!(!set.is_ignored(&dir.join("non_num_5.bin"), false));
        // Unterminated bracket class matches literal '['
        assert!(set.is_ignored(&dir.join("unterminated_[abc"), false));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unanchored_wildcards_match_arbitrary_depth() {
        let dir = temp("deep_wildcards");
        write_ignore(
            &dir,
            ".gitignore",
            "*.bak
",
        );
        let set = IgnoreSet::from_directories(std::slice::from_ref(&dir));

        assert!(set.is_ignored(&dir.join("root.bak"), false));
        assert!(set.is_ignored(&dir.join("a/b/c/d/deep.bak"), false));
        assert!(!set.is_ignored(&dir.join("a/b/c/d/deep.bak.txt"), false));
        fs::remove_dir_all(&dir).unwrap();
    }
}
