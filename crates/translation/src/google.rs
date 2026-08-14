use std::sync::Arc;

use async_trait::async_trait;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};

use crate::{
    HeaderValue, HttpMethod, HttpRequest, HttpTransport, ProviderId, TimeoutConfig,
    TranslationError, TranslationProvider, TranslationRequest, TranslationResult, TransportError,
};

pub(crate) const GOOGLE_PROVIDER_ID: &str = "google-basic-v2";
pub const GOOGLE_TRANSLATE_ENDPOINT: &str =
    "https://translation.googleapis.com/language/translate/v2";

#[derive(Clone, Debug)]
pub struct GoogleBasicV2Provider {
    transport: Arc<dyn HttpTransport>,
    api_key: Arc<SecretString>,
    timeout: TimeoutConfig,
}

impl GoogleBasicV2Provider {
    #[must_use]
    pub fn new(
        transport: Arc<dyn HttpTransport>,
        api_key: SecretString,
        timeout: TimeoutConfig,
    ) -> Self {
        Self {
            transport,
            api_key: Arc::new(api_key),
            timeout,
        }
    }
}

#[async_trait]
impl TranslationProvider for GoogleBasicV2Provider {
    fn id(&self) -> ProviderId {
        ProviderId::new(GOOGLE_PROVIDER_ID)
    }

    async fn translate(
        &self,
        request: &TranslationRequest,
    ) -> Result<TranslationResult, TranslationError> {
        if request.text.is_empty() {
            return Err(TranslationError::UnsupportedInput);
        }
        let payload = GoogleRequest {
            q: &request.text,
            source: request.source_language.as_str(),
            target: request.target_language.as_str(),
            format: "text",
            model: "nmt",
        };
        let body = serde_json::to_vec(&payload).map_err(|_| TranslationError::InvalidResponse)?;
        let response = self
            .transport
            .send(HttpRequest {
                method: HttpMethod::Post,
                url: GOOGLE_TRANSLATE_ENDPOINT.to_owned(),
                headers: vec![
                    (
                        "content-type".to_owned(),
                        HeaderValue::Public("application/json".to_owned()),
                    ),
                    (
                        "x-goog-api-key".to_owned(),
                        HeaderValue::Secret(Arc::clone(&self.api_key)),
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

#[derive(Serialize)]
struct GoogleRequest<'a> {
    q: &'a str,
    source: &'a str,
    target: &'a str,
    format: &'static str,
    model: &'static str,
}

#[derive(Deserialize)]
struct GoogleResponse {
    data: GoogleData,
}

#[derive(Deserialize)]
struct GoogleData {
    translations: Vec<GoogleTranslation>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoogleTranslation {
    translated_text: String,
}

#[derive(Deserialize)]
struct GoogleErrorEnvelope {
    error: Option<GoogleError>,
}

#[derive(Deserialize)]
struct GoogleError {
    code: Option<u16>,
    message: Option<String>,
    status: Option<String>,
}

fn parse_response(status: u16, body: &[u8]) -> Result<String, TranslationError> {
    if !(200..300).contains(&status) {
        return Err(classify_http_error(status, body));
    }
    let response: GoogleResponse =
        serde_json::from_slice(body).map_err(|_| TranslationError::InvalidResponse)?;
    let text = response
        .data
        .translations
        .into_iter()
        .next()
        .ok_or(TranslationError::InvalidResponse)?
        .translated_text;
    if text.is_empty() {
        Err(TranslationError::InvalidResponse)
    } else {
        Ok(text)
    }
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
    let error: Option<GoogleError> = serde_json::from_slice::<GoogleErrorEnvelope>(body)
        .ok()
        .and_then(|value| value.error);
    let credential_error = error.as_ref().is_some_and(|detail| {
        detail.code.is_some_and(|code| matches!(code, 401 | 403))
            || detail.status.as_deref() == Some("PERMISSION_DENIED")
            || detail
                .message
                .as_deref()
                .is_some_and(|message| message.to_ascii_lowercase().contains("api key"))
    });
    if credential_error {
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
