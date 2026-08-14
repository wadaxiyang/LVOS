use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::{num::NonZeroUsize, time::Duration};

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use lvos_core::{CONTENT_KEY_VERSION, LanguageCode, ValidationPolicy, prepare_content};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::*;

fn test_config() -> ServerConfig {
    ServerConfig {
        environment: AppEnvironment::Development,
        bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7770),
        database_url: ":memory:".to_owned(),
        public_server_url: "https://lvos.invalid".to_owned(),
        bootstrap_default_user: true,
        default_username: "default".to_owned(),
        default_password: Some("correct-horse-battery-staple".to_owned()),
        access_token_ttl_seconds: 3_600,
        refresh_idle_ttl_seconds: 90 * 24 * 60 * 60,
        login_rate_limit_enabled: true,
        login_rate_limit_max_failures: 5,
        login_rate_limit_window_seconds: 60,
        max_request_body_bytes: 1_048_576,
        max_sync_body_bytes: 1_048_576,
        max_sync_events_per_batch: 500,
        sync_changes_default_limit: 100,
        sync_changes_max_limit: 500,
        backup_enabled: true,
        backup_dir: PathBuf::from("./backups"),
        backup_retention_count: 14,
        backup_interval_seconds: 24 * 60 * 60,
    }
}

async fn setup(config: ServerConfig) -> (Router, ServerRepository) {
    let repository =
        ServerRepository::in_memory(1).unwrap_or_else(|error| unreachable!("repository: {error}"));
    let app = build_app(config, repository.clone())
        .await
        .unwrap_or_else(|error| unreachable!("app: {error}"));
    (app, repository)
}

fn login_body(username: &str, password: &str, device_id: &str) -> Value {
    json!({
        "username": username,
        "password": password,
        "device_id": device_id,
        "platform": "windows",
        "device_name": "test-pc"
    })
}

async fn request_json(
    app: &Router,
    method: &str,
    uri: &str,
    body: Value,
    access_token: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = access_token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(
            builder
                .body(Body::from(body.to_string()))
                .unwrap_or_else(|error| unreachable!("request: {error}")),
        )
        .await
        .unwrap_or_else(|error| unreachable!("response: {error}"));
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .unwrap_or_else(|error| unreachable!("body: {error}"))
        .to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| unreachable!("json: {error}"))
    };
    (status, value)
}

async fn login_device(app: &Router, username: &str, password: &str, device_id: &str) -> Value {
    let (status, value) = request_json(
        app,
        "POST",
        "/api/v1/auth/login",
        login_body(username, password, device_id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value
}

fn value_string<'a>(value: &'a Value, pointer: &str) -> &'a str {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| unreachable!("missing {pointer}: {value}"))
}

fn favorite_event(event_id: &str, source: &str, base_revision: u64) -> (String, Value) {
    let source_lang = LanguageCode::parse("en").unwrap_or_else(|_| unreachable!("language"));
    let prepared = prepare_content(
        source,
        source_lang,
        ValidationPolicy::new(NonZeroUsize::new(1_000).unwrap_or(NonZeroUsize::MIN)),
    )
    .unwrap_or_else(|error| unreachable!("content: {error}"));
    let content_key = prepared.content_key().to_hex();
    let event = json!({
        "event_id": event_id,
        "operation": "favorite_upsert",
        "content_key": content_key,
        "key_version": CONTENT_KEY_VERSION,
        "base_entity_revision": base_revision,
        "favorite": {
            "kind": prepared.kind().protocol_name(),
            "source_lang": "en",
            "target_lang": "zh-CN",
            "source_text": prepared.source_text(),
            "canonical_text": prepared.canonical_text(),
            "translation": "测试翻译",
            "provider": "test-provider",
            "favorited_at": 100,
            "updated_at": 100
        },
        "query_stats": {
            "query_count": 2,
            "first_queried_at": 90,
            "last_queried_at": 100,
            "updated_at": 100
        }
    });
    (content_key, event)
}

fn query_stats_event(
    event_id: &str,
    content_key: &str,
    count: u64,
    first: i64,
    last: i64,
) -> Value {
    json!({
        "event_id": event_id,
        "operation": "query_stats_upsert",
        "content_key": content_key,
        "key_version": CONTENT_KEY_VERSION,
        "base_entity_revision": 0,
        "query_stats": {
            "query_count": count,
            "first_queried_at": first,
            "last_queried_at": last,
            "updated_at": last
        }
    })
}

