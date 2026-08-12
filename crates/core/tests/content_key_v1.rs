#![allow(
    clippy::panic,
    reason = "golden-vector fixture failures require contextual panic messages"
)]

use std::num::NonZeroUsize;

use lvos_core::{LanguageCode, ValidationPolicy, prepare_content};

struct GoldenVector {
    raw_text: &'static str,
    source_lang: &'static str,
    canonical_text: &'static str,
    content_key: &'static str,
}

const VECTORS: &[GoldenVector] = &[
    GoldenVector {
        raw_text: "Invariant.",
        source_lang: "en",
        canonical_text: "invariant",
        content_key: "e2dc78734bbb112a0af83415d971e4e73233c45245db2ff519760b4172377d78",
    },
    GoldenVector {
        raw_text: "GPT-5",
        source_lang: "en",
        canonical_text: "gpt-5",
        content_key: "a03ebaca865f5b279986ca62d8e3f8c863621388a9f444cd1d17b7b24043fa3e",
    },
    GoldenVector {
        raw_text: "The representation\nshould remain invariant.",
        source_lang: "en",
        canonical_text: "The representation should remain invariant.",
        content_key: "beaeee61e87755245b7d823d8ce5411b23c346f650e6a2276d15805e842d80d9",
    },
    GoldenVector {
        raw_text: "Gift",
        source_lang: "de",
        canonical_text: "gift",
        content_key: "22d42b049891f11bde6d773646501b2d885e909fc6c0c4de2c4438ed84f0603e",
    },
    GoldenVector {
        raw_text: "Straße",
        source_lang: "en",
        canonical_text: "strasse",
        content_key: "09d1051bab5693132f691ff8f206f298c0aa6fbc9419644d740e4a17c20a7bd7",
    },
];

#[test]
fn content_key_v1_matches_independently_generated_golden_vectors() {
    let policy = ValidationPolicy::new(NonZeroUsize::new(1_000).unwrap_or(NonZeroUsize::MIN));

    for vector in VECTORS {
        let language = LanguageCode::parse(vector.source_lang)
            .unwrap_or_else(|error| panic!("invalid fixture language: {error}"));
        let content = prepare_content(vector.raw_text, language, policy)
            .unwrap_or_else(|error| panic!("invalid fixture content: {error}"));
        assert_eq!(content.canonical_text(), vector.canonical_text);
        assert_eq!(content.content_key().to_hex(), vector.content_key);
    }
}
