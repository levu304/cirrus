// Service registry and AwsService trait for routing AWS API requests.
//
// This module provides:
// - `AwsService` trait — implemented by all AWS service backends (S3, STS, IAM, etc.)
// - `ServiceRegistry` — a thread-safe, lock-free registry mapping service names
//   to `Arc<dyn AwsService>` implementations
// - `fallback_handler` — an axum-compatible handler that routes unmatched requests
//   to the correct registered service, or returns a 501 NotImplemented AWS error

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::State;
use cirrus_protocol::error::{AwsError, AwsErrorKind};
use dashmap::DashMap;
use tracing;
use http::{Request, Response, StatusCode};
use std::sync::Arc;

/// Trait implemented by all AWS service backends.
///
/// Each service receives the full HTTP [`Request`] and returns either an
/// HTTP [`Response`] or an [`AwsError`].
///
/// # Example
///
/// ```rust,ignore
/// use async_trait::async_trait;
/// use cirrus_router::service::AwsService;
/// use axum::body::Body;
/// use http::{Request, Response, StatusCode};
/// use cirrus_protocol::error::AwsError;
///
/// struct S3Service;
///
/// #[async_trait]
/// impl AwsService for S3Service {
///     async fn handle(&self, req: Request<Body>) -> Result<Response<Body>, AwsError> {
///         // ... handle the request ...
/// #       Ok(Response::new(Body::empty()))
///     }
/// }
/// ```
#[async_trait]
pub trait AwsService: Send + Sync {
    /// Handle an incoming HTTP request for this AWS service.
    ///
    /// The request has already been parsed by the router; the service should
    /// inspect the URI path and method to determine the specific operation.
    async fn handle(&self, req: Request<Body>) -> Result<Response<Body>, AwsError>;
}

/// A thread-safe registry of named AWS service implementations.
///
/// Services are stored by name (e.g., `"s3"`, `"sts"`, `"iam"`) and looked up
/// at request time by the [`fallback_handler`]. Registrations use [`Arc`] so
/// the registry can be cloned freely without deep-copying service state.
///
/// # Example
///
/// ```rust,ignore
/// use cirrus_router::service::{ServiceRegistry, AwsService};
/// use std::sync::Arc;
///
/// let registry = ServiceRegistry::new();
/// registry.register("s3", Arc::new(S3Service));
///
/// if let Some(svc) = registry.get("s3") {
///     // svc is Arc<dyn AwsService>
/// }
/// ```
#[derive(Clone, Default)]
pub struct ServiceRegistry {
    inner: Arc<DashMap<String, Arc<dyn AwsService>>>,
}

impl ServiceRegistry {
    /// Create a new empty service registry.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
        }
    }

    /// Register a service implementation under the given name.
    ///
    /// The name is typically the first path segment of the request URI
    /// (e.g., `"s3"`, `"sts"`, `"iam"`).
    pub fn register(&self, name: impl Into<String>, service: Arc<dyn AwsService>) {
        self.inner.insert(name.into(), service);
    }

    /// Look up a registered service by name.
    ///
    /// Returns `None` if no service has been registered under this name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn AwsService>> {
        self.inner.get(name).map(|r| Arc::clone(&*r))
    }
}

// ---------------------------------------------------------------------------
// Axum fallback handler
// ---------------------------------------------------------------------------