#[tokio::test]
async fn health_login_me_and_hash_only_persistence_work() {
    let (app, repository) = setup(test_config()).await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/health")
                .body(Body::empty())
                .unwrap_or_else(|error| unreachable!("request: {error}")),
        )
        .await
        .unwrap_or_else(|error| unreachable!("health: {error}"));
    assert_eq!(response.status(), StatusCode::OK);

    let device_id = Uuid::new_v4().to_string();
    let tokens = login_device(&app, "default", "correct-horse-battery-staple", &device_id).await;
    let access = value_string(&tokens, "/access_token");
    let refresh = value_string(&tokens, "/refresh_token");
    assert!(!repository.raw_session_contains(access).unwrap_or(true));
    assert!(!repository.raw_session_contains(refresh).unwrap_or(true));

    let (status, me) =
        request_json(&app, "GET", "/api/v1/auth/me", Value::Null, Some(access)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value_string(&me, "/user/username"), "default");
    assert_eq!(value_string(&me, "/device/device_id"), device_id);
}

#[tokio::test]
async fn refresh_rotates_both_tokens_and_logout_revokes_the_session() {
    let (app, _) = setup(test_config()).await;
    let first = login_device(
        &app,
        "default",
        "correct-horse-battery-staple",
        &Uuid::new_v4().to_string(),
    )
    .await;
    let old_access = value_string(&first, "/access_token").to_owned();
    let old_refresh = value_string(&first, "/refresh_token").to_owned();
    let (status, second) = request_json(
        &app,
        "POST",
        "/api/v1/auth/refresh",
        json!({"refresh_token": old_refresh}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let new_access = value_string(&second, "/access_token");
    let new_refresh = value_string(&second, "/refresh_token");
    assert_ne!(new_access, old_access);
    assert_ne!(new_refresh, value_string(&first, "/refresh_token"));

    let (status, _) = request_json(
        &app,
        "POST",
        "/api/v1/auth/refresh",
        json!({"refresh_token": value_string(&first, "/refresh_token")}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = request_json(
        &app,
        "POST",
        "/api/v1/auth/logout",
        Value::Null,
        Some(new_access),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = request_json(
        &app,
        "GET",
        "/api/v1/auth/me",
        Value::Null,
        Some(new_access),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn device_revocation_is_user_scoped_and_old_identity_stays_revoked() {
    let config = test_config();
    let repository =
        ServerRepository::in_memory(1).unwrap_or_else(|error| unreachable!("repository: {error}"));
    bootstrap_user(
        repository.clone(),
        "other".to_owned(),
        "other-password".to_owned(),
    )
    .await
    .unwrap_or_else(|error| unreachable!("bootstrap: {error}"));
    let app = build_app(config, repository)
        .await
        .unwrap_or_else(|error| unreachable!("app: {error}"));

    let primary_id = Uuid::new_v4().to_string();
    let revoked_id = Uuid::new_v4().to_string();
    let other_id = Uuid::new_v4().to_string();
    let primary = login_device(&app, "default", "correct-horse-battery-staple", &primary_id).await;
    let revoked = login_device(&app, "default", "correct-horse-battery-staple", &revoked_id).await;
    let other = login_device(&app, "other", "other-password", &other_id).await;
    let primary_access = value_string(&primary, "/access_token");

    let (status, _) = request_json(
        &app,
        "POST",
        &format!("/api/v1/devices/{other_id}/revoke"),
        Value::Null,
        Some(primary_access),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = request_json(
        &app,
        "POST",
        &format!("/api/v1/devices/{revoked_id}/revoke"),
        Value::Null,
        Some(primary_access),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = request_json(
        &app,
        "GET",
        "/api/v1/auth/me",
        Value::Null,
        Some(value_string(&revoked, "/access_token")),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = request_json(
        &app,
        "POST",
        "/api/v1/auth/refresh",
        json!({"refresh_token": value_string(&revoked, "/refresh_token")}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, error) = request_json(
        &app,
        "POST",
        "/api/v1/auth/login",
        login_body("default", "correct-horse-battery-staple", &revoked_id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error.pointer("/error/code"), Some(&json!("device_revoked")));
    assert_eq!(
        error.pointer("/error/re_registration_supported"),
        Some(&json!(true))
    );

    let (status, _) = request_json(
        &app,
        "GET",
        "/api/v1/auth/me",
        Value::Null,
        Some(value_string(&other, "/access_token")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let replacement = login_device(
        &app,
        "default",
        "correct-horse-battery-staple",
        &Uuid::new_v4().to_string(),
    )
    .await;
    assert!(value_string(&replacement, "/access_token").starts_with("lvos_at_"));
}

#[tokio::test]
async fn rate_limit_body_limit_expiry_and_disabled_user_fail_closed() {
    let mut config = test_config();
    config.login_rate_limit_max_failures = 2;
    config.max_request_body_bytes = 512;
    let (app, repository) = setup(config).await;
    let device_id = Uuid::new_v4().to_string();
    for expected in [
        StatusCode::UNAUTHORIZED,
        StatusCode::UNAUTHORIZED,
        StatusCode::TOO_MANY_REQUESTS,
    ] {
        let (status, _) = request_json(
            &app,
            "POST",
            "/api/v1/auth/login",
            login_body("blocked", "wrong", &device_id),
            None,
        )
        .await;
        assert_eq!(status, expected);
    }

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("x".repeat(513)))
                .unwrap_or_else(|error| unreachable!("request: {error}")),
        )
        .await
        .unwrap_or_else(|error| unreachable!("response: {error}"));
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let tokens = login_device(
        &app,
        "default",
        "correct-horse-battery-staple",
        &Uuid::new_v4().to_string(),
    )
    .await;
    let access = value_string(&tokens, "/access_token");
    repository
        .expire_access_hash(&token_hash(access), 0)
        .unwrap_or_else(|error| unreachable!("expire: {error}"));
    let (status, error) =
        request_json(&app, "GET", "/api/v1/auth/me", Value::Null, Some(access)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        error.pointer("/error/code"),
        Some(&json!("access_token_expired"))
    );

    repository
        .disable_user("default", unix_timestamp().unwrap_or(1))
        .unwrap_or_else(|error| unreachable!("disable: {error}"));
    let (status, _) = request_json(
        &app,
        "POST",
        "/api/v1/auth/refresh",
        json!({"refresh_token": value_string(&tokens, "/refresh_token")}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn bootstrap_is_idempotent_and_refresh_idle_expiry_is_enforced() {
    let repository =
        ServerRepository::in_memory(1).unwrap_or_else(|error| unreachable!("repository: {error}"));
    let original_user = bootstrap_user(
        repository.clone(),
        "default".to_owned(),
        "original-password".to_owned(),
    )
    .await
    .unwrap_or_else(|error| unreachable!("bootstrap: {error}"));
    let repeated_user = bootstrap_user(
        repository.clone(),
        "default".to_owned(),
        "replacement-password".to_owned(),
    )
    .await
    .unwrap_or_else(|error| unreachable!("bootstrap: {error}"));
    assert_eq!(original_user, repeated_user);
    let stored = repository
        .user_by_username("default")
        .unwrap_or_else(|error| unreachable!("user: {error}"))
        .unwrap_or_else(|| unreachable!("missing user"));
    assert!(
        verify_password("original-password".to_owned(), stored.password_hash.clone())
            .await
            .unwrap_or(false)
    );
    assert!(
        !verify_password("replacement-password".to_owned(), stored.password_hash)
            .await
            .unwrap_or(true)
    );

    let mut config = test_config();
    config.bootstrap_default_user = false;
    config.refresh_idle_ttl_seconds = 60;
    let app = build_app(config, repository.clone())
        .await
        .unwrap_or_else(|error| unreachable!("app: {error}"));
    let tokens = login_device(
        &app,
        "default",
        "original-password",
        &Uuid::new_v4().to_string(),
    )
    .await;
    let refresh = value_string(&tokens, "/refresh_token");
    repository
        .set_refresh_last_seen(&token_hash(refresh), 0)
        .unwrap_or_else(|error| unreachable!("last seen: {error}"));
    let (status, _) = request_json(
        &app,
        "POST",
        "/api/v1/auth/refresh",
        json!({"refresh_token": refresh}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn sync_push_is_idempotent_paginated_and_conflict_safe() {
    let mut config = test_config();
    config.sync_changes_default_limit = 1;
    let (app, _) = setup(config).await;
    let tokens = login_device(
        &app,
        "default",
        "correct-horse-battery-staple",
        &Uuid::new_v4().to_string(),
    )
    .await;
    let access = value_string(&tokens, "/access_token");
    let event_id = Uuid::now_v7().to_string();
    let (content_key, event) = favorite_event(&event_id, "Invariant.", 0);

    let (status, first) = request_json(
        &app,
        "POST",
        "/api/v1/sync/push",
        json!({"events": [event.clone()]}),
        Some(access),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(
        first.pointer("/acknowledgements/0/status"),
        Some(&json!("applied"))
    );
    assert_eq!(first.pointer("/latest_revision"), Some(&json!(1)));

    let (status, retry) = request_json(
        &app,
        "POST",
        "/api/v1/sync/push",
        json!({"events": [event]}),
        Some(access),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{retry}");
    assert_eq!(retry, first);

    let (_, mut reused_id) = favorite_event(&Uuid::now_v7().to_string(), "Invariant.", 1);
    reused_id["event_id"] = json!(event_id);
    reused_id["favorite"]["translation"] = json!("different payload");
    reused_id["query_stats"] = Value::Null;
    let (status, _) = request_json(
        &app,
        "POST",
        "/api/v1/sync/push",
        json!({"events": [reused_id]}),
        Some(access),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, delete) = request_json(
        &app,
        "PATCH",
        &format!("/api/v1/favorites/{content_key}/state"),
        json!({"active": false, "base_entity_revision": 1}),
        Some(access),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{delete}");
    assert_eq!(delete.pointer("/favorite/entity_revision"), Some(&json!(2)));
    assert!(
        delete
            .pointer("/favorite/deleted_at")
            .is_some_and(|value| !value.is_null())
    );

    let (status, conflict) = request_json(
        &app,
        "PATCH",
        &format!("/api/v1/favorites/{content_key}/state"),
        json!({"active": true, "base_entity_revision": 1}),
        Some(access),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    assert_eq!(
        conflict.pointer("/error/code"),
        Some(&json!("favorite_conflict"))
    );
    assert_eq!(
        conflict.pointer("/error/current/entity_revision"),
        Some(&json!(2))
    );
    assert_eq!(conflict.pointer("/error/latest_revision"), Some(&json!(2)));

    let (status, page_one) = request_json(
        &app,
        "GET",
        "/api/v1/sync/changes?since=0",
        Value::Null,
        Some(access),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page_one}");
    assert_eq!(page_one.pointer("/next_revision"), Some(&json!(1)));
    assert_eq!(page_one.pointer("/latest_revision"), Some(&json!(2)));
    assert_eq!(page_one.pointer("/has_more"), Some(&json!(true)));

    let (status, page_two) = request_json(
        &app,
        "GET",
        "/api/v1/sync/changes?since=1",
        Value::Null,
        Some(access),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page_two}");
    assert_eq!(page_two.pointer("/next_revision"), Some(&json!(2)));
    assert_eq!(page_two.pointer("/has_more"), Some(&json!(false)));

    let (status, favorites) =
        request_json(&app, "GET", "/api/v1/favorites", Value::Null, Some(access)).await;
    assert_eq!(status, StatusCode::OK, "{favorites}");
    assert_eq!(favorites.pointer("/favorites"), Some(&json!([])));
}

#[tokio::test]
async fn query_stats_merge_by_device_and_users_remain_isolated() {
    let repository =
        ServerRepository::in_memory(1).unwrap_or_else(|error| unreachable!("repository: {error}"));
    bootstrap_user(repository.clone(), "other".into(), "other-password".into())
        .await
        .unwrap_or_else(|error| unreachable!("bootstrap: {error}"));
    let app = build_app(test_config(), repository)
        .await
        .unwrap_or_else(|error| unreachable!("app: {error}"));
    let first = login_device(
        &app,
        "default",
        "correct-horse-battery-staple",
        &Uuid::new_v4().to_string(),
    )
    .await;
    let second = login_device(
        &app,
        "default",
        "correct-horse-battery-staple",
        &Uuid::new_v4().to_string(),
    )
    .await;
    let other = login_device(&app, "other", "other-password", &Uuid::new_v4().to_string()).await;
    let (content_key, favorite) = favorite_event(&Uuid::now_v7().to_string(), "aggregate", 0);
    let (_, seeded) = request_json(
        &app,
        "POST",
        "/api/v1/sync/push",
        json!({"events": [favorite]}),
        Some(value_string(&first, "/access_token")),
    )
    .await;
    assert_eq!(seeded.pointer("/latest_revision"), Some(&json!(1)));

    let second_device_snapshot =
        query_stats_event(&Uuid::now_v7().to_string(), &content_key, 5, 50, 120);
    let (status, merged) = request_json(
        &app,
        "POST",
        "/api/v1/sync/push",
        json!({"events": [second_device_snapshot]}),
        Some(value_string(&second, "/access_token")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{merged}");
    assert_eq!(
        merged.pointer("/acknowledgements/0/aggregate_query_stats/query_count"),
        Some(&json!(7))
    );
    assert_eq!(
        merged.pointer("/acknowledgements/0/aggregate_query_stats/first_queried_at"),
        Some(&json!(50))
    );
    assert_eq!(
        merged.pointer("/acknowledgements/0/aggregate_query_stats/last_queried_at"),
        Some(&json!(120))
    );

    let lower_retry = query_stats_event(&Uuid::now_v7().to_string(), &content_key, 3, 60, 110);
    let (_, no_change) = request_json(
        &app,
        "POST",
        "/api/v1/sync/push",
        json!({"events": [lower_retry]}),
        Some(value_string(&second, "/access_token")),
    )
    .await;
    assert_eq!(
        no_change.pointer("/acknowledgements/0/status"),
        Some(&json!("no_change"))
    );
    assert_eq!(no_change.pointer("/latest_revision"), Some(&json!(2)));

    let (status, hidden) = request_json(
        &app,
        "GET",
        "/api/v1/favorites",
        Value::Null,
        Some(value_string(&other, "/access_token")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(hidden.pointer("/favorites"), Some(&json!([])));
    let (status, hidden_changes) = request_json(
        &app,
        "GET",
        "/api/v1/sync/changes?since=0&limit=500",
        Value::Null,
        Some(value_string(&other, "/access_token")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(hidden_changes.pointer("/changes"), Some(&json!([])));
}

#[tokio::test]
async fn sync_limits_validation_and_sse_revision_notice_are_enforced() {
    let mut config = test_config();
    config.max_sync_events_per_batch = 1;
    let (app, _) = setup(config).await;
    let tokens = login_device(
        &app,
        "default",
        "correct-horse-battery-staple",
        &Uuid::new_v4().to_string(),
    )
    .await;
    let access = value_string(&tokens, "/access_token").to_owned();
    let (_, event) = favorite_event(&Uuid::now_v7().to_string(), "stream", 0);

    let (_, invalid_id) = favorite_event(&Uuid::new_v4().to_string(), "invalid id", 0);
    let (status, _) = request_json(
        &app,
        "POST",
        "/api/v1/sync/push",
        json!({"events": [invalid_id]}),
        Some(&access),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = request_json(
        &app,
        "POST",
        "/api/v1/sync/push",
        json!({"events": [event.clone(), event.clone()]}),
        Some(&access),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = request_json(
        &app,
        "GET",
        "/api/v1/sync/changes?since=0&limit=501",
        Value::Null,
        Some(&access),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/sync/stream")
                .header(header::AUTHORIZATION, format!("Bearer {access}"))
                .header(header::ACCEPT, "text/event-stream")
                .body(Body::empty())
                .unwrap_or_else(|error| unreachable!("request: {error}")),
        )
        .await
        .unwrap_or_else(|error| unreachable!("stream: {error}"));
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    let initial = tokio::time::timeout(Duration::from_secs(1), body.frame())
        .await
        .unwrap_or_else(|_| unreachable!("initial timeout"))
        .unwrap_or_else(|| unreachable!("initial missing"))
        .unwrap_or_else(|error| unreachable!("initial frame: {error}"));
    let initial = initial.data_ref().map_or_else(String::new, |data| {
        String::from_utf8_lossy(data).into_owned()
    });
    assert!(initial.contains("\"latest_revision\":0"), "{initial}");

    let (status, pushed) = request_json(
        &app,
        "POST",
        "/api/v1/sync/push",
        json!({"events": [event]}),
        Some(&access),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{pushed}");
    let notice = tokio::time::timeout(Duration::from_secs(1), body.frame())
        .await
        .unwrap_or_else(|_| unreachable!("notice timeout"))
        .unwrap_or_else(|| unreachable!("notice missing"))
        .unwrap_or_else(|error| unreachable!("notice frame: {error}"));
    let notice = notice.data_ref().map_or_else(String::new, |data| {
        String::from_utf8_lossy(data).into_owned()
    });
    assert!(notice.contains("\"latest_revision\":1"), "{notice}");
}

#[tokio::test]
async fn sync_body_limit_is_independent_and_fail_closed() {
    let mut config = test_config();
    config.max_sync_body_bytes = 128;
    let (app, _) = setup(config).await;
    let tokens = login_device(
        &app,
        "default",
        "correct-horse-battery-staple",
        &Uuid::new_v4().to_string(),
    )
    .await;
    let (_, event) = favorite_event(&Uuid::now_v7().to_string(), "body boundary", 0);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/sync/push")
                .header(header::CONTENT_TYPE, "application/json")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", value_string(&tokens, "/access_token")),
                )
                .body(Body::from(json!({"events": [event]}).to_string()))
                .unwrap_or_else(|error| unreachable!("request: {error}")),
        )
        .await
        .unwrap_or_else(|error| unreachable!("response: {error}"));
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn revisions_ignore_clock_skew_and_conflicted_batches_roll_back_atomically() {
    let (app, _) = setup(test_config()).await;
    let tokens = login_device(
        &app,
        "default",
        "correct-horse-battery-staple",
        &Uuid::new_v4().to_string(),
    )
    .await;
    let access = value_string(&tokens, "/access_token");
    let (content_key, initial) = favorite_event(&Uuid::now_v7().to_string(), "clock skew", 0);
    let (status, _) = request_json(
        &app,
        "POST",
        "/api/v1/sync/push",
        json!({"events": [initial]}),
        Some(access),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, mut older_clock) = favorite_event(&Uuid::now_v7().to_string(), "clock skew", 1);
    older_clock["favorite"]["translation"] = json!("旧时钟仍是最新意图");
    older_clock["favorite"]["favorited_at"] = json!(-10_000);
    older_clock["favorite"]["updated_at"] = json!(-10_000);
    older_clock["query_stats"] = Value::Null;
    let (status, applied) = request_json(
        &app,
        "POST",
        "/api/v1/sync/push",
        json!({"events": [older_clock]}),
        Some(access),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{applied}");
    assert_eq!(
        applied.pointer("/acknowledgements/0/entity_revision"),
        Some(&json!(2))
    );

    let large_snapshot = query_stats_event(&Uuid::now_v7().to_string(), &content_key, 99, 1, 999);
    let conflicting_delete = json!({
        "event_id": Uuid::now_v7().to_string(),
        "operation": "favorite_delete",
        "content_key": content_key,
        "key_version": CONTENT_KEY_VERSION,
        "base_entity_revision": 1
    });
    let (status, conflict) = request_json(
        &app,
        "POST",
        "/api/v1/sync/push",
        json!({"events": [large_snapshot, conflicting_delete]}),
        Some(access),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    assert_eq!(
        conflict.pointer("/error/current/entity_revision"),
        Some(&json!(2))
    );

    let (status, favorites) =
        request_json(&app, "GET", "/api/v1/favorites", Value::Null, Some(access)).await;
    assert_eq!(status, StatusCode::OK, "{favorites}");
    assert_eq!(
        favorites.pointer("/favorites/0/favorite/translation"),
        Some(&json!("旧时钟仍是最新意图"))
    );
    assert_eq!(
        favorites.pointer("/favorites/0/aggregate_query_stats/query_count"),
        Some(&json!(2))
    );
    assert_eq!(favorites.pointer("/latest_revision"), Some(&json!(2)));
}
