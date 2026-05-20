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
use cirrus_protocol::types::{
    ListAllMyBucketsResult, Buckets, Owner,
    CreateBucketOutput, LocationConstraint,
    S3Object, CopyObjectResult,
    DeleteRequest, DeleteResult, DeletedObject, DeleteError,
};
use cirrus_protocol::xml::{serialize, format_etag, format_http_date};
use crate::storage::{Storage, S3Error};
use crate::service::{s3_error_to_aws, validate_copy_source};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Bucket-level handlers (Phase 5b)
// ---------------------------------------------------------------------------

/// GET / — list all buckets owned by the user.
pub async fn handle_list_buckets<S: Storage>(
    storage: &S,
) -> Result<Response<Body>, AwsError> {
    let bucket_infos = storage.list_buckets().await.map_err(|e| {
        s3_error_to_aws(e, "", "")
    })?;

    let result = ListAllMyBucketsResult {
        owner: Owner {
            id: "000000000000".into(),
            display_name: "webfile".into(),
        },
        buckets: Buckets {
            bucket: bucket_infos,
        },
    };

    let xml = serialize(&result, "ListAllMyBucketsResult")?;
    Response::builder()
        .status(200)
        .body(Body::from(xml))
        .map_err(|e| AwsError::new(AwsErrorKind::InternalError {
            details: Some(format!("response build failed: {e}")),
        }))
}

/// PUT /{bucket} — create a new bucket.
pub async fn handle_create_bucket<S: Storage>(
    storage: &S,
    bucket: &str,
) -> Result<Response<Body>, AwsError> {
    // Validate bucket name: must be 3-63 characters.
    if bucket.len() < 3 || bucket.len() > 63 {
        return Err(AwsError::new(AwsErrorKind::InvalidArgument {
            argument_name: "BucketName".into(),
            value: bucket.into(),
        }));
    }

    storage.create_bucket(bucket).await.map_err(|e| match e {
        S3Error::BucketAlreadyExists => s3_error_to_aws(e, bucket, ""),
        S3Error::MaxCapacityExceeded => s3_error_to_aws(e, bucket, ""),
        other => s3_error_to_aws(other, bucket, ""),
    })?;

    let output = CreateBucketOutput {
        location: format!("https://localhost:4566/{}", bucket),
    };

    let xml = serialize(&output, "CreateBucketOutput")?;
    Response::builder()
        .status(200)
        .header("Location", &output.location)
        .body(Body::from(xml))
        .map_err(|e| AwsError::new(AwsErrorKind::InternalError {
            details: Some(format!("response build failed: {e}")),
        }))
}

/// DELETE /{bucket} — delete an empty bucket.
pub async fn handle_delete_bucket<S: Storage>(
    storage: &S,
    bucket: &str,
) -> Result<Response<Body>, AwsError> {
    storage.delete_bucket(bucket).await.map_err(|e| match e {
        S3Error::NoSuchBucket => s3_error_to_aws(e, bucket, ""),
        S3Error::BucketNotEmpty => s3_error_to_aws(e, bucket, ""),
        other => s3_error_to_aws(other, bucket, ""),
    })?;

    Response::builder()
        .status(204)
        .body(Body::empty())
        .map_err(|e| AwsError::new(AwsErrorKind::InternalError {
            details: Some(format!("response build failed: {e}")),
        }))
}

