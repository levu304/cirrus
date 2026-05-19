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
};
use cirrus_protocol::xml::serialize;
use crate::storage::{Storage, S3Error};
use crate::service::s3_error_to_aws;

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
}
