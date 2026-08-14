use std::{
    collections::HashMap,
    convert::Infallible,
    num::NonZeroUsize,
    str::FromStr,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response, Sse, sse::Event},
};
use lvos_core::{CONTENT_KEY_VERSION, ContentKey, LanguageCode, ValidationPolicy, prepare_content};
use lvos_sync::{
    ChangesResponse, FavoriteStateRequest, FavoriteStateResponse, FavoritesResponse, PushEvent,
    PushRequest, PushResponse, RevisionNotice, SyncOperation,
};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_stream::{StreamExt, wrappers::BroadcastStream};
use uuid::Uuid;

use crate::sync_repository::{SyncConflict, SyncRepositoryError};
use crate::{ApiError, AppState, Principal, RepositoryError, unix_timestamp};

const MAX_SYNC_TEXT_CHARACTERS: usize = 16_384;
const MAX_TRANSLATION_BYTES: usize = 65_536;
const MAX_PROVIDER_BYTES: usize = 128;
const SSE_BUFFER_SIZE: usize = 64;

#[derive(Clone, Default)]
pub(crate) struct RevisionHub {
    senders: Arc<Mutex<HashMap<String, broadcast::Sender<u64>>>>,
}

impl RevisionHub {
    fn subscribe(&self, user_id: &str) -> Result<broadcast::Receiver<u64>, SyncApiError> {
        let mut senders = self.senders.lock().map_err(|_| SyncApiError::internal())?;
        let sender = senders.entry(user_id.to_owned()).or_insert_with(|| {
            let (sender, _) = broadcast::channel(SSE_BUFFER_SIZE);
            sender
        });
        Ok(sender.subscribe())
    }

    pub(crate) fn notify(&self, user_id: &str, revision: u64) {
        if let Ok(senders) = self.senders.lock()
            && let Some(sender) = senders.get(user_id)
        {
            let _ = sender.send(revision);
        }
    }
}

pub(crate) async fn push(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<PushRequest>,
) -> Result<Json<PushResponse>, SyncApiError> {
    if request.events.is_empty() || request.events.len() > state.config.max_sync_events_per_batch {
        return Err(SyncApiError::invalid("sync batch size is invalid"));
    }
    for event in &request.events {
        validate_event(event)?;
    }
    let repository = state.repository.clone();
    let user_id = principal.user_id.clone();
    let device_id = principal.device_id;
    let events = request.events;
    let now = unix_timestamp().map_err(SyncApiError::from)?;
    let result =
        blocking_sync(move || repository.push_events(&user_id, &device_id, &events, now)).await?;
    if let Some(revision) = result.changed_revision {
        state.revision_hub.notify(&principal.user_id, revision);
    }
    Ok(Json(result.value))
}

#[derive(Deserialize)]
pub(crate) struct ChangesQuery {
    since: u64,
    limit: Option<usize>,
}

pub(crate) async fn changes(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(query): Query<ChangesQuery>,
) -> Result<Json<ChangesResponse>, SyncApiError> {
    let limit = query
        .limit
        .unwrap_or(state.config.sync_changes_default_limit);
    if limit == 0 || limit > state.config.sync_changes_max_limit {
        return Err(SyncApiError::invalid("sync change page limit is invalid"));
    }
    let repository = state.repository.clone();
    let user_id = principal.user_id;
    let response =
        blocking_sync(move || repository.changes_since(&user_id, query.since, limit)).await?;
    Ok(Json(response))
}

