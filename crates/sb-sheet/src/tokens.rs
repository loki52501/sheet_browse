//! Token estimate shared by skeleton, find output and the reader (D-200), and the word
//! tokenizer the find scorer uses (B-6.2).
//! Owned by coder A (T-10).

/// Estimated model tokens for `text`: `ceil(chars / 4)` over Unicode scalar values (D-200).
pub fn estimate(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

/// Scoring tokenizer: lowercase, split on anything that is not alphanumeric (Unicode-aware),
/// drop empty pieces. `"E-mail address"` -> `["e", "mail", "address"]`.
pub(crate) fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// `words(text)` joined by single spaces: the normal form of table keys and query phrases.
pub(crate) fn normalize(text: &str) -> String {
    words(text).join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_is_ceil_chars_over_four() {
        assert_eq!(estimate(""), 0);
        assert_eq!(estimate("a"), 1);
        assert_eq!(estimate("abcd"), 1);
        assert_eq!(estimate("abcde"), 2);
        // chars, not bytes: four 2-byte chars are one token.
        assert_eq!(estimate("éééé"), 1);
    }

    #[test]
    fn words_lowercase_and_split() {
        assert_eq!(words("E-mail  Address!"), ["e", "mail", "address"]);
        assert_eq!(
            words("I agree to the Terms"),
            ["i", "agree", "to", "the", "terms"]
        );
        assert_eq!(words("Straße 12b"), ["straße", "12b"]);
        assert_eq!(words("--- ··· "), Vec::<String>::new());
        assert_eq!(words("Grüße·Ünïcode"), ["grüße", "ünïcode"]);
    }

    #[test]
    fn normalize_joins_words() {
        assert_eq!(normalize("  Sign-In "), "sign in");
        assert_eq!(normalize(""), "");
    }
}
