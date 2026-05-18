pub mod address;
pub mod middleware;
pub mod service;

// Re-export key public API items for convenience.
pub use service::{aws_error_response, fallback_handler, AwsService, ServiceRegistry};

/// Build the complete axum Router with middleware stack and service registry.
///
/// Middleware stack order (outermost first):
/// 1. `incomplete_body_detection` — verify body matches Content-Length
/// 2. `entity_too_large_interceptor` — convert 413 to AWS 400 EntityTooLarge
/// 3. `RequestBodyLimitLayer` — enforce max request body size
///
/// The [`RequestBodyLimitLayer`] must sit innermost because it transforms the
/// request body to [`Limited<Body>`](tower_http::body::Limited), which axum's
/// `from_fn` middleware does not support — they only work with `Body`.
///
/// The router uses the [`ServiceRegistry`] via axum's [`State`] extractor and
/// routes all unmatched requests through [`fallback_handler`].
///
/// # Example
///
/// ```rust,ignore
/// use cirrus_router::build_router;
/// use cirrus_router::service::ServiceRegistry;
///
/// let registry = ServiceRegistry::new();
/// let app = build_router(registry);
/// ```
pub fn build_router(registry: ServiceRegistry) -> axum::Router<ServiceRegistry> {
    use axum::Router;
    use tower::ServiceBuilder;
    use tower_http::limit::RequestBodyLimitLayer;

    use crate::middleware::{entity_too_large_interceptor, incomplete_body_detection};

    // Max request body: 100 MB (matching AWS S3 behavior)
    const MAX_REQUEST_BYTES: usize = 100 * 1024 * 1024;

    // `from_fn` middleware work only with `Body`, while `RequestBodyLimitLayer`
    // transforms the body to `Limited<Body>`. The limit layer must therefore sit
    // innermost (closest to the route handler), with `from_fn` middleware outside.
    let middleware_stack = ServiceBuilder::new()
        .layer(axum::middleware::from_fn(incomplete_body_detection))
        .layer(axum::middleware::from_fn(entity_too_large_interceptor))
        .layer(RequestBodyLimitLayer::new(MAX_REQUEST_BYTES));

    Router::new()
        .with_state(registry)
        .layer(middleware_stack)
        .fallback(fallback_handler)
}
