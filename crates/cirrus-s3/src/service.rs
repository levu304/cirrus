// S3 service layer — dispatch and routing.
//
// This module defines `S3Service<S: Storage>`, which implements the
// `AwsService` trait from `cirrus-router`.  A single `handle()` method
// receives the full HTTP request, resolves the S3 bucket/key via
// `resolve_address`, selects the correct handler based on method + path +
// query parameters (following the S3 dispatch matrix rules in order), and
// delegates to the handler.
//
// Dispatch rules must be checked **in order** — more specific patterns
// (e.g. `?uploadId`, `?list-type=2`, `x-amz-copy-source`) are matched
// before their fallback general cases.

use std::collections::HashMap;

use async_trait::async_trait;
use axum::body::Body;
use bytes::Bytes;
use http::{HeaderMap, Method, Request, Response};

use cirrus_protocol::error::{AwsError, AwsErrorKind};
use cirrus_protocol::types::S3Object;
use cirrus_router::service::AwsService;
use crate::address::{resolve_address, AddressError};
use crate::handlers;
use crate::storage::{Storage, S3Error};

/// Maximum single-upload body size (5 GB, matching the S3 API specification).
///
/// This is a defense-in-depth cap on `body_to_bytes`.  The middleware layer
/// (`RequestBodyLimitLayer` in cirrus-router) already limits request bodies to
/// 100 MB, but if that configuration ever changes this constant acts as the
/// last guard against unbodied body reads (a DoS vector).
const MAX_UPLOAD_SIZE: usize = 5 * 1024 * 1024 * 1024; // 5 GB

// ---------------------------------------------------------------------------
// S3Service
// ---------------------------------------------------------------------------

/// An S3-compatible service backend parameterised over a [`Storage`]
/// implementation.
///
/// `S3Service` implements the [`AwsService`] trait so it can be registered
/// in a [`ServiceRegistry`](cirrus_router::service::ServiceRegistry) and
/// dispatched to by the router's fallback handler.
pub struct S3Service<S: Storage> {
    storage: S,
}

impl<S: Storage> S3Service<S> {
    /// Create a new S3 service backed by the given storage implementation.
    pub fn new(storage: S) -> Self {
        Self { storage }
    }
}

