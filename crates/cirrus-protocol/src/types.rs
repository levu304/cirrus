// Cirrus protocol shared types.
//
// This module defines the fundamental data structures shared across all
// Cirrus layers: storage metadata, S3 XML API request/response types,
// and serialization helpers for quick-xml.
//
// XML element naming follows the AWS S3 API specification exactly.
// xmlns attributes and XML declarations are handled by the xml module.

use bytes::Bytes;
use chrono::{DateTime, Utc};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// XML serialization helpers
// ---------------------------------------------------------------------------

/// Expand self-closing XML tags to open/close pairs.
///
/// quick-xml serializes empty `String` values as `<Tag/>` (self-closing).
/// Some S3 clients expect `<Tag></Tag>` instead. This function converts
/// all self-closing tags to the open/close form.
#[allow(dead_code)]
pub(crate) fn expand_empty_tags(xml: &str) -> String {
    let mut result = String::with_capacity(xml.len() + 64);
    let len = xml.len();
    let mut i = 0;
    let bytes = xml.as_bytes();

    while i < len {
        if bytes[i] == b'<' {
            // Look for the closing `>` from this position
            if let Some(gt_offset) = xml[i..].find('>') {
                let tag_content = &xml[i + 1..i + gt_offset];
                // Check if this is a self-closing tag (ends with `/`) with a simple name
                if let Some(name) = tag_content.strip_suffix('/').map(str::trim) {
                    if !name.is_empty()
                        && name
                            .chars()
                            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
                    {
                        result.push('<');
                        result.push_str(name);
                        result.push_str("></");
                        result.push_str(name);
                        result.push('>');
                        i += gt_offset + 1;
                        continue;
                    }
                }
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// Serialize a value to XML with self-closing tags expanded.
///
/// Uses quick-xml for serialization then expands any `<Tag/>` to `<Tag></Tag>`.
#[allow(dead_code)]
pub(crate) fn to_xml_string<T: serde::Serialize>(value: &T) -> String {
    let body =
        quick_xml::se::to_string(value).expect("XML serialization should not fail for valid types");
    expand_empty_tags(&body)
}

// ---------------------------------------------------------------------------
// Core data types
// ---------------------------------------------------------------------------

/// Bucket/object owner information.
///
/// Used in ListBuckets and ListParts responses.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Owner {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "DisplayName")]
    pub display_name: String,
}

/// Canonical S3 object shared across all storage layers.
///
/// This is the in-memory representation of an S3 object, not an XML type.
/// It bridges storage (cirrus-s3) and request handling (cirrus-router).
#[derive(Debug, Clone)]
pub struct S3Object {
    pub data: Bytes,
    /// MD5 hex wrapped in quotes, e.g. `"d41d8cd98f00b204e9800998ecf8427e"`
    pub etag: String,
    /// Default: `"binary/octet-stream"`
    pub content_type: String,
    pub content_length: usize,
    pub last_modified: DateTime<Utc>,
    /// x-amz-meta-* metadata headers
    pub metadata: HashMap<String, String>,
}

impl S3Object {
    /// Default content type for S3 objects when none is specified.
    pub const DEFAULT_CONTENT_TYPE: &'static str = "binary/octet-stream";
}

// ---------------------------------------------------------------------------
// ListAllMyBucketsResult (GET Service)
//
// Response for listing all buckets owned by the authenticated user.
// §4.1 of the S3 API spec.
// ---------------------------------------------------------------------------

/// Root element for the ListBuckets response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ListAllMyBucketsResult {
    #[serde(rename = "Owner")]
    pub owner: Owner,
    #[serde(rename = "Buckets")]
    pub buckets: Buckets,
}

/// Wrapper around the list of buckets (the `<Buckets>` element).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Buckets {
    #[serde(rename = "Bucket")]
    pub bucket: Vec<BucketInfo>,
}

/// Individual bucket entry in a ListBuckets response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BucketInfo {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "CreationDate")]
    pub creation_date: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// CreateBucketOutput
//
// Response after a successful bucket creation.
// §4.2
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
pub struct CreateBucketOutput {
    #[serde(rename = "Location")]
    pub location: String,
}

// ---------------------------------------------------------------------------
// ListBucketResult (ListObjectsV2)
//
// Response for listing objects within a bucket.
// §4.3 / §5.1
// ---------------------------------------------------------------------------

/// Root element for the ListObjectsV2 response.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ListBucketResult {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "ContinuationToken")]
    pub continuation_token: String,
    #[serde(rename = "StartAfter")]
    pub start_after: String,
    #[serde(rename = "Delimiter")]
    pub delimiter: String,
    #[serde(rename = "Prefix")]
    pub prefix: String,
    #[serde(rename = "MaxKeys")]
    pub max_keys: i32,
    #[serde(rename = "KeyCount")]
    pub key_count: i32,
    #[serde(rename = "IsTruncated")]
    pub is_truncated: bool,
    #[serde(rename = "NextContinuationToken")]
    pub next_continuation_token: String,
    #[serde(rename = "Contents", skip_serializing_if = "Vec::is_empty")]
    pub contents: Vec<ObjectInfo>,
    #[serde(rename = "CommonPrefixes", skip_serializing_if = "Vec::is_empty")]
    pub common_prefixes: Vec<CommonPrefixes>,
}

