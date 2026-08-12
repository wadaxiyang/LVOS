use std::num::NonZeroUsize;

use lvos_core::{LanguageCode, UnixTimestamp, ValidationPolicy, prepare_content};
use lvos_storage::{
    HistoryEntry, ProfileDatabase, ProfileMetadata, ProfilePaths, StoredContent,
    TranslationSnapshot,
};
use tempfile::tempdir;
use uuid::Uuid;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;
    let profile_id = Uuid::new_v4();
    let device_id = Uuid::new_v4();
    let metadata = ProfileMetadata {
        profile_id,
        user_id: None,
        username: None,
        device_id,
        platform: "macos".to_owned(),
        server_origin: None,
        last_server_revision: 0,
        created_at: UnixTimestamp::from_seconds(1_780_000_000),
        updated_at: UnixTimestamp::from_seconds(1_780_000_000),
    };
    let mut database =
        ProfileDatabase::open(ProfilePaths::new(root.path(), profile_id), &metadata)?;
    let prepared = prepare_content(
        "Invariant.",
        LanguageCode::parse("en")?,
        ValidationPolicy::new(NonZeroUsize::new(1_000).ok_or("nonzero limit")?),
    )?;
    let entry = HistoryEntry {
        content: StoredContent {
            content_key: prepared.content_key(),
            key_version: prepared.key_version(),
            kind: prepared.kind(),
            source_lang: prepared.source_lang().clone(),
            source_text: prepared.source_text().to_owned(),
            canonical_text: prepared.canonical_text().to_owned(),
        },
        translation: TranslationSnapshot {
            target_lang: LanguageCode::parse("zh-CN")?,
            translation: "不变的；恒定的".to_owned(),
            provider: "diagnostic-fixture".to_owned(),
            updated_at: UnixTimestamp::from_seconds(1_780_000_001),
        },
        last_queried_at: UnixTimestamp::from_seconds(1_780_000_001),
    };
    for offset in 0..3 {
        let mut lookup = entry.clone();
        lookup.last_queried_at = UnixTimestamp::from_seconds(1_780_000_001 + offset);
        database.record_successful_query(&lookup)?;
    }
    database.favorite(
        entry.content.content_key,
        UnixTimestamp::from_seconds(1_780_000_010),
    )?;

    let stats = database
        .query_stats(entry.content.content_key)?
        .ok_or("missing stats")?;
    let favorite = database
        .favorite_by_key(entry.content.content_key)?
        .ok_or("missing favorite")?;
    println!("schema_version={}", database.schema_version());
    println!("profile_id={profile_id}");
    println!("device_id={device_id}");
    println!(
        "history_rows={}",
        database.search_history("Invariant", 10)?.len()
    );
    println!("device_query_count={}", stats.device_query_count);
    println!("favorite_active={}", favorite.deleted_at.is_none());
    println!("outbox_events={}", database.outbox_events()?.len());
    Ok(())
}
