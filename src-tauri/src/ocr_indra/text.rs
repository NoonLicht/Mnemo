//! Text side of the Indra OCR port: CTC decoding, Hangul composition and
//! right-to-left reordering (`vocab.py`, `rtl.py`).

/// A recognizer's token list: class 0 is the CTC blank, class i + 1 is
/// `chars[i]` (a character, or a LaTeX command in the base vocabulary).
pub struct Vocab {
    chars: Vec<String>,
    has_commands: bool,
    jamo: bool,
}

impl Vocab {
    /// Parses one of meta.json's `vocab*` entries: a JSON string holding
    /// `{"blank": 0, "chars": [...]}`.
    pub fn from_meta(entry: &serde_json::Value) -> Result<Self, String> {
        let inner: serde_json::Value = match entry {
            serde_json::Value::String(s) => {
                serde_json::from_str(s).map_err(|e| format!("vocab: {e}"))?
            }
            other => other.clone(),
        };
        if inner.get("blank").and_then(|b| b.as_u64()).unwrap_or(0) != 0 {
            return Err("vocab: only blank = 0 is supported".into());
        }
        let chars: Vec<String> = inner
            .get("chars")
            .and_then(|c| c.as_array())
            .ok_or("vocab: no chars")?
            .iter()
            .map(|c| c.as_str().unwrap_or_default().to_string())
            .collect();
        Ok(Self {
            has_commands: chars.iter().any(|c| c.chars().count() > 1),
            jamo: chars.iter().any(|c| c == "\u{1100}"),
            chars,
        })
    }

    pub fn token(&self, id: i64) -> Option<&str> {
        usize::try_from(id - 1)
            .ok()
            .and_then(|i| self.chars.get(i))
            .map(String::as_str)
    }

    /// Text of the base recognizer's token ids (`Vocab.decode`).
    pub fn decode(&self, ids: &[i64]) -> String {
        let toks: Vec<&str> = ids.iter().filter_map(|&i| self.token(i)).collect();
        if self.jamo {
            return compose_hangul(&toks.concat());
        }
        let mut out = String::new();
        for (k, t) in toks.iter().enumerate() {
            out.push_str(t);
            // "\alpha" + "x" must not read back as "\alphax".
            let command = self.has_commands
                && t.len() > 2
                && t.starts_with('\\')
                && t[1..].chars().all(char::is_alphabetic);
            let next_alpha = toks
                .get(k + 1)
                .and_then(|n| n.chars().next())
                .is_some_and(char::is_alphabetic);
            if command && next_alpha {
                out.push(' ');
            }
        }
        out
    }

    /// Text of a script-group recognizer's tokens: whitespace collapsed,
    /// jamo composed. Right-to-left lines are still in visual order.
    pub fn decode_chars(&self, ids: &[i64]) -> String {
        let raw: String = ids.iter().filter_map(|&i| self.token(i)).collect();
        compose_hangul(&raw.split_whitespace().collect::<Vec<_>>().join(" "))
    }
}

/// NFC for Hangul only: conjoining jamo (L V [T]) become syllables. The CJK
/// recognizer reads Korean as jamo; search needs the composed syllables.
pub fn compose_hangul(text: &str) -> String {
    const S_BASE: u32 = 0xAC00;
    const L_BASE: u32 = 0x1100;
    const V_BASE: u32 = 0x1161;
    const T_BASE: u32 = 0x11A7;
    const L_COUNT: u32 = 19;
    const V_COUNT: u32 = 21;
    const T_COUNT: u32 = 28;
    let mut out: Vec<char> = Vec::with_capacity(text.len());
    for c in text.chars() {
        let code = c as u32;
        if let Some(&last) = out.last() {
            let prev = last as u32;
            if (L_BASE..L_BASE + L_COUNT).contains(&prev)
                && (V_BASE..V_BASE + V_COUNT).contains(&code)
            {
                let s = S_BASE + ((prev - L_BASE) * V_COUNT + (code - V_BASE)) * T_COUNT;
                *out.last_mut().unwrap() = char::from_u32(s).unwrap_or(last);
                continue;
            }
            let lv = prev.wrapping_sub(S_BASE);
            if lv < L_COUNT * V_COUNT * T_COUNT
                && lv.is_multiple_of(T_COUNT)
                && (T_BASE + 1..T_BASE + T_COUNT).contains(&code)
            {
                *out.last_mut().unwrap() = char::from_u32(prev + code - T_BASE).unwrap_or(last);
                continue;
            }
        }
        out.push(c);
    }
    out.into_iter().collect()
}