/// One object entry inside `<Contents>`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ObjectInfo {
    #[serde(rename = "Key")]
    pub key: String,
    #[serde(rename = "LastModified")]
    pub last_modified: DateTime<Utc>,
    #[serde(rename = "ETag")]
    pub etag: String,
    #[serde(rename = "Size")]
    pub size: i64,
    #[serde(rename = "StorageClass")]
    pub storage_class: String,
}

/// One common-prefix entry inside `<CommonPrefixes>`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CommonPrefixes {
    #[serde(rename = "Prefix")]
    pub prefix: String,
}

// ---------------------------------------------------------------------------
// DeleteObjects request & response
//
// §4.4
// ---------------------------------------------------------------------------

/// Incoming DeleteObjects request from the client.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename = "Delete")]
pub struct DeleteRequest {
    #[serde(rename = "Quiet", default)]
    pub quiet: bool,
    #[serde(rename = "Object")]
    pub objects: Vec<DeleteObject>,
}

/// One object key to delete inside the Delete request.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct DeleteObject {
    #[serde(rename = "Key")]
    pub key: String,
    #[serde(rename = "VersionId", default)]
    pub version_id: Option<String>,
}

/// DeleteObjects response returned to the client.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DeleteResult {
    #[serde(rename = "Deleted", skip_serializing_if = "Vec::is_empty")]
    pub deleted: Vec<DeletedObject>,
    #[serde(rename = "Error", skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<DeleteError>,
}

/// A successfully deleted object entry.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DeletedObject {
    #[serde(rename = "Key")]
    pub key: String,
}

/// A delete error entry for an object that could not be deleted.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DeleteError {
    #[serde(rename = "Key")]
    pub key: String,
    #[serde(rename = "Code")]
    pub code: String,
    #[serde(rename = "Message")]
    pub message: String,
}

// ---------------------------------------------------------------------------
// CopyObjectResult
//
// Response for a copy-object operation.
// §4.5
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
pub struct CopyObjectResult {
    #[serde(rename = "ETag")]
    pub etag: String,
    #[serde(rename = "LastModified")]
    pub last_modified: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Multipart Upload types
//
// §4.6–§4.10
// ---------------------------------------------------------------------------

/// Response for initiating a multipart upload.
#[derive(Debug, Clone, serde::Serialize)]
pub struct InitiateMultipartUploadResult {
    #[serde(rename = "Bucket")]
    pub bucket: String,
    #[serde(rename = "Key")]
    pub key: String,
    #[serde(rename = "UploadId")]
    pub upload_id: String,
}

/// Incoming CompleteMultipartUpload request from the client.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename = "CompleteMultipartUpload")]
pub struct CompleteMultipartUploadRequest {
    #[serde(rename = "Part")]
    pub parts: Vec<Part>,
}

/// One part entry in a CompleteMultipartUpload request.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Part {
    #[serde(rename = "PartNumber")]
    pub part_number: u32,
    #[serde(rename = "ETag")]
    pub etag: String,
}

/// Response for a completed multipart upload.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CompleteMultipartUploadResult {
    #[serde(rename = "Location")]
    pub location: String,
    #[serde(rename = "Bucket")]
    pub bucket: String,
    #[serde(rename = "Key")]
    pub key: String,
    #[serde(rename = "ETag")]
    pub etag: String,
}

/// Response for listing parts of an in-progress multipart upload.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ListPartsResult {
    #[serde(rename = "Bucket")]
    pub bucket: String,
    #[serde(rename = "Key")]
    pub key: String,
    #[serde(rename = "UploadId")]
    pub upload_id: String,
    #[serde(rename = "Initiator")]
    pub initiator: Owner,
    #[serde(rename = "Owner")]
    pub owner: Owner,
    #[serde(rename = "MaxParts")]
    pub max_parts: i32,
    #[serde(rename = "NextPartNumberMarker")]
    pub next_part_number_marker: String,
    #[serde(rename = "StorageClass")]
    pub storage_class: String,
    #[serde(rename = "Part", skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<PartInfo>,
    #[serde(rename = "IsTruncated")]
    pub is_truncated: bool,
}

