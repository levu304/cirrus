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
        location: format!("http://localhost:4566/{}", bucket),
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
    storage: &S,
    dst_bucket: &str,
    dst_key: &str,
    copy_source: &str,
) -> Result<Response<Body>, AwsError> {
    let (src_bucket, src_key) = validate_copy_source(copy_source)?;

    let obj = storage
        .copy_object(src_bucket, src_key, dst_bucket, dst_key)
        .await
        .map_err(|e| s3_error_to_aws(e, src_bucket, src_key))?;

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
        .header("x-amz-id-2", "cirrus-v0.1.0")
        .body(Body::from(xml))
        .map_err(|e| AwsError::new(AwsErrorKind::InternalError {
            details: Some(format!("response build failed: {e}")),
        }))
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
    let content_type = if content_type.is_empty() {
        S3Object::DEFAULT_CONTENT_TYPE
    } else {
        content_type
    };

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
        .header("x-amz-server-side-encryption", "AES256")
        .header("x-amz-request-id", &request_id)
        .header("x-amz-id-2", "cirrus-v0.1.0")
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
        .header("Accept-Ranges", "bytes")
        .header("x-amz-request-id", &request_id)
        .header("x-amz-id-2", "cirrus-v0.1.0");

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
        .header("Accept-Ranges", "bytes")
        .header("x-amz-request-id", &request_id)
        .header("x-amz-id-2", "cirrus-v0.1.0");

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
        .header("x-amz-id-2", "cirrus-v0.1.0")
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
        assert_eq!(location, "http://localhost:4566/my-new-bucket");
        let body = body_to_string(resp.into_body()).await;
        assert!(body.contains("<CreateBucketOutput"));
        assert!(body.contains("<Location>http://localhost:4566/my-new-bucket</Location>"));
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
        assert_eq!(
            resp.headers()
                .get("x-amz-server-side-encryption")
                .unwrap()
                .to_str()
                .unwrap(),
            "AES256"
        );
        assert!(resp.headers().get("x-amz-request-id").is_some());
        assert_eq!(
            resp.headers().get("x-amz-id-2").unwrap().to_str().unwrap(),
            "cirrus-v0.1.0"
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
        let accept_ranges = resp
            .headers()
            .get("Accept-Ranges")
            .map(|v| v.to_str().unwrap().to_string());

        let resp_body = body_to_string(resp.into_body()).await;

        assert_eq!(content_type.unwrap(), "application/json");
        assert_eq!(content_length.unwrap(), "20"); // "hello world from get".len() = 20
        assert_eq!(etag.unwrap(), format_etag(&body));
        assert!(last_modified);
        assert_eq!(accept_ranges.unwrap(), "bytes");
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
        assert_eq!(
            resp.headers()
                .get("Accept-Ranges")
                .unwrap()
                .to_str()
                .unwrap(),
            "bytes"
        );

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

        let resp = handle_copy_object(&storage, "copy-dst", "dest.txt", "/copy-src/source.txt")
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
        )
        .await
        .unwrap_err();
        assert_eq!(err.status_code(), 404);
        assert_eq!(err.error_code(), "NoSuchBucket");
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
        )
        .await
        .unwrap_err();
        assert_eq!(err.status_code(), 400);
        assert_eq!(err.error_code(), "InvalidArgument");
    }

    #[tokio::test]
    async fn test_copy_object_same_bucket() {
        let storage = DefaultStorage::new();
        storage.create_bucket("b").await.unwrap();

        let body = Bytes::from("same bucket copy");
        handle_put_object(&storage, "b", "original.txt", "text/plain", HashMap::new(), body.clone())
            .await
            .expect("put_object");

        let resp = handle_copy_object(&storage, "b", "copy.txt", "/b/original.txt")
            .await
            .expect("copy_object same bucket");
        assert_eq!(resp.status(), 200);

        // Both objects should exist and have the same content.
        let orig = storage.get_object("b", "original.txt").await.unwrap();
        let copy = storage.get_object("b", "copy.txt").await.unwrap();
        assert_eq!(orig.object.data, copy.object.data);
    }
}
