// S3 request handlers.
//
// This module contains the per-operation handler functions for the S3 API.
// Each handler is a generic async function parameterized over `S: Storage`.
//
// Phase 5a: Stub handlers — all return NotImplemented.
// Each handler will be implemented in its respective phase (5b–5e).

use axum::body::Body;
use bytes::Bytes;
use http::Response;
use cirrus_protocol::error::{AwsError, AwsErrorKind};
use crate::storage::Storage;

// ---------------------------------------------------------------------------
// Bucket-level handlers (Phase 5b)
// ---------------------------------------------------------------------------

/// GET / — list all buckets owned by the user.
pub async fn handle_list_buckets<S: Storage>(
    _storage: &S,
) -> Result<Response<Body>, AwsError> {
    Err(AwsError::new(AwsErrorKind::NotImplemented))
}

/// PUT /{bucket} — create a new bucket.
pub async fn handle_create_bucket<S: Storage>(
    _storage: &S,
    _bucket: &str,
) -> Result<Response<Body>, AwsError> {
    Err(AwsError::new(AwsErrorKind::NotImplemented))
}

/// DELETE /{bucket} — delete an empty bucket.
pub async fn handle_delete_bucket<S: Storage>(
    _storage: &S,
    _bucket: &str,
) -> Result<Response<Body>, AwsError> {
    Err(AwsError::new(AwsErrorKind::NotImplemented))
}

/// GET /{bucket}?location — get the bucket's region.
pub async fn handle_get_bucket_location<S: Storage>(
    _storage: &S,
    _bucket: &str,
) -> Result<Response<Body>, AwsError> {
    Err(AwsError::new(AwsErrorKind::NotImplemented))
}

/// GET /{bucket}?list-type=2 (or plain GET /{bucket}) — list objects.
pub async fn handle_list_objects_v2<S: Storage>(
    _storage: &S,
    _bucket: &str,
    _query: &str,
) -> Result<Response<Body>, AwsError> {
    Err(AwsError::new(AwsErrorKind::NotImplemented))
}

/// POST /{bucket}?delete — delete multiple objects.
pub async fn handle_delete_objects<S: Storage>(
    _storage: &S,
    _bucket: &str,
    _body: Bytes,
) -> Result<Response<Body>, AwsError> {
    Err(AwsError::new(AwsErrorKind::NotImplemented))
}

// ---------------------------------------------------------------------------
// Object-level handlers (Phase 5c)
// ---------------------------------------------------------------------------

/// PUT /{bucket}/{key} with x-amz-copy-source header — copy an object.
pub async fn handle_copy_object<S: Storage>(
    _storage: &S,
    _dst_bucket: &str,
    _dst_key: &str,
    _copy_source: &str,
) -> Result<Response<Body>, AwsError> {
    Err(AwsError::new(AwsErrorKind::NotImplemented))
}

/// PUT /{bucket}/{key} — upload an object.
pub async fn handle_put_object<S: Storage>(
    _storage: &S,
    _bucket: &str,
    _key: &str,
    _content_type: &str,
    _body: Bytes,
) -> Result<Response<Body>, AwsError> {
    Err(AwsError::new(AwsErrorKind::NotImplemented))
}

/// GET /{bucket}/{key} — retrieve an object.
pub async fn handle_get_object<S: Storage>(
    _storage: &S,
    _bucket: &str,
    _key: &str,
) -> Result<Response<Body>, AwsError> {
    Err(AwsError::new(AwsErrorKind::NotImplemented))
}

/// HEAD /{bucket}/{key} — return object metadata (headers only, no body).
pub async fn handle_head_object<S: Storage>(
    _storage: &S,
    _bucket: &str,
    _key: &str,
) -> Result<Response<Body>, AwsError> {
    Err(AwsError::new(AwsErrorKind::NotImplemented))
}

/// DELETE /{bucket}/{key} — delete an object.
pub async fn handle_delete_object<S: Storage>(
    _storage: &S,
    _bucket: &str,
    _key: &str,
) -> Result<Response<Body>, AwsError> {
    Err(AwsError::new(AwsErrorKind::NotImplemented))
}

// ---------------------------------------------------------------------------
// Multipart upload handlers (Phase 5d–5e)
// ---------------------------------------------------------------------------

/// POST /{bucket}/{key}?uploads — initiate a multipart upload.
pub async fn handle_create_multipart_upload<S: Storage>(
    _storage: &S,
    _bucket: &str,
    _key: &str,
) -> Result<Response<Body>, AwsError> {
    Err(AwsError::new(AwsErrorKind::NotImplemented))
}

/// PUT /{bucket}/{key}?partNumber=N&uploadId=ID — upload a part.
pub async fn handle_upload_part<S: Storage>(
    _storage: &S,
    _bucket: &str,
    _key: &str,
    _part_number: u32,
    _upload_id: &str,
    _body: Bytes,
) -> Result<Response<Body>, AwsError> {
    Err(AwsError::new(AwsErrorKind::NotImplemented))
}

/// POST /{bucket}/{key}?uploadId=ID — complete a multipart upload.
pub async fn handle_complete_multipart_upload<S: Storage>(
    _storage: &S,
    _bucket: &str,
    _key: &str,
    _upload_id: &str,
    _body: Bytes,
) -> Result<Response<Body>, AwsError> {
    Err(AwsError::new(AwsErrorKind::NotImplemented))
}

/// DELETE /{bucket}/{key}?uploadId=ID — abort a multipart upload.
pub async fn handle_abort_multipart_upload<S: Storage>(
    _storage: &S,
    _bucket: &str,
    _key: &str,
    _upload_id: &str,
) -> Result<Response<Body>, AwsError> {
    Err(AwsError::new(AwsErrorKind::NotImplemented))
}

/// GET /{bucket}/{key}?uploadId=ID — list parts of an in-progress upload.
pub async fn handle_list_parts<S: Storage>(
    _storage: &S,
    _bucket: &str,
    _key: &str,
    _upload_id: &str,
    _query: &str,
) -> Result<Response<Body>, AwsError> {
    Err(AwsError::new(AwsErrorKind::NotImplemented))
}