/// One part entry in a ListParts response.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PartInfo {
    #[serde(rename = "PartNumber")]
    pub part_number: u32,
    #[serde(rename = "LastModified")]
    pub last_modified: DateTime<Utc>,
    #[serde(rename = "ETag")]
    pub etag: String,
    #[serde(rename = "Size")]
    pub size: i64,
}

// ---------------------------------------------------------------------------
// LocationConstraint
//
// Used in CreateBucket configuration for region specification.
// §4.11
// ---------------------------------------------------------------------------

/// Simple text-content wrapper for the region string.
///
/// Serializes as `<LocationConstraint>us-east-1</LocationConstraint>`.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename = "LocationConstraint")]
pub struct LocationConstraint {
    #[serde(rename = "$text")]
    pub location: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use quick_xml::de::from_str;
    use quick_xml::se::to_string;

    // -- Owner -----------------------------------------------------------

    #[test]
    fn test_owner_serialize() {
        let owner = Owner {
            id: "user-123".into(),
            display_name: "Test Owner".into(),
        };
        let xml = to_string(&owner).expect("serialize Owner");
        assert!(xml.contains("<Owner>"));
        assert!(xml.contains("<ID>user-123</ID>"));
        assert!(xml.contains("<DisplayName>Test Owner</DisplayName>"));
        assert!(xml.contains("</Owner>"));
    }

    // -- ListAllMyBucketsResult ------------------------------------------

    #[test]
    fn test_list_all_my_buckets_result_serialize() {
        let result = ListAllMyBucketsResult {
            owner: Owner {
                id: "owner-id".into(),
                display_name: "bucket-owner".into(),
            },
            buckets: Buckets {
                bucket: vec![
                    BucketInfo {
                        name: "alpha".into(),
                        creation_date: Utc::now(),
                    },
                    BucketInfo {
                        name: "beta".into(),
                        creation_date: Utc::now(),
                    },
                ],
            },
        };
        let xml = to_xml_string(&result);
        assert!(xml.contains("<ListAllMyBucketsResult>"));
        assert!(xml.contains("<Owner>"));
        assert!(xml.contains("<Buckets>"));
        assert!(xml.contains("<Bucket>"));
        assert!(xml.contains("<Name>alpha</Name>"));
        assert!(xml.contains("<Name>beta</Name>"));
        assert!(xml.contains("<ID>owner-id</ID>"));
    }

    // -- ListBucketResult (ListObjectsV2) --------------------------------

    #[test]
    fn test_list_bucket_result_serialize() {
        let result = ListBucketResult {
            name: "my-bucket".into(),
            continuation_token: String::new(),
            start_after: String::new(),
            delimiter: String::new(),
            prefix: String::new(),
            max_keys: 1000,
            key_count: 1,
            is_truncated: false,
            next_continuation_token: String::new(),
            contents: vec![ObjectInfo {
                key: "photos/cat.jpg".into(),
                last_modified: Utc::now(),
                etag: "\"d41d8cd98f00b204e9800998ecf8427e\"".into(),
                size: 1024,
                storage_class: "STANDARD".into(),
            }],
            common_prefixes: vec![CommonPrefixes {
                prefix: "photos/".into(),
            }],
        };
        let xml = to_xml_string(&result);
        // Root element
        assert!(xml.contains("<ListBucketResult>"));
        // Echo fields should render as <E></E> (via expand_empty_tags)
        assert!(xml.contains("<ContinuationToken></ContinuationToken>"));
        assert!(xml.contains("<StartAfter></StartAfter>"));
        // Object content
        assert!(xml.contains("<Contents>"));
        assert!(xml.contains("<Key>photos/cat.jpg</Key>"));
        assert!(xml.contains("<Size>1024</Size>"));
        assert!(xml.contains("<StorageClass>STANDARD</StorageClass>"));
        // Common prefixes
        assert!(xml.contains("<CommonPrefixes>"));
        assert!(xml.contains("<Prefix>photos/</Prefix>"));
    }

