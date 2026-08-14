use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use lvos_auth::{AuthError, CredentialKey, CredentialScope, CredentialStore};
use lvos_core::LanguageCode;
use lvos_translation::{
    CredentialReader, DEFAULT_TOKENHUB_MODEL, GOOGLE_TRANSLATE_ENDPOINT, GoogleBasicV2Provider,
    HeaderValue, HttpRequest, HttpResponse, HttpTransport, ProviderCredentialError, ProviderId,
    ProviderRegistry, RouterSettings, SettingsError, TOKENHUB_TRANSLATE_ENDPOINT,
    TencentTokenHubProvider, TimeoutConfig, TranslationError, TranslationProvider,
    TranslationRequest, TranslationResult, TranslationRouter, TransportError,
};
use secrecy::SecretString;

#[derive(Debug)]
struct MockTransport {
    responses: Mutex<VecDeque<Result<HttpResponse, TransportError>>>,
    requests: Mutex<Vec<HttpRequest>>,
}

impl MockTransport {
    fn new(responses: impl IntoIterator<Item = Result<HttpResponse, TransportError>>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn take_request(&self) -> HttpRequest {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(0)
    }
}

#[async_trait]
impl HttpTransport for MockTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request);
        self.responses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
            .unwrap_or(Err(TransportError::Network))
    }
}

fn request() -> TranslationRequest {
    TranslationRequest {
        text: "hello".to_owned(),
        source_language: LanguageCode::parse("en")
            .unwrap_or_else(|error| unreachable!("fixture: {error}")),
        target_language: LanguageCode::parse("zh-CN")
            .unwrap_or_else(|error| unreachable!("fixture: {error}")),
    }
}

fn response(status: u16, body: &str) -> HttpResponse {
    HttpResponse {
        status,
        body: body.as_bytes().to_vec(),
    }
}

#[tokio::test]
async fn tokenhub_uses_fixed_official_endpoint_and_translation_schema() {
    let transport = Arc::new(MockTransport::new([Ok(response(
        200,
        r#"{"choices":[{"finish_reason":"stop","message":{"content":"你好"}}]}"#,
    ))]));
    let provider = TencentTokenHubProvider::new(
        transport.clone(),
        SecretString::from("tokenhub-secret".to_owned()),
        TimeoutConfig::default(),
    );
    let translated = provider
        .translate(&request())
        .await
        .unwrap_or_else(|error| unreachable!("provider: {error}"));
    assert_eq!(translated.text, "你好");
    let sent = transport.take_request();
    assert_eq!(sent.url, TOKENHUB_TRANSLATE_ENDPOINT);
    let payload: serde_json::Value =
        serde_json::from_slice(&sent.body).unwrap_or_else(|error| unreachable!("payload: {error}"));
    assert_eq!(payload["model"], DEFAULT_TOKENHUB_MODEL);
    assert_eq!(payload["text"], "hello");
    assert_eq!(payload["source"], "en");
    assert_eq!(payload["target"], "zh");
    assert_eq!(payload["stream"], false);
    assert!(sent.headers.iter().any(|(name, value)| {
        name == "authorization" && matches!(value, HeaderValue::Secret(_))
    }));
    let diagnostic = format!("{sent:?}");
    assert!(!diagnostic.contains("tokenhub-secret"));
}

#[tokio::test]
async fn tokenhub_sends_a_bounded_user_configured_model_name() {
    let transport = Arc::new(MockTransport::new([Ok(response(
        200,
        r#"{"choices":[{"finish_reason":"stop","message":{"content":"你好"}}]}"#,
    ))]));
    let provider = TencentTokenHubProvider::new(
        transport.clone(),
        SecretString::from("tokenhub-secret".to_owned()),
        TimeoutConfig::default(),
    )
    .with_model("  organization/custom-translation-v1  ")
    .unwrap_or_else(|error| unreachable!("model: {error}"));
    provider
        .translate(&request())
        .await
        .unwrap_or_else(|error| unreachable!("provider: {error}"));
    let sent = transport.take_request();
    let payload: serde_json::Value =
        serde_json::from_slice(&sent.body).unwrap_or_else(|error| unreachable!("payload: {error}"));
    assert_eq!(payload["model"], "organization/custom-translation-v1");
    assert!(
        TencentTokenHubProvider::new(
            Arc::new(MockTransport::new([])),
            SecretString::from("tokenhub-secret".to_owned()),
            TimeoutConfig::default(),
        )
        .with_model("invalid model")
        .is_err()
    );
}

#[tokio::test]
async fn google_basic_v2_uses_nmt_text_request_and_secret_header() {
    let transport = Arc::new(MockTransport::new([Ok(response(
        200,
        r#"{"data":{"translations":[{"translatedText":"你好"}]}}"#,
    ))]));
    let provider = GoogleBasicV2Provider::new(
        transport.clone(),
        SecretString::from("google-secret".to_owned()),
        TimeoutConfig::default(),
    );
    assert_eq!(
        provider
            .translate(&request())
            .await
            .unwrap_or_else(|error| unreachable!("provider: {error}"))
            .text,
        "你好"
    );
    let sent = transport.take_request();
    assert_eq!(sent.url, GOOGLE_TRANSLATE_ENDPOINT);
    let payload: serde_json::Value =
        serde_json::from_slice(&sent.body).unwrap_or_else(|error| unreachable!("payload: {error}"));
    assert_eq!(payload["q"], "hello");
    assert_eq!(payload["format"], "text");
    assert_eq!(payload["model"], "nmt");
    assert!(sent.headers.iter().any(|(name, value)| {
        name == "x-goog-api-key" && matches!(value, HeaderValue::Secret(_))
    }));
    assert!(!format!("{sent:?}").contains("google-secret"));
}

