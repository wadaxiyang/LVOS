use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt,
    num::NonZeroUsize,
    str::FromStr,
};

use lvos_core::{
    CONTENT_KEY_VERSION, ContentKey, EXPORT_FORMAT_VERSION, LanguageCode, MAX_PORTABLE_JSON_BYTES,
    MAX_PORTABLE_RECORDS, SOFTWARE_VERSION, TextKind, UnixTimestamp, ValidationPolicy,
    prepare_content,
};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    Favorite, HistoryEntry, ProfileDatabase, StorageError, StoredContent, TranslationSnapshot,
    profile::{
        FAVORITE_SELECT_FIELDS, HISTORY_SELECT, favorite_from_history_tx, favorite_tx,
        parse_favorite, parse_history, read_favorite, read_history, upsert_history,
    },
};

const PORTABLE_FORMAT: &str = "lvos-data-export";
const MAX_SOURCE_CHARACTERS: usize = 16_384;
const MAX_TRANSLATION_BYTES: usize = 65_536;
const MAX_PROVIDER_BYTES: usize = 128;
const MAX_USERNAME_BYTES: usize = 128;
const MAX_APP_VERSION_BYTES: usize = 64;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortableProfile {
    pub username: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortableContent {
    pub content_key: String,
    pub key_version: u32,
    pub kind: String,
    pub source_lang: String,
    pub source_text: String,
    pub canonical_text: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortableTranslation {
    pub target_lang: String,
    pub translation: String,
    pub provider: String,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortableHistory {
    pub content: PortableContent,
    pub translation: PortableTranslation,
    pub last_queried_at: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortableFavorite {
    pub content: PortableContent,
    pub translation: PortableTranslation,
    pub favorited_at: i64,
    pub updated_at: i64,
    pub deleted_at: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortableQueryStatsArchive {
    pub content_key: String,
    pub query_count: u64,
    pub first_queried_at: i64,
    pub last_queried_at: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortableDataExport {
    pub format: String,
    pub export_version: u32,
    pub app_version: String,
    pub schema_version: u32,
    pub profile: PortableProfile,
    pub history: Vec<PortableHistory>,
    pub favorites: Vec<PortableFavorite>,
    pub query_stats_archive: Vec<PortableQueryStatsArchive>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PortableImportPreview {
    pub history_add: u64,
    pub history_update: u64,
    pub history_skip: u64,
    pub favorite_add: u64,
    pub favorite_reactivate: u64,
    pub favorite_skip: u64,
    pub tombstone_archive: u64,
    pub query_stats_archive: u64,
}

pub type PortableImportResult = PortableImportPreview;

pub struct PortableImportPlan {
    target_profile_id: Uuid,
    export: ValidatedExport,
    preview: PortableImportPreview,
}

impl PortableImportPlan {
    #[must_use]
    pub const fn preview(&self) -> PortableImportPreview {
        self.preview
    }
}

impl fmt::Debug for PortableImportPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PortableImportPlan")
            .field("target_profile_id", &self.target_profile_id)
            .field("preview", &self.preview)
            .field("history_records", &self.export.history.len())
            .field("favorite_records", &self.export.favorites.len())
            .field("query_stats_archives", &self.export.query_stats_count)
            .finish()
    }
}

#[derive(Clone)]
struct ValidatedExport {
    history: Vec<HistoryEntry>,
    favorites: Vec<Favorite>,
    history_keys: HashSet<ContentKey>,
    query_stats_count: usize,
}

impl ProfileDatabase {
    /// Serializes the active Profile's portable User data as bounded versioned JSON.
    ///
    /// Credentials, User/Device identity, Sessions, Outbox, revisions, and sync cursors are never
    /// represented by this schema.
    ///
    /// # Errors
    /// Returns an error for malformed persisted data, record-limit overflow, or an export larger
    /// than the V1 portable JSON limit.
    pub fn export_portable_json(&self) -> Result<Vec<u8>, PortableDataError> {
        if self.portable_record_count()? > MAX_PORTABLE_RECORDS {
            return Err(PortableDataError::TooManyRecords);
        }
        let metadata = self.metadata()?;
        let history = self.export_history()?;
        let favorites = self.export_favorites()?;
        let query_stats_archive = self.export_query_stats_archive()?;
        enforce_record_limit(history.len(), favorites.len(), query_stats_archive.len())?;
        let export = PortableDataExport {
            format: PORTABLE_FORMAT.to_owned(),
            export_version: EXPORT_FORMAT_VERSION,
            app_version: SOFTWARE_VERSION.to_owned(),
            schema_version: self.schema_version(),
            profile: PortableProfile {
                username: metadata.username,
            },
            history,
            favorites,
            query_stats_archive,
        };
        let bytes = serde_json::to_vec_pretty(&export)?;
        if bytes.len() > MAX_PORTABLE_JSON_BYTES {
            return Err(PortableDataError::TooLarge);
        }
        Ok(bytes)
    }

    /// Fully validates an import and calculates a non-mutating merge preview.
    ///
    /// # Errors
    /// Returns an error before mutation for malformed, unsupported, oversized, duplicate, or
    /// content-identity-inconsistent input.
    pub fn preview_portable_import(
        &self,
        bytes: &[u8],
    ) -> Result<PortableImportPlan, PortableDataError> {
        if bytes.len() > MAX_PORTABLE_JSON_BYTES {
            return Err(PortableDataError::TooLarge);
        }
        if bytes.is_empty() {
            return Err(PortableDataError::Invalid("empty document"));
        }
        let export: PortableDataExport = serde_json::from_slice(bytes)?;
        let export = validate_export(export)?;
        let preview = preview_merge(self, &export)?;
        Ok(PortableImportPlan {
            target_profile_id: self.metadata()?.profile_id,
            export,
            preview,
        })
    }

    /// Re-evaluates and applies a validated import in one transaction.
    ///
    /// History uses newer-wins semantics, active Favorites use the normal Outbox-producing path,
    /// imported tombstones remain archival, and `QueryStats` archives never become this Device's
    /// contribution.
    ///
    /// # Errors
    /// Returns an error and rolls back every mutation if the target Profile changed or any merge
    /// operation fails.
    pub fn apply_portable_import(
        &mut self,
        plan: PortableImportPlan,
        now: UnixTimestamp,
    ) -> Result<PortableImportResult, PortableDataError> {
        let PortableImportPlan {
            target_profile_id,
            export,
            preview: _,
        } = plan;
        if self.metadata()?.profile_id != target_profile_id {
            return Err(PortableDataError::WrongProfile);
        }
        let transaction = self.connection.transaction()?;
        let result = apply_merge(&transaction, &export, now)?;
        transaction.commit()?;
        Ok(result)
    }

    fn export_history(&self) -> Result<Vec<PortableHistory>, PortableDataError> {
        let mut statement = self
            .connection
            .prepare(&format!("{HISTORY_SELECT} ORDER BY content_key"))?;
        let rows = statement.query_map([], read_history)?;
        rows.map(|row| {
            let entry = parse_history(row?)?;
            Ok(portable_history(&entry))
        })
        .collect()
    }

    fn portable_record_count(&self) -> Result<usize, PortableDataError> {
        let count = self.connection.query_row(
            "SELECT (SELECT COUNT(*) FROM history_entries)
                  + (SELECT COUNT(*) FROM favorites)
                  + (SELECT COUNT(*) FROM query_stats)",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        usize::try_from(count).map_err(|_| PortableDataError::TooManyRecords)
    }

    fn export_favorites(&self) -> Result<Vec<PortableFavorite>, PortableDataError> {
        let mut statement = self
            .connection
            .prepare(&format!("{FAVORITE_SELECT_FIELDS} ORDER BY content_key"))?;
        let rows = statement.query_map([], read_favorite)?;
        rows.map(|row| {
            let favorite = parse_favorite(row?)?;
            Ok(portable_favorite(&favorite))
        })
        .collect()
    }

    fn export_query_stats_archive(
        &self,
    ) -> Result<Vec<PortableQueryStatsArchive>, PortableDataError> {
        let mut statement = self
            .connection
            .prepare("SELECT content_key FROM query_stats ORDER BY content_key")?;
        let keys = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        keys.into_iter()
            .map(|key| {
                let key = ContentKey::from_str(&key)
                    .map_err(|_| PortableDataError::Invalid("stored content key"))?;
                let stats = self
                    .query_stats(key)?
                    .ok_or(PortableDataError::Invalid("stored QueryStats disappeared"))?;
                let first = stats
                    .server_first_queried_at
                    .map_or(stats.first_queried_at.as_seconds(), |server| {
                        server.as_seconds().min(stats.first_queried_at.as_seconds())
                    });
                let last = stats
                    .server_last_queried_at
                    .map_or(stats.last_queried_at.as_seconds(), |server| {
                        server.as_seconds().max(stats.last_queried_at.as_seconds())
                    });
                Ok(PortableQueryStatsArchive {
                    content_key: key.to_string(),
                    query_count: stats.effective_total(),
                    first_queried_at: first,
                    last_queried_at: last,
                })
            })
            .collect()
    }
}

fn validate_export(export: PortableDataExport) -> Result<ValidatedExport, PortableDataError> {
    if export.format != PORTABLE_FORMAT || export.export_version != EXPORT_FORMAT_VERSION {
        return Err(PortableDataError::UnsupportedVersion);
    }
    if export.app_version.is_empty()
        || export.app_version.len() > MAX_APP_VERSION_BYTES
        || export.app_version.chars().any(char::is_control)
        || export.schema_version == 0
    {
        return Err(PortableDataError::Invalid("export metadata"));
    }
    if export.profile.username.as_ref().is_some_and(|username| {
        username.is_empty()
            || username.len() > MAX_USERNAME_BYTES
            || username.chars().any(char::is_control)
    }) {
        return Err(PortableDataError::Invalid("Profile label"));
    }
    enforce_record_limit(
        export.history.len(),
        export.favorites.len(),
        export.query_stats_archive.len(),
    )?;
    let mut history_keys = HashSet::with_capacity(export.history.len());
    let mut history_content = HashMap::with_capacity(export.history.len());
    let mut history = Vec::with_capacity(export.history.len());
    for value in export.history {
        let entry = validate_history(value)?;
        if !history_keys.insert(entry.content.content_key) {
            return Err(PortableDataError::Invalid("duplicate History content key"));
        }
        history_content.insert(entry.content.content_key, entry.content.clone());
        history.push(entry);
    }
    let mut favorite_keys = HashSet::with_capacity(export.favorites.len());
    let mut favorites = Vec::with_capacity(export.favorites.len());
    for value in export.favorites {
        let favorite = validate_favorite(value)?;
        if !favorite_keys.insert(favorite.content.content_key) {
            return Err(PortableDataError::Invalid("duplicate Favorite content key"));
        }
        if history_content
            .get(&favorite.content.content_key)
            .is_some_and(|content| content != &favorite.content)
        {
            return Err(PortableDataError::Invalid(
                "History and Favorite content mismatch",
            ));
        }
        favorites.push(favorite);
    }
    let known_keys: HashSet<_> = history_keys.union(&favorite_keys).copied().collect();
    let mut archive_keys = HashSet::with_capacity(export.query_stats_archive.len());
    for archive in &export.query_stats_archive {
        validate_archive(archive, &known_keys)?;
        let key = ContentKey::from_str(&archive.content_key)
            .map_err(|_| PortableDataError::Invalid("QueryStats archive content key"))?;
        if !archive_keys.insert(key) {
            return Err(PortableDataError::Invalid(
                "duplicate QueryStats archive content key",
            ));
        }
    }
    Ok(ValidatedExport {
        history,
        favorites,
        history_keys,
        query_stats_count: export.query_stats_archive.len(),
    })
}

fn validate_history(value: PortableHistory) -> Result<HistoryEntry, PortableDataError> {
    if value.last_queried_at < 0 {
        return Err(PortableDataError::Invalid("History timestamp"));
    }
    let translation = validate_translation(value.translation)?;
    if translation.updated_at.as_seconds() > value.last_queried_at {
        return Err(PortableDataError::Invalid("History timestamp order"));
    }
    Ok(HistoryEntry {
        content: validate_content(value.content)?,
        translation,
        last_queried_at: UnixTimestamp::from_seconds(value.last_queried_at),
    })
}

fn validate_favorite(value: PortableFavorite) -> Result<Favorite, PortableDataError> {
    if value.favorited_at < 0
        || value.updated_at < 0
        || value.favorited_at > value.updated_at
        || value.deleted_at.is_some_and(|timestamp| timestamp < 0)
        || value
            .deleted_at
            .is_some_and(|timestamp| timestamp < value.updated_at)
    {
        return Err(PortableDataError::Invalid("Favorite timestamp"));
    }
    let translation = validate_translation(value.translation)?;
    if translation.updated_at.as_seconds() != value.updated_at {
        return Err(PortableDataError::Invalid("Favorite translation timestamp"));
    }
    Ok(Favorite {
        content: validate_content(value.content)?,
        translation,
        created_at: UnixTimestamp::from_seconds(value.favorited_at),
        updated_at: UnixTimestamp::from_seconds(value.updated_at),
        deleted_at: value.deleted_at.map(UnixTimestamp::from_seconds),
        entity_revision: 0,
    })
}

fn validate_content(value: PortableContent) -> Result<StoredContent, PortableDataError> {
    if value.key_version != CONTENT_KEY_VERSION {
        return Err(PortableDataError::UnsupportedVersion);
    }
    if value.source_text.chars().count() > MAX_SOURCE_CHARACTERS {
        return Err(PortableDataError::Invalid("source content length"));
    }
    let source_lang = LanguageCode::parse(&value.source_lang)
        .map_err(|_| PortableDataError::Invalid("source language"))?;
    let prepared = prepare_content(
        &value.source_text,
        source_lang.clone(),
        ValidationPolicy::new(
            NonZeroUsize::new(MAX_SOURCE_CHARACTERS).unwrap_or(NonZeroUsize::MIN),
        ),
    )
    .map_err(|_| PortableDataError::Invalid("source content"))?;
    let kind = match value.kind.as_str() {
        "word" => TextKind::Word,
        "text" => TextKind::Text,
        _ => return Err(PortableDataError::Invalid("content kind")),
    };
    let key = ContentKey::from_str(&value.content_key)
        .map_err(|_| PortableDataError::Invalid("content key"))?;
    if prepared.content_key() != key
        || prepared.key_version() != value.key_version
        || prepared.kind() != kind
        || prepared.source_text() != value.source_text
        || prepared.canonical_text() != value.canonical_text
    {
        return Err(PortableDataError::Invalid("content identity mismatch"));
    }
    Ok(StoredContent {
        content_key: key,
        key_version: value.key_version,
        kind,
        source_lang,
        source_text: value.source_text,
        canonical_text: value.canonical_text,
    })
}

fn validate_translation(
    value: PortableTranslation,
) -> Result<TranslationSnapshot, PortableDataError> {
    if value.translation.is_empty()
        || value.translation.len() > MAX_TRANSLATION_BYTES
        || value.provider.is_empty()
        || value.provider.len() > MAX_PROVIDER_BYTES
        || value.provider.chars().any(char::is_control)
        || value
            .translation
            .chars()
            .any(|character| character.is_control() && !character.is_whitespace())
        || value.updated_at < 0
    {
        return Err(PortableDataError::Invalid("translation snapshot"));
    }
    let target_lang = LanguageCode::parse(&value.target_lang)
        .map_err(|_| PortableDataError::Invalid("target language"))?;
    Ok(TranslationSnapshot {
        target_lang,
        translation: value.translation,
        provider: value.provider,
        updated_at: UnixTimestamp::from_seconds(value.updated_at),
    })
}

fn validate_archive(
    value: &PortableQueryStatsArchive,
    known_keys: &HashSet<ContentKey>,
) -> Result<(), PortableDataError> {
    let key = ContentKey::from_str(&value.content_key)
        .map_err(|_| PortableDataError::Invalid("QueryStats archive content key"))?;
    if !known_keys.contains(&key)
        || value.first_queried_at < 0
        || value.last_queried_at < value.first_queried_at
    {
        return Err(PortableDataError::Invalid("QueryStats archive"));
    }
    Ok(())
}

fn enforce_record_limit(
    history: usize,
    favorites: usize,
    query_stats: usize,
) -> Result<(), PortableDataError> {
    let total = history
        .checked_add(favorites)
        .and_then(|value| value.checked_add(query_stats))
        .ok_or(PortableDataError::TooManyRecords)?;
    if total > MAX_PORTABLE_RECORDS {
        return Err(PortableDataError::TooManyRecords);
    }
    Ok(())
}

fn preview_merge(
    database: &ProfileDatabase,
    export: &ValidatedExport,
) -> Result<PortableImportPreview, PortableDataError> {
    let mut preview = PortableImportPreview {
        query_stats_archive: u64::try_from(export.query_stats_count)
            .map_err(|_| PortableDataError::TooManyRecords)?,
        ..PortableImportPreview::default()
    };
    for entry in &export.history {
        match database.history(entry.content.content_key)? {
            None => preview.history_add += 1,
            Some(local)
                if entry.last_queried_at.as_seconds() > local.last_queried_at.as_seconds() =>
            {
                preview.history_update += 1;
            }
            Some(_) => preview.history_skip += 1,
        }
    }
    for favorite in &export.favorites {
        if !export.history_keys.contains(&favorite.content.content_key)
            && database.history(favorite.content.content_key)?.is_none()
        {
            preview.history_add += 1;
        }
        match (
            favorite.deleted_at.is_none(),
            database.favorite_by_key(favorite.content.content_key)?,
        ) {
            (true, None) => preview.favorite_add += 1,
            (true, Some(local)) if local.deleted_at.is_some() => preview.favorite_reactivate += 1,
            (false, None) => preview.tombstone_archive += 1,
            _ => preview.favorite_skip += 1,
        }
    }
    Ok(preview)
}

fn apply_merge(
    transaction: &Transaction<'_>,
    export: &ValidatedExport,
    now: UnixTimestamp,
) -> Result<PortableImportResult, PortableDataError> {
    let mut result = PortableImportPreview {
        query_stats_archive: u64::try_from(export.query_stats_count)
            .map_err(|_| PortableDataError::TooManyRecords)?,
        ..PortableImportPreview::default()
    };
    for entry in &export.history {
        merge_history(transaction, entry, &mut result)?;
    }
    for favorite in &export.favorites {
        if !export.history_keys.contains(&favorite.content.content_key) {
            let history = history_from_favorite(favorite);
            if history_tx(transaction, favorite.content.content_key)?.is_none() {
                insert_history_with_zero_stats(transaction, &history)?;
                result.history_add += 1;
            }
        }
        let existing = favorite_tx(transaction, favorite.content.content_key)?;
        if favorite.deleted_at.is_none() {
            match existing {
                Some(local) if local.deleted_at.is_none() => result.favorite_skip += 1,
                Some(_) => {
                    favorite_from_history_tx(transaction, favorite.content.content_key, now)?;
                    result.favorite_reactivate += 1;
                }
                None => {
                    favorite_from_history_tx(transaction, favorite.content.content_key, now)?;
                    result.favorite_add += 1;
                }
            }
        } else if existing.is_some() {
            result.favorite_skip += 1;
        } else {
            archive_tombstone(transaction, favorite)?;
            result.tombstone_archive += 1;
        }
    }
    Ok(result)
}

fn merge_history(
    transaction: &Transaction<'_>,
    imported: &HistoryEntry,
    result: &mut PortableImportResult,
) -> Result<(), PortableDataError> {
    match history_tx(transaction, imported.content.content_key)? {
        None => {
            insert_history_with_zero_stats(transaction, imported)?;
            result.history_add += 1;
        }
        Some(local)
            if imported.last_queried_at.as_seconds() > local.last_queried_at.as_seconds() =>
        {
            upsert_history(transaction, imported)?;
            result.history_update += 1;
        }
        Some(_) => result.history_skip += 1,
    }
    Ok(())
}

fn history_tx(
    transaction: &Transaction<'_>,
    key: ContentKey,
) -> Result<Option<HistoryEntry>, PortableDataError> {
    Ok(transaction
        .query_row(
            &format!("{HISTORY_SELECT} WHERE content_key=?1"),
            [key.to_string()],
            read_history,
        )
        .optional()?
        .map(parse_history)
        .transpose()?)
}

fn insert_history_with_zero_stats(
    transaction: &Transaction<'_>,
    entry: &HistoryEntry,
) -> Result<(), PortableDataError> {
    upsert_history(transaction, entry)?;
    transaction.execute(
        "INSERT OR IGNORE INTO query_stats(content_key,device_query_count,first_queried_at,
         last_queried_at) VALUES(?1,0,?2,?2)",
        params![
            entry.content.content_key.to_string(),
            entry.last_queried_at.as_seconds()
        ],
    )?;
    Ok(())
}

fn archive_tombstone(
    transaction: &Transaction<'_>,
    favorite: &Favorite,
) -> Result<(), PortableDataError> {
    transaction.execute(
        "INSERT INTO favorites(content_key,key_version,kind,source_lang,target_lang,source_text,
         canonical_text,translation,provider,created_at,updated_at,deleted_at,entity_revision)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,0)",
        params![
            favorite.content.content_key.to_string(),
            favorite.content.key_version,
            favorite.content.kind.protocol_name(),
            favorite.content.source_lang.as_str(),
            favorite.translation.target_lang.as_str(),
            favorite.content.source_text,
            favorite.content.canonical_text,
            favorite.translation.translation,
            favorite.translation.provider,
            favorite.created_at.as_seconds(),
            favorite.updated_at.as_seconds(),
            favorite.deleted_at.map(UnixTimestamp::as_seconds)
        ],
    )?;
    Ok(())
}

fn history_from_favorite(favorite: &Favorite) -> HistoryEntry {
    HistoryEntry {
        content: favorite.content.clone(),
        translation: favorite.translation.clone(),
        last_queried_at: favorite.updated_at,
    }
}

fn portable_content(content: &StoredContent) -> PortableContent {
    PortableContent {
        content_key: content.content_key.to_string(),
        key_version: content.key_version,
        kind: content.kind.protocol_name().to_owned(),
        source_lang: content.source_lang.to_string(),
        source_text: content.source_text.clone(),
        canonical_text: content.canonical_text.clone(),
    }
}

fn portable_translation(translation: &TranslationSnapshot) -> PortableTranslation {
    PortableTranslation {
        target_lang: translation.target_lang.to_string(),
        translation: translation.translation.clone(),
        provider: translation.provider.clone(),
        updated_at: translation.updated_at.as_seconds(),
    }
}

fn portable_history(entry: &HistoryEntry) -> PortableHistory {
    PortableHistory {
        content: portable_content(&entry.content),
        translation: portable_translation(&entry.translation),
        last_queried_at: entry.last_queried_at.as_seconds(),
    }
}

fn portable_favorite(favorite: &Favorite) -> PortableFavorite {
    PortableFavorite {
        content: portable_content(&favorite.content),
        translation: portable_translation(&favorite.translation),
        favorited_at: favorite.created_at.as_seconds(),
        updated_at: favorite.updated_at.as_seconds(),
        deleted_at: favorite.deleted_at.map(UnixTimestamp::as_seconds),
    }
}

#[derive(Debug)]
pub enum PortableDataError {
    Storage(StorageError),
    Json(serde_json::Error),
    TooLarge,
    TooManyRecords,
    UnsupportedVersion,
    WrongProfile,
    Invalid(&'static str),
}

impl fmt::Display for PortableDataError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(error) => write!(formatter, "portable data storage failed: {error}"),
            Self::Json(_) => formatter.write_str("portable JSON is malformed"),
            Self::TooLarge => formatter.write_str("portable JSON exceeds the size limit"),
            Self::TooManyRecords => formatter.write_str("portable JSON exceeds the record limit"),
            Self::UnsupportedVersion => formatter.write_str("portable JSON version is unsupported"),
            Self::WrongProfile => formatter.write_str("import preview belongs to another Profile"),
            Self::Invalid(field) => write!(formatter, "portable JSON contains invalid {field}"),
        }
    }
}

impl Error for PortableDataError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Storage(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::TooLarge
            | Self::TooManyRecords
            | Self::UnsupportedVersion
            | Self::WrongProfile
            | Self::Invalid(_) => None,
        }
    }
}