    #[test]
    fn test_list_bucket_result_empty_contents_omitted() {
        let result = ListBucketResult {
            name: "empty-bucket".into(),
            continuation_token: String::new(),
            start_after: String::new(),
            delimiter: String::new(),
            prefix: String::new(),
            max_keys: 1000,
            key_count: 0,
            is_truncated: false,
            next_continuation_token: String::new(),
            contents: vec![],
            common_prefixes: vec![],
        };
        let xml = to_xml_string(&result);
        // Empty Vec fields should be omitted
        assert!(!xml.contains("<Contents>"));
        assert!(!xml.contains("<CommonPrefixes>"));
        // Echo fields with no value should render as <E></E>
        assert!(xml.contains("<ContinuationToken></ContinuationToken>"));
        assert!(xml.contains("<StartAfter></StartAfter>"));
        assert!(xml.contains("<Delimiter></Delimiter>"));
        assert!(xml.contains("<Prefix></Prefix>"));
        assert!(xml.contains("<NextContinuationToken></NextContinuationToken>"));
    }

    // -- CreateBucketOutput ----------------------------------------------

    #[test]
    fn test_create_bucket_output_serialize() {
        let output = CreateBucketOutput {
            location: "http://localhost:4566/my-bucket".into(),
        };
        let xml = to_xml_string(&output);
        assert!(xml.contains("<CreateBucketOutput>"));
        assert!(xml.contains("<Location>http://localhost:4566/my-bucket</Location>"));
    }

    // -- Delete request (deserialize) ------------------------------------

    #[test]
    fn test_delete_request_deserialize() {
        let xml = r#"
            <Delete>
                <Quiet>true</Quiet>
                <Object><Key>key1</Key></Object>
                <Object><Key>key2</Key><VersionId>version-2</VersionId></Object>
            </Delete>
        "#;
        let req: DeleteRequest = from_str(xml).expect("deserialize DeleteRequest");
        assert!(req.quiet, "Quiet should be true");
        assert_eq!(req.objects.len(), 2);
        assert_eq!(req.objects[0].key, "key1");
        assert_eq!(req.objects[1].key, "key2");
        assert_eq!(req.objects[0].version_id, None);
        assert_eq!(req.objects[1].version_id, Some("version-2".into()));
    }

    #[test]
    fn test_delete_request_defaults() {
        let xml = r#"
            <Delete>
                <Object><Key>single</Key></Object>
            </Delete>
        "#;
        let req: DeleteRequest = from_str(xml).expect("deserialize DeleteRequest without Quiet");
        assert!(!req.quiet, "Quiet should default to false");
        assert_eq!(req.objects.len(), 1);
        assert_eq!(req.objects[0].key, "single");
        assert_eq!(req.objects[0].version_id, None);
    }

    // -- DeleteResult (serialize) ----------------------------------------

    #[test]
    fn test_delete_result_serialize() {
        let result = DeleteResult {
            deleted: vec![DeletedObject { key: "deleted-key".into() }],
            errors: vec![DeleteError {
                key: "failed-key".into(),
                code: "NoSuchKey".into(),
                message: "The specified key does not exist.".into(),
            }],
        };
        let xml = to_xml_string(&result);
        assert!(xml.contains("<DeleteResult>"));
        assert!(xml.contains("<Deleted><Key>deleted-key</Key></Deleted>"));
        assert!(xml.contains("<Error>"));
        assert!(xml.contains("<Code>NoSuchKey</Code>"));
    }

    #[test]
    fn test_delete_result_empty_omitted() {
        let result = DeleteResult {
            deleted: vec![],
            errors: vec![],
        };
        let xml = to_xml_string(&result);
        // Empty Vecs should be omitted
        assert!(!xml.contains("<Deleted>"));
        assert!(!xml.contains("<Error>"));
    }

    // -- CopyObjectResult ------------------------------------------------

    #[test]
    fn test_copy_object_result_serialize() {
        let result = CopyObjectResult {
            etag: "\"etag-value\"".into(),
            last_modified: Utc::now(),
        };
        let xml = to_xml_string(&result);
        assert!(xml.contains("<CopyObjectResult>"));
        assert!(xml.contains("<ETag>"));
        assert!(xml.contains("<LastModified>"));
    }

    // -- InitiateMultipartUploadResult -----------------------------------

    #[test]
    fn test_initiate_multipart_upload_result_serialize() {
        let result = InitiateMultipartUploadResult {
            bucket: "my-bucket".into(),
            key: "large-file.zip".into(),
            upload_id: "upload-id-123".into(),
        };
        let xml = to_xml_string(&result);
        assert!(xml.contains("<Bucket>my-bucket</Bucket>"));
        assert!(xml.contains("<Key>large-file.zip</Key>"));
        assert!(xml.contains("<UploadId>upload-id-123</UploadId>"));
    }

    // -- CompleteMultipartUpload request (deserialize) -------------------

