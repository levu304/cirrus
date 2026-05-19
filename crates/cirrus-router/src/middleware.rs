// Middleware implementations for the Cirrus router.
//
// This module provides axum middleware for:
// - Detecting incomplete request bodies (mismatch with Content-Length)
// - Converting HTTP 413 responses to AWS `EntityTooLarge` errors

use axum::{
    body::Body,
    extract::Request,
    middleware::Next,
    response::Response,
};
use cirrus_protocol::error::{AwsError, AwsErrorKind};
use http::StatusCode;
use http_body_util::BodyExt;
use tracing;

use crate::service::aws_error_response;
use crate::MAX_REQUEST_BYTES;

// ---------------------------------------------------------------------------
// incomplete_body_detection
// ---------------------------------------------------------------------------

/// Middleware that verifies the request body length matches the `Content-Length` header.
///
/// If `Content-Length` is present and the actual bytes consumed from the body
/// are fewer than the declared length, the request is considered to have an
/// incomplete body and an [`AwsError::IncompleteBody`] response (HTTP 400) is
/// returned.
///
/// If `Content-Length` is absent (chunked transfer encoding), the check is
/// skipped entirely — there is no declared length to compare against.
///
/// # How it works
///
/// 1. Reads the `Content-Length` header from the request (if present).
/// 2. Collects the entire request body into memory (bounded by the 100 MB
///    limit enforced by [`RequestBodyLimitLayer`](tower_http::limit::RequestBodyLimitLayer)).
/// 3. Reconstructs the request with the collected body so the handler receives
///    the full body stream.
/// 4. After the handler completes, compares actual bytes read against the
///    declared `Content-Length`.
/// 5. If fewer bytes were consumed, replaces the response with an
///    `IncompleteBody` error.
///
/// # Middleware ordering
///
/// This middleware should sit **inside** the `RequestBodyLimitLayer` so that
/// oversized bodies are rejected before this middleware attempts to collect them.
pub async fn incomplete_body_detection(
    request: Request,
    next: Next,
) -> Result<Response, Response> {
    // Read Content-Length from request headers, if present.
    let content_length = match request.headers().get(http::header::CONTENT_LENGTH) {
        None => None,
        Some(v) => match v.to_str() {
            Err(_) => {
                // Non-UTF8 header value — rare edge case, treat like no
                // Content-Length (chunked encoding), pass through.
                None
            }
            Ok(s) => match s.parse::<u64>() {
                Ok(n) => Some(n),
                Err(_) => {
                    // Non-numeric or overflow Content-Length — the request is
                    // malformed.  Return 400 Bad Request.
                    return Ok(aws_error_response(AwsError::new(
                        AwsErrorKind::MissingRequestHeader {
                            header_name: "Content-Length".to_string(),
                        },
                    )));
                }
            },
        },
    };

    // If no Content-Length header (e.g. chunked encoding), pass through
    // without attempting a body-length comparison.
    let Some(expected) = content_length else {
        return Ok(next.run(request).await);
    };

    // Reject immediately if the declared Content-Length exceeds the maximum
    // allowed body size — before collecting the body into memory. Without this
    // check, an attacker could send a large Content-Length to OOM the server.
    if expected > MAX_REQUEST_BYTES as u64 {
        tracing::warn!(
            content_length = expected,
            max_allowed = MAX_REQUEST_BYTES,
            "Request body exceeds maximum allowed size"
        );
        return Ok(aws_error_response(AwsError::new(
            AwsErrorKind::EntityTooLarge {
                entity: "request body".to_string(),
            },
        )));
    }

    // Collect the entire body to count actual bytes consumed.
    let (parts, body) = request.into_parts();

    let collected = body.collect().await.map_err(|e| {
        tracing::error!(error = %e, "Failed to read request body");
        aws_error_response(AwsError::new(AwsErrorKind::InternalError {
            details: None,
        }))
    })?;

    let body_bytes = collected.to_bytes();
    let actual = body_bytes.len() as u64;

    // Reconstruct the request with the collected body so the handler can read it.
    let request = Request::from_parts(parts, Body::from(body_bytes));

    // Run the inner handler.
    let response = next.run(request).await;

    // If body bytes don't match the declared Content-Length, return IncompleteBody.
    if actual != expected {
        tracing::warn!(
            content_length = expected,
            actual_bytes = actual,
            "Request body size does not match declared Content-Length"
        );
        Ok(aws_error_response(
            AwsError::new(AwsErrorKind::IncompleteBody),
        ))
    } else {
        Ok(response)
    }
}

// ---------------------------------------------------------------------------
// entity_too_large_interceptor
// ---------------------------------------------------------------------------

/// Middleware that converts bare HTTP 413 responses into AWS `EntityTooLarge` errors.
///
/// [`tower_http::limit::RequestBodyLimitLayer`] returns a plain HTTP 413
/// (Payload Too Large) when the request body exceeds the configured limit.
/// AWS S3 instead returns `EntityTooLarge` as HTTP 400 with an XML body.
///
/// This middleware intercepts the 413 response produced by the body-limit
/// layer and replaces it with a properly formatted AWS error response.
///
/// # Middleware ordering
///
/// This middleware should sit **outside** the `RequestBodyLimitLayer` so it
/// can intercept the 413 response before it reaches the client.
pub async fn entity_too_large_interceptor(
    request: Request,
    next: Next,
) -> Result<Response, Response> {
    let response = next.run(request).await;

    if response.status() == StatusCode::PAYLOAD_TOO_LARGE {
        tracing::warn!("Request body exceeded maximum allowed size");
        Ok(aws_error_response(
            AwsError::new(AwsErrorKind::EntityTooLarge {
                entity: "request body".to_string(),
            }),
        ))
    } else {
        Ok(response)
    }
}
