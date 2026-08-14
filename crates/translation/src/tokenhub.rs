use std::sync::Arc;

use async_trait::async_trait;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};

use crate::{
    HeaderValue, HttpMethod, HttpRequest, HttpTransport, ProviderId, TimeoutConfig,
    TranslationError, TranslationProvider, TranslationRequest, TranslationResult, TransportError,
};

pub(crate) const TOKENHUB_PROVIDER_ID: &str = "tencent-tokenhub";
pub const DEFAULT_TOKENHUB_MODEL: &str = "hy-mt2-lite";
pub const TOKENHUB_TRANSLATE_ENDPOINT: &str =
    "https://tokenhub.tencentmaas.com/v1/api/translations";

#[derive(Clone, Debug)]
pub struct TencentTokenHubProvider {
    transport: Arc<dyn HttpTransport>,
    api_key: Arc<SecretString>,
    model: String,
    timeout: TimeoutConfig,
}

impl TencentTokenHubProvider {
    #[must_use]
    pub fn new(
        transport: Arc<dyn HttpTransport>,
        api_key: SecretString,
        timeout: TimeoutConfig,
    ) -> Self {
        Self {
            transport,
            api_key: Arc::new(api_key),
            model: DEFAULT_TOKENHUB_MODEL.to_owned(),
            timeout,
        }
    }

    /// Selects one of the official `TokenHub` translation model identifiers.
    ///
    /// # Errors
    /// Returns an error when `model` is not an official supported translation model.
    pub fn with_model(mut self, model: &str) -> Result<Self, TranslationError> {
        if !matches!(model, "hy-mt2-lite" | "hy-mt2-plus" | "hy-mt2-pro") {
            return Err(TranslationError::MissingConfiguration);
        }
        model.clone_into(&mut self.model);
        Ok(self)
    }
}

#[async_trait]
impl TranslationProvider for TencentTokenHubProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new(TOKENHUB_PROVIDER_ID)
    }

    async fn translate(
        &self,
        request: &TranslationRequest,
    ) -> Result<TranslationResult, TranslationError> {
        if request.text.is_empty() {
            return Err(TranslationError::UnsupportedInput);
        }
        let source = tokenhub_language(request.source_language.as_str())?;
        let target = tokenhub_language(request.target_language.as_str())?;
        let payload = TokenHubRequest {
            model: &self.model,
            text: &request.text,
            source,
            target,
            stream: false,
        };
        let body = serde_json::to_vec(&payload).map_err(|_| TranslationError::InvalidResponse)?;
        let response = self
            .transport
            .send(HttpRequest {
                method: HttpMethod::Post,
                url: TOKENHUB_TRANSLATE_ENDPOINT.to_owned(),
                headers: vec![
                    (
                        "content-type".to_owned(),
                        HeaderValue::Public("application/json".to_owned()),
                    ),
                    (
                        "authorization".to_owned(),
                        HeaderValue::Secret(Arc::new(SecretString::from(format!(
                            "Bearer {}",
                            secrecy::ExposeSecret::expose_secret(self.api_key.as_ref())
                        )))),
                    ),
                ],
                body,
                request_timeout: self.timeout.request,
            })
            .await
            .map_err(map_transport_error)?;
        parse_response(response.status, &response.body).map(|text| TranslationResult {
            text,
            provider: self.id(),
        })
    }
}

fn tokenhub_language(language: &str) -> Result<&str, TranslationError> {
    match language {
        "zh-CN" => Ok("zh"),
        "zh" | "en" | "fr" | "pt" | "es" | "ja" | "tr" | "ru" | "ar" | "ko" | "th" | "it"
        | "de" | "vi" | "ms" | "id" | "fil" | "hi" | "pl" | "cs" | "nl" | "km" | "my" | "fa"
        | "gu" | "ur" | "te" | "mr" | "he" | "bn" | "ta" | "uk" | "bo" | "kk" | "mn" | "ug"
        | "yue" => Ok(language),
        _ => Err(TranslationError::UnsupportedInput),
    }
}

#[derive(Serialize)]
struct TokenHubRequest<'a> {
    model: &'a str,
    text: &'a str,
    source: &'a str,
    target: &'a str,
    stream: bool,
}

#[derive(Deserialize)]
struct TokenHubResponse {
    choices: Vec<TokenHubChoice>,
}

#[derive(Deserialize)]
struct TokenHubChoice {
    finish_reason: Option<String>,
    message: TokenHubMessage,
}

#[derive(Deserialize)]
struct TokenHubMessage {
    content: String,
}

#[derive(Deserialize)]
struct TokenHubErrorEnvelope {
    error: Option<TokenHubError>,
}

#[derive(Deserialize)]
struct TokenHubError {
    code: Option<String>,
    message: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
}

fn parse_response(status: u16, body: &[u8]) -> Result<String, TranslationError> {
    if !(200..300).contains(&status) {
        return Err(classify_http_error(status, body));
    }
    let response: TokenHubResponse =
        serde_json::from_slice(body).map_err(|_| TranslationError::InvalidResponse)?;
    let choice = response
        .choices
        .into_iter()
        .next()
        .ok_or(TranslationError::InvalidResponse)?;
    if choice.finish_reason.as_deref() == Some("sensitive") {
        return Err(TranslationError::UnsupportedInput);
    }
    if choice.message.content.is_empty() {
        return Err(TranslationError::InvalidResponse);
    }
    Ok(choice.message.content)
}

fn classify_http_error(status: u16, body: &[u8]) -> TranslationError {
    if matches!(status, 401 | 403) {
        return TranslationError::Unauthorized;
    }
    if status == 429 {
        return TranslationError::RateLimited;
    }
    if status >= 500 {
        return TranslationError::ProviderUnavailable;
    }
    let envelope: Option<TokenHubErrorEnvelope> = serde_json::from_slice(body).ok();
    let detail = envelope
        .and_then(|value| value.error)
        .map(|error| {
            format!(
                "{} {} {}",
                error.code.unwrap_or_default(),
                error.kind.unwrap_or_default(),
                error.message.unwrap_or_default()
            )
            .to_ascii_lowercase()
        })
        .unwrap_or_default();
    if detail.contains("api_key")
        || detail.contains("apikey")
        || detail.contains("authentication")
        || detail.contains("unauthorized")
    {
        TranslationError::Unauthorized
    } else if status == 408 {
        TranslationError::RequestTimeout
    } else {
        TranslationError::UnsupportedInput
    }
}

fn map_transport_error(error: TransportError) -> TranslationError {
    match error {
        TransportError::Configuration => TranslationError::MissingConfiguration,
        TransportError::Network => TranslationError::Network,
        TransportError::ConnectTimeout => TranslationError::ConnectTimeout,
        TransportError::RequestTimeout => TranslationError::RequestTimeout,
        TransportError::ResponseTooLarge => TranslationError::InvalidResponse,
    }
}
