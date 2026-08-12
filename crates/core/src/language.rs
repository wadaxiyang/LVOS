use std::{error::Error, fmt, str::FromStr};

/// A validated protocol language code.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct LanguageCode(String);

impl LanguageCode {
    /// Parses a compact BCP-47-compatible language code.
    ///
    /// This intentionally validates only protocol-safe syntax. It does not maintain a registry of
    /// every language tag assigned by IANA.
    ///
    /// # Errors
    ///
    /// Returns [`LanguageCodeError`] for an empty, overlong, or malformed code.
    pub fn parse(value: &str) -> Result<Self, LanguageCodeError> {
        if value.is_empty() {
            return Err(LanguageCodeError::Empty);
        }
        if value.len() > 35 {
            return Err(LanguageCodeError::TooLong);
        }
        if !value.is_ascii() {
            return Err(LanguageCodeError::InvalidCharacter);
        }

        let mut parts = value.split('-');
        let primary = parts.next().ok_or(LanguageCodeError::Empty)?;
        if !(2..=8).contains(&primary.len())
            || !primary.bytes().all(|byte| byte.is_ascii_alphabetic())
        {
            return Err(LanguageCodeError::InvalidPrimarySubtag);
        }

        for part in parts {
            if part.is_empty()
                || part.len() > 8
                || !part.bytes().all(|byte| byte.is_ascii_alphanumeric())
            {
                return Err(LanguageCodeError::InvalidSubtag);
            }
        }

        Ok(Self(canonicalize_tag(value)))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LanguageCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for LanguageCode {
    type Err = LanguageCodeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LanguageCodeError {
    Empty,
    TooLong,
    InvalidCharacter,
    InvalidPrimarySubtag,
    InvalidSubtag,
}

impl fmt::Display for LanguageCodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "language code is empty",
            Self::TooLong => "language code is too long",
            Self::InvalidCharacter => "language code contains a non-ASCII character",
            Self::InvalidPrimarySubtag => "language code has an invalid primary subtag",
            Self::InvalidSubtag => "language code has an invalid subtag",
        })
    }
}

impl Error for LanguageCodeError {}

fn canonicalize_tag(value: &str) -> String {
    value
        .split('-')
        .enumerate()
        .map(|(index, part)| {
            if index == 0 {
                part.to_ascii_lowercase()
            } else if part.len() == 2 && part.bytes().all(|byte| byte.is_ascii_alphabetic()) {
                part.to_ascii_uppercase()
            } else {
                part.to_ascii_lowercase()
            }
        })
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_v1_language_codes() {
        assert_eq!(
            LanguageCode::parse("EN").map(|code| code.0),
            Ok("en".into())
        );
        assert_eq!(
            LanguageCode::parse("zh-cn").map(|code| code.0),
            Ok("zh-CN".into())
        );
    }

    #[test]
    fn rejects_malformed_language_codes() {
        for value in ["", "e", "zh--CN", "中文", "en_Us", "english-toolongsubtag"] {
            assert!(LanguageCode::parse(value).is_err(), "accepted {value:?}");
        }
    }
}
