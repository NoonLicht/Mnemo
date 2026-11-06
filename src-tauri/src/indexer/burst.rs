//! Screenshot burst grouping.
//!
//! Bursts are deliberately conservative: an image joins an earlier burst only
//! when its perceptual hash and its text lines both look like the same page
//! (possibly scrolled), and only inside a short capture window in the same
//! directory. Distinct pages that share a sidebar must not collapse into one.

use std::collections::HashSet;

/// Images further apart than this are never considered part of one burst.
pub const BURST_WINDOW_SECS: u64 = 10 * 60;

/// Number of line hashes kept per image. Bottom-k sampling makes the signature
/// independent of line order and bounded for very long OCR output.
pub const MAX_SIGNATURE_LINES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextOverlap {
    pub shared: usize,
    pub a_lines: usize,
    pub b_lines: usize,
    /// Sørensen–Dice coefficient: `2 * shared / (a + b)`.
    pub dice: f32,
}

impl TextOverlap {
    fn usable(self) -> bool {
        self.a_lines >= 3 && self.b_lines >= 3
    }
}

/// Hashes normalized non-empty lines into a compact, sorted bottom-k signature.
///
/// Short labels such as sidebar items are kept: dropping them would make it
/// harder to tell a scrolled page from a different page with the same chrome.
pub fn line_signature(text: &str) -> Vec<u8> {
    let mut hashes: Vec<u64> = text
        .lines()
        .filter_map(normalize_line)
        .map(|line| fnv1a(&line))
        .collect();
    hashes.sort_unstable();
    hashes.dedup();
    hashes.truncate(MAX_SIGNATURE_LINES);

    let mut bytes = Vec::with_capacity(hashes.len() * 8);
    for hash in hashes {
        bytes.extend_from_slice(&hash.to_le_bytes());
    }
    bytes
}

fn normalize_line(line: &str) -> Option<String> {
    let mut normalized = String::with_capacity(line.len());
    let mut alphanumeric = 0usize;
    for character in line.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            alphanumeric += 1;
            normalized.push(character);
        } else if !normalized.ends_with(' ') {
            normalized.push(' ');
        }
    }
    if alphanumeric < 3 {
        return None;
    }
    Some(normalized.trim().to_string())
}

fn fnv1a(text: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn decode_signature(bytes: &[u8]) -> Vec<u64> {
    bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|chunk| u64::from_le_bytes(*chunk))
        .collect()
}

/// Computes line-set overlap between two stored signatures.
pub fn text_overlap(a: &[u8], b: &[u8]) -> TextOverlap {
    let a = decode_signature(a);
    let b = decode_signature(b);
    let mut shared = 0usize;
    if !a.is_empty() && !b.is_empty() {
        let set: HashSet<u64> = a.iter().copied().collect();
        shared = b.iter().filter(|hash| set.contains(hash)).count();
    }
    let total = a.len() + b.len();
    let dice = if total == 0 {
        0.0
    } else {
        (2.0 * shared as f32) / total as f32
    };
    TextOverlap {
        shared,
        a_lines: a.len(),
        b_lines: b.len(),
        dice,
    }
}

/// Decides whether a pair belongs to the same screenshot burst.
///
/// * Strong line overlap plus a close visual match (`<= 10` bits) is a burst.
/// * A farther visual match (`<= 32` bits) is allowed only when at least five
///   lines overlap strongly enough to explain scrolling.
/// * With too little text to compare, only a very close visual match counts.
pub fn is_same_burst(visual_distance: u32, text: TextOverlap) -> bool {
    let text_match = text.usable() && text.shared >= 3 && text.dice >= 0.70;
    if text_match && visual_distance <= 10 {
        return true;
    }
    if text.usable() && text.shared >= 5 && text.dice >= 0.70 && visual_distance <= 32 {
        return true;
    }
    !text.usable() && visual_distance <= 6
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_is_order_independent_and_bounded() {
        let a = line_signature("alpha beta\nGamma delta\nepsilon zeta\n");
        let b = line_signature("epsilon zeta\n\ngamma delta\nalpha beta");
        assert_eq!(a, b);

        let long = (0..200)
            .map(|i| format!("content line number {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(line_signature(&long).len(), MAX_SIGNATURE_LINES * 8);
    }

    #[test]
    fn short_and_empty_lines_are_ignored() {
        assert!(line_signature("").is_empty());
        assert!(line_signature("\n\n -- \n").is_empty());
        assert!(!line_signature("hello world\n").is_empty());
    }

    struct LabeledPair {
        label: &'static str,
        left: &'static str,
        right: &'static str,
        visual_distance: u32,
        same: bool,
    }

    #[test]
    fn labeled_fixture_keeps_distinct_pages_apart() {
        let sidebar = "Home\nSearch\nSettings\nProfile\nHelp\nAbout\nPrivacy\nTerms";
        let cases = vec![
            LabeledPair {
                label: "identical screenshot",
                left: "Quarterly revenue\nTotal 42\nGrowth 12%\nNext steps",
                right: "Quarterly revenue\nTotal 42\nGrowth 12%\nNext steps",
                visual_distance: 0,
                same: true,
            },
            LabeledPair {
                label: "scrolled page",
                left: "Header\nalpha one\nbeta two\ngamma three\ndelta four\nepsilon five\nzeta six\neta seven\ntheta eight",
                right: "gamma three\ndelta four\nepsilon five\nzeta six\neta seven\ntheta eight\niota nine\nkappa ten",
                visual_distance: 24,
                same: true,
            },
            LabeledPair {
                label: "distinct pages sharing a sidebar",
                left: "Home\nSearch\nSettings\nProfile\nHelp\nAbout\nPrivacy\nTerms\nQuarterly revenue\nTotal 42\nGrowth 12%\nNext steps",
                right: "Home\nSearch\nSettings\nProfile\nHelp\nAbout\nPrivacy\nTerms\nWeather today\nRain expected\nWindy afternoon\nUmbrella needed",
                visual_distance: 12,
                same: false,
            },
            LabeledPair {
                label: "same chrome, different article",
                left: "Home\nSearch\nSettings\nProject Atlas\nRelease notes\nBug fixes\nPerformance",
                right: "Home\nSearch\nSettings\nProject Borealis\nDesign review\nTypography\nSpacing",
                visual_distance: 8,
                same: false,
            },
        ];

        // Keep the literal in the fixture so it is obvious what the shared
        // chrome is even if the cases above evolve.
        assert!(sidebar.contains("Settings"));

        for case in cases {
            let overlap = text_overlap(&line_signature(case.left), &line_signature(case.right));
            assert_eq!(
                is_same_burst(case.visual_distance, overlap),
                case.same,
                "{} (shared {}, dice {:.2}, distance {})",
                case.label,
                overlap.shared,
                overlap.dice,
                case.visual_distance
            );
        }
    }

    #[test]
    fn imageless_text_falls_back_to_strong_visual_only() {
        let none = text_overlap(&[], &[]);
        assert!(is_same_burst(4, none));
        assert!(!is_same_burst(12, none));
    }
}
