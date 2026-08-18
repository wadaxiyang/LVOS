use std::{error::Error, fmt, sync::Arc, time::Duration};

use async_trait::async_trait;
use futures_util::StreamExt;
use secrecy::{ExposeSecret, SecretString};

const MAX_PROVIDER_RESPONSE_BYTES: usize = 1_048_576;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimeoutConfig {
    pub connect: Duration,
    pub request: Duration,
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(5),
            request: Duration::from_secs(15),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HttpMethod {
    Post,
}

#[derive(Clone)]
pub enum HeaderValue {
    Public(String),
    Secret(Arc<SecretString>),
}

impl HeaderValue {
    fn expose(&self) -> &str {
        match self {
            Self::Public(value) => value,
            Self::Secret(value) => value.expose_secret(),
        }
    }
}

impl fmt::Debug for HeaderValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Public(value) => formatter.debug_tuple("Public").field(value).finish(),
            Self::Secret(_) => formatter.write_str("Secret([REDACTED])"),
        }
    }
}

#[derive(Clone)]
pub struct HttpRequest {
    pub method: HttpMethod,
    pub url: String,
    pub headers: Vec<(String, HeaderValue)>,
    pub body: Vec<u8>,
    pub request_timeout: Duration,
}

impl fmt::Debug for HttpRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("url", &redact_query(&self.url))
            .field("headers", &self.headers)
            .field("body_length", &self.body.len())
            .field("request_timeout", &self.request_timeout)
            .finish()
    }
}

fn redact_query(url: &str) -> String {
    url.split_once('?')
        .map_or_else(|| url.to_owned(), |(base, _)| format!("{base}?[REDACTED]"))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

#[async_trait]
pub trait HttpTransport: fmt::Debug + Send + Sync {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError>;
}

#[derive(Clone, Debug)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    /// Creates an HTTPS transport with redirects disabled and an explicit connect timeout.
    ///
    /// # Errors
    /// Returns an error when either timeout is zero or the native client cannot be built.
    pub fn new(timeout: TimeoutConfig, proxy_url: Option<&str>) -> Result<Self, TransportError> {
        if timeout.connect.is_zero() || timeout.request.is_zero() {
            return Err(TransportError::Configuration);
        }
        let provider = rustls::crypto::ring::default_provider();
        let _ = provider.install_default();
        let mut builder = reqwest::Client::builder()
            .connect_timeout(timeout.connect)
            .redirect(reqwest::redirect::Policy::none());
        if let Some(proxy_url) = proxy_url {
            builder = builder
                .proxy(reqwest::Proxy::all(proxy_url).map_err(|_| TransportError::Configuration)?);
        }
        let client = builder.build().map_err(|_| TransportError::Configuration)?;
        Ok(Self { client })
    }
}

#[async_trait]
impl HttpTransport for ReqwestTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let mut builder = match request.method {
            HttpMethod::Post => self.client.post(&request.url),
        }
        .timeout(request.request_timeout)
        .body(request.body);
        for (name, value) in request.headers {
            builder = builder.header(name, value.expose());
        }
        let response = builder
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        let status = response.status().as_u16();
        if response.content_length().is_some_and(|length| {
            usize::try_from(length).map_or(true, |length| length > MAX_PROVIDER_RESPONSE_BYTES)
        }) {
            return Err(TransportError::ResponseTooLarge);
        }
        let mut body = Vec::new();
        let mut chunks = response.bytes_stream();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(|error| classify_reqwest_error(&error))?;
            if body.len().saturating_add(chunk.len()) > MAX_PROVIDER_RESPONSE_BYTES {
                return Err(TransportError::ResponseTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(HttpResponse { status, body })
    }
}

fn classify_reqwest_error(error: &reqwest::Error) -> TransportError {
    if error.is_timeout() && error.is_connect() {
        TransportError::ConnectTimeout
    } else if error.is_timeout() {
        TransportError::RequestTimeout
    } else {
        TransportError::Network
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportError {
    Configuration,
    Network,
    ConnectTimeout,
    RequestTimeout,
    ResponseTooLarge,
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Configuration => "HTTP transport configuration is invalid",
            Self::Network => "HTTP transport failed",
            Self::ConnectTimeout => "HTTP connection timed out",
            Self::RequestTimeout => "HTTP request timed out",
            Self::ResponseTooLarge => "HTTP response exceeded the configured safety limit",
        })
    }
}

impl Error for TransportError {}
