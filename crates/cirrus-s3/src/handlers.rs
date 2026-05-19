// S3 request handlers.
//
// This module contains the per-operation handler functions for the S3 API.
// Each handler is a generic async function parameterized over `S: Storage`.
//
// Phase 5b: Bucket-level handlers (ListBuckets, CreateBucket, DeleteBucket,
//           GetBucketLocation, ListObjectsV2).
// Phase 5c: Object-level handlers (PutObject, GetObject, HeadObject,
//           DeleteObject, CopyObject, DeleteObjects).
// Phase 5d: Multipart upload handlers (CreateMultipartUpload, UploadPart).
// Phase 5e: Remaining multipart handlers (CompleteMultipartUpload,
//           AbortMultipartUpload, ListParts).

use axum::body::Body;
use bytes::Bytes;
use chrono::Utc;
use http::{Response, StatusCode};
use md5::{Digest, Md5};
use std::collections::HashMap;

use cirrus_protocol::error::{AwsError, AwsErrorKind};
use cirrus_protocol::types::{
    to_xml_string, Buckets, CompleteMultipartUploadRequest,
    CompleteMultipartUploadResult, CopyObjectResult, DeleteError, DeleteResult,
    DeletedObject, InitiateMultipartUploadResult, ListAllMyBucketsResult,
    ListBucketResult, ListPartsResult, LocationConstraint, Owner, S3Object,
    StorageClass,
};
use crate::storage::{S3Error, Storage};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert an [`S3Error`] into an [`AwsError`] with appropriate error kind.
fn s3_error(err: S3Error, bucket: &str, key: &str) -> AwsError {
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

/// Build an XML response with proper Content-Type and XML declaration.
fn xml_response<T: serde::Serialize>(value: &T) -> Result<Response<Body>, AwsError> {
    let body = to_xml_string(value)?;
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n{}",
        body
    );
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/xml")
        .body(Body::from(xml))
        .map_err(|e| {
            AwsError::new(AwsErrorKind::InternalError {
                details: Some(format!("Failed to build XML response: {}", e)),
            })
        })
}

/// Build a response with only a status code and no body (e.g. 204 No Content).
fn empty_response(status: StatusCode) -> Result<Response<Body>, AwsError> {
    Response::builder()
        .status(status)
        .body(Body::empty())
        .map_err(|e| {
            AwsError::new(AwsErrorKind::InternalError {
                details: Some(format!("Failed to build response: {}", e)),
            })
        })
}