impl From<StorageError> for PortableDataError {
    fn from(value: StorageError) -> Self {
        Self::Storage(value)
    }
}

impl From<rusqlite::Error> for PortableDataError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Storage(StorageError::Sqlite(value))
    }
}

impl From<serde_json::Error> for PortableDataError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::{ProfileMetadata, ProfilePaths};

    fn metadata(profile_id: Uuid) -> ProfileMetadata {
        ProfileMetadata {
            profile_id,
            user_id: None,
            username: None,
            device_id: Uuid::new_v4(),
            platform: "macos".to_owned(),
            server_origin: None,
            last_server_revision: 0,
            created_at: UnixTimestamp::from_seconds(100),
            updated_at: UnixTimestamp::from_seconds(100),
        }
    }

    fn database(root: &std::path::Path) -> ProfileDatabase {
        let profile_id = Uuid::new_v4();
        ProfileDatabase::open(ProfilePaths::new(root, profile_id), &metadata(profile_id))
            .unwrap_or_else(|error| unreachable!("fixture database: {error}"))
    }

    fn history(source: &str) -> HistoryEntry {
        let prepared = prepare_content(
            source,
            LanguageCode::parse("en").unwrap_or_else(|error| unreachable!("fixture: {error}")),
            ValidationPolicy::new(
                NonZeroUsize::new(1_000).unwrap_or_else(|| unreachable!("fixture limit")),
            ),
        )
        .unwrap_or_else(|error| unreachable!("fixture content: {error}"));
        HistoryEntry {
            content: StoredContent {
                content_key: prepared.content_key(),
                key_version: prepared.key_version(),
                kind: prepared.kind(),
                source_lang: prepared.source_lang().clone(),
                source_text: prepared.source_text().to_owned(),
                canonical_text: prepared.canonical_text().to_owned(),
            },
            translation: TranslationSnapshot {
                target_lang: LanguageCode::parse("zh-CN")
                    .unwrap_or_else(|error| unreachable!("fixture: {error}")),
                translation: "事务".to_owned(),
                provider: "fixture".to_owned(),
                updated_at: UnixTimestamp::from_seconds(200),
            },
            last_queried_at: UnixTimestamp::from_seconds(200),
        }
    }

    #[test]
    fn apply_failure_rolls_back_history_stats_favorite_and_outbox() {
        let source_root = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
        let target_root = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
        let mut source = database(source_root.path());
        let entry = history("Transactional portable import");
        let key = entry.content.content_key;
        source
            .record_successful_query(&entry)
            .unwrap_or_else(|error| unreachable!("query: {error}"));
        source
            .favorite(key, UnixTimestamp::from_seconds(201))
            .unwrap_or_else(|error| unreachable!("favorite: {error}"));
        let bytes = source
            .export_portable_json()
            .unwrap_or_else(|error| unreachable!("export: {error}"));

        let mut target = database(target_root.path());
        let plan = target
            .preview_portable_import(&bytes)
            .unwrap_or_else(|error| unreachable!("preview: {error}"));
        target
            .connection
            .execute_batch(
                "CREATE TRIGGER reject_portable_favorite
                 BEFORE INSERT ON favorites
                 BEGIN SELECT RAISE(ABORT, 'fixture rejection'); END;",
            )
            .unwrap_or_else(|error| unreachable!("trigger: {error}"));
        assert!(
            target
                .apply_portable_import(plan, UnixTimestamp::from_seconds(300))
                .is_err()
        );
        assert!(target.history(key).unwrap_or_default().is_none());
        assert!(target.query_stats(key).unwrap_or_default().is_none());
        assert!(target.favorite_by_key(key).unwrap_or_default().is_none());
        assert!(target.outbox_events().unwrap_or_default().is_empty());
    }
}
