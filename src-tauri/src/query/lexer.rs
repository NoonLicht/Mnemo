#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    Word(String),
    Quoted(String),
    KeyValue {
        negated: bool,
        key: String,
        value: String,
        quoted_value: bool,
    },
    NegatedWord(String),
    NegatedQuoted(String),
    Regex(String),
}

/// Inside quotes, a backslash escapes only `"` and another backslash. Any other
/// backslash is literal, so Windows paths (`in:"C:\Users\me"`) and regex
/// classes (`re:"\d+"`) survive as typed.
fn is_escape(chars: &[char], i: usize) -> bool {
    chars[i] == '\\' && matches!(chars.get(i + 1), Some('"') | Some('\\'))
}

pub fn tokenize(input: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        // Skip whitespace
        while i < len && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= len {
            break;
        }

        let negated = if chars[i] == '-' && i + 1 < len && !chars[i + 1].is_whitespace() {
            i += 1;
            true
        } else {
            false
        };

        // Check if starting quote
        if i < len && chars[i] == '"' {
            i += 1; // skip opening quote
            let mut text = String::new();
            while i < len && chars[i] != '"' {
                if is_escape(&chars, i) {
                    i += 1;
                }
                text.push(chars[i]);
                i += 1;
            }
            if i < len && chars[i] == '"' {
                i += 1; // skip closing quote
            }
            if negated {
                tokens.push(Token::NegatedQuoted(text));
            } else {
                tokens.push(Token::Quoted(text));
            }
            continue;
        }

        let start_pos = i;
        let mut has_colon = false;
        let mut colon_pos = None;

        while i < len && !chars[i].is_whitespace() {
            if chars[i] == ':' && !has_colon {
                has_colon = true;
                colon_pos = Some(i);
                if i + 1 < len && chars[i + 1] == '"' {
                    i += 1; // stop at ':'
                    break;
                }
            }
            i += 1;
        }

        if has_colon {
            let key = chars[start_pos..colon_pos.unwrap()]
                .iter()
                .collect::<String>();
            if i < len && chars[i] == '"' {
                i += 1; // skip opening quote
                let mut val = String::new();
                while i < len && chars[i] != '"' {
                    if is_escape(&chars, i) {
                        i += 1;
                    }
                    val.push(chars[i]);
                    i += 1;
                }
                if i < len && chars[i] == '"' {
                    i += 1; // skip closing quote
                }
                if key.eq_ignore_ascii_case("re") {
                    tokens.push(Token::Regex(val));
                } else {
                    tokens.push(Token::KeyValue {
                        negated,
                        key,
                        value: val,
                        quoted_value: true,
                    });
                }
            } else {
                let val = chars[(colon_pos.unwrap() + 1)..i]
                    .iter()
                    .collect::<String>();
                let trimmed_val = val.trim_matches(|c| c == '(' || c == ')').to_string();
                if key.eq_ignore_ascii_case("re") {
                    tokens.push(Token::Regex(trimmed_val));
                } else {
                    tokens.push(Token::KeyValue {
                        negated,
                        key,
                        value: trimmed_val,
                        quoted_value: false,
                    });
                }
            }
        } else {
            let word = chars[start_pos..i].iter().collect::<String>();
            let trimmed_word = word.trim_matches(|c| c == '(' || c == ')').to_string();
            if trimmed_word.is_empty() {
                continue;
            }
            if negated {
                tokens.push(Token::NegatedWord(trimmed_word));
            } else {
                tokens.push(Token::Word(trimmed_word));
            }
        }
    }

    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kv_value(input: &str) -> String {
        match tokenize(input).into_iter().next() {
            Some(Token::KeyValue { value, .. }) => value,
            other => panic!("expected key/value, got {other:?}"),
        }
    }

    #[test]
    fn quoted_windows_path_keeps_backslashes() {
        assert_eq!(kv_value(r#"in:"c:\Folder""#), r"c:\Folder");
        assert_eq!(
            kv_value(r#"in:"C:\Users\me\My Docs""#),
            r"C:\Users\me\My Docs"
        );
    }

    #[test]
    fn escaped_quote_and_backslash_still_unescape() {
        assert_eq!(kv_value(r#"in:"/tmp/a\"b\\c""#), r#"/tmp/a"b\c"#);
        assert_eq!(kv_value(r#"in:"C:\\Users\\me""#), r"C:\Users\me");
    }

    #[test]
    fn quoted_phrase_keeps_literal_backslash() {
        assert_eq!(
            tokenize(r#""C:\Temp""#),
            vec![Token::Quoted(r"C:\Temp".to_string())]
        );
    }

    #[test]
    fn regex_class_keeps_backslash() {
        assert_eq!(
            tokenize(r#"re:"\d+""#),
            vec![Token::Regex(r"\d+".to_string())]
        );
    }
}