/// Combining marks of the right-to-left scripts and generic diacritics: they
/// stay after their base character when a run is reversed.
fn is_mark(c: char) -> bool {
    matches!(c as u32,
        0x0300..=0x036F | 0x0591..=0x05BD | 0x05BF | 0x05C1..=0x05C2 | 0x05C4..=0x05C5
        | 0x05C7 | 0x0610..=0x061A | 0x064B..=0x065F | 0x0670 | 0x06D6..=0x06DC
        | 0x06DF..=0x06E4 | 0x06E7..=0x06E8 | 0x06EA..=0x06ED)
}

fn is_arabic_digit(c: char) -> bool {
    matches!(c as u32, 0x0660..=0x0669 | 0x06F0..=0x06F9)
}

/// Bidi class R or AL (Hebrew, Arabic, Syriac, Thaana, N'Ko and their
/// presentation forms), without marks and digits.
pub fn is_rtl(c: char) -> bool {
    matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF)
        && !is_mark(c)
        && !is_arabic_digit(c)
        && !c.is_whitespace()
}

/// Bidi class L, EN or AN: reads left to right inside right-to-left text.
fn is_ltr(c: char) -> bool {
    !is_rtl(c) && !is_mark(c) && c.is_alphanumeric()
}

/// Logical order from visual order (`rtl.flip`; the operation is its own
/// inverse). `base_rtl`: the page reads right to left.
pub fn flip(text: &str, base_rtl: bool) -> String {
    if !text.chars().any(is_rtl) {
        return text.to_string();
    }
    let mut clusters: Vec<String> = Vec::new();
    for c in text.chars() {
        match clusters.last_mut() {
            Some(last) if is_mark(c) => last.push(c),
            _ => clusters.push(c.to_string()),
        }
    }
    let kind = |cl: &String| {
        let c = cl.chars().next().unwrap_or(' ');
        if is_rtl(c) {
            'R'
        } else if is_ltr(c) {
            'L'
        } else {
            'N'
        }
    };
    let kinds: Vec<char> = clusters.iter().map(kind).collect();
    // [start, end) spans from a `strong` cluster to the last `strong` one
    // before a `stop` cluster.
    let runs = |strong: char, stop: char| {
        let mut spans = Vec::new();
        let (mut start, mut last) = (None, 0);
        for (i, &k) in kinds.iter().chain(std::iter::once(&'#')).enumerate() {
            if k == strong {
                start.get_or_insert(i);
                last = i;
            } else if (k == stop || k == '#')
                && let Some(s) = start.take()
            {
                spans.push((s, last + 1));
            }
        }
        spans
    };
    let mirror = |cl: &str| {
        let mut chars = cl.chars();
        let first = match chars.next() {
            Some('(') => ')',
            Some(')') => '(',
            Some('[') => ']',
            Some(']') => '[',
            Some('{') => '}',
            Some('}') => '{',
            Some('<') => '>',
            Some('>') => '<',
            Some('«') => '»',
            Some('»') => '«',
            Some(c) => c,
            None => return String::new(),
        };
        std::iter::once(first).chain(chars).collect::<String>()
    };
    if base_rtl {
        // Units: left-to-right runs kept whole, every other cluster alone;
        // the units reverse.
        let mut units: Vec<String> = Vec::new();
        let mut i = 0;
        for (a, b) in runs('L', 'R')
            .into_iter()
            .chain([(clusters.len(), clusters.len())])
        {
            units.extend(clusters[i..a].iter().map(|c| mirror(c)));
            if a < b {
                units.push(clusters[a..b].concat());
            }
            i = b;
        }
        return units.into_iter().rev().collect();
    }
    // Left-to-right base: each right-to-left run reverses in place.
    let mut out = clusters.clone();
    for (a, b) in runs('R', 'L') {
        for (k, cl) in clusters[a..b].iter().rev().enumerate() {
            out[a + k] = mirror(cl);
        }
    }
    out.concat()
}

