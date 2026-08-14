use lvos_core::ContentKey;
use lvos_translation::LookupCardErrorKind;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MainSection {
    History,
    Favorites,
    Settings(SettingsSection),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsSection {
    General,
    Translation,
    Account,
    Sync,
    Devices,
    History,
    Data,
    Update,
}

impl SettingsSection {
    pub const ALL: [Self; 8] = [
        Self::General,
        Self::Translation,
        Self::Account,
        Self::Sync,
        Self::Devices,
        Self::History,
        Self::Data,
        Self::Update,
    ];

    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Translation => "Translation",
            Self::Account => "Account",
            Self::Sync => "Sync",
            Self::Devices => "Devices",
            Self::History => "History",
            Self::Data => "Data",
            Self::Update => "Update",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PopupFocusState {
    Hidden,
    VisibleNoActivate,
    Interactive,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LookupCardState {
    Hidden,
    Loading {
        generation: u64,
        source: String,
    },
    Ready {
        generation: u64,
        content_key: ContentKey,
        source: String,
        translation: String,
        favorite: bool,
        effective_query_count: u64,
    },
    Error {
        generation: u64,
        source: String,
        kind: LookupCardErrorKind,
    },
}

impl LookupCardState {
    #[must_use]
    pub const fn generation(&self) -> Option<u64> {
        match self {
            Self::Loading { generation, .. }
            | Self::Ready { generation, .. }
            | Self::Error { generation, .. } => Some(*generation),
            Self::Hidden => None,
        }
    }

    /// Applies a completion only when it belongs to the currently displayed generation.
    pub fn apply_if_current(&mut self, completion: Self) -> bool {
        if self.generation().is_some() && self.generation() == completion.generation() {
            *self = completion;
            true
        } else {
            false
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SyncUiState {
    LoginRequired,
    Idle,
    Syncing,
    Connected {
        last_server_revision: u64,
        pending_outbox: u64,
    },
    Disconnected {
        pending_outbox: u64,
    },
    Conflict {
        message: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceUiState {
    pub name: String,
    pub platform: String,
    pub last_seen: String,
    pub current: bool,
    pub revoked: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_settings_sections_are_present_in_frozen_order() {
        let titles = SettingsSection::ALL.map(SettingsSection::title);
        assert_eq!(
            titles,
            [
                "General",
                "Translation",
                "Account",
                "Sync",
                "Devices",
                "History",
                "Data",
                "Update"
            ]
        );
    }

    #[test]
    fn stale_lookup_completion_cannot_replace_current_card() {
        let mut state = LookupCardState::Loading {
            generation: 2,
            source: "new".to_owned(),
        };
        assert!(!state.apply_if_current(LookupCardState::Error {
            generation: 1,
            source: "old".to_owned(),
            kind: LookupCardErrorKind::TranslationUnavailable,
        }));
        assert_eq!(state.generation(), Some(2));
    }
}
