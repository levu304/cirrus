//! S3 address resolution: bucket/key extraction and AWS service name detection.
//!
//! This module provides two core address-resolution primitives:
//!
//! * [`extract_service`] — determines the AWS service name from request
//!   headers (SigV4 `Authorization`, `X-Amz-Target`) or query parameters.
//! * [`resolve_address`] — parses an S3 request into `(bucket, key)` from
//!   the `Host` header and request path, supporting both path-style and
//!   virtual-hosted addressing modes.
//!
//! # Anti-pattern fix: URL decoding
//!
//! All path segments are URL-decoded via the `urlencoding` crate. Raw
//! segments without decoding break Unicode keys and percent-encoded
//! characters (e.g. `hello%20world` → `hello world`).

use http::HeaderMap;
use std::borrow::Cow;
use thiserror::Error;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur during S3 address resolution.
#[derive(Debug, Error)]
pub enum AddressError {
    /// The `Host` header is missing or empty.
    #[error("host header is missing or empty")]
    MissingHost,

    /// No bucket could be parsed from the request (e.g. path-style with no
    /// path segments).
    #[error("no bucket could be parsed from the request")]
    MissingBucket,

    /// A URL-encoded path segment could not be decoded.
    #[error("failed to decode URL-encoded segment: {0}")]
    DecodeError(#[from] std::string::FromUtf8Error),

    /// The request is structurally malformed.
    #[error("malformed request: {0}")]
    MalformedRequest(String),
}

// ---------------------------------------------------------------------------
// Service extraction
// ---------------------------------------------------------------------------

/// Extract the AWS service name from an incoming HTTP request.
///
/// The check order follows the AWS SDK precedence:
///
/// 1. **`Authorization` header** (SigV4 format) — the service is the 4th
///    slash-delimited segment of the credential scope:
///    `AWS4-HMAC-SHA256 Credential=AKID/date/region/service/aws4_request`
/// 2. **`X-Amz-Target` header** — the part before the first `.`, lowercased
///    and stripped of any leading sub-prefix before `_`.
///    e.g. `S3_Test.Bla` → `s3`
/// 3. **`?Service=` query parameter** — lowercased.
/// 4. **Default**: `"s3"` if none of the above match.
pub fn extract_service(headers: &HeaderMap, query: Option<&str>) -> String {
    // 1. Authorization header (SigV4).
    if let Some(auth) = headers
        .get(http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        if let Some(service) = try_extract_service_from_auth(auth) {
            return service;
        }
    }

    // 2. X-Amz-Target header.
    if let Some(target) = headers
        .get("x-amz-target")
        .and_then(|v| v.to_str().ok())
    {
        if let Some(service) = try_extract_service_from_target(target) {
            return service;
        }
    }

    // 3. Query parameter.
    if let Some(qs) = query {
        if let Some(service) = try_extract_service_from_query(qs) {
            return service;
        }
    }

    // 4. Default.
    "s3".to_string()
}

/// Parse the service from a SigV4 `Authorization` header value.
///
/// Expected format (credential scope):
/// `AWS4-HMAC-SHA256 Credential=<access-key>/<date>/<region>/<service>/<suffix>`
///
/// The service is segment index 3 (0-based) after `Credential=`.
fn try_extract_service_from_auth(auth: &str) -> Option<String> {
    // Find the "Credential=..." token.
    let credential = auth
        .split_whitespace()
        .find(|s| s.starts_with("Credential="))?;

    let scope = credential.strip_prefix("Credential=")?;
    let segments: Vec<&str> = scope.split('/').collect();

    // The service is the 4th segment (index 3).
    // Full scope: access-key / date / region / service / aws4_request
    if segments.len() >= 4 {
        Some(segments[3].to_lowercase())
    } else {
        None
    }
}

/// Parse the service from an `X-Amz-Target` header value.
///
/// Format: `<Service>[_<Sub>].<Operation>`.
/// We extract the part before the first `.`, then take the segment before
/// any `_` to get the bare service name, lowercased.
fn try_extract_service_from_target(target: &str) -> Option<String> {
    let prefix = target.split('.').next()?;
    let service = prefix.split('_').next()?;
    if service.is_empty() {
        return None;
    }
    Some(service.to_lowercase())
}

/// Parse the service from a query string by looking for `Service=...`.
fn try_extract_service_from_query(query: &str) -> Option<String> {
    for param in query.split('&') {
        if let Some(value) = param.strip_prefix("Service=") {
            if !value.is_empty() {
                return Some(value.to_lowercase());
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Address resolution
// ---------------------------------------------------------------------------

/// Parse the bucket and key from an S3 request's `Host` header and path.
///
/// Supports two addressing modes:
///
/// * **Virtual-hosted**: `http://bucket.s3.amazonaws.com/key`  
///   The bucket name is the left-most subdomain label.
/// * **Path-style**: `http://s3.amazonaws.com/bucket/key`  
///   The bucket is the first path segment.
///
/// Both addressing modes URL-decode their segments via the `urlencoding`
/// crate, so `%20` → space, `%2F` → `/`, etc.
///
/// # Errors
///
/// Returns [`AddressError::MissingHost`] when `host` is empty,
/// [`AddressError::MissingBucket`] when no bucket can be parsed, and
/// [`AddressError::DecodeError`] when a percent-encoded segment is invalid.

/// Strip the port suffix from a host string, handling IPv6 bracket notation.
///
/// * `"host:9000"` → `"host"`
/// * `"[::1]:9000"` → `"::1"`
/// * `"localhost"` → `"localhost"`
fn strip_port(host: &str) -> &str {
    if let Some(bracket_end) = host.find(']') {
        // IPv6 bracketed host: return the content inside brackets.
        // host is like "[::1]:9000" — we want "::1".
        &host[1..bracket_end]
    } else {
        // Non-bracketed host: split on ':' to remove port.
        host.split(':').next().unwrap_or(host)
    }
}

pub fn resolve_address(host: &str, path: &str) -> Result<(String, String), AddressError> {
    if host.is_empty() {
        return Err(AddressError::MissingHost);
    }

    // Strip any port suffix (e.g. "host:9000" → "host").
    // Handles IPv6 bracketed hosts like "[::1]:9000" → "::1".
    let host = strip_port(host);

    let host_parts: Vec<&str> = host.split('.').collect();

    match classify_host_style(&host_parts) {
        AddressingStyle::VirtualHosted => resolve_virtual_hosted(&host_parts, path),
        AddressingStyle::PathStyle => resolve_path_style(path),
    }
}

/// Distinguishes between virtual-hosted and path-style addressing.
enum AddressingStyle {
    VirtualHosted,
    PathStyle,
}

/// Classify the addressing style based on the hostname structure.
///
/// Heuristic:
/// - 0–2 labels → path-style (no room for a bucket subdomain).
/// - 3+ labels where the first label starts with `s3` → path-style
///   (standard S3 endpoint like `s3.amazonaws.com` or
///   `s3-us-east-1.amazonaws.com`).
/// - 3+ labels where the first label does NOT start with `s3` →
///   virtual-hosted (the first label is the bucket name, e.g.
///   `my-bucket.s3.amazonaws.com`).
///
/// This heuristic covers the vast majority of real-world S3 deployments.
/// One known limitation: a bucket whose name starts with `s3` (e.g.
/// `s3photos`) used with virtual-hosted style on a 3-label endpoint
/// (`s3photos.s3.amazonaws.com`) would be misclassified as path-style.
/// This is extremely rare in practice.
fn classify_host_style(host_parts: &[&str]) -> AddressingStyle {
    match host_parts.len() {
        // 0–2 labels: definitely path-style.
        0..=2 => AddressingStyle::PathStyle,
        // 3+ labels: S3 endpoints always start with `s3`.
        _ if host_parts[0].starts_with("s3") => AddressingStyle::PathStyle,
        // 3+ labels, first isn't an S3 endpoint → virtual-hosted.
        _ => AddressingStyle::VirtualHosted,
    }
}

/// Resolve bucket and key for virtual-hosted style.
///
/// `host_parts[0]` is the bucket name. The entire path (after the leading
/// `/`) is the object key.
fn resolve_virtual_hosted(
    host_parts: &[&str],
    path: &str,
) -> Result<(String, String), AddressError> {
    let bucket_raw = host_parts[0];
    let bucket = decode_segment(bucket_raw)?;

    let key_path = path.trim_start_matches('/');
    let key = if key_path.is_empty() {
        String::new()
    } else {
        decode_segment(key_path)?
    };

    Ok((bucket, key))
}

/// Resolve bucket and key for path-style addressing.
///
/// The first non-empty path segment is the bucket; everything after is the
/// key.
fn resolve_path_style(path: &str) -> Result<(String, String), AddressError> {
    let trimmed = path.trim_start_matches('/');

    if trimmed.is_empty() || trimmed == "/" {
        return Err(AddressError::MissingBucket);
    }

    let mut segments = trimmed.splitn(2, '/');
    let bucket_seg = segments.next().unwrap_or("");

    if bucket_seg.is_empty() {
        return Err(AddressError::MissingBucket);
    }

    let key_seg = segments.next().unwrap_or("");

    let bucket = decode_segment(bucket_seg)?;
    let key = decode_segment(key_seg)?;

    Ok((bucket, key))
}

/// Decode a single percent-encoded path segment.
fn decode_segment(segment: &str) -> Result<String, AddressError> {
    match urlencoding::decode(segment) {
        Ok(Cow::Owned(s)) => Ok(s),
        Ok(Cow::Borrowed(s)) => Ok(s.to_owned()),
        Err(e) => Err(AddressError::DecodeError(e)),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- extract_service tests ------------------------------------------------

    #[test]
    fn test_extract_service_from_auth_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            "AWS4-HMAC-SHA256 Credential=AKID/20210518/us-east-1/s3/aws4_request"
                .parse()
                .unwrap(),
        );
        assert_eq!(extract_service(&headers, None), "s3");
    }

    #[test]
    fn test_extract_service_from_auth_header_custom_service() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            "AWS4-HMAC-SHA256 Credential=AKID/20210518/us-east-1/iam/aws4_request"
                .parse()
                .unwrap(),
        );
        assert_eq!(extract_service(&headers, None), "iam");
    }

    #[test]
    fn test_extract_service_from_x_amz_target() {
        let mut headers = HeaderMap::new();
        headers.insert("x-amz-target", "S3_Test.Bla".parse().unwrap());
        assert_eq!(extract_service(&headers, None), "s3");
    }

    #[test]
    fn test_extract_service_from_x_amz_target_dynamodb() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-amz-target",
            "DynamoDB_20120810.PutItem".parse().unwrap(),
        );
        assert_eq!(extract_service(&headers, None), "dynamodb");
    }