/// Whether most letters on the page are right-to-left.
pub fn page_is_rtl<'a>(texts: impl Iterator<Item = &'a str>) -> bool {
    let (mut rtl, mut letters) = (0usize, 0usize);
    for c in texts.flat_map(str::chars).filter(|c| c.is_alphabetic()) {
        letters += 1;
        rtl += usize::from(is_rtl(c));
    }
    rtl > letters - rtl
}

/// A line of leader characters only (dot leaders, fill-in rules, specks)
/// carries no text (`tidy_lines`). A short lone dash stays: it can be an
/// empty table cell.
pub fn is_leader_line(text: &str, width: f32, height: f32) -> bool {
    const LEADERS: &str = ".·…‥_-–—=";
    let core: Vec<char> = text.chars().filter(|&c| c != ' ').collect();
    if core.is_empty() || !core.iter().all(|c| LEADERS.contains(*c)) {
        return false;
    }
    core.len() >= 3 || width >= 3.0 * height || !core.iter().any(|c| "-–—".contains(*c))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vocab(chars: &[&str]) -> Vocab {
        let meta = serde_json::json!({"blank": 0, "chars": chars}).to_string();
        Vocab::from_meta(&serde_json::Value::String(meta)).unwrap()
    }

    #[test]
    fn decodes_ids_and_spaces_latex_commands() {
        let v = vocab(&["a", "b", " ", "\\alpha", "x"]);
        assert_eq!(v.decode(&[1, 3, 2]), "a b");
        assert_eq!(v.decode(&[4, 5]), "\\alpha x");
        assert_eq!(v.decode(&[0, 99]), "", "blank and unknown ids are dropped");
        assert_eq!(v.decode_chars(&[3, 1, 3, 3, 2, 3]), "a b");
    }

    #[test]
    fn hangul_jamo_compose_into_syllables() {
        // ᄒ ᅡ ᆫ ᄀ ᅳ ᆯ -> 한글
        assert_eq!(
            compose_hangul("\u{1112}\u{1161}\u{11AB}\u{1100}\u{1173}\u{11AF}"),
            "한글"
        );
        assert_eq!(compose_hangul("abc"), "abc");
    }

    #[test]
    fn rtl_flip_matches_the_reference_and_is_its_own_inverse() {
        // Visual "םולש" (as the recognizer reads it) is logical "שלום".
        assert_eq!(flip("םולש", true), "שלום");
        // Latin and digits keep their order inside a right-to-left line.
        assert_eq!(flip("2024 םולש", true), "שלום 2024");
        // Left-to-right base: only the Hebrew run reverses.
        assert_eq!(flip("see םולש now", false), "see שלום now");
        for (text, base) in [("(בא) abc 12", true), ("x אב y", false)] {
            assert_eq!(flip(&flip(text, base), base), text);
        }
        assert!(page_is_rtl(["שלום", "abc"].into_iter()));
        assert!(!page_is_rtl(["hello", "שלום"].into_iter()));
    }

    #[test]
    fn leader_lines_are_dropped_but_lone_dashes_stay() {
        assert!(is_leader_line("......", 50.0, 10.0));
        assert!(is_leader_line(".", 5.0, 10.0));
        assert!(!is_leader_line("-", 5.0, 10.0));
        assert!(!is_leader_line("a.b", 50.0, 10.0));
    }
}
