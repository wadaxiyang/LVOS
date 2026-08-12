/// A UTC Unix timestamp in whole seconds.
///
/// The type deliberately does not implement `Ord`: timestamps are behavior data, not Favorite or
/// synchronization conflict order. Callers performing learning-statistic min/max operations must
/// do so explicitly on [`Self::as_seconds`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct UnixTimestamp(i64);

impl UnixTimestamp {
    #[must_use]
    pub const fn from_seconds(seconds: i64) -> Self {
        Self(seconds)
    }

    #[must_use]
    pub const fn as_seconds(self) -> i64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_seconds_without_formatting() {
        let timestamp = UnixTimestamp::from_seconds(1_780_000_000);
        assert_eq!(timestamp.as_seconds(), 1_780_000_000);
    }
}
