use std::{error::Error, fmt, num::NonZeroUsize, str::FromStr};

use sha2::{Digest, Sha256};
use unicode_casefold::UnicodeCaseFold;
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};
use unicode_script::{Script, UnicodeScript};

use crate::{CONTENT_KEY_VERSION, LanguageCode, NORMALIZATION_VERSION};

/// The conservative V1 content classification.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TextKind {
    Word,
    Text,
}

impl TextKind {
    #[must_use]
    pub const fn protocol_name(self) -> &'static str {
        match self {
            Self::Word => "word",
            Self::Text => "text",
        }
    }
}

/// Caller-supplied validation limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ValidationPolicy {
    max_characters: NonZeroUsize,
}

impl ValidationPolicy {
    #[must_use]
    pub const fn new(max_characters: NonZeroUsize) -> Self {
        Self { max_characters }
    }

    #[must_use]
    pub const fn max_characters(self) -> NonZeroUsize {
        self.max_characters
    }
}

/// A validated and canonicalized source Content value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedContent {
    source_text: String,
    canonical: CanonicalContent,
    content_key: ContentKey,
}

impl PreparedContent {
    #[must_use]
    pub fn source_text(&self) -> &str {
        &self.source_text
    }

    #[must_use]
    pub const fn kind(&self) -> TextKind {
        self.canonical.kind
    }

    #[must_use]
    pub fn source_lang(&self) -> &LanguageCode {
        &self.canonical.source_lang
    }

    #[must_use]
    pub fn canonical_text(&self) -> &str {
        &self.canonical.text
    }

    #[must_use]
    pub const fn key_version(&self) -> u32 {
        self.canonical.key_version
    }

    #[must_use]
    pub const fn normalization_version(&self) -> u32 {
        self.canonical.normalization_version
    }

    #[must_use]
    pub const fn content_key(&self) -> ContentKey {
        self.content_key
    }
}

/// The exact fields used to construct a V1 Content identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonicalContent {
    key_version: u32,
    normalization_version: u32,
    kind: TextKind,
    source_lang: LanguageCode,
    text: String,
}

impl CanonicalContent {
    #[must_use]
    pub fn content_key(&self) -> ContentKey {
        let mut digest = Sha256::new();
        digest.update(self.key_version.to_string().as_bytes());
        digest.update([0]);
        digest.update(self.kind.protocol_name().as_bytes());
        digest.update([0]);
        digest.update(self.source_lang.as_str().as_bytes());
        digest.update([0]);
        digest.update(self.text.as_bytes());
        ContentKey(digest.finalize().into())
    }
}

/// A binary SHA-256 Content key rendered as lowercase hexadecimal at protocol boundaries.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ContentKey([u8; 32]);

impl ContentKey {
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    #[must_use]
    pub fn to_hex(self) -> String {
        use fmt::Write as _;

        let mut output = String::with_capacity(64);
        for byte in self.0 {
            let result = write!(&mut output, "{byte:02x}");
            debug_assert!(result.is_ok(), "writing to String cannot fail");
        }
        output
    }
}

impl fmt::Display for ContentKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for ContentKey {
    type Err = ContentKeyParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 64 {
            return Err(ContentKeyParseError::InvalidLength);
        }
        let mut bytes = [0_u8; 32];
        for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
            bytes[index] = (decode_hex(pair[0])? << 4) | decode_hex(pair[1])?;
        }
        Ok(Self(bytes))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentKeyParseError {
    InvalidLength,
    InvalidHex,
}

impl fmt::Display for ContentKeyParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidLength => "content key must contain 64 hexadecimal characters",
            Self::InvalidHex => "content key contains a non-hexadecimal character",
        })
    }
}

impl Error for ContentKeyParseError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationError {
    Empty,
    TooLong { max_characters: usize },
    NoLatinText,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("captured text is empty"),
            Self::TooLong { max_characters } => {
                write!(
                    formatter,
                    "captured text exceeds {max_characters} characters"
                )
            }
            Self::NoLatinText => formatter.write_str("captured text contains no Latin-script text"),
        }
    }
}

impl Error for ValidationError {}