    #[test]
    fn test_extract_service_from_query() {
        let headers = HeaderMap::new();
        assert_eq!(extract_service(&headers, Some("Service=s3")), "s3");
    }

    #[test]
    fn test_extract_service_from_query_custom() {
        let headers = HeaderMap::new();
        assert_eq!(
            extract_service(&headers, Some("Action=ListBuckets&Service=S3")),
            "s3"
        );
    }

    #[test]
    fn test_extract_service_default() {
        let headers = HeaderMap::new();
        assert_eq!(extract_service(&headers, None), "s3");
    }

    #[test]
    fn test_extract_service_auth_takes_precedence() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            "AWS4-HMAC-SHA256 Credential=AKID/20210518/us-east-1/iam/aws4_request"
                .parse()
                .unwrap(),
        );
        headers.insert("x-amz-target", "S3_Test.Bla".parse().unwrap());
        // Authorization should win over X-Amz-Target.
        assert_eq!(extract_service(&headers, Some("Service=ec2")), "iam");
    }

    #[test]
    fn test_extract_service_malformed_auth_short_scope() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            "AWS4-HMAC-SHA256 Credential=AKID/20210518".parse().unwrap(),
        );
        // Falls through because credential scope has fewer than 4 segments.
        assert_eq!(extract_service(&headers, None), "s3");
    }

    #[test]
    fn test_extract_service_malformed_auth_no_credential() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            "AWS4-HMAC-SHA256 Algorithm=HMAC-SHA256"
                .parse()
                .unwrap(),
        );
        assert_eq!(extract_service(&headers, None), "s3");
    }

    #[test]
    fn test_extract_service_empty_target() {
        let mut headers = HeaderMap::new();
        headers.insert("x-amz-target", ".Operation".parse().unwrap());
        // Prefix before '.' is empty, so fall through.
        assert_eq!(extract_service(&headers, None), "s3");
    }

    // -- resolve_address tests -------------------------------------------------

    #[test]
    fn test_path_style_bucket_key() {
        let (bucket, key) = resolve_address("s3.amazonaws.com", "/bucket/key").unwrap();
        assert_eq!(bucket, "bucket");
        assert_eq!(key, "key");
    }

    #[test]
    fn test_path_style_key_with_subdirs() {
        let (bucket, key) = resolve_address("s3.amazonaws.com", "/bucket/path/to/key").unwrap();
        assert_eq!(bucket, "bucket");
        assert_eq!(key, "path/to/key");
    }

    #[test]
    fn test_path_style_root_listing() {
        let (bucket, key) = resolve_address("s3.amazonaws.com", "/bucket").unwrap();
        assert_eq!(bucket, "bucket");
        assert_eq!(key, "");
    }

    #[test]
    fn test_path_style_bucket_with_trailing_slash() {
        let (bucket, key) = resolve_address("s3.amazonaws.com", "/bucket/").unwrap();
        assert_eq!(bucket, "bucket");
        assert_eq!(key, "");
    }

    #[test]
    fn test_virtual_hosted_bucket_key() {
        let (bucket, key) = resolve_address("bucket.s3.amazonaws.com", "/key").unwrap();
        assert_eq!(bucket, "bucket");
        assert_eq!(key, "key");
    }

    #[test]
    fn test_virtual_hosted_key_with_subdirs() {
        let (bucket, key) =
            resolve_address("bucket.s3.amazonaws.com", "/path/to/key").unwrap();
        assert_eq!(bucket, "bucket");
        assert_eq!(key, "path/to/key");
    }

    #[test]
    fn test_virtual_hosted_root_listing() {
        let (bucket, key) = resolve_address("bucket.s3.amazonaws.com", "/").unwrap();
        assert_eq!(bucket, "bucket");
        assert_eq!(key, "");
    }

    #[test]
    fn test_virtual_hosted_no_path() {
        let (bucket, key) = resolve_address("bucket.s3.amazonaws.com", "").unwrap();
        assert_eq!(bucket, "bucket");
        assert_eq!(key, "");
    }

    #[test]
    fn test_virtual_hosted_deep_path() {
        let (bucket, key) =
            resolve_address("my-bucket.s3.amazonaws.com", "/a/b/c/d").unwrap();
        assert_eq!(bucket, "my-bucket");
        assert_eq!(key, "a/b/c/d");
    }

    // -- URL-encoding tests ----------------------------------------------------

    #[test]
    fn test_url_decode_path_style_key() {
        let (bucket, key) = resolve_address("s3.amazonaws.com", "/bucket/hello%20world").unwrap();
        assert_eq!(bucket, "bucket");
        assert_eq!(key, "hello world");
    }

    #[test]
    fn test_url_decode_path_style_bucket() {
        let (bucket, key) = resolve_address("s3.amazonaws.com", "/my%20bucket/key").unwrap();
        assert_eq!(bucket, "my bucket");
        assert_eq!(key, "key");
    }

    #[test]
    fn test_url_decode_virtual_hosted_bucket() {
        let (bucket, key) =
            resolve_address("my%20bucket.s3.amazonaws.com", "/key").unwrap();
        assert_eq!(bucket, "my bucket");
        assert_eq!(key, "key");
    }

    #[test]
    fn test_url_decode_virtual_hosted_key() {
        let (bucket, key) =
            resolve_address("bucket.s3.amazonaws.com", "/hello%20world").unwrap();
        assert_eq!(bucket, "bucket");
        assert_eq!(key, "hello world");
    }

    #[test]
    fn test_url_decode_encoded_slashes() {
        // %2F is decoded to '/' by urlencoding.
        let (bucket, key) = resolve_address(
            "bucket.s3.amazonaws.com",
            "/path%2Fto%2Ffile.txt",
        )
        .unwrap();
        assert_eq!(bucket, "bucket");
        assert_eq!(key, "path/to/file.txt");
    }

    #[test]
    fn test_url_decode_plus_is_preserved() {
        // urlencoding::decode preserves '+' literally (does not convert to space).
        let (bucket, key) = resolve_address("s3.amazonaws.com", "/bucket/hello+world").unwrap();
        assert_eq!(bucket, "bucket");
        assert_eq!(key, "hello+world");
    }

    // -- Error cases -----------------------------------------------------------

    #[test]
    fn test_error_empty_host() {
        let err = resolve_address("", "/bucket/key").unwrap_err();
        assert!(matches!(err, AddressError::MissingHost));
    }

    #[test]
    fn test_error_path_style_no_bucket() {
        let err = resolve_address("s3.amazonaws.com", "/").unwrap_err();
        assert!(matches!(err, AddressError::MissingBucket));
    }

    #[test]
    fn test_error_path_style_empty_path() {
        let err = resolve_address("s3.amazonaws.com", "").unwrap_err();
        assert!(matches!(err, AddressError::MissingBucket));
    }

    #[test]
    fn test_error_invalid_url_encoding() {
        // %FF alone is an invalid UTF-8 byte (0xFF), triggering DecodeError.
        let err = resolve_address("s3.amazonaws.com", "/bucket/%FF").unwrap_err();
        assert!(matches!(err, AddressError::DecodeError(_)));
    }

    // -- Host with port --------------------------------------------------------

    #[test]
    fn test_host_with_port_path_style() {
        let (bucket, key) = resolve_address("localhost:9000", "/bucket/key").unwrap();
        assert_eq!(bucket, "bucket");
        assert_eq!(key, "key");
    }

    #[test]
    fn test_host_with_port_virtual_hosted() {
        // 3+ host labels → virtual-hosted, bucket is the first subdomain.
        let (bucket, key) =
            resolve_address("bucket.s3.example.com:9000", "/key").unwrap();
        assert_eq!(bucket, "bucket");
        assert_eq!(key, "key");
    }

    #[test]
    fn test_ipv6_loopback_path_style() {
        // IPv6 loopback with port, path-style request.
        let (bucket, key) =
            resolve_address("[::1]:9000", "/bucket/key").unwrap();
        assert_eq!(bucket, "bucket");
        assert_eq!(key, "key");
    }

    #[test]
    fn test_ipv6_loopback_no_bucket() {
        // IPv6 with no bucket in path should return MissingBucket (not crash).
        let err = resolve_address("[::1]:9000", "/").unwrap_err();
        assert!(matches!(err, AddressError::MissingBucket));
    }

    #[test]
    fn test_ipv6_loopback_no_port() {
        // IPv6 without port should also work.
        let (bucket, key) = resolve_address("[::1]", "/bucket/key").unwrap();
        assert_eq!(bucket, "bucket");
        assert_eq!(key, "key");
    }

    // -- Short host names (single-label) ---------------------------------------

    #[test]
    fn test_single_label_host_path_style() {
        // "localhost" has one label → path-style.
        let (bucket, key) = resolve_address("localhost", "/bucket/key").unwrap();
        assert_eq!(bucket, "bucket");
        assert_eq!(key, "key");
    }

    // -- Authorization header parsing helpers ----------------------------------

    #[test]
    fn test_try_extract_service_from_auth_full() {
        let result = try_extract_service_from_auth(
            "AWS4-HMAC-SHA256 Credential=AKID/20210518/us-east-1/s3/aws4_request",
        );
        assert_eq!(result, Some("s3".to_string()));
    }

    #[test]
    fn test_try_extract_service_from_auth_minimal_scope() {
        // Exactly 4 segments (no aws4_request suffix).
        let result = try_extract_service_from_auth(
            "AWS4-HMAC-SHA256 Credential=AKID/20210518/us-east-1/s3",
        );
        assert_eq!(result, Some("s3".to_string()));
    }

    #[test]
    fn test_try_extract_service_from_auth_too_few_segments() {
        let result = try_extract_service_from_auth(
            "AWS4-HMAC-SHA256 Credential=AKID/20210518",
        );
        assert_eq!(result, None);
    }

    #[test]
    fn test_try_extract_service_from_auth_no_credential_token() {
        let result = try_extract_service_from_auth(
            "AWS4-HMAC-SHA256 SignedHeaders=host;x-amz-date",
        );
        assert_eq!(result, None);
    }

    #[test]
    fn test_try_extract_service_from_target_standard() {
        assert_eq!(
            try_extract_service_from_target("S3_Test.Bla"),
            Some("s3".to_string())
        );
    }

    #[test]
    fn test_try_extract_service_from_target_no_underscore() {
        assert_eq!(
            try_extract_service_from_target("S3.PutObject"),
            Some("s3".to_string())
        );
    }

    #[test]
    fn test_try_extract_service_from_target_empty_prefix() {
        assert_eq!(try_extract_service_from_target(".Operation"), None);
    }

    #[test]
    fn test_try_extract_service_from_target_no_dot() {
        assert_eq!(try_extract_service_from_target("JustAString"), Some("justastring".to_string()));
    }

    #[test]
    fn test_try_extract_service_from_query_present() {
        assert_eq!(
            try_extract_service_from_query("Action=ListBuckets&Service=S3"),
            Some("s3".to_string())
        );
    }

    #[test]
    fn test_try_extract_service_from_query_absent() {
        assert_eq!(try_extract_service_from_query("Action=ListBuckets"), None);
    }

    #[test]
    fn test_try_extract_service_from_query_empty_value() {
        assert_eq!(try_extract_service_from_query("Service="), None);
    }
}