#[tokio::test]
async fn provider_errors_preserve_strict_fallback_categories() {
    for (status, expected) in [
        (401, TranslationError::Unauthorized),
        (429, TranslationError::RateLimited),
        (503, TranslationError::ProviderUnavailable),
        (400, TranslationError::UnsupportedInput),
    ] {
        let transport = Arc::new(MockTransport::new([Ok(response(
            status,
            r#"{"error":{"message":"failure"}}"#,
        ))]));
        let provider = GoogleBasicV2Provider::new(
            transport,
            SecretString::from("key".to_owned()),
            TimeoutConfig::default(),
        );
        assert_eq!(provider.translate(&request()).await, Err(expected));
    }
}

#[derive(Debug)]
struct StubProvider {
    id: ProviderId,
    answers: Mutex<VecDeque<Result<&'static str, TranslationError>>>,
}

#[async_trait]
impl TranslationProvider for StubProvider {
    fn id(&self) -> ProviderId {
        self.id.clone()
    }

    async fn translate(
        &self,
        _request: &TranslationRequest,
    ) -> Result<TranslationResult, TranslationError> {
        self.answers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
            .unwrap_or(Err(TranslationError::InvalidResponse))
            .map(|text| TranslationResult {
                text: text.to_owned(),
                provider: self.id.clone(),
            })
    }
}

fn stub(
    id: &str,
    answers: impl IntoIterator<Item = Result<&'static str, TranslationError>>,
) -> Arc<StubProvider> {
    Arc::new(StubProvider {
        id: ProviderId::new(id),
        answers: Mutex::new(answers.into_iter().collect()),
    })
}

#[tokio::test]
async fn router_falls_back_only_for_explicit_transient_failure() {
    let primary = stub("primary", [Err(TranslationError::RequestTimeout)]);
    let fallback = stub("fallback", [Ok("fallback result")]);
    let mut registry = ProviderRegistry::default();
    registry.register(primary);
    registry.register(fallback);
    let router = TranslationRouter::new(
        &registry,
        &RouterSettings {
            primary: ProviderId::new("primary"),
            fallback: Some(ProviderId::new("fallback")),
        },
    )
    .unwrap_or_else(|error| unreachable!("settings: {error}"));
    assert_eq!(
        router
            .translate(&request())
            .await
            .unwrap_or_else(|error| unreachable!("router: {error}"))
            .text,
        "fallback result"
    );

    let primary = stub("primary", [Err(TranslationError::Unauthorized)]);
    let fallback = stub("fallback", [Ok("must not run")]);
    let mut registry = ProviderRegistry::default();
    registry.register(primary);
    registry.register(fallback.clone());
    let router = TranslationRouter::new(
        &registry,
        &RouterSettings {
            primary: ProviderId::new("primary"),
            fallback: Some(ProviderId::new("fallback")),
        },
    )
    .unwrap_or_else(|error| unreachable!("settings: {error}"));
    assert_eq!(
        router.translate(&request()).await,
        Err(TranslationError::Unauthorized)
    );
    assert_eq!(
        fallback
            .answers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len(),
        1
    );
}

#[test]
fn unconfigured_selected_provider_blocks_settings() {
    let registry = ProviderRegistry::default();
    assert_eq!(
        registry.validate(&RouterSettings::default()),
        Err(SettingsError::ProviderNotConfigured(ProviderId::new(
            "tencent-tokenhub"
        )))
    );
}

#[derive(Debug, Default)]
struct MemoryCredentials {
    values: Mutex<HashMap<CredentialScope, Vec<u8>>>,
}

impl CredentialStore for MemoryCredentials {
    fn get(&self, scope: &CredentialScope) -> Result<Option<Vec<u8>>, AuthError> {
        Ok(self
            .values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(scope)
            .cloned())
    }

    fn contains(&self, scope: &CredentialScope) -> Result<bool, AuthError> {
        Ok(self
            .values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(scope))
    }

    fn set(&self, scope: &CredentialScope, secret: &[u8]) -> Result<(), AuthError> {
        self.values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(scope.clone(), secret.to_vec());
        Ok(())
    }

    fn delete(&self, scope: &CredentialScope) -> Result<(), AuthError> {
        self.values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(scope);
        Ok(())
    }
}

#[test]
fn credential_reader_uses_profile_scoped_tokenhub_key() {
    let store = MemoryCredentials::default();
    let scope = CredentialScope {
        server_origin: "https://server".to_owned(),
        user_id: "user".to_owned(),
        device_id: "device".to_owned(),
        key: CredentialKey::TencentTokenHubApiKey,
    };
    store
        .set(&scope, b"secret")
        .unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let reader = CredentialReader::new(&store, "https://server", "user", "device");
    assert!(reader.tokenhub_api_key().is_ok());
    assert!(matches!(
        reader.google_api_key(),
        Err(ProviderCredentialError::Missing)
    ));
}