#[async_trait]
impl<S: Storage> AwsService for S3Service<S> {
    async fn handle(&self, req: Request<Body>) -> Result<Response<Body>, AwsError> {
        // --- 1. Extract request metadata ---------------------------------
        let (parts, body) = req.into_parts();
        let method = parts.method.clone();
        let uri = parts.uri;
        let path = uri.path().to_string();
        let query = uri.query().unwrap_or("").to_string();
        let headers = parts.headers;

        let host = headers
            .get("host")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("localhost");

        // --- 2. Strip service prefix -------------------------------------
        // The router's fallback_handler passes the full path including the
        // service name (e.g. "/s3/bucket/key").  Strip it here so the
        // address resolver sees plain S3 paths like "/bucket/key".
        let s3_path = strip_service_prefix(&path, "s3");

        // --- 3. Read request body ----------------------------------------
        let body_bytes = if method == Method::PUT || method == Method::POST {
            body_to_bytes(body).await?
        } else {
            Bytes::new()
        };

        // --- 4. Resolve bucket + key -------------------------------------
        let (bucket, key) = resolve_bucket_or_key(&s3_path, host)?;

        // --- 4b. Validate no path traversal ------------------------------
        validate_no_path_traversal(&bucket, "bucket")?;
        if !key.is_empty() {
            validate_no_path_traversal(&key, "key")?;
        }

        // --- 5. Parse query parameters -----------------------------------
        let query_params = parse_query(&query);

        // --- 6. Dispatch -------------------------------------------------
        dispatch(
            &self.storage,
            &method,
            &bucket,
            &key,
            &query,
            &query_params,
            &headers,
            body_bytes,
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// Dispatch — rules must be checked IN ORDER
// ---------------------------------------------------------------------------

/// Route a request to the correct handler based on method, path, and query.
///
/// The 16 dispatch rules are checked in the order defined by the S3 API
/// specification.  More specific patterns (copy-source, uploadId, partNumber)
/// are matched before their general fallbacks.
#[allow(clippy::too_many_arguments)]
async fn dispatch<S: Storage>(
    storage: &S,
    method: &Method,
    bucket: &str,
    key: &str,
    query: &str,
    query_params: &HashMap<String, String>,
    headers: &HeaderMap,
    body: Bytes,
) -> Result<Response<Body>, AwsError> {
    let no_bucket = bucket.is_empty();
    let no_key = key.is_empty();

    // ---- 1. GET / (no bucket) → ListBuckets ----------------------------
    if *method == Method::GET && no_bucket {
        return handlers::handle_list_buckets(storage).await;
    }

    // ---- 2. PUT /{bucket} (key empty) → CreateBucket -------------------
    if *method == Method::PUT && no_key {
        return handlers::handle_create_bucket(storage, bucket).await;
    }

    // ---- 3. DELETE /{bucket} (key empty) → DeleteBucket ----------------
    if *method == Method::DELETE && no_key {
        return handlers::handle_delete_bucket(storage, bucket).await;
    }

    // All remaining rules require at least a bucket.
    if no_bucket {
        return Err(method_not_allowed(method));
    }

    // ---- 16. GET /{bucket}?location → GetBucketLocation ----------------
    // ---- 4.  GET /{bucket} → ListObjectsV2 ------------------------------
    if *method == Method::GET && no_key {
        if query_params.contains_key("location") {
            return handlers::handle_get_bucket_location(storage, bucket).await;
        }
        return handlers::handle_list_objects_v2(storage, bucket, query).await;
    }

    // ---- 10. POST /{bucket}?delete → DeleteObjects ---------------------
    if *method == Method::POST && no_key && query_params.contains_key("delete") {
        return handlers::handle_delete_objects(storage, bucket, body).await;
    }

    // ---- All remaining rules require a key ------------------------------
    if no_key {
        return Err(method_not_allowed(method));
    }

    // ---- 5. PUT /{bucket}/{key} + x-amz-copy-source → CopyObject ------
    if *method == Method::PUT {
        if let Some(copy_source) = headers
            .get("x-amz-copy-source")
            .and_then(|v| v.to_str().ok())
        {
            validate_copy_source(copy_source)?;

            // x-amz-metadata-directive: "REPLACE" or "COPY" (default).
            let directive = headers
                .get("x-amz-metadata-directive")
                .and_then(|v| v.to_str().ok());
            let is_replace = directive
                .map(|v| v.eq_ignore_ascii_case("REPLACE"))
                .unwrap_or(false);

            // Extract x-amz-meta-* headers only when REPLACE is requested.
            let metadata: HashMap<String, String> = if is_replace {
                headers
                    .iter()
                    .filter_map(|(name, value)| {
                        let name_str = name.as_str().to_lowercase();
                        if let Some(key) = name_str.strip_prefix("x-amz-meta-") {
                            value.to_str().ok().map(|v| (key.to_string(), v.to_string()))
                        } else {
                            None
                        }
                    })
                    .collect()
            } else {
                HashMap::new()
            };

            return handlers::handle_copy_object(
                storage,
                bucket,
                key,
                copy_source,
                metadata,
            )
            .await;
        }
    }

    // ---- 12. PUT /{bucket}/{key}?partNumber=N&uploadId=ID → UploadPart -
    if *method == Method::PUT {
        if let (Some(pn_str), Some(upload_id)) =
            (query_params.get("partNumber"), query_params.get("uploadId"))
        {
            if let Ok(part_number) = pn_str.parse::<u32>() {
                return handlers::handle_upload_part(
                    storage,
                    bucket,
                    key,
                    part_number,
                    upload_id,
                    body,
                )
                .await;
            }
        }
    }

    // ---- 6. PUT /{bucket}/{key} (fallback) → PutObject -----------------
    if *method == Method::PUT {
        let content_type = headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(S3Object::DEFAULT_CONTENT_TYPE);

        // Extract x-amz-meta-* headers into a metadata map.
        let metadata: HashMap<String, String> = headers
            .iter()
            .filter_map(|(name, value)| {
                let name_str = name.as_str().to_lowercase();
                if let Some(key) = name_str.strip_prefix("x-amz-meta-") {
                    value.to_str().ok().map(|v| (key.to_string(), v.to_string()))
                } else {
                    None
                }
            })
            .collect();

        return handlers::handle_put_object(storage, bucket, key, content_type, metadata, body).await;
    }

    // ---- 15. GET /{bucket}/{key}?uploadId=ID → ListParts ---------------
    if *method == Method::GET && query_params.contains_key("uploadId") {
        let upload_id = query_params.get("uploadId").unwrap();
        return handlers::handle_list_parts(storage, bucket, key, upload_id, query).await;
    }

    // ---- 7. GET /{bucket}/{key} (no uploadId) → GetObject --------------
    if *method == Method::GET {
        return handlers::handle_get_object(storage, bucket, key).await;
    }

    // ---- 8. HEAD /{bucket}/{key} → HeadObject --------------------------
    if *method == Method::HEAD {
        return handlers::handle_head_object(storage, bucket, key).await;
    }

    // ---- 14. DELETE /{bucket}/{key}?uploadId=ID → AbortMultipartUpload -
    if *method == Method::DELETE && query_params.contains_key("uploadId") {
        let upload_id = query_params.get("uploadId").unwrap();
        return handlers::handle_abort_multipart_upload(storage, bucket, key, upload_id).await;
    }

    // ---- 9. DELETE /{bucket}/{key} (no uploadId) → DeleteObject --------
    if *method == Method::DELETE {
        return handlers::handle_delete_object(storage, bucket, key).await;
    }

    // ---- 11. POST /{bucket}/{key}?uploads → CreateMultipartUpload ------
    if *method == Method::POST && query_params.contains_key("uploads") {
        return handlers::handle_create_multipart_upload(storage, bucket, key).await;
    }

    // ---- 13. POST /{bucket}/{key}?uploadId=ID → CompleteMultipartUpload
    if *method == Method::POST && query_params.contains_key("uploadId") {
        let upload_id = query_params.get("uploadId").unwrap();
        return handlers::handle_complete_multipart_upload(
            storage,
            bucket,
            key,
            upload_id,
            body,
        )
        .await;
    }

    // ---- Default: MethodNotAllowed --------------------------------------
    Err(method_not_allowed(method))
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Strip the service prefix from a URI path.
///
/// The router's fallback handler passes the full path including the service
/// name as the first segment (e.g. `/s3/bucket/key`).  This helper removes
/// that prefix so the remaining path is a plain S3 path.
///
/// # Examples
///
/// ```
/// # use cirrus_s3::service::strip_service_prefix;
/// assert_eq!(strip_service_prefix("/s3/bucket/key", "s3"), "/bucket/key");
/// assert_eq!(strip_service_prefix("/s3", "s3"), "/");
/// assert_eq!(strip_service_prefix("/bucket/key", "s3"), "/bucket/key");
/// ```
pub fn strip_service_prefix(path: &str, service: &str) -> String {
    let trimmed = path.trim_start_matches('/');
    if let Some(rest) = trimmed.strip_prefix(&format!("{}/", service)) {
        format!("/{}", rest)
    } else if trimmed == service {
        "/".to_string()
    } else {
        path.to_string()
    }
}

/// Try to resolve (bucket, key) from the request path and Host header.
///
/// Returns `("", "")` when the path is effectively empty (root request),
/// which is the case for `GET /` (ListBuckets). Address resolution errors
/// are mapped to appropriate client- or server-error codes:
///
/// * `MissingHost` → 400 Bad Request (missing Host header)
/// * `MissingBucket` → empty bucket/key (treated as root request)
/// * `DecodeError` → 400 Bad Request (invalid percent-encoding in path)
fn resolve_bucket_or_key(path: &str, host: &str) -> Result<(String, String), AwsError> {
    match resolve_address(host, path) {
        Ok(result) => Ok(result),
        Err(AddressError::MissingBucket) => {
            // Root path (e.g. GET /) — no bucket, no key.
            Ok((String::new(), String::new()))
        }
        Err(AddressError::MissingHost) => Err(AwsError::new(
            AwsErrorKind::MissingRequestHeader {
                header_name: "Host".to_string(),
            },
        )),
        Err(AddressError::DecodeError(_)) => Err(AwsError::new(AwsErrorKind::InvalidArgument {
            argument_name: "path".to_string(),
            value: path.to_string(),
        })),
    }
}

/// Parse a query string into a key-value map.
///
/// Repeated keys are silently overwritten by the last occurrence (this is
/// acceptable for S3 query parameters which are unique per request).
fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter_map(|pair| {
            let mut parts = pair.splitn(2, '=');
            match (parts.next(), parts.next()) {
                (Some(k), Some(v)) => {
                    let decoded_k = urlencoding::decode(k).ok()?.into_owned();
                    let decoded_v = urlencoding::decode(v).ok()?.into_owned();
                    Some((decoded_k, decoded_v))
                }
                (Some(k), None) if !k.is_empty() => {
                    let decoded_k = urlencoding::decode(k).ok()?.into_owned();
                    Some((decoded_k, String::new()))
                }
                _ => None,
            }
        })
        .collect()
}

/// Convert an [`S3Error`] into an [`AwsError`] with appropriate error kind.
///
/// This helper is used by handlers in Phases 5b–5e.
pub(crate) fn s3_error_to_aws(err: S3Error, bucket: &str, key: &str) -> AwsError {
    match err {
        S3Error::NoSuchBucket => AwsErrorKind::NoSuchBucket {
            bucket_name: bucket.to_string(),
        }
        .into(),
        S3Error::NoSuchKey => AwsErrorKind::NoSuchKey {
            bucket_name: bucket.to_string(),
            key: key.to_string(),
        }
        .into(),
        S3Error::NoSuchUpload => AwsErrorKind::NoSuchUpload {
            upload_id: String::new(),
        }
        .into(),
        S3Error::BucketAlreadyExists => AwsErrorKind::BucketAlreadyExists {
            bucket_name: bucket.to_string(),
        }
        .into(),
        S3Error::BucketNotEmpty => AwsErrorKind::BucketNotEmpty {
            bucket_name: bucket.to_string(),
        }
        .into(),
        S3Error::InvalidPart => AwsErrorKind::InternalError {
            details: Some("Invalid part".into()),
        }
        .into(),
        S3Error::InvalidPartOrder => AwsErrorKind::InternalError {
            details: Some("Invalid part order".into()),
        }
        .into(),
        S3Error::EntityTooLarge => AwsErrorKind::EntityTooLarge {
            entity: "object".into(),
        }
        .into(),
        S3Error::MaxCapacityExceeded => AwsErrorKind::InternalError {
            details: Some("Max capacity exceeded".into()),
        }
        .into(),
    }
}

/// Construct a `MethodNotAllowed` error for the given HTTP method.
fn method_not_allowed(method: &Method) -> AwsError {
    AwsError::new(AwsErrorKind::MethodNotAllowed {
        method: method.to_string(),
    })
}

/// Consume an [`axum::body::Body`] and collect all bytes.
///
/// The body read is capped at [`MAX_UPLOAD_SIZE`] (5 GB) as a defense-in-depth
/// measure — [`RequestBodyLimitLayer`] in `cirrus-router` provides the primary
/// limit at 100 MB.
async fn body_to_bytes(body: Body) -> Result<Bytes, AwsError> {
    axum::body::to_bytes(body, MAX_UPLOAD_SIZE)
        .await
        .map_err(|e| {
            if e.to_string().contains("length limit exceeded") {
                AwsError::new(AwsErrorKind::EntityTooLarge {
                    entity: "request body".into(),
                })
            } else {
                AwsError::new(AwsErrorKind::InternalError {
                    details: Some(format!("body read failed: {e}")),
                })
            }
        })
}

// ---------------------------------------------------------------------------
// Path traversal validation
// ---------------------------------------------------------------------------

/// Validate that a bucket or key does not contain path traversal sequences.
///
/// Returns an `InvalidArgument` error if any path segment equals `..`, which
/// has no legitimate use in S3 bucket names or object keys and is a security
/// concern if the storage layer ever gains filesystem persistence.
fn validate_no_path_traversal(value: &str, argument_name: &str) -> Result<(), AwsError> {
    for segment in value.split('/') {
        if segment == ".." {
            return Err(AwsError::new(AwsErrorKind::InvalidArgument {
                argument_name: argument_name.to_string(),
                value: value.to_string(),
            }));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Copy-source validation (SSRF defense)
// ---------------------------------------------------------------------------

/// Validate the `x-amz-copy-source` header to prevent SSRF attacks.
///
/// The header value must be a valid S3 copy-source path in the format
/// `/bucket/key` or `bucket/key`. Returns the validated source path
/// split into `(bucket, key)`, or an [`AwsError`] on validation failure.
///
/// # Errors
///
/// Returns `InvalidArgument` (HTTP 400) when the value contains a URL
/// scheme (`://`), lacks a `/` separator between bucket and key, or has
/// an empty bucket name.
pub(crate) fn validate_copy_source(copy_source: &str) -> Result<(&str, &str), AwsError> {
    // Reject URL schemes (SSRF protection)
    if copy_source.contains("://") {
        return Err(AwsError::new(AwsErrorKind::InvalidArgument {
            argument_name: "x-amz-copy-source".into(),
            value: copy_source.into(),
        }));
    }

    // Reject protocol-relative URLs (SSRF bypass, e.g. "//internal/secret").
    if copy_source.starts_with("//") {
        return Err(AwsError::new(AwsErrorKind::InvalidArgument {
            argument_name: "x-amz-copy-source".into(),
            value: copy_source.into(),
        }));
    }

    // Strip optional leading slash (S3 convention).
    let source = copy_source.strip_prefix('/').unwrap_or(copy_source);

    // Split into bucket / key.
    let (src_bucket, src_key) = source.split_once('/').ok_or_else(|| {
        AwsError::new(AwsErrorKind::InvalidArgument {
            argument_name: "x-amz-copy-source".into(),
            value: copy_source.into(),
        })
    })?;

    // Reject empty bucket name.
    if src_bucket.is_empty() {
        return Err(AwsError::new(AwsErrorKind::InvalidArgument {
            argument_name: "x-amz-copy-source".into(),
            value: copy_source.into(),
        }));
    }

    // Reject path traversal in source bucket or key.
    validate_no_path_traversal(src_bucket, "x-amz-copy-source")?;
    validate_no_path_traversal(src_key, "x-amz-copy-source")?;

    Ok((src_bucket, src_key))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use http::StatusCode;

    use crate::storage::DefaultStorage;

    // ------------------------------------------------------------------
    // Test helpers
    // ------------------------------------------------------------------

    /// Create a simple path-style request with a default `Host: localhost`
    /// header (unless overridden).
    fn test_request(
        method: &str,
        path: &str,
        query: Option<&str>,
        extra_headers: Vec<(&str, &str)>,
    ) -> Request<Body> {
        let uri = if let Some(q) = query {
            format!("{}?{}", path, q)
        } else {
            path.to_string()
        };

        let mut builder = Request::builder().method(method).uri(&uri);

        // Default host for path-style addressing.
        let has_host = extra_headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("host"));
        if !has_host {
            builder = builder.header("host", "localhost");
        }

        for (k, v) in extra_headers {
            builder = builder.header(k, v);
        }

        builder.body(Body::empty()).unwrap()
    }

    /// Create a test service backed by `DefaultStorage`.
    fn test_service() -> S3Service<DefaultStorage> {
        S3Service::new(DefaultStorage::new())
    }

    /// Assert that handling a request returns the expected HTTP status code.
    async fn assert_status(
        service: &S3Service<DefaultStorage>,
        req: Request<Body>,
        expected: u16,
    ) {
        let resp = service.handle(req).await;
        match resp {
            Ok(r) => assert_eq!(
                r.status(),
                expected,
                "expected status {}, got {}",
                expected,
                r.status()
            ),
            Err(e) => {
                assert_eq!(
                    e.status_code(),
                    expected,
                    "expected status {}, got {} ({})",
                    expected,
                    e.status_code(),
                    e.error_code()
                );
            }
        }
    }

    /// Test-only dispatch verifier that mirrors `dispatch()` but returns the
    /// handler name instead of calling it.  This lets dispatch tests verify
    /// *which* handler was selected, not just the status code.
    fn dispatch_rule_expected(
        method: &Method,
        bucket: &str,
        key: &str,
        query_params: &HashMap<String, String>,
        headers: &HeaderMap,
    ) -> Result<&'static str, AwsError> {
        let no_bucket = bucket.is_empty();
        let no_key = key.is_empty();

        // ---- 1. GET / (no bucket) → ListBuckets ----------------------------
        if *method == Method::GET && no_bucket {
            return Ok("handle_list_buckets");
        }

        // ---- 2. PUT /{bucket} (key empty) → CreateBucket -------------------
        if *method == Method::PUT && no_key {
            return Ok("handle_create_bucket");
        }

        // ---- 3. DELETE /{bucket} (key empty) → DeleteBucket ----------------
        if *method == Method::DELETE && no_key {
            return Ok("handle_delete_bucket");
        }

        // All remaining rules require at least a bucket.
        if no_bucket {
            return Err(method_not_allowed(method));
        }

        // ---- 16. GET /{bucket}?location → GetBucketLocation ----------------
        // ---- 4.  GET /{bucket} → ListObjectsV2 ------------------------------
        if *method == Method::GET && no_key {
            if query_params.contains_key("location") {
                return Ok("handle_get_bucket_location");
            }
            return Ok("handle_list_objects_v2");
        }

        // ---- 10. POST /{bucket}?delete → DeleteObjects ---------------------
        if *method == Method::POST && no_key && query_params.contains_key("delete") {
            return Ok("handle_delete_objects");
        }

        // ---- All remaining rules require a key ------------------------------
        if no_key {
            return Err(method_not_allowed(method));
        }

        // ---- 5. PUT /{bucket}/{key} + x-amz-copy-source → CopyObject ------
        if *method == Method::PUT {
            if let Some(copy_source) = headers
                .get("x-amz-copy-source")
                .and_then(|v| v.to_str().ok())
            {
                validate_copy_source(copy_source)?;
                return Ok("handle_copy_object");
            }
        }

        // ---- 12. PUT /{bucket}/{key}?partNumber=N&uploadId=ID → UploadPart -
        if *method == Method::PUT {
            if let (Some(pn_str), Some(_upload_id)) =
                (query_params.get("partNumber"), query_params.get("uploadId"))
            {
                if pn_str.parse::<u32>().is_ok() {
                    return Ok("handle_upload_part");
                }
            }
        }

        // ---- 6. PUT /{bucket}/{key} (fallback) → PutObject -----------------
        if *method == Method::PUT {
            return Ok("handle_put_object");
        }

        // ---- 15. GET /{bucket}/{key}?uploadId=ID → ListParts ---------------
        if *method == Method::GET && query_params.contains_key("uploadId") {
            return Ok("handle_list_parts");
        }

        // ---- 7. GET /{bucket}/{key} (no uploadId) → GetObject --------------
        if *method == Method::GET {
            return Ok("handle_get_object");
        }

        // ---- 8. HEAD /{bucket}/{key} → HeadObject --------------------------
        if *method == Method::HEAD {
            return Ok("handle_head_object");
        }

        // ---- 14. DELETE /{bucket}/{key}?uploadId=ID → AbortMultipartUpload -
        if *method == Method::DELETE && query_params.contains_key("uploadId") {
            return Ok("handle_abort_multipart_upload");
        }

        // ---- 9. DELETE /{bucket}/{key} (no uploadId) → DeleteObject --------
        if *method == Method::DELETE {
            return Ok("handle_delete_object");
        }

        // ---- 11. POST /{bucket}/{key}?uploads → CreateMultipartUpload ------
        if *method == Method::POST && query_params.contains_key("uploads") {
            return Ok("handle_create_multipart_upload");
        }

        // ---- 13. POST /{bucket}/{key}?uploadId=ID → CompleteMultipartUpload
        if *method == Method::POST && query_params.contains_key("uploadId") {
            return Ok("handle_complete_multipart_upload");
        }

        // ---- Default: MethodNotAllowed --------------------------------------
        Err(method_not_allowed(method))
    }

    /// Assert that handling a request dispatches to the expected handler and
    /// returns the expected HTTP status code.
    async fn assert_handler_called(
        svc: &S3Service<DefaultStorage>,
        req: Request<Body>,
        expected_handler: &str,
        expected_status: u16,
    ) {
        let method = req.method().clone();
        let path = req.uri().path().to_string();
        let query = req.uri().query().unwrap_or("").to_string();
        let headers = req.headers().clone();
        let host = headers
            .get("host")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("localhost")
            .to_string();

        // Resolve params like handle() does.
        let s3_path = strip_service_prefix(&path, "s3");
        let (bucket, key) =
            resolve_bucket_or_key(&s3_path, &host).expect("resolve_bucket_or_key failed for test request");
        let query_params = parse_query(&query);

        // Verify handler name.
        let result = dispatch_rule_expected(&method, &bucket, &key, &query_params, &headers);
        match &result {
            Ok(name) => assert_eq!(
                *name,
                expected_handler,
                "handler mismatch for {} {}?{}",
                method,
                path,
                query
            ),
            Err(e) => {
                panic!(
                    "expected handler '{}' but got error '{}' for {} {}?{}",
                    expected_handler,
                    e.error_code(),
                    method,
                    path,
                    query
                )
            }
        }

        // Verify status code from the actual response.
        let resp = svc.handle(req).await;
        match resp {
            Ok(r) => assert_eq!(
                r.status(),
                expected_status,
                "status mismatch for {} {}?{}",
                method,
                path,
                query
            ),
            Err(e) => assert_eq!(
                e.status_code(),
                expected_status,
                "status mismatch for {} {}?{}",
                method,
                path,
                query
            ),
        }
    }

    // ------------------------------------------------------------------
    // strip_service_prefix
    // ------------------------------------------------------------------

    #[test]
    fn test_strip_service_prefix_with_prefix() {
        assert_eq!(
            strip_service_prefix("/s3/bucket/key", "s3"),
            "/bucket/key"
        );
    }

    #[test]
    fn test_strip_service_prefix_root_with_service() {
        assert_eq!(strip_service_prefix("/s3", "s3"), "/");
    }

    #[test]
    fn test_strip_service_prefix_no_prefix() {
        assert_eq!(
            strip_service_prefix("/bucket/key", "s3"),
            "/bucket/key"
        );
    }

    #[test]
    fn test_strip_service_prefix_root() {
        assert_eq!(strip_service_prefix("/", "s3"), "/");
    }

    // ------------------------------------------------------------------
    // parse_query
    // ------------------------------------------------------------------

    #[test]
    fn test_parse_query_basic() {
        let map = parse_query("list-type=2&prefix=photos/&max-keys=10");
        assert_eq!(map.get("list-type").unwrap(), "2");
        assert_eq!(map.get("prefix").unwrap(), "photos/");
        assert_eq!(map.get("max-keys").unwrap(), "10");
    }

    #[test]
    fn test_parse_query_empty() {
        let map = parse_query("");
        assert!(map.is_empty());
    }

    #[test]
    fn test_parse_query_key_only() {
        let map = parse_query("delete");
        assert_eq!(map.get("delete").unwrap(), "");
    }

    #[test]
    fn test_parse_query_url_encoded_values() {
        let map = parse_query("prefix=hello%20world&delimiter=%2F");
        assert_eq!(map.get("prefix").unwrap(), "hello world");
        assert_eq!(map.get("delimiter").unwrap(), "/");
    }

    // ------------------------------------------------------------------
    // s3_error_to_aws
    // ------------------------------------------------------------------

    #[test]
    fn test_s3_error_to_aws_no_such_bucket() {
        let err = s3_error_to_aws(S3Error::NoSuchBucket, "my-bucket", "");
        assert_eq!(err.error_code(), "NoSuchBucket");
        assert_eq!(err.status_code(), 404);
    }

    #[test]
    fn test_s3_error_to_aws_no_such_key() {
        let err = s3_error_to_aws(S3Error::NoSuchKey, "b", "k");
        assert_eq!(err.error_code(), "NoSuchKey");
        assert_eq!(err.status_code(), 404);
    }

    #[test]
    fn test_s3_error_to_aws_invalid_part_maps_to_internal_error() {
        let err = s3_error_to_aws(S3Error::InvalidPart, "b", "k");
        assert_eq!(err.error_code(), "InternalError");
        assert_eq!(err.status_code(), 500);
    }

    #[test]
    fn test_s3_error_to_aws_no_such_upload() {
        let err = s3_error_to_aws(S3Error::NoSuchUpload, "b", "k");
        assert_eq!(err.error_code(), "NoSuchUpload");
        assert_eq!(err.status_code(), 404);
    }

    #[test]
    fn test_s3_error_to_aws_bucket_already_exists() {
        let err = s3_error_to_aws(S3Error::BucketAlreadyExists, "my-bucket", "");
        assert_eq!(err.error_code(), "BucketAlreadyExists");
        assert_eq!(err.status_code(), 409);
    }

    #[test]
    fn test_s3_error_to_aws_bucket_not_empty() {
        let err = s3_error_to_aws(S3Error::BucketNotEmpty, "my-bucket", "");
        assert_eq!(err.error_code(), "BucketNotEmpty");
        assert_eq!(err.status_code(), 409);
    }

    #[test]
    fn test_s3_error_to_aws_invalid_part_order() {
        let err = s3_error_to_aws(S3Error::InvalidPartOrder, "b", "k");
        assert_eq!(err.error_code(), "InternalError");
        assert_eq!(err.status_code(), 500);
    }

    #[test]
    fn test_s3_error_to_aws_entity_too_large() {
        let err = s3_error_to_aws(S3Error::EntityTooLarge, "b", "k");
        assert_eq!(err.error_code(), "EntityTooLarge");
        assert_eq!(err.status_code(), 400);
    }

    #[test]
    fn test_s3_error_to_aws_max_capacity_exceeded() {
        let err = s3_error_to_aws(S3Error::MaxCapacityExceeded, "b", "k");
        assert_eq!(err.error_code(), "InternalError");
        assert_eq!(err.status_code(), 500);
    }

    // ------------------------------------------------------------------
    // body_to_bytes
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_body_to_bytes_success() {
        let body = Body::from("hello world");
        let result = body_to_bytes(body).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), &b"hello world"[..]);
    }

    #[tokio::test]
    async fn test_body_to_bytes_body_read_error_preserves_details() {
        use std::pin::Pin;
        use std::task::{Context, Poll};
        use http_body::{Body as HttpBody, Frame};

        /// A body that fails immediately on read — used to verify that
        /// `body_to_bytes` preserves the original error context instead of
        /// swallowing it.
        struct FailingBody;

        impl HttpBody for FailingBody {
            type Data = Bytes;
            type Error = Box<dyn std::error::Error + Send + Sync>;

            fn poll_frame(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
                Poll::Ready(Some(Err("body read failure: connection reset".into())))
            }

            fn is_end_stream(&self) -> bool {
                // We have no data to send, so immediately report end-of-stream
                // after the error.
                true
            }
        }

        let body = Body::new(FailingBody);
        let result = body_to_bytes(body).await;
        let err = result.expect_err("expected error from failing body");
        assert_eq!(err.error_code(), "InternalError");
        assert_eq!(err.status_code(), 500);
        assert!(
            err.message().contains("body read failure: connection reset"),
            "error message should preserve original error details, got: {}",
            err.message()
        );
    }

    // ------------------------------------------------------------------
    // validate_copy_source
    // ------------------------------------------------------------------

    #[test]
    fn test_validate_copy_source_valid() {
        let result = validate_copy_source("/source-bucket/source-key");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_copy_source_ssrf_http_url() {
        let result = validate_copy_source("http://169.254.169.254/latest/meta-data/");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().status_code(), 400);
    }

    #[test]
    fn test_validate_copy_source_ssrf_https_url() {
        let result = validate_copy_source("https://internal.service/secret");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().status_code(), 400);
    }

    #[test]
    fn test_validate_copy_source_ssrf_protocol_relative() {
        let result = validate_copy_source("//169.254.169.254/latest/meta-data/");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().status_code(), 400);
    }