pub(crate) async fn favorites(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<FavoritesResponse>, SyncApiError> {
    let repository = state.repository.clone();
    let user_id = principal.user_id;
    let (favorites, latest_revision) =
        blocking_sync(move || repository.active_favorites(&user_id)).await?;
    Ok(Json(FavoritesResponse {
        favorites,
        latest_revision,
    }))
}

pub(crate) async fn set_favorite_state(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(content_key): Path<String>,
    Json(request): Json<FavoriteStateRequest>,
) -> Result<Json<FavoriteStateResponse>, SyncApiError> {
    validate_content_key(&content_key)?;
    let repository = state.repository.clone();
    let user_id = principal.user_id.clone();
    let now = unix_timestamp().map_err(SyncApiError::from)?;
    let result = blocking_sync(move || {
        repository.set_favorite_state(
            &user_id,
            &content_key,
            request.active,
            request.base_entity_revision,
            now,
        )
    })
    .await?;
    if let Some(revision) = result.changed_revision {
        state.revision_hub.notify(&principal.user_id, revision);
    }
    let latest_revision = result
        .changed_revision
        .unwrap_or(u64::try_from(principal.latest_revision).map_err(|_| SyncApiError::internal())?);
    Ok(Json(FavoriteStateResponse {
        favorite: result.value,
        latest_revision,
    }))
}

pub(crate) async fn stream(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>, SyncApiError> {
    let receiver = state.revision_hub.subscribe(&principal.user_id)?;
    let repository = state.repository.clone();
    let user_id = principal.user_id;
    let (_, latest_revision) = blocking_sync(move || repository.active_favorites(&user_id)).await?;
    let initial = tokio_stream::once(Ok(revision_event(latest_revision)));
    let updates = BroadcastStream::new(receiver).filter_map(|result| match result {
        Ok(revision) => Some(Ok(revision_event(revision))),
        Err(_) => None,
    });
    Ok(Sse::new(initial.chain(updates)).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

fn revision_event(revision: u64) -> Event {
    Event::default()
        .event("revision")
        .json_data(RevisionNotice {
            latest_revision: revision,
        })
        .unwrap_or_else(|_| Event::default().event("revision"))
}

fn validate_event(event: &PushEvent) -> Result<(), SyncApiError> {
    let event_id = Uuid::parse_str(&event.event_id)
        .map_err(|_| SyncApiError::invalid("event_id must be a UUIDv7"))?;
    if event_id.get_version_num() != 7 {
        return Err(SyncApiError::invalid("event_id must be a UUIDv7"));
    }
    validate_content_key(&event.content_key)?;
    if event.key_version != CONTENT_KEY_VERSION {
        return Err(SyncApiError::invalid("unsupported content key version"));
    }
    match event.operation {
        SyncOperation::FavoriteUpsert => {
            let favorite = event
                .favorite
                .as_ref()
                .ok_or_else(|| SyncApiError::invalid("favorite snapshot is required"))?;
            let source_lang = LanguageCode::parse(&favorite.source_lang)
                .map_err(|_| SyncApiError::invalid("source language is invalid"))?;
            LanguageCode::parse(&favorite.target_lang)
                .map_err(|_| SyncApiError::invalid("target language is invalid"))?;
            let policy = ValidationPolicy::new(
                NonZeroUsize::new(MAX_SYNC_TEXT_CHARACTERS).unwrap_or(NonZeroUsize::MIN),
            );
            let prepared = prepare_content(&favorite.source_text, source_lang, policy)
                .map_err(|_| SyncApiError::invalid("favorite source text is invalid"))?;
            if prepared.content_key().to_hex() != event.content_key
                || prepared.key_version() != event.key_version
                || prepared.kind().protocol_name() != favorite.kind
                || prepared.canonical_text() != favorite.canonical_text
                || prepared.source_lang().as_str() != favorite.source_lang
            {
                return Err(SyncApiError::invalid(
                    "favorite content identity is inconsistent",
                ));
            }
            if favorite.translation.is_empty()
                || favorite.translation.len() > MAX_TRANSLATION_BYTES
                || favorite.provider.is_empty()
                || favorite.provider.len() > MAX_PROVIDER_BYTES
            {
                return Err(SyncApiError::invalid(
                    "favorite translated fields are invalid",
                ));
            }
        }
        SyncOperation::FavoriteDelete | SyncOperation::QueryStatsUpsert => {
            if event.favorite.is_some() {
                return Err(SyncApiError::invalid("favorite snapshot is not allowed"));
            }
        }
    }
    if let Some(stats) = &event.query_stats
        && (stats.query_count == 0 || stats.first_queried_at > stats.last_queried_at)
    {
        return Err(SyncApiError::invalid("query statistics are invalid"));
    }
    if event.operation == SyncOperation::QueryStatsUpsert && event.query_stats.is_none() {
        return Err(SyncApiError::invalid("query statistics are required"));
    }
    Ok(())
}

fn validate_content_key(value: &str) -> Result<(), SyncApiError> {
    ContentKey::from_str(value)
        .map(|_| ())
        .map_err(|_| SyncApiError::invalid("content_key is invalid"))
}

async fn blocking_sync<T, F>(operation: F) -> Result<T, SyncApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, SyncRepositoryError> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| SyncApiError::internal())?
        .map_err(SyncApiError::from)
}

#[derive(Debug)]
pub(crate) enum SyncApiError {
    Basic(ApiError),
    Invalid(&'static str),
    Conflict(Box<SyncConflict>),
}

impl SyncApiError {
    fn invalid(message: &'static str) -> Self {
        Self::Invalid(message)
    }

    fn internal() -> Self {
        Self::Basic(ApiError::internal())
    }
}

impl From<ApiError> for SyncApiError {
    fn from(error: ApiError) -> Self {
        Self::Basic(error)
    }
}

impl From<SyncRepositoryError> for SyncApiError {
    fn from(error: SyncRepositoryError) -> Self {
        match error {
            SyncRepositoryError::Repository(error) => Self::Basic(ApiError::from(error)),
            SyncRepositoryError::Conflict(conflict) => Self::Conflict(conflict),
            SyncRepositoryError::InvalidEvent => Self::Invalid("synchronization event is invalid"),
        }
    }
}

#[derive(Serialize)]
struct SyncErrorEnvelope<T> {
    error: T,
}

#[derive(Serialize)]
struct BasicSyncError {
    code: &'static str,
    message: &'static str,
}

#[derive(Serialize)]
struct ConflictBody {
    code: &'static str,
    message: &'static str,
    event_id: Option<String>,
    current: Option<lvos_sync::FavoriteRecord>,
    latest_revision: u64,
}

impl IntoResponse for SyncApiError {
    fn into_response(self) -> Response {
        match self {
            Self::Basic(error) => error.into_response(),
            Self::Invalid(message) => (
                StatusCode::BAD_REQUEST,
                Json(SyncErrorEnvelope {
                    error: BasicSyncError {
                        code: "invalid_request",
                        message,
                    },
                }),
            )
                .into_response(),
            Self::Conflict(conflict) => (
                StatusCode::CONFLICT,
                Json(SyncErrorEnvelope {
                    error: ConflictBody {
                        code: "favorite_conflict",
                        message: "the favorite revision conflicts with the server",
                        event_id: conflict.event_id,
                        current: conflict.current,
                        latest_revision: conflict.latest_revision,
                    },
                }),
            )
                .into_response(),
        }
    }
}

impl From<RepositoryError> for SyncApiError {
    fn from(error: RepositoryError) -> Self {
        Self::Basic(ApiError::from(error))
    }
}