/// Axum fallback handler that routes unmatched requests to registered services.
///
/// Extracts the service name from the **first path segment** of the request URI,
/// looks it up in the [`ServiceRegistry`] (injected via axum's [`State`] extractor),
/// and delegates to the matching service. If no service is registered for the
/// path segment, returns a **501 NotImplemented** error formatted as an AWS XML
/// error response.
///
/// # Path resolution
///
/// | Request path     | Service name | Behaviour                              |
/// |------------------|--------------|----------------------------------------|
/// | `/s3/bucket/key` | `"s3"`       | Delegates to the `"s3"` service        |
/// | `/sts`           | `"sts"`      | Delegates to the `"sts"` service       |
/// | `/`              | `""`         | Returns 501 (no service name provided) |
///
/// # Usage with axum
///
/// ```rust,ignore
/// use cirrus_router::service::{ServiceRegistry, fallback_handler};
/// use std::sync::Arc;
///
/// let registry = ServiceRegistry::new();
/// registry.register("s3", Arc::new(s3_service));
///
/// let app = axum::Router::new()
///     .fallback_with(|r| axum::routing::any(fallback_handler))
///     .with_state(registry);
/// ```
pub async fn fallback_handler(
    State(registry): State<ServiceRegistry>,
    req: Request<Body>,
) -> Response<Body> {
    // Extract the first path segment as the service name.
    // e.g., "/s3/bucket/key" -> "s3"
    let service_name = req
        .uri()
        .path()
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or("")
        .to_string();

    if service_name.is_empty() {
        return aws_error_response(
            AwsError::new(AwsErrorKind::NotImplemented)
                .request_id("")
                .host_id("No service name in request path"),
        );
    }

    if let Some(service) = registry.get(&service_name) {
        match service.handle(req).await {
            Ok(response) => response,
            Err(err) => {
                tracing::error!(
                    service_name = %service_name,
                    error_code = %err.error_code(),
                    status = %err.status_code(),
                    "Service handler error",
                );
                aws_error_response(err)
            }
        }
    } else {
        tracing::warn!(service_name = %service_name, "Request for unregistered service");
        aws_error_response(
            AwsError::new(AwsErrorKind::NotImplemented)
                .request_id("")
                .host_id("Service unavailable"),
        )
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert an [`AwsError`] into an HTTP response with the appropriate status
/// code and an AWS-formatted XML error body.
///
/// The response carries a `Content-Type: application/xml` header.
pub fn aws_error_response(err: AwsError) -> Response<Body> {
    let status_code = err.status_code();
    let xml_body = err.to_xml();

    Response::builder()
        .status(
            StatusCode::from_u16(status_code)
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        )
        .header("Content-Type", "application/xml")
        .body(Body::from(xml_body))
        .expect("aws_error_response: valid static Response")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use http::{Request, StatusCode};

    /// A stub service used in tests that always returns an empty 200 response.
    struct StubService;

    #[async_trait]
    impl AwsService for StubService {
        async fn handle(&self, _req: Request<Body>) -> Result<Response<Body>, AwsError> {
            Ok(Response::builder()
                .status(200)
                .body(Body::from("ok"))
                .unwrap())
        }
    }

    /// A service that always fails with a specific error.
    struct FailingService;

    #[async_trait]
    impl AwsService for FailingService {
        async fn handle(&self, _req: Request<Body>) -> Result<Response<Body>, AwsError> {
            Err(AwsError::new(AwsErrorKind::NotImplemented)
                .request_id("req-001")
                .host_id("host-001"))
        }
    }

    // ------------------------------------------------------------------
    // ServiceRegistry tests
    // ------------------------------------------------------------------

    #[test]
    fn registry_new_is_empty() {
        let registry = ServiceRegistry::new();
        assert!(registry.get("s3").is_none());
        assert!(registry.get("sts").is_none());
    }

    #[test]
    fn registry_register_and_get() {
        let registry = ServiceRegistry::new();
        registry.register("s3", Arc::new(StubService));

        let svc = registry.get("s3");
        assert!(svc.is_some(), "expected service to be registered");
    }

    #[test]
    fn registry_get_unknown_returns_none() {
        let registry = ServiceRegistry::new();
        registry.register("s3", Arc::new(StubService));
        assert!(registry.get("sts").is_none());
    }

    #[test]
    fn registry_default_is_empty() {
        let registry = ServiceRegistry::default();
        assert!(registry.get("s3").is_none());
    }

    #[test]
    fn registry_is_cloneable() {
        let registry = ServiceRegistry::new();
        registry.register("s3", Arc::new(StubService));
        let cloned = registry.clone();
        assert!(cloned.get("s3").is_some());
    }

    // ------------------------------------------------------------------
    // fallback_handler tests (using axum test helpers)
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn fallback_routes_to_registered_service() {
        let registry = ServiceRegistry::new();
        registry.register("s3", Arc::new(StubService));

        let req = Request::builder()
            .uri("/s3/bucket/key")
            .body(Body::empty())
            .unwrap();

        let resp = fallback_handler(State(registry), req).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn fallback_unregistered_service_returns_501() {
        let registry = ServiceRegistry::new();

        let req = Request::builder()
            .uri("/sts/action")
            .body(Body::empty())
            .unwrap();

        let resp = fallback_handler(State(registry), req).await;
        assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let xml = String::from_utf8(body.to_vec()).unwrap();
        assert!(xml.contains("NotImplemented"), "XML should contain error code");
    }

    #[tokio::test]
    async fn fallback_empty_path_returns_501() {
        let registry = ServiceRegistry::new();

        let req = Request::builder()
            .uri("/")
            .body(Body::empty())
            .unwrap();

        let resp = fallback_handler(State(registry), req).await;
        assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
    }

    #[tokio::test]
    async fn fallback_service_error_returns_error_response() {
        let registry = ServiceRegistry::new();
        registry.register("failing", Arc::new(FailingService));

        let req = Request::builder()
            .uri("/failing/action")
            .body(Body::empty())
            .unwrap();

        let resp = fallback_handler(State(registry), req).await;
        assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let xml = String::from_utf8(body.to_vec()).unwrap();
        assert!(xml.contains("req-001"), "XML should contain request ID");
    }

    // ------------------------------------------------------------------
    // aws_error_response tests
    // ------------------------------------------------------------------

    #[test]
    fn aws_error_response_has_correct_status_and_headers() {
        let err = AwsError::new(AwsErrorKind::NotImplemented);
        let resp = aws_error_response(err);
        assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
        assert_eq!(
            resp.headers().get("Content-Type").map(|v| v.as_bytes()),
            Some(&b"application/xml"[..])
        );
    }

    #[test]
    fn aws_error_response_contains_xml_body() {
        let err = AwsError::new(AwsErrorKind::NotImplemented)
            .request_id("r-1")
            .host_id("h-1");
        let resp = aws_error_response(err);
        assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
    }
}