/// GET /{bucket}?location — get the bucket's region.
pub async fn handle_get_bucket_location<S: Storage>(
    storage: &S,
    bucket: &str,
) -> Result<Response<Body>, AwsError> {
    let location = storage.get_bucket_location(bucket).await.map_err(|e| {
        s3_error_to_aws(e, bucket, "")
    })?;

    let lc = LocationConstraint {
        location,
    };

    let xml = serialize(&lc, "LocationConstraint")?;
    Response::builder()
        .status(200)
        .body(Body::from(xml))
        .map_err(|e| AwsError::new(AwsErrorKind::InternalError {
            details: Some(format!("response build failed: {e}")),
        }))
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
///
/// Parses an S3 Delete XML body and iterates over each object key,
/// deleting them sequentially (non-transactional per S3 spec).
/// Failed deletes (e.g. NoSuchKey) are reported as `<Error>` entries
/// in the response rather than failing the entire request.
///
/// When `<Quiet>true</Quiet>`, `<Deleted>` entries are omitted from
/// the response (only `<Error>` entries are included).
pub async fn handle_delete_objects<S: Storage>(
    storage: &S,
    bucket: &str,
    body: Bytes,
) -> Result<Response<Body>, AwsError> {
    // Parse the XML body as a Delete request.
    let body_str = std::str::from_utf8(&body).map_err(|_| {
        AwsError::new(AwsErrorKind::InvalidArgument {
            argument_name: "body".into(),
            value: "request body is not valid UTF-8".into(),
        })
    })?;
    let delete_req: DeleteRequest = quick_xml::de::from_str(body_str).map_err(|e| {
        AwsError::new(AwsErrorKind::InvalidArgument {
            argument_name: "body".into(),
            value: format!("Malformed XML in Delete request: {e}"),
        })
    })?;

    // S3 spec limits Delete to 1000 keys per request.
    if delete_req.objects.len() > 1000 {
        return Err(AwsError::new(AwsErrorKind::InvalidArgument {
            argument_name: "Delete".into(),
            value: "The number of keys in a single Delete request must not exceed 1000"
                .into(),
        }));
    }

    let mut deleted: Vec<DeletedObject> = Vec::new();
    let mut errors: Vec<DeleteError> = Vec::new();

    for obj in &delete_req.objects {
        match storage.delete_object(bucket, &obj.key).await {
            Ok(()) => {
                deleted.push(DeletedObject {
                    key: obj.key.clone(),
                    version_id: obj.version_id.clone(),
                    delete_marker: None,
                    delete_marker_version_id: None,
                });
            }
            Err(S3Error::NoSuchBucket) => {
                // Bucket doesn't exist — record per-key error and continue.
                // Per S3 spec, DeleteObjects always returns a 200 with per-key errors.
                errors.push(DeleteError {
                    key: obj.key.clone(),
                    code: "NoSuchBucket".into(),
                    message: "The specified bucket does not exist.".into(),
                    version_id: obj.version_id.clone(),
                });
            }
            Err(S3Error::NoSuchKey) => {
                errors.push(DeleteError {
                    key: obj.key.clone(),
                    code: "NoSuchKey".into(),
                    message: "The specified key does not exist.".into(),
                    version_id: obj.version_id.clone(),
                });
            }
            Err(_) => {
                // Catch-all for unexpected storage errors.
                // Use a generic client-safe message to avoid leaking internal
                // details (e.g. MaxCapacityExceeded) per AWS S3 convention.
                errors.push(DeleteError {
                    key: obj.key.clone(),
                    code: "InternalError".into(),
                    message: "We encountered an internal error. Please try again.".into(),
                    version_id: obj.version_id.clone(),
                });
            }
        }
    }

    // Build the result, respecting the Quiet flag.
    let result = DeleteResult {
        deleted: if delete_req.quiet { Vec::new() } else { deleted },
        errors,
    };

    let xml = serialize(&result, "DeleteResult")?;
    let request_id = uuid::Uuid::new_v4().to_string();
    Response::builder()
        .status(200)
        .header("Content-Type", "application/xml")
        .header("x-amz-request-id", &request_id)
        .header("x-amz-id-2", S3_ID_2)
        .body(Body::from(xml))
        .map_err(|e| AwsError::new(AwsErrorKind::InternalError {
            details: Some(format!("response build failed: {e}")),
        }))
}

/// Opaque value for the x-amz-id-2 response header.
/// Must NOT leak version or implementation details — real AWS S3 returns
/// a base64-looking opaque token.
const S3_ID_2: &str = "4v8y2k5j9h3q1w7e6r0t4v8y2k5j9h3q1w7e6r0t";

// ---------------------------------------------------------------------------
// Object-level handlers (Phase 5c)
// ---------------------------------------------------------------------------

/// PUT /{bucket}/{key} with x-amz-copy-source header — copy an object.
///
/// When `metadata` is non-empty (x-amz-metadata-directive: REPLACE), the
/// provided metadata replaces the source object's metadata.  An empty map
/// means the source metadata is preserved (COPY mode, the default).
pub async fn handle_copy_object<S: Storage>(
    storage: &S,
    dst_bucket: &str,
    dst_key: &str,
    copy_source: &str,
    metadata: HashMap<String, String>,
) -> Result<Response<Body>, AwsError> {
    let (src_bucket, src_key) = validate_copy_source(copy_source)?;

    let obj = storage
        .copy_object(src_bucket, src_key, dst_bucket, dst_key, &metadata)
        .await
        .map_err(|e| match e {
            S3Error::NoSuchBucket => s3_error_to_aws(e, dst_bucket, dst_key),
            other => s3_error_to_aws(other, src_bucket, src_key),
        })?;

    let result = CopyObjectResult {
        etag: obj.etag,
        last_modified: obj.last_modified,
    };

    let xml = serialize(&result, "CopyObjectResult")?;
    let request_id = uuid::Uuid::new_v4().to_string();
    Response::builder()
        .status(200)
        .header("Content-Type", "application/xml")
        .header("x-amz-request-id", &request_id)
        .header("x-amz-id-2", S3_ID_2)
        .body(Body::from(xml))
        .map_err(|e| AwsError::new(AwsErrorKind::InternalError {
            details: Some(format!("response build failed: {e}")),
        }))
}

/// Sanitize a Content-Type value to prevent stored XSS.
///
/// If the Content-Type is empty or matches a dangerous prefix that browsers
/// would render as active documents (HTML, SVG, JavaScript, XHTML, XML),
/// returns [`S3Object::DEFAULT_CONTENT_TYPE`] (`"binary/octet-stream"`).
/// Otherwise returns the content type unchanged.
fn sanitize_content_type(content_type: &str) -> &str {
    let trimmed = content_type.trim();

    if trimmed.is_empty() {
        return S3Object::DEFAULT_CONTENT_TYPE;
    }

    // Content-Type prefixes that browsers may render as active documents.
    // Matching by prefix also catches variants like `text/html; charset=utf-8`.
    const DANGEROUS_PREFIXES: &[&str] = &[
        "text/html",
        "application/xhtml+xml",
        "image/svg+xml",
        "text/javascript",
        "application/javascript",
        "application/ecmascript",
        "text/ecmascript",
        "application/xml",
    ];

    if DANGEROUS_PREFIXES.iter().any(|p| trimmed.starts_with(p)) {
        return S3Object::DEFAULT_CONTENT_TYPE;
    }

    content_type
}

/// PUT /{bucket}/{key} — upload an object.
pub async fn handle_put_object<S: Storage>(
    storage: &S,
    bucket: &str,
    key: &str,
    content_type: &str,
    metadata: HashMap<String, String>,
    body: Bytes,
) -> Result<Response<Body>, AwsError> {
    let content_type = sanitize_content_type(content_type);

    let etag = format_etag(&body);
    let object = S3Object {
        data: body,
        etag: etag.clone(),
        content_type: content_type.to_string(),
        last_modified: chrono::Utc::now(),
        metadata,
    };

    storage.put_object(bucket, key, object).await.map_err(|e| {
        s3_error_to_aws(e, bucket, key)
    })?;

    let request_id = uuid::Uuid::new_v4().to_string();
    Response::builder()
        .status(200)
        .header("ETag", &etag)
        .header("x-amz-request-id", &request_id)
        .header("x-amz-id-2", S3_ID_2)
        .body(Body::empty())
        .map_err(|e| AwsError::new(AwsErrorKind::InternalError {
            details: Some(format!("response build failed: {e}")),
        }))
}

/// GET /{bucket}/{key} — retrieve an object.
pub async fn handle_get_object<S: Storage>(
    storage: &S,
    bucket: &str,
    key: &str,
) -> Result<Response<Body>, AwsError> {
    let result = storage.get_object(bucket, key).await.map_err(|e| {
        s3_error_to_aws(e, bucket, key)
    })?;

    let object = result.object;
    let request_id = uuid::Uuid::new_v4().to_string();

    let mut builder = Response::builder()
        .status(200)
        .header("Content-Type", &object.content_type)
        .header("Content-Length", object.content_length().to_string())
        .header("ETag", &object.etag)
        .header("Last-Modified", format_http_date(object.last_modified))
        .header("x-amz-request-id", &request_id)
        .header("x-amz-id-2", S3_ID_2);

    for (key, value) in &object.metadata {
        builder = builder.header(format!("x-amz-meta-{}", key), value);
    }

    builder
        .body(Body::from(object.data))
        .map_err(|e| AwsError::new(AwsErrorKind::InternalError {
            details: Some(format!("response build failed: {e}")),
        }))
}

/// HEAD /{bucket}/{key} — return object metadata (headers only, no body).
pub async fn handle_head_object<S: Storage>(
    storage: &S,
    bucket: &str,
    key: &str,
) -> Result<Response<Body>, AwsError> {
    let result = storage.head_object(bucket, key).await.map_err(|e| {
        s3_error_to_aws(e, bucket, key)
    })?;

    let object = result.object;
    let request_id = uuid::Uuid::new_v4().to_string();

    let mut builder = Response::builder()
        .status(200)
        .header("Content-Type", &object.content_type)
        .header("Content-Length", object.content_length().to_string())
        .header("ETag", &object.etag)
        .header("Last-Modified", format_http_date(object.last_modified))
        .header("x-amz-request-id", &request_id)
        .header("x-amz-id-2", S3_ID_2);

    for (key, value) in &object.metadata {
        builder = builder.header(format!("x-amz-meta-{}", key), value);
    }

    builder
        .body(Body::empty())
        .map_err(|e| AwsError::new(AwsErrorKind::InternalError {
            details: Some(format!("response build failed: {e}")),
        }))
}

/// DELETE /{bucket}/{key} — delete an object.
pub async fn handle_delete_object<S: Storage>(
    storage: &S,
    bucket: &str,
    key: &str,
) -> Result<Response<Body>, AwsError> {
    match storage.delete_object(bucket, key).await {
        Ok(()) => {}
        Err(S3Error::NoSuchKey) => {
            // Idempotent delete: missing key → 204 No Content (not an error).
        }
        Err(e) => {
            return Err(s3_error_to_aws(e, bucket, key));
        }
    }

    let request_id = uuid::Uuid::new_v4().to_string();
    Response::builder()
        .status(204)
        .header("x-amz-request-id", &request_id)
        .header("x-amz-id-2", S3_ID_2)
        .body(Body::empty())
        .map_err(|e| AwsError::new(AwsErrorKind::InternalError {
            details: Some(format!("response build failed: {e}")),
        }))
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::DefaultStorage;
    use axum::body::to_bytes;

    // -- Helper: read body to string -------------------------------------

    async fn body_to_string(body: Body) -> String {
        let bytes = to_bytes(body, 10 * 1024 * 1024).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    // -- handle_list_buckets tests ---------------------------------------

    #[tokio::test]
    async fn test_list_buckets_returns_valid_xml_with_empty_list() {
        let storage = DefaultStorage::new();
        let resp = handle_list_buckets(&storage).await.expect("should succeed");
        assert_eq!(resp.status(), 200);
        let body = body_to_string(resp.into_body()).await;
        assert!(body.contains("<ListAllMyBucketsResult"));
        assert!(body.contains("xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\""));
        assert!(body.contains("<Owner>"));
        assert!(body.contains("<ID>000000000000</ID>"));
        assert!(body.contains("<DisplayName>webfile</DisplayName>"));
        assert!(body.contains("<Buckets>"));
        assert!(body.contains("</Buckets>"));
    }

    #[tokio::test]
    async fn test_list_buckets_returns_created_buckets() {
        let storage = DefaultStorage::new();
        storage.create_bucket("alpha").await.unwrap();
        storage.create_bucket("beta").await.unwrap();

        let resp = handle_list_buckets(&storage).await.expect("should succeed");
        assert_eq!(resp.status(), 200);
        let body = body_to_string(resp.into_body()).await;
        assert!(body.contains("<Name>alpha</Name>"));
        assert!(body.contains("<Name>beta</Name>"));
        assert!(body.contains("<CreationDate>"));
    }

    // -- handle_create_bucket tests --------------------------------------

    #[tokio::test]
    async fn test_create_bucket_returns_200_with_location_header() {
        let storage = DefaultStorage::new();
        let resp = handle_create_bucket(&storage, "my-new-bucket").await.expect("should succeed");
        assert_eq!(resp.status(), 200);
        let location = resp.headers().get("Location").unwrap().to_str().unwrap();
        assert_eq!(location, "https://localhost:4566/my-new-bucket");
        let body = body_to_string(resp.into_body()).await;
        assert!(body.contains("<CreateBucketOutput"));
        assert!(body.contains("<Location>https://localhost:4566/my-new-bucket</Location>"));
    }

    #[tokio::test]
    async fn test_create_bucket_rejects_duplicate() {
        let storage = DefaultStorage::new();
        handle_create_bucket(&storage, "dup-bucket").await.unwrap();
        let err = handle_create_bucket(&storage, "dup-bucket").await.unwrap_err();
        assert_eq!(err.status_code(), 409);
        assert_eq!(err.error_code(), "BucketAlreadyExists");
    }

    #[tokio::test]
    async fn test_create_bucket_rejects_invalid_name_too_short() {
        let storage = DefaultStorage::new();
        let err = handle_create_bucket(&storage, "ab").await.unwrap_err();
        assert_eq!(err.status_code(), 400);
        assert_eq!(err.error_code(), "InvalidArgument");
    }

    #[tokio::test]
    async fn test_create_bucket_rejects_invalid_name_too_long() {
        let storage = DefaultStorage::new();
        let long_name = "a".repeat(64);
        let err = handle_create_bucket(&storage, &long_name).await.unwrap_err();
        assert_eq!(err.status_code(), 400);
        assert_eq!(err.error_code(), "InvalidArgument");
    }

    // -- handle_delete_bucket tests --------------------------------------

    #[tokio::test]
    async fn test_delete_bucket_returns_204_on_success() {
        let storage = DefaultStorage::new();
        storage.create_bucket("del-me").await.unwrap();
        let resp = handle_delete_bucket(&storage, "del-me").await.expect("should succeed");
        assert_eq!(resp.status(), 204);
    }

    #[tokio::test]
    async fn test_delete_bucket_returns_404_no_such_bucket() {
        let storage = DefaultStorage::new();
        let err = handle_delete_bucket(&storage, "nonexistent").await.unwrap_err();
        assert_eq!(err.status_code(), 404);
        assert_eq!(err.error_code(), "NoSuchBucket");
    }

    #[tokio::test]
    async fn test_delete_bucket_returns_409_bucket_not_empty() {
        use cirrus_protocol::types::S3Object;
        use std::collections::HashMap;
        use md5::{Md5, Digest};

        let storage = DefaultStorage::new();
        storage.create_bucket("nonempty").await.unwrap();

        // Insert an object via the public API.
        let hash = Md5::digest(b"hello");
        let obj = S3Object {
            data: Bytes::from("hello"),
            etag: format!("\"{:x}\"", hash),
            content_type: "text/plain".into(),
            last_modified: chrono::Utc::now(),
            metadata: HashMap::new(),
        };
        storage.put_object("nonempty", "some-key", obj).await.unwrap();

        let err = handle_delete_bucket(&storage, "nonempty").await.unwrap_err();
        assert_eq!(err.status_code(), 409);
        assert_eq!(err.error_code(), "BucketNotEmpty");
    }

    // -- handle_get_bucket_location tests --------------------------------

    #[tokio::test]
    async fn test_get_bucket_location_returns_xml_with_us_east_1() {
        let storage = DefaultStorage::new();
        let resp = handle_get_bucket_location(&storage, "any-bucket").await.expect("should succeed");
        assert_eq!(resp.status(), 200);
        let body = body_to_string(resp.into_body()).await;
        assert!(body.contains("<LocationConstraint"));
        assert!(body.contains("xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\""));
        assert!(body.contains("us-east-1"));
    }

    // -- handle_put_object tests -----------------------------------------

    #[tokio::test]
    async fn test_put_object_stores_and_returns_correct_etag() {
        let storage = DefaultStorage::new();
        storage.create_bucket("put-test").await.unwrap();

        let body = Bytes::from("hello world");
        let resp = handle_put_object(&storage, "put-test", "hello.txt", "text/plain", HashMap::new(), body.clone())
            .await
            .expect("put_object should succeed");
        assert_eq!(resp.status(), 200);

        let expected_etag = format_etag(&body);
        assert_eq!(
            resp.headers().get("ETag").unwrap().to_str().unwrap(),
            expected_etag
        );
        assert!(resp.headers().get("x-amz-request-id").is_some());
        assert_eq!(
            resp.headers().get("x-amz-id-2").unwrap().to_str().unwrap(),
            S3_ID_2
        );

        // Verify the object was actually stored.
        let result = storage
            .get_object("put-test", "hello.txt")
            .await
            .expect("stored object should be retrievable");
        assert_eq!(result.object.data, body);
        assert_eq!(result.object.content_type, "text/plain");
    }

    #[tokio::test]
    async fn test_put_object_uses_default_content_type_when_empty() {
        let storage = DefaultStorage::new();
        storage.create_bucket("put-test-2").await.unwrap();

        let body = Bytes::from("test data");
        handle_put_object(&storage, "put-test-2", "f", "", HashMap::new(), body)
            .await
            .expect("put_object should succeed");

        let result = storage
            .get_object("put-test-2", "f")
            .await
            .expect("object should exist");
        assert_eq!(result.object.content_type, S3Object::DEFAULT_CONTENT_TYPE);
    }

    #[tokio::test]
    async fn test_put_object_sanitizes_dangerous_content_types() {
        let storage = DefaultStorage::new();
        storage.create_bucket("sanitize-ct-test").await.unwrap();

        let dangerous_types = [
            "text/html",
            "application/xhtml+xml",
            "image/svg+xml",
            "text/javascript",
            "application/javascript",
            "application/ecmascript",
            "text/ecmascript",
            "application/xml",
        ];

        for ct in dangerous_types {
            let body = Bytes::from("content");
            handle_put_object(
                &storage,
                "sanitize-ct-test",
                "obj",
                ct,
                HashMap::new(),
                body,
            )
            .await
            .expect("put_object should succeed");

            let result = storage
                .get_object("sanitize-ct-test", "obj")
                .await
                .expect("object should exist");
            assert_eq!(
                result.object.content_type,
                S3Object::DEFAULT_CONTENT_TYPE,
                "content_type should be sanitized from \"{}\" to {}",
                ct,
                S3Object::DEFAULT_CONTENT_TYPE,
            );
        }
    }

    #[tokio::test]
    async fn test_put_object_preserves_safe_content_types() {
        let storage = DefaultStorage::new();
        storage.create_bucket("preserve-ct-test").await.unwrap();

        let safe_types = [
            ("application/json", "application/json"),
            ("image/png", "image/png"),
            ("text/plain; charset=utf-8", "text/plain; charset=utf-8"),
            ("application/pdf", "application/pdf"),
        ];

        for (input, expected) in safe_types {
            let body = Bytes::from("content");
            handle_put_object(
                &storage,
                "preserve-ct-test",
                "obj",
                input,
                HashMap::new(),
                body,
            )
            .await
            .expect("put_object should succeed");

            let result = storage
                .get_object("preserve-ct-test", "obj")
                .await
                .expect("object should exist");
            assert_eq!(
                result.object.content_type, expected,
                "safe content_type \"{}\" should pass through unchanged",
                input,
            );
        }
    }

    #[tokio::test]
    async fn test_put_object_returns_404_no_such_bucket() {
        let storage = DefaultStorage::new();
        let err = handle_put_object(
            &storage,
            "nonexistent",
            "key",
            "text/plain",
            HashMap::new(),
            Bytes::from("data"),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status_code(), 404);
        assert_eq!(err.error_code(), "NoSuchBucket");
    }

    #[tokio::test]
    async fn test_put_object_with_empty_body() {
        let storage = DefaultStorage::new();
        storage.create_bucket("empty-put-test").await.unwrap();

        let body = Bytes::new();
        let resp = handle_put_object(
            &storage,
            "empty-put-test",
            "empty.txt",
            "text/plain",
            HashMap::new(),
            body.clone(),
        )
        .await
        .expect("put_object with empty body should succeed");
        assert_eq!(resp.status(), 200);

        // MD5 of empty data.
        let expected_etag = format_etag(&body);
        assert_eq!(
            resp.headers().get("ETag").unwrap().to_str().unwrap(),
            expected_etag
        );

        // Verify the stored body is empty and content_type is preserved.
        let result = storage
            .get_object("empty-put-test", "empty.txt")
            .await
            .expect("stored object should be retrievable");
        assert!(result.object.data.is_empty(), "stored body should be empty");
        assert_eq!(result.object.content_type, "text/plain");
    }

    #[tokio::test]
    async fn test_put_object_overwrites_existing_key() {
        let storage = DefaultStorage::new();
        storage.create_bucket("overwrite-test").await.unwrap();

        // First PUT.
        let original_body = Bytes::from("original data");
        let resp1 = handle_put_object(
            &storage,
            "overwrite-test",
            "file.txt",
            "text/plain",
            HashMap::new(),
            original_body.clone(),
        )
        .await
        .expect("first put_object should succeed");
        assert_eq!(resp1.status(), 200);
        let original_etag = resp1
            .headers()
            .get("ETag")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        // Second PUT — overwrite with new data.
        let new_body = Bytes::from("overwritten data");
        let resp2 = handle_put_object(
            &storage,
            "overwrite-test",
            "file.txt",
            "text/plain",
            HashMap::new(),
            new_body.clone(),
        )
        .await
        .expect("second put_object should succeed");
        assert_eq!(resp2.status(), 200);
        let new_etag = resp2
            .headers()
            .get("ETag")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        // ETags must differ because the bodies differ.
        assert_ne!(original_etag, new_etag);

        // Get the object and verify it contains the new data.
        let result = storage
            .get_object("overwrite-test", "file.txt")
            .await
            .expect("object should exist after overwrite");
        assert_eq!(
            result.object.data, new_body,
            "stored body should be the overwritten data"
        );
        assert_eq!(
            result.object.etag, new_etag,
            "stored ETag should reflect the new body"
        );
    }

    // -- handle_get_object tests -----------------------------------------

    #[tokio::test]
    async fn test_get_object_returns_object_with_correct_headers_and_body() {
        let storage = DefaultStorage::new();
        storage.create_bucket("get-test").await.unwrap();

        let body = Bytes::from("hello world from get");
        handle_put_object(&storage, "get-test", "file.txt", "application/json", HashMap::new(), body.clone())
            .await
            .expect("put_object");

        let resp = handle_get_object(&storage, "get-test", "file.txt")
            .await
            .expect("get_object should succeed");
        assert_eq!(resp.status(), 200);

        // Extract header values as owned strings before consuming the response.
        let content_type = resp
            .headers()
            .get("Content-Type")
            .map(|v| v.to_str().unwrap().to_string());
        let content_length = resp
            .headers()
            .get("Content-Length")
            .map(|v| v.to_str().unwrap().to_string());
        let etag = resp
            .headers()
            .get("ETag")
            .map(|v| v.to_str().unwrap().to_string());
        let last_modified = resp.headers().get("Last-Modified").is_some();

        let resp_body = body_to_string(resp.into_body()).await;

        assert_eq!(content_type.unwrap(), "application/json");
        assert_eq!(content_length.unwrap(), "20"); // "hello world from get".len() = 20
        assert_eq!(etag.unwrap(), format_etag(&body));
        assert!(last_modified);
        assert_eq!(resp_body, "hello world from get");
    }

    #[tokio::test]
    async fn test_get_object_returns_404_no_such_key() {
        let storage = DefaultStorage::new();
        storage.create_bucket("get-test-2").await.unwrap();
        let err = handle_get_object(&storage, "get-test-2", "does-not-exist")
            .await
            .unwrap_err();
        assert_eq!(err.status_code(), 404);
        assert_eq!(err.error_code(), "NoSuchKey");
    }

    #[tokio::test]
    async fn test_get_object_returns_404_no_such_bucket() {
        let storage = DefaultStorage::new();
        let err = handle_get_object(&storage, "no-such-bucket", "key")
            .await
            .unwrap_err();
        assert_eq!(err.status_code(), 404);
        assert_eq!(err.error_code(), "NoSuchBucket");
    }

    #[tokio::test]
    async fn test_get_object_returns_metadata_headers() {
        let storage = DefaultStorage::new();
        storage.create_bucket("meta-get").await.unwrap();

        let mut metadata = HashMap::new();
        metadata.insert("Color".into(), "Red".into());
        metadata.insert("Project".into(), "Cirrus".into());

        let body = Bytes::from("metadata test");
        handle_put_object(&storage, "meta-get", "meta-file.txt", "text/plain", metadata, body)
            .await
            .expect("put_object");

        let resp = handle_get_object(&storage, "meta-get", "meta-file.txt")
            .await
            .expect("get_object should succeed");

        assert_eq!(resp.status(), 200);
        assert_eq!(
            resp.headers()
                .get("x-amz-meta-Color")
                .unwrap()
                .to_str()
                .unwrap(),
            "Red"
        );
        assert_eq!(
            resp.headers()
                .get("x-amz-meta-Project")
                .unwrap()
                .to_str()
                .unwrap(),
            "Cirrus"
        );
    }

    // -- handle_head_object tests ----------------------------------------

    #[tokio::test]
    async fn test_head_object_returns_same_headers_as_get_but_empty_body() {
        let storage = DefaultStorage::new();
        storage.create_bucket("head-test").await.unwrap();

        let body = Bytes::from("head body content");
        handle_put_object(&storage, "head-test", "head-file.txt", "text/plain", HashMap::new(), body.clone())
            .await
            .expect("put_object");

        let resp = handle_head_object(&storage, "head-test", "head-file.txt")
            .await
            .expect("head_object should succeed");
        assert_eq!(resp.status(), 200);

        // Verify headers match GET semantics.
        assert_eq!(
            resp.headers()
                .get("Content-Type")
                .unwrap()
                .to_str()
                .unwrap(),
            "text/plain"
        );
        let content_length = resp
            .headers()
            .get("Content-Length")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert_eq!(content_length, "17"); // "head body content".len()
        assert_eq!(
            resp.headers().get("ETag").unwrap().to_str().unwrap(),
            format_etag(&body)
        );
        assert!(resp.headers().get("Last-Modified").is_some());

        // Body must be empty for HEAD.
        let resp_body = body_to_string(resp.into_body()).await;
        assert!(resp_body.is_empty(), "HEAD response body should be empty");
    }

    #[tokio::test]
    async fn test_head_object_returns_metadata_headers() {
        let storage = DefaultStorage::new();
        storage.create_bucket("meta-head").await.unwrap();

        let mut metadata = HashMap::new();
        metadata.insert("Content-Type".into(), "image/png".into());
        metadata.insert("Author".into(), "test-user".into());

        let body = Bytes::from("head metadata");
        handle_put_object(&storage, "meta-head", "head-meta.txt", "text/plain", metadata, body)
            .await
            .expect("put_object");

        let resp = handle_head_object(&storage, "meta-head", "head-meta.txt")
            .await
            .expect("head_object should succeed");

        assert_eq!(resp.status(), 200);
        assert_eq!(
            resp.headers()
                .get("x-amz-meta-Content-Type")
                .unwrap()
                .to_str()
                .unwrap(),
            "image/png"
        );
        assert_eq!(
            resp.headers()
                .get("x-amz-meta-Author")
                .unwrap()
                .to_str()
                .unwrap(),
            "test-user"
        );
    }

    #[tokio::test]
    async fn test_head_object_returns_404_no_such_key() {
        let storage = DefaultStorage::new();
        storage.create_bucket("head-test-2").await.unwrap();
        let err = handle_head_object(&storage, "head-test-2", "nope")
            .await
            .unwrap_err();
        assert_eq!(err.status_code(), 404);
        assert_eq!(err.error_code(), "NoSuchKey");
    }

    #[tokio::test]
    async fn test_head_object_returns_404_no_such_bucket() {
        let storage = DefaultStorage::new();
        let err = handle_head_object(&storage, "no-such-head-bucket", "key")
            .await
            .unwrap_err();
        assert_eq!(err.status_code(), 404);
        assert_eq!(err.error_code(), "NoSuchBucket");
    }

    #[tokio::test]
    async fn test_get_object_with_empty_metadata_has_no_meta_headers() {
        let storage = DefaultStorage::new();
        storage.create_bucket("empty-meta").await.unwrap();

        let body = Bytes::from("no metadata");
        handle_put_object(&storage, "empty-meta", "plain.txt", "text/plain", HashMap::new(), body)
            .await
            .expect("put_object");

        let resp = handle_get_object(&storage, "empty-meta", "plain.txt")
            .await
            .expect("get_object should succeed");

        // Verify no x-amz-meta-* headers are present.
        let meta_headers: Vec<_> = resp
            .headers()
            .iter()
            .filter(|(name, _)| name.as_str().starts_with("x-amz-meta-"))
            .collect();
        assert!(
            meta_headers.is_empty(),
            "expected no x-amz-meta-* headers, got: {:?}",
            meta_headers
        );
    }

    // -- handle_delete_object tests --------------------------------------

    #[tokio::test]
    async fn test_delete_object_returns_204_on_success() {
        let storage = DefaultStorage::new();
        storage.create_bucket("del-obj").await.unwrap();

        let body = Bytes::from("delete me");
        handle_put_object(&storage, "del-obj", "target.txt", "text/plain", HashMap::new(), body)
            .await
            .expect("put_object");

        let resp = handle_delete_object(&storage, "del-obj", "target.txt")
            .await
            .expect("delete_object should succeed");
        assert_eq!(resp.status(), 204);

        // Verify the object is gone.
        let err = storage
            .get_object("del-obj", "target.txt")
            .await
            .unwrap_err();
        assert!(matches!(err, S3Error::NoSuchKey));
    }

    #[tokio::test]
    async fn test_delete_object_is_idempotent_for_missing_key() {
        let storage = DefaultStorage::new();
        storage.create_bucket("del-obj-idem").await.unwrap();

        // Delete a key that never existed — must return 204, not an error.
        let resp = handle_delete_object(&storage, "del-obj-idem", "never-existed")
            .await
            .expect("delete_object of missing key should succeed (idempotent)");
        assert_eq!(resp.status(), 204);
    }

    #[tokio::test]
    async fn test_delete_object_twice_is_idempotent() {
        let storage = DefaultStorage::new();
        storage.create_bucket("del-obj-twice").await.unwrap();

        let body = Bytes::from("data");
        handle_put_object(&storage, "del-obj-twice", "k", "text/plain", HashMap::new(), body)
            .await
            .expect("put_object");

        // First delete.
        handle_delete_object(&storage, "del-obj-twice", "k")
            .await
            .expect("first delete");
        // Second delete — must still be 204.
        let resp = handle_delete_object(&storage, "del-obj-twice", "k")
            .await
            .expect("second delete should also succeed (idempotent)");
        assert_eq!(resp.status(), 204);
    }

    #[tokio::test]
    async fn test_delete_object_returns_404_no_such_bucket() {
        let storage = DefaultStorage::new();
        let err = handle_delete_object(&storage, "no-such-del-bucket", "key")
            .await
            .unwrap_err();
        assert_eq!(err.status_code(), 404);
        assert_eq!(err.error_code(), "NoSuchBucket");
    }

    // -- handle_copy_object tests ----------------------------------------

    #[tokio::test]
    async fn test_copy_object_returns_copy_object_result_xml() {
        let storage = DefaultStorage::new();
        storage.create_bucket("copy-src").await.unwrap();
        storage.create_bucket("copy-dst").await.unwrap();

        let body = Bytes::from("copy this content");
        handle_put_object(&storage, "copy-src", "source.txt", "text/plain", HashMap::new(), body.clone())
            .await
            .expect("put_object");

        let resp = handle_copy_object(&storage, "copy-dst", "dest.txt", "/copy-src/source.txt", HashMap::new())
            .await
            .expect("copy_object should succeed");
        assert_eq!(resp.status(), 200);

        assert_eq!(
            resp.headers()
                .get("Content-Type")
                .unwrap()
                .to_str()
                .unwrap(),
            "application/xml"
        );

        let resp_body = body_to_string(resp.into_body()).await;

        // Verify CopyObjectResult XML structure.
        assert!(resp_body.contains("<CopyObjectResult"));
        assert!(resp_body.contains("xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\""));
        assert!(resp_body.contains("<ETag>"));
        assert!(resp_body.contains("<LastModified>"));
        // Must have the correct ETag value.
        let expected_etag = format_etag(&body);
        assert!(
            resp_body.contains(&format!("<ETag>{}</ETag>", expected_etag)),
            "expected ETag {} in body: {}",
            expected_etag,
            resp_body
        );

        // Verify the destination actually has the copied data.
        let dst = storage
            .get_object("copy-dst", "dest.txt")
            .await
            .expect("copied object should exist");
        assert_eq!(dst.object.data, body);
    }

    #[tokio::test]
    async fn test_copy_object_returns_404_no_such_source_bucket() {
        let storage = DefaultStorage::new();
        storage.create_bucket("copy-dst-404").await.unwrap();

        let err = handle_copy_object(
            &storage,
            "copy-dst-404",
            "dest.txt",
            "/no-such-src/source.txt",
            HashMap::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status_code(), 404);
        assert_eq!(err.error_code(), "NoSuchBucket");
        assert!(
            err.message().contains("copy-dst-404"),
            "expected message to reference the destination bucket (the context used for NoSuchBucket in copy_object), got: {}",
            err.message()
        );
    }

    #[tokio::test]
    async fn test_copy_object_returns_404_no_such_destination_bucket() {
        let storage = DefaultStorage::new();
        storage.create_bucket("copy-src-404-dst").await.unwrap();

        let body = Bytes::from("content for dest-bucket test");
        handle_put_object(
            &storage,
            "copy-src-404-dst",
            "source.txt",
            "text/plain",
            HashMap::new(),
            body,
        )
        .await
        .expect("put_object should succeed");

        // Do NOT create "no-such-dst-bucket".
        let err = handle_copy_object(
            &storage,
            "no-such-dst-bucket",
            "dest.txt",
            "/copy-src-404-dst/source.txt",
            HashMap::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status_code(), 404);
        assert_eq!(err.error_code(), "NoSuchBucket");
        assert!(
            err.message().contains("no-such-dst-bucket"),
            "expected message to reference the missing destination bucket, got: {}",
            err.message()
        );
    }

    #[tokio::test]
    async fn test_copy_object_rejects_invalid_copy_source() {
        let storage = DefaultStorage::new();

        // SSRF attempt via URL scheme.
        let err = handle_copy_object(
            &storage,
            "dst",
            "key",
            "http://evil.com/steal",
            HashMap::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status_code(), 400);
        assert_eq!(err.error_code(), "InvalidArgument");
    }

    #[tokio::test]
    async fn test_copy_object_returns_404_no_such_source_key() {
        let storage = DefaultStorage::new();
        storage.create_bucket("copy-src-key-test").await.unwrap();
        storage.create_bucket("copy-dst-key-test").await.unwrap();

        let err = handle_copy_object(
            &storage,
            "copy-dst-key-test",
            "dest.txt",
            "/copy-src-key-test/nonexistent-key",
            HashMap::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status_code(), 404);
        assert_eq!(err.error_code(), "NoSuchKey");
        assert!(
            err.message().contains("copy-src-key-test"),
            "expected message to reference the source bucket/key, got: {}",
            err.message()
        );
    }

    #[tokio::test]
    async fn test_copy_object_same_bucket() {
        let storage = DefaultStorage::new();
        storage.create_bucket("b").await.unwrap();

        let body = Bytes::from("same bucket copy");
        handle_put_object(&storage, "b", "original.txt", "text/plain", HashMap::new(), body.clone())
            .await
            .expect("put_object");

        let resp = handle_copy_object(&storage, "b", "copy.txt", "/b/original.txt", HashMap::new())
            .await
            .expect("copy_object same bucket");
        assert_eq!(resp.status(), 200);

        // Both objects should exist and have the same content.
        let orig = storage.get_object("b", "original.txt").await.unwrap();
        let copy = storage.get_object("b", "copy.txt").await.unwrap();
        assert_eq!(orig.object.data, copy.object.data);
    }

    // -- CopyObject metadata directive tests -----------------------------

    #[tokio::test]
    async fn test_copy_object_with_metadata_copy() {
        let storage = DefaultStorage::new();
        storage.create_bucket("meta-copy-src").await.unwrap();
        storage.create_bucket("meta-copy-dst").await.unwrap();

        // PUT source object with metadata.
        let mut src_metadata = HashMap::new();
        src_metadata.insert("Color".into(), "Red".into());
        src_metadata.insert("Project".into(), "Alpha".into());

        let body = Bytes::from("metadata copy test");
        handle_put_object(
            &storage,
            "meta-copy-src",
            "source.txt",
            "text/plain",
            src_metadata,
            body.clone(),
        )
        .await
        .expect("put_object");

        // COPY without metadata-directive (defaults to COPY mode).
        let resp = handle_copy_object(
            &storage,
            "meta-copy-dst",
            "dest.txt",
            "/meta-copy-src/source.txt",
            HashMap::new(), // empty = COPY mode
        )
        .await
        .expect("copy_object should succeed");
        assert_eq!(resp.status(), 200);

        // Verify destination has source metadata.
        let dst = storage
            .get_object("meta-copy-dst", "dest.txt")
            .await
            .expect("copied object should exist");
        assert_eq!(
            dst.object.metadata.get("Color").map(|s| s.as_str()),
            Some("Red"),
            "COPY mode should preserve source metadata"
        );
        assert_eq!(
            dst.object.metadata.get("Project").map(|s| s.as_str()),
            Some("Alpha"),
            "COPY mode should preserve all source metadata"
        );
    }

    #[tokio::test]
    async fn test_copy_object_with_metadata_replace() {
        let storage = DefaultStorage::new();
        storage.create_bucket("meta-replace-src").await.unwrap();
        storage.create_bucket("meta-replace-dst").await.unwrap();

        // PUT source object with metadata.
        let mut src_metadata = HashMap::new();
        src_metadata.insert("Color".into(), "Red".into());
        src_metadata.insert("Project".into(), "Alpha".into());

        let body = Bytes::from("metadata replace test");
        handle_put_object(
            &storage,
            "meta-replace-src",
            "source.txt",
            "text/plain",
            src_metadata,
            body.clone(),
        )
        .await
        .expect("put_object");

        // COPY with REPLACE metadata directive — new metadata replaces source.
        let mut replace_metadata = HashMap::new();
        replace_metadata.insert("Color".into(), "Blue".into());

        let resp = handle_copy_object(
            &storage,
            "meta-replace-dst",
            "dest.txt",
            "/meta-replace-src/source.txt",
            replace_metadata,
        )
        .await
        .expect("copy_object should succeed");
        assert_eq!(resp.status(), 200);

        // Verify destination has REPLACE metadata (not source metadata).
        let dst = storage
            .get_object("meta-replace-dst", "dest.txt")
            .await
            .expect("copied object should exist");
        assert_eq!(
            dst.object.metadata.get("Color").map(|s| s.as_str()),
            Some("Blue"),
            "REPLACE mode should use new metadata"
        );
        // "Project" was in source metadata but not in REPLACE — should NOT be present.
        assert_eq!(
            dst.object.metadata.get("Project").map(|s| s.as_str()),
            None,
            "REPLACE mode should not preserve source metadata not in the replace set"
        );

        // Source object's metadata must be unchanged.
        let src = storage
            .get_object("meta-replace-src", "source.txt")
            .await
            .expect("source object should exist");
        assert_eq!(
            src.object.metadata.get("Color").map(|s| s.as_str()),
            Some("Red"),
            "REPLACE mode must not modify source object metadata"
        );
        assert_eq!(
            src.object.metadata.get("Project").map(|s| s.as_str()),
            Some("Alpha"),
            "REPLACE mode must not modify source object metadata"
        );
    }

    #[tokio::test]
    async fn test_copy_object_with_metadata_replace_overrides_all() {
        let storage = DefaultStorage::new();
        storage.create_bucket("meta-full-replace-src").await.unwrap();
        storage.create_bucket("meta-full-replace-dst").await.unwrap();

        // PUT source with Color=Red.
        let mut src_metadata = HashMap::new();
        src_metadata.insert("Color".into(), "Red".into());

        let body = Bytes::from("full replace test");
        handle_put_object(
            &storage,
            "meta-full-replace-src",
            "source.txt",
            "text/plain",
            src_metadata,
            body.clone(),
        )
        .await
        .expect("put_object");

        // COPY with REPLACE: completely replace metadata (even with empty set).
        let resp = handle_copy_object(
            &storage,
            "meta-full-replace-dst",
            "dest.txt",
            "/meta-full-replace-src/source.txt",
            HashMap::new(), // empty map = COPY mode (not replace)
        )
        .await
        .expect("copy_object should succeed");
        assert_eq!(resp.status(), 200);

        // With empty metadata, it's COPY mode — source metadata preserved.
        let dst = storage
            .get_object("meta-full-replace-dst", "dest.txt")
            .await
            .expect("copied object should exist");
        assert_eq!(
            dst.object.metadata.get("Color").map(|s| s.as_str()),
            Some("Red"),
            "Empty metadata map means COPY mode — source metadata preserved"
        );
    }

    // -- handle_delete_objects tests ------------------------------------

    /// Build a Delete XML body from a list of object keys.
    fn delete_xml_body(objects: &[&str], quiet: Option<bool>) -> Bytes {
        let quiet_elem = match quiet {
            Some(true) => "<Quiet>true</Quiet>",
            Some(false) => "<Quiet>false</Quiet>",
            None => "",
        };
        let objects_xml: String = objects
            .iter()
            .map(|key| format!("<Object><Key>{}</Key></Object>", key))
            .collect();
        let xml = format!("<Delete>{}{}</Delete>", quiet_elem, objects_xml);
        Bytes::from(xml)
    }

    #[tokio::test]
    async fn test_delete_objects_empty_list_returns_200_with_empty_result() {
        let storage = DefaultStorage::new();
        storage.create_bucket("del-empty-list").await.unwrap();

        let body = delete_xml_body(&[], None);
        let resp = handle_delete_objects(&storage, "del-empty-list", body)
            .await
            .expect("should succeed");
        assert_eq!(resp.status(), 200);

        let resp_body = body_to_string(resp.into_body()).await;
        assert!(resp_body.contains("<DeleteResult"));
        assert!(resp_body.contains("xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\""));
        assert!(!resp_body.contains("<Deleted>"), "no Deleted elements expected");
        assert!(!resp_body.contains("<Error>"), "no Error elements expected");
    }

    #[tokio::test]
    async fn test_delete_objects_single_object_success() {
        let storage = DefaultStorage::new();
        storage.create_bucket("del-single-obj").await.unwrap();

        let body_data = Bytes::from("data");
        handle_put_object(
            &storage,
            "del-single-obj",
            "k1",
            "text/plain",
            HashMap::new(),
            body_data,
        )
        .await
        .expect("put_object");

        let req_body = delete_xml_body(&["k1"], None);
        let resp = handle_delete_objects(&storage, "del-single-obj", req_body)
            .await
            .expect("should succeed");
        assert_eq!(resp.status(), 200);

        // Object is gone from storage.
        let err = storage
            .get_object("del-single-obj", "k1")
            .await
            .unwrap_err();
        assert!(matches!(err, S3Error::NoSuchKey));

        let resp_body = body_to_string(resp.into_body()).await;
        assert!(resp_body.contains("<Deleted>"));
        assert!(resp_body.contains("<Key>k1</Key>"));
    }

    #[tokio::test]
    async fn test_delete_objects_multiple_objects_all_succeed() {
        let storage = DefaultStorage::new();
        storage.create_bucket("del-multi-all").await.unwrap();

        for key in &["a.txt", "b.txt", "c.txt"] {
            handle_put_object(
                &storage,
                "del-multi-all",
                key,
                "text/plain",
                HashMap::new(),
                Bytes::from("data"),
            )
            .await
            .expect("put_object");
        }

        let req_body = delete_xml_body(&["a.txt", "b.txt", "c.txt"], None);
        let resp = handle_delete_objects(&storage, "del-multi-all", req_body)
            .await
            .expect("should succeed");
        assert_eq!(resp.status(), 200);

        let resp_body = body_to_string(resp.into_body()).await;
        assert_eq!(resp_body.matches("<Deleted>").count(), 3);
        assert!(resp_body.contains("<Key>a.txt</Key>"));
        assert!(resp_body.contains("<Key>b.txt</Key>"));
        assert!(resp_body.contains("<Key>c.txt</Key>"));

        // All objects gone from storage.
        for key in &["a.txt", "b.txt", "c.txt"] {
            let err = storage.get_object("del-multi-all", key).await.unwrap_err();
            assert!(matches!(err, S3Error::NoSuchKey));
        }
    }

    #[tokio::test]
    async fn test_delete_objects_quiet_mode_suppresses_deleted() {
        let storage = DefaultStorage::new();
        storage.create_bucket("del-quiet-mode").await.unwrap();

        handle_put_object(
            &storage,
            "del-quiet-mode",
            "quiet-key",
            "text/plain",
            HashMap::new(),
            Bytes::from("data"),
        )
        .await
        .expect("put_object");

        let req_body = delete_xml_body(&["quiet-key"], Some(true));
        let resp = handle_delete_objects(&storage, "del-quiet-mode", req_body)
            .await
            .expect("should succeed");
        assert_eq!(resp.status(), 200);

        let resp_body = body_to_string(resp.into_body()).await;
        assert!(
            !resp_body.contains("<Deleted>"),
            "Quiet mode should suppress Deleted entries"
        );
        assert!(!resp_body.contains("<Error>"), "No errors expected");
    }

    #[tokio::test]
    async fn test_delete_objects_non_quiet_mode_includes_deleted() {
        let storage = DefaultStorage::new();
        storage.create_bucket("del-nonquiet-mode").await.unwrap();

        handle_put_object(
            &storage,
            "del-nonquiet-mode",
            "visible-key",
            "text/plain",
            HashMap::new(),
            Bytes::from("data"),
        )
        .await
        .expect("put_object");

        let req_body = delete_xml_body(&["visible-key"], Some(false));
        let resp = handle_delete_objects(&storage, "del-nonquiet-mode", req_body)
            .await
            .expect("should succeed");
        assert_eq!(resp.status(), 200);

        let resp_body = body_to_string(resp.into_body()).await;
        assert!(
            resp_body.contains("<Deleted>"),
            "Non-Quiet mode should include Deleted entries"
        );
        assert!(resp_body.contains("<Key>visible-key</Key>"));
    }

    #[tokio::test]
    async fn test_delete_objects_quiet_default_is_false() {
        // When <Quiet> is absent, default is false (Deleted entries shown).
        let storage = DefaultStorage::new();
        storage.create_bucket("del-quiet-default").await.unwrap();

        handle_put_object(
            &storage,
            "del-quiet-default",
            "default-key",
            "text/plain",
            HashMap::new(),
            Bytes::from("data"),
        )
        .await
        .expect("put_object");

        let req_body = delete_xml_body(&["default-key"], None); // No Quiet element
        let resp = handle_delete_objects(&storage, "del-quiet-default", req_body)
            .await
            .expect("should succeed");
        assert_eq!(resp.status(), 200);

        let resp_body = body_to_string(resp.into_body()).await;
        assert!(
            resp_body.contains("<Deleted>"),
            "Default Quiet=false should include Deleted entries"
        );
    }

    #[tokio::test]
    async fn test_delete_objects_partial_failures_mix_success_and_error() {
        let storage = DefaultStorage::new();
        storage.create_bucket("del-partial-fail").await.unwrap();

        // Only "exists" is in storage; "missing" is not.
        handle_put_object(
            &storage,
            "del-partial-fail",
            "exists",
            "text/plain",
            HashMap::new(),
            Bytes::from("data"),
        )
        .await
        .expect("put_object");

        let req_body = delete_xml_body(&["exists", "missing"], None);
        let resp = handle_delete_objects(&storage, "del-partial-fail", req_body)
            .await
            .expect("should succeed");
        assert_eq!(resp.status(), 200);

        let resp_body = body_to_string(resp.into_body()).await;
        assert_eq!(
            resp_body.matches("<Deleted>").count(),
            1,
            "expected 1 Deleted entry"
        );
        assert_eq!(
            resp_body.matches("<Error>").count(),
            1,
            "expected 1 Error entry"
        );
        assert!(resp_body.contains("<Key>exists</Key>"));
        assert!(resp_body.contains("<Key>missing</Key>"));
        assert!(resp_body.contains("<Code>NoSuchKey</Code>"));
    }

    #[tokio::test]
    async fn test_delete_objects_all_failures_only_error_entries() {
        let storage = DefaultStorage::new();
        storage.create_bucket("del-all-fail").await.unwrap();

        let req_body = delete_xml_body(&["nope1", "nope2"], None);
        let resp = handle_delete_objects(&storage, "del-all-fail", req_body)
            .await
            .expect("should succeed");
        assert_eq!(resp.status(), 200);

        let resp_body = body_to_string(resp.into_body()).await;
        assert!(
            !resp_body.contains("<Deleted>"),
            "no Deleted entries expected"
        );
        assert_eq!(
            resp_body.matches("<Error>").count(),
            2,
            "expected 2 Error entries"
        );
        assert!(resp_body.contains("<Code>NoSuchKey</Code>"));
    }

    #[tokio::test]
    async fn test_delete_objects_returns_404_no_such_bucket() {
        let storage = DefaultStorage::new();
        // No bucket created.

        let req_body = delete_xml_body(&["key1"], None);
        let resp = handle_delete_objects(&storage, "nonexistent-bucket", req_body)
            .await
            .expect("should return 200 with per-key errors per S3 spec");
        assert_eq!(resp.status(), 200);

        let resp_body = body_to_string(resp.into_body()).await;
        assert!(
            !resp_body.contains("<Deleted>"),
            "no Deleted entries expected"
        );
        assert_eq!(
            resp_body.matches("<Error>").count(),
            1,
            "expected 1 Error entry"
        );
        assert!(resp_body.contains("<Code>NoSuchBucket</Code>"));
        assert!(resp_body.contains("<Key>key1</Key>"));
    }

    #[tokio::test]
    async fn test_delete_objects_all_no_such_bucket_multiple_keys() {
        let storage = DefaultStorage::new();
        // No bucket created — all keys get per-key NoSuchBucket errors.

        let req_body = delete_xml_body(&["alpha", "beta", "gamma"], None);
        let resp = handle_delete_objects(&storage, "missing-bucket", req_body)
            .await
            .expect("should return 200 with per-key errors per S3 spec");
        assert_eq!(resp.status(), 200);

        let resp_body = body_to_string(resp.into_body()).await;
        assert!(
            !resp_body.contains("<Deleted>"),
            "no Deleted entries expected"
        );
        assert_eq!(
            resp_body.matches("<Error>").count(),
            3,
            "expected 3 Error entries (one per key)"
        );
        assert!(resp_body.contains("<Key>alpha</Key>"));
        assert!(resp_body.contains("<Key>beta</Key>"));
        assert!(resp_body.contains("<Key>gamma</Key>"));
        assert!(resp_body.contains("<Code>NoSuchBucket</Code>"));
    }

    #[tokio::test]
    async fn test_delete_objects_malformed_xml_returns_error() {
        let storage = DefaultStorage::new();
        storage.create_bucket("del-malformed").await.unwrap();

        let body = Bytes::from("not valid xml at all");
        let err = handle_delete_objects(&storage, "del-malformed", body)
            .await
            .unwrap_err();
        assert_eq!(err.status_code(), 400);
        assert_eq!(err.error_code(), "InvalidArgument");
    }

    #[tokio::test]
    async fn test_delete_objects_empty_body_returns_error() {
        let storage = DefaultStorage::new();
        storage.create_bucket("del-empty-body").await.unwrap();

        let body = Bytes::new();
        let err = handle_delete_objects(&storage, "del-empty-body", body)
            .await
            .unwrap_err();
        assert_eq!(err.status_code(), 400);
        assert_eq!(err.error_code(), "InvalidArgument");
    }

    #[tokio::test]
    async fn test_delete_objects_with_version_id_is_echoed_in_response() {
        // Per S3 spec, the VersionId from the request must be echoed back
        // in the <Deleted> element of the response.
        let storage = DefaultStorage::new();
        storage.create_bucket("del-versioned").await.unwrap();

        handle_put_object(
            &storage,
            "del-versioned",
            "v-key",
            "text/plain",
            HashMap::new(),
            Bytes::from("data"),
        )
        .await
        .expect("put_object");

        let xml = Bytes::from(
            "<Delete><Object><Key>v-key</Key><VersionId>abc123</VersionId></Object></Delete>",
        );
        let resp = handle_delete_objects(&storage, "del-versioned", xml)
            .await
            .expect("should succeed with version_id present");
        assert_eq!(resp.status(), 200);

        let resp_body = body_to_string(resp.into_body()).await;
        assert!(
            resp_body.contains("<VersionId>abc123</VersionId>"),
            "VersionId should be echoed in response: {resp_body}"
        );
        assert!(resp_body.contains("<Key>v-key</Key>"));
    }

    #[tokio::test]
    async fn test_delete_objects_version_id_echoed_in_nosuchkey_error() {
        // Per S3 spec, VersionId from the request must be echoed back
        // in <Error> elements when the key doesn't exist.
        let storage = DefaultStorage::new();
        storage.create_bucket("del-versioned-error").await.unwrap();

        // Delete a non-existent key with a VersionId — should get a NoSuchKey error
        // that echoes the VersionId back.
        let xml = Bytes::from(
            "<Delete><Object><Key>no-such-key</Key><VersionId>ver-999</VersionId></Object></Delete>",
        );
        let resp = handle_delete_objects(&storage, "del-versioned-error", xml)
            .await
            .expect("should succeed with per-key error");
        assert_eq!(resp.status(), 200);

        let resp_body = body_to_string(resp.into_body()).await;
        assert!(
            resp_body.contains("<VersionId>ver-999</VersionId>"),
            "VersionId should be echoed in error response: {resp_body}"
        );
        assert!(resp_body.contains("<Code>NoSuchKey</Code>"));
        assert!(resp_body.contains("<Key>no-such-key</Key>"));
    }

    #[tokio::test]
    async fn test_delete_objects_version_id_absent_when_request_has_none() {
        // When the request has no VersionId, the response should not include
        // a <VersionId> element (skip_serializing_if handles this).
        let storage = DefaultStorage::new();
        storage.create_bucket("del-no-version").await.unwrap();

        handle_put_object(
            &storage,
            "del-no-version",
            "plain-key",
            "text/plain",
            HashMap::new(),
            Bytes::from("data"),
        )
        .await
        .expect("put_object");

        let xml = Bytes::from("<Delete><Object><Key>plain-key</Key></Object></Delete>");
        let resp = handle_delete_objects(&storage, "del-no-version", xml)
            .await
            .expect("should succeed");
        assert_eq!(resp.status(), 200);

        let resp_body = body_to_string(resp.into_body()).await;
        assert!(
            !resp_body.contains("<VersionId>"),
            "VersionId should not appear when request has none: {resp_body}"
        );
        assert!(resp_body.contains("<Key>plain-key</Key>"));
    }

    #[tokio::test]
    async fn test_delete_objects_over_1000_returns_invalid_argument() {
        let storage = DefaultStorage::new();

        // 1001 keys — exceeds the 1000-key limit.
        let keys: Vec<String> = (0..1001).map(|i| format!("k-{}", i)).collect();
        let key_refs: Vec<&str> = keys.iter().map(|s| s.as_str()).collect();
        let body = delete_xml_body(&key_refs, None);

        let err = handle_delete_objects(&storage, "some-bucket", body)
            .await
            .unwrap_err();
        assert_eq!(err.status_code(), 400);
        assert_eq!(err.error_code(), "InvalidArgument");
        // The XML response should carry the Limit message.
        let xml = err.to_xml();
        assert!(xml.contains("must not exceed 1000"));
    }

    #[tokio::test]
    async fn test_delete_objects_exactly_1000_succeeds() {
        let storage = DefaultStorage::new();
        storage.create_bucket("del-1000").await.unwrap();

        let keys: Vec<String> = (0..1000).map(|i| format!("k-{}", i)).collect();
        let key_refs: Vec<&str> = keys.iter().map(|s| s.as_str()).collect();
        let body = delete_xml_body(&key_refs, None);

        let resp = handle_delete_objects(&storage, "del-1000", body)
            .await
            .expect("1000 keys should be allowed");
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_delete_objects_999_succeeds() {
        let storage = DefaultStorage::new();
        storage.create_bucket("del-999").await.unwrap();

        let keys: Vec<String> = (0..999).map(|i| format!("k-{}", i)).collect();
        let key_refs: Vec<&str> = keys.iter().map(|s| s.as_str()).collect();
        let body = delete_xml_body(&key_refs, None);

        let resp = handle_delete_objects(&storage, "del-999", body)
            .await
            .expect("999 keys should be allowed");
        assert_eq!(resp.status(), 200);
    }
}