    #[test]
    fn test_complete_multipart_upload_request_deserialize() {
        let xml = r#"
            <CompleteMultipartUpload>
                <Part><PartNumber>1</PartNumber><ETag>"etag-1"</ETag></Part>
                <Part><PartNumber>2</PartNumber><ETag>"etag-2"</ETag></Part>
            </CompleteMultipartUpload>
        "#;
        let req: CompleteMultipartUploadRequest =
            from_str(xml).expect("deserialize CompleteMultipartUploadRequest");
        assert_eq!(req.parts.len(), 2);
        assert_eq!(req.parts[0].part_number, 1);
        assert_eq!(req.parts[0].etag, "\"etag-1\"");
        assert_eq!(req.parts[1].part_number, 2);
        assert_eq!(req.parts[1].etag, "\"etag-2\"");
    }

    // -- CompleteMultipartUploadResult -----------------------------------

    #[test]
    fn test_complete_multipart_upload_result_serialize() {
        let result = CompleteMultipartUploadResult {
            location: "http://localhost:4566/my-bucket/file.zip".into(),
            bucket: "my-bucket".into(),
            key: "file.zip".into(),
            etag: "\"a1b2c3d4-3\"".into(),
        };
        let xml = to_xml_string(&result);
        assert!(xml.contains("<Bucket>my-bucket</Bucket>"));
        assert!(xml.contains("<Key>file.zip</Key>"));
        assert!(xml.contains("<ETag>"));
        assert!(xml.contains(
            "<Location>http://localhost:4566/my-bucket/file.zip</Location>"
        ));
    }

    // -- ListPartsResult -------------------------------------------------

    #[test]
    fn test_list_parts_result_serialize() {
        let now = Utc::now();
        let result = ListPartsResult {
            bucket: "my-bucket".into(),
            key: "large-file.zip".into(),
            upload_id: "upload-xyz".into(),
            initiator: Owner {
                id: "init-id".into(),
                display_name: "initiator".into(),
            },
            owner: Owner {
                id: "owner-id".into(),
                display_name: "bucket-owner".into(),
            },
            max_parts: 1000,
            next_part_number_marker: String::new(),
            storage_class: "STANDARD".into(),
            parts: vec![PartInfo {
                part_number: 1,
                last_modified: now,
                etag: "\"etag-1\"".into(),
                size: 5_242_880,
            }],
            is_truncated: false,
        };
        let xml = to_xml_string(&result);
        assert!(xml.contains("<ListPartsResult>"));
        assert!(xml.contains("<Initiator>"));
        assert!(xml.contains("<Owner>"));
        assert!(xml.contains("<Part>"));
        assert!(xml.contains("<PartNumber>1</PartNumber>"));
        assert!(xml.contains("<Size>5242880</Size>"));
        assert!(xml.contains(
            "<NextPartNumberMarker></NextPartNumberMarker>"
        ));
    }

    // -- LocationConstraint ----------------------------------------------

    #[test]
    fn test_location_constraint_serialize() {
        let lc = LocationConstraint {
            location: "us-east-1".into(),
        };
        let xml = to_xml_string(&lc);
        assert!(
            xml.contains("<LocationConstraint>us-east-1</LocationConstraint>"),
            "expected LocationConstraint element, got: {xml}"
        );
    }

    // -- expand_empty_tags -----------------------------------------------

    #[test]
    fn test_expand_empty_tags_basic() {
        let input = "<Foo><Bar/><Baz>content</Baz></Foo>";
        let output = expand_empty_tags(input);
        assert_eq!(output, "<Foo><Bar></Bar><Baz>content</Baz></Foo>");
    }

    #[test]
    fn test_expand_empty_tags_no_change() {
        let input = "<Root><A>hello</A><B><C>world</C></B></Root>";
        let output = expand_empty_tags(input);
        assert_eq!(output, input);
    }

    #[test]
    fn test_expand_empty_tags_trailing_slash_in_content() {
        // Must not expand non-tag patterns like "5 / 2" embedded in text
        let input = r#"<Math>5 / 2</Math>"#;
        let output = expand_empty_tags(input);
        assert_eq!(output, input);
    }

    // -- to_xml_string wrapper -------------------------------------------

    #[test]
    fn test_to_xml_string_expands_empty_tags() {
        #[derive(serde::Serialize)]
        struct S {
            #[serde(rename = "A")]
            a: String,
            #[serde(rename = "B")]
            b: String,
        }
        let s = S {
            a: "".into(),
            b: "val".into(),
        };
        let xml = to_xml_string(&s);
        assert!(
            !xml.contains("<A/>"),
            "self-closing tag should not appear: {xml}"
        );
        assert!(
            xml.contains("<A></A>"),
            "empty field should have open/close tags: {xml}"
        );
        assert!(xml.contains("<B>val</B>"));
    }
}
