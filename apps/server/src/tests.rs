use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
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
