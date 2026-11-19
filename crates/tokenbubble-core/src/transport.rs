//! Bounded, cookie-less, non-redirecting HTTP used by every collector.

use std::fmt;
use std::time::Duration;

use crate::secret::Secret;

/// An HTTP request. `Debug` never shows the bearer token.
#[derive(Clone, Debug)]
pub struct Request {
    pub url: String,
    pub bearer: Option<Secret>,
    pub headers: Vec<(&'static str, String)>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
    /// `Retry-After` in delta-seconds form; HTTP-date values are ignored.
    pub retry_after: Option<Duration>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransportError {
    Timeout,
    BodyTooLarge {
        limit: u64,
    },
    /// A 3xx response. Redirects are never followed so credentials cannot leave the origin.
    Redirect {
        status: u16,
    },
    Network(String),
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout => f.write_str("request timed out"),
            Self::BodyTooLarge { limit } => write!(f, "response exceeded {limit} bytes"),
            Self::Redirect { status } => write!(f, "refused to follow HTTP {status} redirect"),
            Self::Network(message) => write!(f, "network error: {message}"),
        }
    }
}

impl std::error::Error for TransportError {}

pub trait Transport: Send + Sync {
    fn get(&self, request: &Request) -> Result<Response, TransportError>;

    /// POST `body` as `application/json`. The body is a [`Secret`] because it may carry
    /// credentials (an OAuth refresh token); it never reaches logs or errors.
    fn post_json(&self, request: &Request, body: &Secret) -> Result<Response, TransportError>;
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Whole-request deadline including connect, TLS, and body.
    pub timeout: Duration,
    pub max_body_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(20),
            max_body_bytes: 1024 * 1024,
        }
    }
}

/// Production transport: rustls, environment proxies, no cookies, no redirects.
pub struct HttpTransport {
    agent: ureq::Agent,
    limits: Limits,
}

impl HttpTransport {
    pub fn new(limits: Limits) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(limits.timeout))
            .max_redirects(0)
            .http_status_as_error(false)
            .user_agent(concat!("TokenBubble/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Self { agent, limits }
    }
}

impl Default for HttpTransport {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl Transport for HttpTransport {
    fn get(&self, request: &Request) -> Result<Response, TransportError> {
        let result = with_headers(self.agent.get(&request.url), request).call();
        self.finish(result)
    }

    fn post_json(&self, request: &Request, body: &Secret) -> Result<Response, TransportError> {
        let mut builder = with_headers(self.agent.post(&request.url), request);
        if request.header("Content-Type").is_none() {
            builder = builder.content_type("application/json");
        }
        let result = builder.send(body.expose());
        self.finish(result)
    }
}

fn with_headers<B>(
    mut builder: ureq::RequestBuilder<B>,
    request: &Request,
) -> ureq::RequestBuilder<B> {
    if let Some(bearer) = &request.bearer {
        builder = builder.header("Authorization", format!("Bearer {}", bearer.expose()));
    }
    for (name, value) in &request.headers {
        builder = builder.header(*name, value);
    }
    builder
}

impl HttpTransport {
    /// Applies the shared redirect, status, and body-size policy to any response.
    fn finish(
        &self,
        result: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    ) -> Result<Response, TransportError> {
        let mut response = result.map_err(|error| map_error(error, self.limits))?;
        let status = response.status().as_u16();
        if (300..400).contains(&status) {
            return Err(TransportError::Redirect { status });
        }
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let body = response
            .body_mut()
            .with_config()
            .limit(self.limits.max_body_bytes)
            .read_to_vec()
            .map_err(|error| map_error(error, self.limits))?;
        Ok(Response {
            status,
            body,
            retry_after,
        })
    }
}

fn map_error(error: ureq::Error, limits: Limits) -> TransportError {
    match error {
        ureq::Error::Timeout(_) => TransportError::Timeout,
        ureq::Error::BodyExceedsLimit(_) => TransportError::BodyTooLarge {
            limit: limits.max_body_bytes,
        },
        ureq::Error::Io(io) if io.kind() == std::io::ErrorKind::TimedOut => TransportError::Timeout,
        other => TransportError::Network(other.to_string()),
    }
}