/// Cleans, validates, classifies, canonicalizes, and keys captured text.
///
/// # Errors
///
/// Returns [`ValidationError`] when the cleaned input is empty, exceeds the caller's Provider
/// limit, or contains no Latin-script letter for the V1 English-to-Chinese direction.
pub fn prepare_content(
    raw_text: &str,
    source_lang: LanguageCode,
    policy: ValidationPolicy,
) -> Result<PreparedContent, ValidationError> {
    let source_text = clean_control_characters(raw_text);
    let source_text = source_text.trim();
    if source_text.is_empty() {
        return Err(ValidationError::Empty);
    }

    let character_count = source_text.chars().count();
    if character_count > policy.max_characters.get() {
        return Err(ValidationError::TooLong {
            max_characters: policy.max_characters.get(),
        });
    }
    if !source_text
        .chars()
        .any(|character| character.script() == Script::Latin && character.is_alphabetic())
    {
        return Err(ValidationError::NoLatinText);
    }

    let kind = classify(source_text);
    let canonical_text = canonicalize(source_text, kind);
    let canonical = CanonicalContent {
        key_version: CONTENT_KEY_VERSION,
        normalization_version: NORMALIZATION_VERSION,
        kind,
        source_lang,
        text: canonical_text,
    };
    let content_key = canonical.content_key();

    Ok(PreparedContent {
        source_text: source_text.to_owned(),
        canonical,
        content_key,
    })
}

fn clean_control_characters(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control() || character.is_whitespace())
        .collect()
}

fn classify(value: &str) -> TextKind {
    let token = trim_terminal_word_punctuation(value);
    if !token.is_empty()
        && token.chars().any(char::is_alphabetic)
        && token.chars().all(is_word_character)
    {
        TextKind::Word
    } else {
        TextKind::Text
    }
}

fn is_word_character(character: char) -> bool {
    character.is_alphanumeric()
        || is_combining_mark(character)
        || matches!(character, '-' | '\'' | '’')
}

fn trim_terminal_word_punctuation(value: &str) -> &str {
    value.trim_end_matches(['.', ',', '!', '?', ':', ';'])
}

fn canonicalize(value: &str, kind: TextKind) -> String {
    match kind {
        TextKind::Word => collapse_whitespace(trim_terminal_word_punctuation(value).trim())
            .nfc()
            .case_fold()
            .nfc()
            .collect(),
        TextKind::Text => collapse_whitespace(value).nfc().collect(),
    }
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_hex(byte: u8) -> Result<u8, ContentKeyParseError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(ContentKeyParseError::InvalidHex),
    }
}

#[cfg(test)]
#[allow(
    clippy::panic,
    reason = "test fixture failures require contextual panic messages"
)]
mod tests {
    use super::*;

    fn policy() -> ValidationPolicy {
        ValidationPolicy::new(NonZeroUsize::new(1_000).unwrap_or(NonZeroUsize::MIN))
    }

    fn english() -> LanguageCode {
        LanguageCode::parse("en").unwrap_or_else(|error| panic!("fixture failed: {error}"))
    }

    fn prepare(value: &str) -> Result<PreparedContent, ValidationError> {
        prepare_content(value, english(), policy())
    }

    #[test]
    fn rejects_empty_punctuation_numeric_and_non_latin_inputs() {
        for value in ["", "     ", "\n\n", "...", "12345", "完整中文句子"] {
            assert!(prepare(value).is_err(), "accepted {value:?}");
        }
    }

    #[test]
    fn accepts_research_terms_and_classifies_conservatively() {
        for value in [
            "invariant",
            "invariant.",
            "GPT-5",
            "HRTF",
            "LiDAR",
            "speaker-invariant",
            "transformer-based",
            "naïve",
        ] {
            let content = prepare(value).unwrap_or_else(|error| panic!("{value:?}: {error}"));
            assert_eq!(content.kind(), TextKind::Word, "{value:?}");
        }

        for value in [
            "remain invariant",
            "speaker-invariant representation",
            "The representation should remain invariant.",
            "complete 英文 mixed content",
        ] {
            let content = prepare(value).unwrap_or_else(|error| panic!("{value:?}: {error}"));
            assert_eq!(content.kind(), TextKind::Text, "{value:?}");
        }
    }

    #[test]
    fn word_equivalents_share_canonical_text_and_key() {
        let values = ["Invariant", "invariant", " invariant ", "invariant."];
        let prepared =
            values.map(|value| prepare(value).unwrap_or_else(|error| panic!("{value:?}: {error}")));
        let expected = prepared[0].content_key();
        for content in prepared {
            assert_eq!(content.canonical_text(), "invariant");
            assert_eq!(content.content_key(), expected);
        }
    }