/// Parse a query string into a key-value map.
fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter_map(|pair| {
            let mut parts = pair.splitn(2, '=');
            match (parts.next(), parts.next()) {
                (Some(k), Some(v)) => Some((k.to_string(), v.to_string())),
                (Some(k), None) if !k.is_empty() => Some((k.to_string(), String::new())),
                _ => None,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Bucket-level handlers
// ---------------------------------------------------------------------------

/// GET / — list all buckets owned by the user.
pub async fn handle_list_buckets<S: Storage>(
    storage: &S,
) -> Result<Response<Body>, AwsError> {
    let bucket_infos = storage.list_buckets().await.map_err(|e| s3_error(e, "", ""))?;

    let result = ListAllMyBucketsResult {
        owner: Owner {
            id: "000000000000".into(),
            display_name: "cirrus".into(),
        },
        buckets: Buckets {
            bucket: bucket_infos,
        },
    };

    xml_response(&result)
}

/// PUT /{bucket} — create a new bucket.
pub async fn handle_create_bucket<S: Storage>(
    storage: &S,
    bucket: &str,
) -> Result<Response<Body>, AwsError> {
    storage
        .create_bucket(bucket)
        .await
        .map_err(|e| s3_error(e, bucket, ""))?;

    Response::builder()
        .status(StatusCode::OK)
        .header("Location", format!("/{}", bucket))
        .header("Content-Length", "0")
        .body(Body::empty())
        .map_err(|e| {
            AwsError::new(AwsErrorKind::InternalError {
                details: Some(format!("Failed to build response: {}", e)),
            })
        })
}

/// DELETE /{bucket} — delete an empty bucket.
pub async fn handle_delete_bucket<S: Storage>(
    storage: &S,
    bucket: &str,
) -> Result<Response<Body>, AwsError> {
    storage
        .delete_bucket(bucket)
        .await
        .map_err(|e| s3_error(e, bucket, ""))?;

    empty_response(StatusCode::NO_CONTENT)
}

/// GET /{bucket}?location — get the bucket's region.
pub async fn handle_get_bucket_location<S: Storage>(
    storage: &S,
    bucket: &str,
) -> Result<Response<Body>, AwsError> {
    let location = storage
        .get_bucket_location(bucket)
        .await
        .map_err(|e| s3_error(e, bucket, ""))?;

    let result = LocationConstraint {
        location,
    };

    xml_response(&result)
}

/// GET /{bucket}?list-type=2 (or plain GET /{bucket}) — list objects.
pub async fn handle_list_objects_v2<S: Storage>(
    storage: &S,
    bucket: &str,
    query: &str,
) -> Result<Response<Body>, AwsError> {
    let params = parse_query(query);

    let prefix = params.get("prefix").map_or("", |s| s.as_str());
    let delimiter = params.get("delimiter").map_or("", |s| s.as_str());
    let start_after = params.get("start-after").map_or("", |s| s.as_str());
    let max_keys: u32 = params
        .get("max-keys")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000);
    let continuation_token = params.get("continuation-token").map_or("", |s| s.as_str());

    let list = storage
        .list_objects_v2(bucket, prefix, delimiter, start_after, max_keys, continuation_token)
        .await
        .map_err(|e| s3_error(e, bucket, ""))?;

    let result = ListBucketResult {
        name: bucket.to_string(),
        continuation_token: continuation_token.to_string(),
        start_after: start_after.to_string(),
        delimiter: delimiter.to_string(),
        encoding_type: None,
        prefix: prefix.to_string(),
        max_keys,
        key_count: list.key_count,
        is_truncated: list.is_truncated,
        next_continuation_token: list.next_continuation_token,
        contents: list.contents,
        common_prefixes: list.common_prefixes,
    };

    xml_response(&result)
}

/// POST /{bucket}?delete — delete multiple objects.
pub async fn handle_delete_objects<S: Storage>(
    storage: &S,
    bucket: &str,
    body: Bytes,
) -> Result<Response<Body>, AwsError> {
    use quick_xml::de as qx_de;

    let delete_req: cirrus_protocol::types::DeleteRequest = qx_de::from_reader(&body[..]).map_err(
        |e| {
            AwsError::new(AwsErrorKind::XmlSerializationError {
                details: format!("Failed to parse Delete request XML: {}", e),
            })
        },
    )?;

    let quiet = delete_req.quiet;
    let mut deleted: Vec<DeletedObject> = Vec::new();
    let mut errors: Vec<DeleteError> = Vec::new();

    for obj in &delete_req.objects {
        match storage.delete_object(bucket, &obj.key).await {
            Ok(()) => {
                if !quiet {
                    deleted.push(DeletedObject {
                        key: obj.key.clone(),
                        version_id: None,
                        delete_marker: None,
                        delete_marker_version_id: None,
                    });
                }
            }
            Err(e) => {
                errors.push(DeleteError {
                    key: obj.key.clone(),
                    code: match &e {
                        S3Error::NoSuchKey => "NoSuchKey",
                        _ => "InternalError",
                    }
                    .to_string(),
                    message: e.to_string(),
                    version_id: None,
                });
            }
        }
    }

    let result = DeleteResult { deleted, errors };
    xml_response(&result)
}

// ---------------------------------------------------------------------------
// Object-level handlers
// ---------------------------------------------------------------------------

/// PUT /{bucket}/{key} with x-amz-copy-source header — copy an object.
///
/// The `copy_source` value is the raw header value (possibly URL-encoded).
/// It is parsed here: leading `/` is stripped, query parameters are removed,
/// then split at the first `/` to obtain source-bucket and source-key.
pub async fn handle_copy_object<S: Storage>(
    storage: &S,
    dst_bucket: &str,
    dst_key: &str,
    copy_source: &str,
) -> Result<Response<Body>, AwsError> {
    // Strip leading slash and remove query params (e.g. ?versionId=xxx).
    let source = copy_source.strip_prefix('/').unwrap_or(copy_source);
    let source = source.split('?').next().unwrap_or(source);

    // Split at the first '/' into source-bucket / source-key.
    let (src_bucket, src_key) = source.split_once('/').ok_or_else(|| {
        AwsError::new(AwsErrorKind::InternalError {
            details: Some(format!("Invalid copy source: {}", copy_source)),
        })
    })?;

    // URL-decode the bucket and key.
    let src_bucket = urlencoding::decode(src_bucket).map_err(|e| {
        AwsError::new(AwsErrorKind::InternalError {
            details: Some(format!("Failed to decode copy-source bucket: {}", e)),
        })
    })?;
    let src_key = urlencoding::decode(src_key).map_err(|e| {
        AwsError::new(AwsErrorKind::InternalError {
            details: Some(format!("Failed to decode copy-source key: {}", e)),
        })
    })?;

    // Perform the copy via the storage layer.
    storage
        .copy_object(&src_bucket, &src_key, dst_bucket, dst_key)
        .await
        .map_err(|e| s3_error(e, dst_bucket, dst_key))?;

    // Retrieve the copied object to return etag and last-modified.
    let result = storage
        .get_object(dst_bucket, dst_key)
        .await
        .map_err(|e| s3_error(e, dst_bucket, dst_key))?;

    let copy_result = CopyObjectResult {
        etag: result.object.etag,
        last_modified: result.object.last_modified,
    };

    xml_response(&copy_result)
}

/// PUT /{bucket}/{key} — upload an object.
pub async fn handle_put_object<S: Storage>(
    storage: &S,
    bucket: &str,
    key: &str,
    content_type: &str,
    body: Bytes,
) -> Result<Response<Body>, AwsError> {
    let hash = Md5::digest(&body);
    let etag = format!("\"{:x}\"", hash);

    let obj = S3Object {
        data: body,
        etag: etag.clone(),
        content_type: content_type.to_string(),
        last_modified: Utc::now(),
        metadata: HashMap::new(),
    };

    storage
        .put_object(bucket, key, obj)
        .await
        .map_err(|e| s3_error(e, bucket, key))?;

    // Return 200 OK with the ETag header and no body.
    Response::builder()
        .status(StatusCode::OK)
        .header("ETag", etag.as_str())
        .body(Body::empty())
        .map_err(|e| {
            AwsError::new(AwsErrorKind::InternalError {
                details: Some(format!("Failed to build response: {}", e)),
            })
        })
}

/// GET /{bucket}/{key} — retrieve an object.
pub async fn handle_get_object<S: Storage>(
    storage: &S,
    bucket: &str,
    key: &str,
) -> Result<Response<Body>, AwsError> {
    let result = storage
        .get_object(bucket, key)
        .await
        .map_err(|e| s3_error(e, bucket, key))?;

    let obj = result.object;
    let content_length = obj.content_length();
    let last_modified = obj.last_modified.to_rfc2822();

    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", obj.content_type.as_str())
        .header("Content-Length", content_length.to_string())
        .header("ETag", obj.etag.as_str())
        .header("Last-Modified", last_modified.as_str());

    // Attach x-amz-meta-* metadata headers.
    for (meta_key, meta_val) in &obj.metadata {
        let header_name = format!("x-amz-meta-{}", meta_key);
        builder = builder.header(&header_name, meta_val.as_str());
    }

    builder.body(Body::from(obj.data)).map_err(|e| {
        AwsError::new(AwsErrorKind::InternalError {
            details: Some(format!("Failed to build response: {}", e)),
        })
    })
}

/// HEAD /{bucket}/{key} — return object metadata (headers only, no body).
pub async fn handle_head_object<S: Storage>(
    storage: &S,
    bucket: &str,
    key: &str,
) -> Result<Response<Body>, AwsError> {
    let result = storage
        .head_object(bucket, key)
        .await
        .map_err(|e| s3_error(e, bucket, key))?;

    let obj = result.object;
    let content_length = obj.content_length();
    let last_modified = obj.last_modified.to_rfc2822();

    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", obj.content_type.as_str())
        .header("Content-Length", content_length.to_string())
        .header("ETag", obj.etag.as_str())
        .header("Last-Modified", last_modified.as_str());

    for (meta_key, meta_val) in &obj.metadata {
        let header_name = format!("x-amz-meta-{}", meta_key);
        builder = builder.header(&header_name, meta_val.as_str());
    }

    builder.body(Body::empty()).map_err(|e| {
        AwsError::new(AwsErrorKind::InternalError {
            details: Some(format!("Failed to build response: {}", e)),
        })
    })
}

/// DELETE /{bucket}/{key} — delete an object.
pub async fn handle_delete_object<S: Storage>(
    storage: &S,
    bucket: &str,
    key: &str,
) -> Result<Response<Body>, AwsError> {
    storage
        .delete_object(bucket, key)
        .await
        .map_err(|e| s3_error(e, bucket, key))?;

    empty_response(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Multipart upload handlers
// ---------------------------------------------------------------------------

/// POST /{bucket}/{key}?uploads — initiate a multipart upload.
pub async fn handle_create_multipart_upload<S: Storage>(
    storage: &S,
    bucket: &str,
    key: &str,
) -> Result<Response<Body>, AwsError> {
    let upload_id = storage
        .create_multipart_upload(bucket, key)
        .await
        .map_err(|e| s3_error(e, bucket, key))?;

    let result = InitiateMultipartUploadResult {
        bucket: bucket.to_string(),
        key: key.to_string(),
        upload_id,
    };

    xml_response(&result)
}

/// PUT /{bucket}/{key}?partNumber=N&uploadId=ID — upload a part.
pub async fn handle_upload_part<S: Storage>(
    storage: &S,
    bucket: &str,
    key: &str,
    part_number: u32,
    upload_id: &str,
    body: Bytes,
) -> Result<Response<Body>, AwsError> {
    let etag = storage
        .upload_part(bucket, key, upload_id, part_number, body)
        .await
        .map_err(|e| s3_error(e, bucket, key))?;

    Response::builder()
        .status(StatusCode::OK)
        .header("ETag", etag.as_str())
        .body(Body::empty())
        .map_err(|e| {
            AwsError::new(AwsErrorKind::InternalError {
                details: Some(format!("Failed to build response: {}", e)),
            })
        })
}

/// POST /{bucket}/{key}?uploadId=ID — complete a multipart upload.
pub async fn handle_complete_multipart_upload<S: Storage>(
    storage: &S,
    bucket: &str,
    key: &str,
    upload_id: &str,
    body: Bytes,
) -> Result<Response<Body>, AwsError> {
    use quick_xml::de as qx_de;

    let req: CompleteMultipartUploadRequest = qx_de::from_reader(&body[..]).map_err(|e| {
        AwsError::new(AwsErrorKind::XmlSerializationError {
            details: format!(
                "Failed to parse CompleteMultipartUpload request XML: {}",
                e
            ),
        })
    })?;

    // The protocol types use the same `Part` type as the storage trait.
    let final_etag = storage
        .complete_multipart_upload(bucket, key, upload_id, &req.parts)
        .await
        .map_err(|e| s3_error(e, bucket, key))?;

    let result = CompleteMultipartUploadResult {
        location: format!("/{}/{}", bucket, key),
        bucket: bucket.to_string(),
        key: key.to_string(),
        etag: final_etag,
    };

    xml_response(&result)
}

/// DELETE /{bucket}/{key}?uploadId=ID — abort a multipart upload.
pub async fn handle_abort_multipart_upload<S: Storage>(
    storage: &S,
    bucket: &str,
    key: &str,
    upload_id: &str,
) -> Result<Response<Body>, AwsError> {
    storage
        .abort_multipart_upload(bucket, key, upload_id)
        .await
        .map_err(|e| s3_error(e, bucket, key))?;

    empty_response(StatusCode::NO_CONTENT)
}

/// GET /{bucket}/{key}?uploadId=ID — list parts of an in-progress upload.
pub async fn handle_list_parts<S: Storage>(
    storage: &S,
    bucket: &str,
    key: &str,
    upload_id: &str,
    query: &str,
) -> Result<Response<Body>, AwsError> {
    let params = parse_query(query);

    let max_parts: u32 = params
        .get("max-parts")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000);
    let part_number_marker: u32 = params
        .get("part-number-marker")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let parts_list = storage
        .list_parts(bucket, key, upload_id, max_parts, part_number_marker)
        .await
        .map_err(|e| s3_error(e, bucket, key))?;

    let result = ListPartsResult {
        bucket: bucket.to_string(),
        key: key.to_string(),
        upload_id: upload_id.to_string(),
        initiator: Owner {
            id: "000000000000".into(),
            display_name: "cirrus".into(),
        },
        owner: Owner {
            id: "000000000000".into(),
            display_name: "cirrus".into(),
        },
        max_parts,
        next_part_number_marker: parts_list.next_part_number_marker,
        part_number_marker: part_number_marker.to_string(),
        storage_class: StorageClass::STANDARD,
        parts: parts_list.parts,
        is_truncated: parts_list.is_truncated,
    };

    xml_response(&result)
}