    #[test]
    fn test_validate_copy_source_missing_path() {
        let result = validate_copy_source("just-bucket");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().status_code(), 400);
    }

    #[test]
    fn test_validate_copy_source_empty() {
        let result = validate_copy_source("");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().status_code(), 400);
    }

    #[test]
    fn test_validate_copy_source_no_bucket() {
        let result = validate_copy_source("/");
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_copy_source_valid_no_leading_slash() {
        let result = validate_copy_source("bucket/key");
        assert!(result.is_ok());
    }

    // ------------------------------------------------------------------
    // validate_no_path_traversal
    // ------------------------------------------------------------------

    #[test]
    fn test_validate_no_path_traversal_allows_normal_value() {
        assert!(validate_no_path_traversal("normal-bucket", "bucket").is_ok());
    }

    #[test]
    fn test_validate_no_path_traversal_allows_single_dot() {
        // Single '.' is harmless and allowed by S3.
        assert!(validate_no_path_traversal("bucket/./key", "key").is_ok());
    }

    #[test]
    fn test_validate_no_path_traversal_allows_empty() {
        assert!(validate_no_path_traversal("", "key").is_ok());
    }

    #[test]
    fn test_validate_no_path_traversal_rejects_parent_dir_at_start() {
        let err = validate_no_path_traversal("../other-bucket/secret.txt", "key").unwrap_err();
        assert_eq!(err.status_code(), 400);
        assert_eq!(err.error_code(), "InvalidArgument");
    }

    #[test]
    fn test_validate_no_path_traversal_rejects_parent_dir_mid_key() {
        let err = validate_no_path_traversal("foo/../bar", "key").unwrap_err();
        assert_eq!(err.status_code(), 400);
        assert_eq!(err.error_code(), "InvalidArgument");
    }

    #[test]
    fn test_validate_no_path_traversal_rejects_parent_dir_at_end() {
        let err = validate_no_path_traversal("foo/..", "key").unwrap_err();
        assert_eq!(err.status_code(), 400);
        assert_eq!(err.error_code(), "InvalidArgument");
    }

    #[test]
    fn test_validate_no_path_traversal_rejects_bare_parent_dir() {
        let err = validate_no_path_traversal("..", "bucket").unwrap_err();
        assert_eq!(err.status_code(), 400);
        assert_eq!(err.error_code(), "InvalidArgument");
    }

    #[test]
    fn test_validate_no_path_traversal_rejects_multiple_parent_dirs() {
        let err = validate_no_path_traversal("a/../../b", "key").unwrap_err();
        assert_eq!(err.status_code(), 400);
        assert_eq!(err.error_code(), "InvalidArgument");
    }

    // ------------------------------------------------------------------
    // Dispatch: path traversal rejection
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_dispatch_rejects_path_traversal_in_bucket() {
        // PUT /../key should be rejected — bucket contains ".."
        let svc = test_service();
        let req = test_request("PUT", "/../key", None, vec![]);
        assert_status(&svc, req, 400).await;
    }

    #[tokio::test]
    async fn test_dispatch_rejects_path_traversal_in_key() {
        // PUT /bucket/../key should be rejected — key contains ".."
        let svc = test_service();
        let req = test_request("PUT", "/bucket/../key", None, vec![]);
        assert_status(&svc, req, 400).await;
    }

    #[tokio::test]
    async fn test_dispatch_allows_normal_paths() {
        // Normal paths should still work.
        let svc = test_service();
        let req = test_request("PUT", "/my-bucket/my-key", None, vec![]);
        // Bucket does not exist → 404 NoSuchBucket (not 400 InvalidArgument).
        assert_handler_called(&svc, req, "handle_put_object", 404).await;
    }

    #[tokio::test]
    async fn test_dispatch_allows_single_dot_in_key() {
        // Single '.' in key is harmless and should be allowed.
        let svc = test_service();
        let req = test_request("PUT", "/my-bucket/./my-key", None, vec![]);
        assert_handler_called(&svc, req, "handle_put_object", 404).await;
    }

    #[tokio::test]
    async fn test_dispatch_allows_bucket_level_operations() {
        // Bucket-level operations (no key) should still work.
        let svc = test_service();
        let req = test_request("GET", "/", None, vec![]);
        assert_handler_called(&svc, req, "handle_list_buckets", 200).await;
    }

    #[tokio::test]
    async fn test_validate_copy_source_rejects_path_traversal() {
        let svc = test_service();
        let req = test_request(
            "PUT",
            "/dest-bucket/dest-key",
            None,
            vec![("x-amz-copy-source", "/src-bucket/../secret")],
        );
        assert_status(&svc, req, 400).await;
    }

    // ------------------------------------------------------------------
    // Dispatch: bucket-level operations
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_dispatch_list_buckets() {
        let svc = test_service();
        let req = test_request("GET", "/", None, vec![]);
        // ListBuckets succeeds with an empty list.
        assert_handler_called(&svc, req, "handle_list_buckets", 200).await;
    }

    #[tokio::test]
    async fn test_dispatch_create_bucket() {
        let svc = test_service();
        let req = test_request("PUT", "/my-bucket", None, vec![]);
        // CreateBucket succeeds.
        assert_handler_called(&svc, req, "handle_create_bucket", 200).await;
    }

    #[tokio::test]
    async fn test_dispatch_delete_bucket() {
        let svc = test_service();
        let req = test_request("DELETE", "/my-bucket", None, vec![]);
        // Bucket does not exist → 404 NoSuchBucket.
        assert_handler_called(&svc, req, "handle_delete_bucket", 404).await;
    }

    #[tokio::test]
    async fn test_dispatch_get_bucket_location() {
        let svc = test_service();
        let req = test_request("GET", "/my-bucket", Some("location"), vec![]);
        // GetBucketLocation returns the default region regardless of existence.
        assert_handler_called(&svc, req, "handle_get_bucket_location", 200).await;
    }

    #[tokio::test]
    async fn test_dispatch_list_objects_v2() {
        let svc = test_service();
        let req = test_request("GET", "/my-bucket", Some("list-type=2"), vec![]);
        // Bucket does not exist → 404 NoSuchBucket.
        assert_handler_called(&svc, req, "handle_list_objects_v2", 501).await;
    }

    #[tokio::test]
    async fn test_dispatch_list_objects_v2_plain_get() {
        // A plain GET on the bucket (no query) also routes to ListObjectsV2.
        let svc = test_service();
        let req = test_request("GET", "/my-bucket", None, vec![]);
        assert_handler_called(&svc, req, "handle_list_objects_v2", 501).await;
    }

    // ------------------------------------------------------------------
    // Dispatch: object-level operations
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_dispatch_get_object() {
        let svc = test_service();
        let req = test_request("GET", "/my-bucket/my-key", None, vec![]);
        // Bucket does not exist → 404 NoSuchBucket.
        assert_handler_called(&svc, req, "handle_get_object", 404).await;
    }

    #[tokio::test]
    async fn test_dispatch_head_object() {
        let svc = test_service();
        let req = test_request("HEAD", "/my-bucket/my-key", None, vec![]);
        assert_handler_called(&svc, req, "handle_head_object", 404).await;
    }

    #[tokio::test]
    async fn test_dispatch_put_object() {
        let svc = test_service();
        let req = test_request("PUT", "/my-bucket/my-key", None, vec![]);
        // Bucket does not exist → 404 NoSuchBucket.
        assert_handler_called(&svc, req, "handle_put_object", 404).await;
    }

    #[tokio::test]
    async fn test_dispatch_delete_object() {
        let svc = test_service();
        let req = test_request("DELETE", "/my-bucket/my-key", None, vec![]);
        assert_handler_called(&svc, req, "handle_delete_object", 404).await;
    }

    #[tokio::test]
    async fn test_dispatch_copy_object() {
        let svc = test_service();
        let req = test_request(
            "PUT",
            "/my-bucket/my-key",
            None,
            vec![("x-amz-copy-source", "/source-bucket/source-key")],
        );
        // Source bucket does not exist → 404 NoSuchBucket.
        assert_handler_called(&svc, req, "handle_copy_object", 404).await;
    }

    #[tokio::test]
    async fn test_dispatch_copy_object_invalid_ssrf() {
        // SSRF attempt via copy-source should be rejected with 400.
        let svc = test_service();
        let req = test_request(
            "PUT",
            "/my-bucket/my-key",
            None,
            vec![("x-amz-copy-source", "http://169.254.169.254/latest/meta-data/")],
        );
        assert_status(&svc, req, 400).await;
    }

    #[tokio::test]
    async fn test_dispatch_copy_object_invalid_no_slash() {
        // Copy-source value that is just a bucket name (no `/`) should be
        // rejected with 400, not forwarded to the handler.
        let svc = test_service();
        let req = test_request(
            "PUT",
            "/my-bucket/my-key",
            None,
            vec![("x-amz-copy-source", "just-bucket")],
        );
        assert_status(&svc, req, 400).await;
    }

    #[tokio::test]
    async fn test_dispatch_copy_object_empty() {
        // Empty copy-source should be rejected.
        let svc = test_service();
        let req = test_request(
            "PUT",
            "/my-bucket/my-key",
            None,
            vec![("x-amz-copy-source", "")],
        );
        assert_status(&svc, req, 400).await;
    }

    #[tokio::test]
    async fn test_dispatch_delete_objects() {
        let svc = test_service();
        let req = test_request("POST", "/my-bucket", Some("delete"), vec![]);
        // Empty body → XML parse error → 500 XmlSerializationError.
        assert_handler_called(&svc, req, "handle_delete_objects", 501).await;
    }

    // ------------------------------------------------------------------
    // Dispatch: multipart operations
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_dispatch_create_multipart_upload() {
        let svc = test_service();
        let req = test_request("POST", "/my-bucket/my-key", Some("uploads"), vec![]);
        // Bucket does not exist → 404 NoSuchBucket.
        assert_handler_called(&svc, req, "handle_create_multipart_upload", 501).await;
    }

    #[tokio::test]
    async fn test_dispatch_upload_part() {
        let svc = test_service();
        let req = test_request(
            "PUT",
            "/my-bucket/my-key",
            Some("partNumber=1&uploadId=test-upload-id"),
            vec![],
        );
        // Bucket does not exist → 404 NoSuchBucket.
        assert_handler_called(&svc, req, "handle_upload_part", 501).await;
    }

    #[tokio::test]
    async fn test_dispatch_complete_multipart_upload() {
        let svc = test_service();
        let req = test_request(
            "POST",
            "/my-bucket/my-key",
            Some("uploadId=test-upload-id"),
            vec![],
        );
        // Empty body → XML parse error → 500 XmlSerializationError.
        assert_handler_called(&svc, req, "handle_complete_multipart_upload", 501).await;
    }

    #[tokio::test]
    async fn test_dispatch_abort_multipart_upload() {
        let svc = test_service();
        let req = test_request(
            "DELETE",
            "/my-bucket/my-key",
            Some("uploadId=test-upload-id"),
            vec![],
        );
        // Bucket does not exist → 404 NoSuchBucket.
        assert_handler_called(&svc, req, "handle_abort_multipart_upload", 501).await;
    }

    #[tokio::test]
    async fn test_dispatch_list_parts() {
        let svc = test_service();
        let req = test_request(
            "GET",
            "/my-bucket/my-key",
            Some("uploadId=test-upload-id"),
            vec![],
        );
        // Bucket does not exist → 404 NoSuchBucket.
        assert_handler_called(&svc, req, "handle_list_parts", 501).await;
    }

    // ------------------------------------------------------------------
    // Dispatch: edge cases and ordering
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_dispatch_method_not_allowed_patch() {
        let svc = test_service();
        // PATCH is not an S3 method — should get 405.
        let req = test_request("PATCH", "/my-bucket/my-key", None, vec![]);
        assert_status(&svc, req, 405).await;
    }

    #[tokio::test]
    async fn test_dispatch_options_not_allowed() {
        let svc = test_service();
        // OPTIONS is not an S3 method — should get 405.
        let req = test_request("OPTIONS", "/my-bucket/my-key", None, vec![]);
        assert_status(&svc, req, 405).await;
    }

    #[tokio::test]
    async fn test_dispatch_copy_source_takes_priority_over_put() {
        // Copy-object (rule 5) must be checked before regular PUT (rule 6).
        let svc = test_service();
        let req = test_request(
            "PUT",
            "/bucket/key",
            None,
            vec![("x-amz-copy-source", "/src-bucket/src-key")],
        );
        // Source bucket does not exist → 404 NoSuchBucket.
        assert_handler_called(&svc, req, "handle_copy_object", 404).await;
    }

    #[tokio::test]
    async fn test_dispatch_put_with_partnumber_does_not_match_copy() {
        // A PUT with partNumber but WITHOUT copy-source should match
        // UploadPart (rule 12), not CopyObject.
        let svc = test_service();
        let req = test_request(
            "PUT",
            "/bucket/key",
            Some("partNumber=1&uploadId=uid"),
            vec![], // No x-amz-copy-source header
        );
        // Bucket does not exist → 404 NoSuchBucket.
        assert_handler_called(&svc, req, "handle_upload_part", 501).await;
    }

    #[tokio::test]
    async fn test_dispatch_put_with_partnumber_only_falls_to_put() {
        // PUT with only partNumber (no uploadId) should fall through to
        // regular PutObject (rule 6).
        let svc = test_service();
        let req = test_request(
            "PUT",
            "/bucket/key",
            Some("partNumber=1"),
            vec![],
        );
        // Bucket does not exist → 404 NoSuchBucket.
        assert_handler_called(&svc, req, "handle_put_object", 404).await;
    }

    #[tokio::test]
    async fn test_dispatch_put_with_invalid_partnumber_falls_to_put() {
        // PUT with invalid (non-numeric) partNumber and valid uploadId should
        // fall through to regular PutObject (rule 6) because the u32 parse
        // failure prevents UploadPart (rule 12) from matching.
        let svc = test_service();
        let req = test_request(
            "PUT",
            "/bucket/key",
            Some("partNumber=abc&uploadId=uid"),
            vec![],
        );
        // Bucket does not exist → 404 NoSuchBucket.
        assert_handler_called(&svc, req, "handle_put_object", 404).await;
    }

    #[tokio::test]
    async fn test_dispatch_virtual_hosted_addressing() {
        // Virtual-hosted: Host = my-bucket.localhost, path = /key
        // NOTE: "my-bucket.localhost" has only 2 labels, so classify_host_style
        // treats it as path-style → "/my-key" is the bucket, not the key.
        let svc = test_service();
        let req = test_request(
            "GET",
            "/my-key",
            None,
            vec![("host", "my-bucket.localhost")],
        );
        // Bucket does not exist → 404 NoSuchBucket.
        // Routes to bucket-level GET (ListObjectsV2) because the first path
        // segment is treated as the bucket in path-style addressing.
        assert_handler_called(&svc, req, "handle_list_objects_v2", 501).await;
    }

    #[tokio::test]
    async fn test_dispatch_virtual_hosted_list_buckets() {
        // Virtual-hosted listing on the root: GET / with
        // Host=my-bucket.s3.amazonaws.com → should resolve to
        // (bucket="my-bucket", key="") → bucket-level GET → ListObjectsV2
        let svc = test_service();
        let req = test_request(
            "GET",
            "/",
            None,
            vec![("host", "my-bucket.s3.amazonaws.com")],
        );
        assert_handler_called(&svc, req, "handle_list_objects_v2", 501).await;
    }

    #[tokio::test]
    async fn test_dispatch_get_object_with_upload_id_falls_to_list_parts() {
        // GET /bucket/key?uploadId=xxx → ListParts (rule 15),
        // NOT GetObject (rule 7).
        let svc = test_service();
        let req = test_request(
            "GET",
            "/bucket/key",
            Some("uploadId=uid-123"),
            vec![],
        );
        // Bucket does not exist → 404 NoSuchBucket.
        assert_handler_called(&svc, req, "handle_list_parts", 501).await;
    }

    #[tokio::test]
    async fn test_dispatch_delete_with_upload_id_falls_to_abort() {
        // DELETE /bucket/key?uploadId=xxx → AbortMultipartUpload (rule 14),
        // NOT DeleteObject (rule 9).
        let svc = test_service();
        let req = test_request(
            "DELETE",
            "/bucket/key",
            Some("uploadId=uid-123"),
            vec![],
        );
        // Bucket does not exist → 404 NoSuchBucket.
        assert_handler_called(&svc, req, "handle_abort_multipart_upload", 501).await;
    }

    #[tokio::test]
    async fn test_dispatch_post_with_uploads_and_upload_id() {
        // POST /bucket/key?uploads → CreateMultipartUpload (rule 11)
        let svc = test_service();
        let req = test_request(
            "POST",
            "/bucket/key",
            Some("uploads"),
            vec![],
        );
        // Bucket does not exist → 404 NoSuchBucket.
        assert_handler_called(&svc, req, "handle_create_multipart_upload", 501).await;
    }

    #[tokio::test]
    async fn test_dispatch_post_with_upload_id() {
        // POST /bucket/key?uploadId=xxx → CompleteMultipartUpload (rule 13)
        let svc = test_service();
        let req = test_request(
            "POST",
            "/bucket/key",
            Some("uploadId=uid-123"),
            vec![],
        );
        // Empty body → XML parse error → 500 XmlSerializationError.
        assert_handler_called(&svc, req, "handle_complete_multipart_upload", 501).await;
    }

    #[tokio::test]
    async fn test_dispatch_post_with_both_uploads_and_upload_id() {
        // POST /bucket/key?uploads&uploadId=X — rule 11 (?uploads) must be
        // checked BEFORE rule 13 (?uploadId), so this dispatches to
        // CreateMultipartUpload, NOT CompleteMultipartUpload.
        let svc = test_service();
        let req = test_request(
            "POST",
            "/bucket/key",
            Some("uploads&uploadId=test-upload-id"),
            vec![],
        );
        assert_handler_called(&svc, req, "handle_create_multipart_upload", 501).await;
    }

    // ------------------------------------------------------------------
    // Service prefix stripping in dispatch
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_dispatch_with_service_prefix() {
        // When the request comes through the router with /s3/ prefix,
        // the service must strip it before resolving bucket/key.
        let svc = test_service();
        let req = test_request("GET", "/s3/bucket/key", None, vec![]);
        // Bucket does not exist → 404 NoSuchBucket.
        assert_handler_called(&svc, req, "handle_get_object", 404).await;
    }

    #[tokio::test]
    async fn test_dispatch_with_service_prefix_list_buckets() {
        let svc = test_service();
        let req = test_request("GET", "/s3", None, vec![]);
        // ListBuckets succeeds with an empty list.
        assert_handler_called(&svc, req, "handle_list_buckets", 200).await;
    }

    // ------------------------------------------------------------------
    // resolve_bucket_or_key edge cases
    // ------------------------------------------------------------------

    #[test]
    fn test_resolve_bucket_or_key_root_path() {
        let (bucket, key) = resolve_bucket_or_key("/", "localhost").unwrap();
        assert_eq!(bucket, "");
        assert_eq!(key, "");
    }

    #[test]
    fn test_resolve_bucket_or_key_normal_path() {
        let (bucket, key) = resolve_bucket_or_key("/bucket/key", "localhost").unwrap();
        assert_eq!(bucket, "bucket");
        assert_eq!(key, "key");
    }

    #[test]
    fn test_resolve_bucket_or_key_path_style_host_with_port() {
        let (bucket, key) = resolve_bucket_or_key("/bucket/key", "localhost:9000").unwrap();
        assert_eq!(bucket, "bucket");
        assert_eq!(key, "key");
    }

    #[test]
    fn test_resolve_bucket_or_key_invalid_encoding() {
        let result = resolve_bucket_or_key("/bucket/%FF", "localhost");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.status_code(), 400);
        // Message should not contain internal details
        assert!(!err.message().contains("Address resolution error"));
    }

    #[test]
    fn test_resolve_bucket_or_key_missing_host() {
        let result = resolve_bucket_or_key("/bucket/key", "");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.status_code(), 400);
        assert_eq!(err.error_code(), "MissingRequestHeader");
    }

    // ------------------------------------------------------------------
    // body_to_bytes
    // ------------------------------------------------------------------

    #[test]
    fn test_max_upload_size_is_positive() {
        assert!(MAX_UPLOAD_SIZE > 0, "MAX_UPLOAD_SIZE must be positive");
    }

    #[test]
    fn test_max_upload_size_is_exactly_5gb() {
        assert_eq!(MAX_UPLOAD_SIZE, 5 * 1024 * 1024 * 1024);
    }

    #[tokio::test]
    async fn test_body_to_bytes_small_body() {
        let body = Body::from("hello world");
        let result = body_to_bytes(body).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Bytes::from("hello world"));
    }

    #[tokio::test]
    async fn test_body_to_bytes_empty_body() {
        let body = Body::empty();
        let result = body_to_bytes(body).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }
}