    #[test]
    fn text_collapses_whitespace_but_preserves_case_and_punctuation() {
        let first = prepare("The representation   should remain invariant.")
            .unwrap_or_else(|error| panic!("fixture failed: {error}"));
        let second = prepare("The representation\nshould remain invariant.")
            .unwrap_or_else(|error| panic!("fixture failed: {error}"));
        let lowercase = prepare("the representation should remain invariant.")
            .unwrap_or_else(|error| panic!("fixture failed: {error}"));

        assert_eq!(
            first.canonical_text(),
            "The representation should remain invariant."
        );
        assert_eq!(first.content_key(), second.content_key());
        assert_ne!(first.content_key(), lowercase.content_key());
    }

    #[test]
    fn nfc_equivalents_share_identity() {
        let composed = prepare("naïve").unwrap_or_else(|error| panic!("fixture failed: {error}"));
        let decomposed =
            prepare("nai\u{308}ve").unwrap_or_else(|error| panic!("fixture failed: {error}"));
        assert_eq!(composed.canonical_text(), decomposed.canonical_text());
        assert_eq!(composed.content_key(), decomposed.content_key());
    }

    #[test]
    fn full_unicode_case_fold_is_stable() {
        let sharp_s = prepare("Straße").unwrap_or_else(|error| panic!("fixture failed: {error}"));
        let expanded = prepare("STRASSE").unwrap_or_else(|error| panic!("fixture failed: {error}"));
        assert_eq!(sharp_s.canonical_text(), "strasse");
        assert_eq!(sharp_s.content_key(), expanded.content_key());
    }

    #[test]
    fn source_language_and_kind_are_part_of_identity() {
        let en = prepare("gift").unwrap_or_else(|error| panic!("fixture failed: {error}"));
        let de = prepare_content(
            "gift",
            LanguageCode::parse("de").unwrap_or_else(|error| panic!("fixture failed: {error}")),
            policy(),
        )
        .unwrap_or_else(|error| panic!("fixture failed: {error}"));
        let text = prepare("gift word").unwrap_or_else(|error| panic!("fixture failed: {error}"));

        assert_ne!(en.content_key(), de.content_key());
        assert_ne!(en.content_key(), text.content_key());
    }

    #[test]
    fn enforces_character_limit_after_control_cleanup_and_trim() {
        let limited = ValidationPolicy::new(NonZeroUsize::new(3).unwrap_or(NonZeroUsize::MIN));
        assert!(prepare_content(" abc ", english(), limited).is_ok());
        assert_eq!(
            prepare_content("abcd", english(), limited),
            Err(ValidationError::TooLong { max_characters: 3 })
        );
    }

    #[test]
    fn content_key_round_trips_lowercase_hex() {
        let key = prepare("invariant")
            .unwrap_or_else(|error| panic!("fixture failed: {error}"))
            .content_key();
        let text = key.to_hex();
        assert_eq!(text.len(), 64);
        assert_eq!(ContentKey::from_str(&text), Ok(key));
        assert_eq!(
            ContentKey::from_str(&text.to_uppercase()),
            Err(ContentKeyParseError::InvalidHex)
        );
    }

    #[test]
    fn removes_non_whitespace_control_characters() {
        let content =
            prepare("invar\u{0}iant").unwrap_or_else(|error| panic!("fixture failed: {error}"));
        assert_eq!(content.source_text(), "invariant");
        assert_eq!(content.canonical_text(), "invariant");
    }

    #[test]
    fn canonicalization_is_idempotent_for_representative_inputs() {
        for value in [
            "Invariant.",
            "GPT-5",
            "Straße",
            "The representation\nshould remain invariant.",
            "speaker-invariant representation",
        ] {
            let first = prepare(value).unwrap_or_else(|error| panic!("{value:?}: {error}"));
            let second = prepare(first.canonical_text())
                .unwrap_or_else(|error| panic!("canonical {value:?}: {error}"));
            assert_eq!(first.kind(), second.kind());
            assert_eq!(first.canonical_text(), second.canonical_text());
            assert_eq!(first.content_key(), second.content_key());
        }
    }

    #[test]
    fn text_whitespace_variants_share_identity() {
        let variants = [
            "remain invariant",
            " remain invariant ",
            "remain\tinvariant",
            "remain\ninvariant",
            "remain\r\n\t invariant",
        ];
        let expected = prepare(variants[0])
            .unwrap_or_else(|error| panic!("fixture failed: {error}"))
            .content_key();
        for value in variants {
            let content = prepare(value).unwrap_or_else(|error| panic!("{value:?}: {error}"));
            assert_eq!(content.canonical_text(), "remain invariant");
            assert_eq!(content.content_key(), expected);
        }
    }
}
