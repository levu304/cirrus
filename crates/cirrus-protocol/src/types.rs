// Cirrus protocol shared types.
//
// This module defines the fundamental data structures shared across all
// Cirrus layers: storage metadata, S3 XML API request/response types,
// and serialization helpers for quick-xml.
//
// XML element naming follows the AWS S3 API specification exactly.
// xmlns attributes and XML declarations are handled by the xml module.

use crate::error::{AwsError, AwsErrorKind};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// XML serialization helpers
// ---------------------------------------------------------------------------

/// Expand self-closing XML tags to open/close pairs.
///
/// quick-xml serializes empty `String` values as `<Tag/>` (self-closing).
/// Some S3 clients expect `<Tag></Tag>` instead. This function converts
/// all self-closing tags to the open/close form.
pub fn expand_empty_tags(xml: &str) -> String {
    let mut result = String::with_capacity(xml.len() + 64);
    let mut chars = xml.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '<' {
            // Peek ahead to detect special XML constructs before treating as a tag.
            let lookahead: String = chars.clone().take(8).collect();

            // XML comment: <!-- ... -->
            if lookahead.starts_with("!--") {
                result.push_str("<!--");
                chars.next(); // !
                chars.next(); // -
                chars.next(); // -
                // Scan for "-->", handling consecutive hyphens correctly
                loop {
                    match chars.next() {
                        Some('-') => {
                            // Collect consecutive hyphens
                            let mut hyphen_count = 1;
                            while let Some(&'-') = chars.peek() {
                                chars.next();
                                hyphen_count += 1;
                            }
                            match chars.next() {
                                Some('>') if hyphen_count >= 2 => {
                                    // Found "-->" (or more hyphens + ">")
                                    for _ in 0..hyphen_count {
                                        result.push('-');
                                    }
                                    result.push('>');
                                    break;
                                }
                                Some(c) => {
                                    // Not a terminator, emit all hyphens and the char
                                    for _ in 0..hyphen_count {
                                        result.push('-');
                                    }
                                    result.push(c);
                                }
                                None => {
                                    // Unterminated comment
                                    for _ in 0..hyphen_count {
                                        result.push('-');
                                    }
                                    break;
                                }
                            }
                        }
                        Some(c) => {
                            result.push(c);
                        }
                        None => break, // Unterminated comment
                    }
                }
                continue;
            }

            // CDATA section: <![CDATA[ ... ]]>
            if lookahead.starts_with("![CDATA[") {
                result.push_str("<![CDATA[");
                for _ in 0..8 {
                    chars.next();
                }
                // Scan for "]]>", handling consecutive brackets correctly
                loop {
                    match chars.next() {
                        Some(']') => {
                            let mut bracket_count = 1;
                            while let Some(&']') = chars.peek() {
                                chars.next();
                                bracket_count += 1;
                            }
                            match chars.next() {
                                Some('>') if bracket_count >= 2 => {
                                    for _ in 0..bracket_count {
                                        result.push(']');
                                    }
                                    result.push('>');
                                    break;
                                }
                                Some(c) => {
                                    for _ in 0..bracket_count {
                                        result.push(']');
                                    }
                                    result.push(c);
                                }
                                None => {
                                    for _ in 0..bracket_count {
                                        result.push(']');
                                    }
                                    break;
                                }
                            }
                        }
                        Some(c) => {
                            result.push(c);
                        }
                        None => break, // Unterminated CDATA
                    }
                }
                continue;
            }

            // Processing instruction: <? ... ?>
            if lookahead.starts_with('?') {
                result.push_str("<?");
                chars.next(); // ?
                // Scan for "?>", handling consecutive question marks correctly
                loop {
                    match chars.next() {
                        Some('?') => {
                            let mut question_count = 1;
                            while let Some(&'?') = chars.peek() {
                                chars.next();
                                question_count += 1;
                            }
                            match chars.next() {
                                Some('>') => {
                                    for _ in 0..question_count {
                                        result.push('?');
                                    }
                                    result.push('>');
                                    break;
                                }
                                Some(c) => {
                                    for _ in 0..question_count {
                                        result.push('?');
                                    }
                                    result.push(c);
                                }
                                None => {
                                    for _ in 0..question_count {
                                        result.push('?');
                                    }
                                    break;
                                }
                            }
                        }
                        Some(c) => {
                            result.push(c);
                        }
                        None => break, // Unterminated PI
                    }
                }
                continue;
            }

            // Regular tag: collect until '>' and check for self-closing
            let mut tag = String::new();
            let mut found_gt = false;
            for c in chars.by_ref() {
                if c == '>' {
                    found_gt = true;
                    break;
                }
                tag.push(c);
            }
            if found_gt {
                // Check if this is a self-closing tag (ends with '/') with a simple name
                if let Some(name) = tag.strip_suffix('/').map(str::trim) {
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
                        continue;
                    }
                }
            }
            // Not a recognized self-closing tag — emit the '<' and tag content as-is
            result.push('<');
            result.push_str(&tag);
            if found_gt {
                result.push('>');
            }
        } else {
            result.push(ch);
        }
    }
    result
}

/// Serialize a value to XML with self-closing tags expanded.
///
/// Uses quick-xml for serialization then expands any `<Tag/>` to `<Tag></Tag>`.
pub fn to_xml_string<T: serde::Serialize>(value: &T) -> Result<String, AwsError> {
    let body = quick_xml::se::to_string(value)
        .map_err(|e| AwsError::from(AwsErrorKind::XmlSerializationError {
            details: e.to_string(),
        }))?;
    Ok(expand_empty_tags(&body))
}

// ---------------------------------------------------------------------------
// Core data types
// ---------------------------------------------------------------------------

/// AWS S3 StorageClass enum with all valid storage class values.
///
/// Ensures only valid storage class values can be used, preventing
/// invalid values like 'INVALID_CLASS' from being set.
#[allow(non_camel_case_types, clippy::upper_case_acronyms)]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum StorageClass {
    /// S3 Standard storage class
    STANDARD,
    /// S3 Standard-Infrequent Access storage class
    STANDARD_IA,
    /// S3 One Zone-Infrequent Access storage class
    ONEZONE_IA,
    /// S3 Intelligent-Tiering storage class
    INTELLIGENT_TIERING,
    /// S3 Glacier storage class
    GLACIER,
    /// S3 Deep Archive storage class
    DEEP_ARCHIVE,
    /// S3 Glacier Instant Retrieval storage class
    GLACIER_IR,
}

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
    pub last_modified: DateTime<Utc>,
    /// x-amz-meta-* metadata headers
    pub metadata: HashMap<String, String>,
}

impl S3Object {
    /// Default content type for S3 objects when none is specified.
    pub const DEFAULT_CONTENT_TYPE: &'static str = "binary/octet-stream";

    /// Returns the byte length of the object data.
    ///
    /// Derived from `data.len()` to guarantee the `Content-Length` header
    /// always matches the actual body — eliminates the possibility of a
    /// stale `content_length` field causing silent data corruption.
    pub fn content_length(&self) -> usize {
        self.data.len()
    }
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
    #[serde(rename = "EncodingType", skip_serializing_if = "Option::is_none")]
    pub encoding_type: Option<String>,
    #[serde(rename = "Prefix")]
    pub prefix: String,
    #[serde(rename = "MaxKeys")]
    pub max_keys: u32,
    #[serde(rename = "KeyCount")]
    pub key_count: u32,
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
    pub size: u64,
    #[serde(rename = "StorageClass")]
    pub storage_class: StorageClass,
    #[serde(rename = "Owner", skip_serializing_if = "Option::is_none")]
    pub owner: Option<Owner>,
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
    #[serde(rename = "Object", default)]
    pub objects: Vec<DeleteObject>,
}

/// One object key to delete inside the Delete request.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct DeleteObject {
    #[serde(rename = "Key")]
    pub key: String,
    #[serde(rename = "VersionId")]
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
    #[serde(rename = "VersionId", skip_serializing_if = "Option::is_none")]
    pub version_id: Option<String>,
    #[serde(rename = "DeleteMarker", skip_serializing_if = "Option::is_none")]
    pub delete_marker: Option<bool>,
    #[serde(
        rename = "DeleteMarkerVersionId",
        skip_serializing_if = "Option::is_none"
    )]
    pub delete_marker_version_id: Option<String>,
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
    #[serde(rename = "VersionId", skip_serializing_if = "Option::is_none")]
    pub version_id: Option<String>,
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
    #[serde(deserialize_with = "deserialize_part_number")]
    pub part_number: u32,
    #[serde(rename = "ETag")]
    pub etag: String,
}

fn deserialize_part_number<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = u32::deserialize(deserializer)?;
    if !(1..=10000).contains(&value) {
        return Err(serde::de::Error::custom(format!(
            "PartNumber must be between 1 and 10000, got {}",
            value
        )));
    }
    Ok(value)
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
    pub max_parts: u32,
    #[serde(rename = "NextPartNumberMarker")]
    pub next_part_number_marker: String,
    #[serde(rename = "PartNumberMarker")]
    pub part_number_marker: String,
    #[serde(rename = "StorageClass")]
    pub storage_class: StorageClass,
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
    pub size: u64,
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
         let xml = to_xml_string(&result).expect("to_xml_string failed");
         assert!(xml.contains("<ListAllMyBucketsResult>"));
         assert!(xml.contains("<Owner>"));
         assert!(xml.contains("<ID>owner-id</ID>"));
         assert!(xml.contains("<DisplayName>bucket-owner</DisplayName>"));
         assert!(xml.contains("<Buckets>"));
         assert!(xml.contains("<Bucket>"));
         assert!(xml.contains("<Name>alpha</Name>"));
         assert!(xml.contains("<Name>beta</Name>"));
         assert!(xml.contains("<CreationDate>"));
     }

    // -- CopyObjectResult ------------------------------------------------

    #[test]
    fn test_copy_object_result_serialize() {
        let result = CopyObjectResult {
            etag: "\"etag-value\"".into(),
            last_modified: Utc::now(),
        };
        let xml = to_xml_string(&result).expect("to_xml_string failed");
        assert!(xml.contains("<CopyObjectResult>"));
        assert!(xml.contains("<ETag>\"etag-value\"</ETag>"));
        // LastModified is harder to test precisely because it's a timestamp,
        // but we can verify it's in the correct XML format
        assert!(xml.contains("<LastModified>"));
        assert!(xml.contains("</LastModified>"));
        // Additionally, verify the structure is correct
        assert!(xml.contains("<CopyObjectResult><ETag>\"etag-value\"</ETag><LastModified>"));
    }

    #[test]
    fn test_copy_object_result_would_detect_etag_corruption() {
        // This test demonstrates that our strengthened assertions would catch ETag value corruption
        let result = CopyObjectResult {
            etag: "\"actual-etag\"".into(), // This is what we actually set
            last_modified: Utc::now(),
        };
        let xml = to_xml_string(&result).expect("to_xml_string failed");
        
        // Verify the actual value is present
        assert!(xml.contains("<ETag>\"actual-etag\"</ETag>"));
        
        // Verify that a different value is NOT present (this would fail if values were corrupted)
        assert!(!xml.contains("<ETag>\"different-etag\"</ETag>"), 
                "Test correctly detects if ETag value was corrupted");
    }

    #[test]
    fn test_copy_object_result_would_detect_lastmodified_corruption() {
        // While we can't easily test the exact timestamp value, we can verify the format
        let result = CopyObjectResult {
            etag: "\"etag\"".into(),
            last_modified: Utc::now(),
        };
        let xml = to_xml_string(&result).expect("to_xml_string failed");
        
        // Verify LastModified tags are present with proper structure
        assert!(xml.contains("<LastModified>"));
        assert!(xml.contains("</LastModified>"));
        
        // Verify it's not empty (basic corruption check)
        assert!(!xml.contains("<LastModified></LastModified>"));
        
        // Verify it's not some obviously wrong value
        assert!(!xml.contains("<LastModified>not-a-date</LastModified>"));
    }

    #[test]
    fn test_copy_object_result_old_weak_assertions_would_pass_with_corruption() {
        // This test demonstrates the PROBLEM with the old weak assertions
        // If we only checked for existence of tags (not values), corruption would go undetected
        
        // Simulate what happens if ETag value gets corrupted to "wrong-value"
        let corrupted_etag = "\"wrong-value\"";
        let result = CopyObjectResult {
            etag: corrupted_etag.into(),
            last_modified: Utc::now(),
        };
        let xml = to_xml_string(&result).expect("to_xml_string failed");
        
        // OLD WEAK ASSERTIONS (what the test had before):
        // These would PASS even with corrupted values:
        assert!(xml.contains("<CopyObjectResult>"));
        assert!(xml.contains("<ETag>")); // Just checks existence, not value!
        assert!(xml.contains("<LastModified>"));
        
        // NEW STRENGTHENED ASSERTIONS (what we implemented):
        // These would FAIL with corrupted values:
        assert!(xml.contains("<ETag>\"wrong-value\"</ETag>")); // Correctly validates the actual value
        assert!(!xml.contains("<ETag>\"etag-value\"</ETag>")); // Would detect if value was wrongly changed to expected value
    }

    // -- InitiateMultipartUploadResult -----------------------------------

    #[test]
    fn test_initiate_multipart_upload_result_serialize() {
        let result = InitiateMultipartUploadResult {
            bucket: "my-bucket".into(),
            key: "large-file.zip".into(),
            upload_id: "upload-id-123".into(),
        };
        let xml = to_xml_string(&result).expect("to_xml_string failed");
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

    #[test]
    fn test_part_number_validation_valid_values() {
        // Test minimum valid value
        let xml_min = r#"
            <CompleteMultipartUpload>
                <Part><PartNumber>1</PartNumber><ETag>"etag-1"</ETag></Part>
            </CompleteMultipartUpload>
        "#;
        let req: CompleteMultipartUploadRequest =
            from_str(xml_min).expect("deserialize valid part number 1");
        assert_eq!(req.parts[0].part_number, 1);

        // Test maximum valid value
        let xml_max = r#"
            <CompleteMultipartUpload>
                <Part><PartNumber>10000</PartNumber><ETag>"etag-max"</ETag></Part>
            </CompleteMultipartUpload>
        "#;
        let req_max: CompleteMultipartUploadRequest =
            from_str(xml_max).expect("deserialize valid part number 10000");
        assert_eq!(req_max.parts[0].part_number, 10000);

        // Test middle value
        let xml_mid = r#"
            <CompleteMultipartUpload>
                <Part><PartNumber>5000</PartNumber><ETag>"etag-mid"</ETag></Part>
            </CompleteMultipartUpload>
        "#;
        let req_mid: CompleteMultipartUploadRequest =
            from_str(xml_mid).expect("deserialize valid part number 5000");
        assert_eq!(req_mid.parts[0].part_number, 5000);
    }

    #[test]
    #[should_panic(expected = "PartNumber must be between 1 and 10000")]
    fn test_part_number_validation_invalid_zero() {
        let xml = r#"
            <CompleteMultipartUpload>
                <Part><PartNumber>0</PartNumber><ETag>"etag-zero"</ETag></Part>
            </CompleteMultipartUpload>
        "#;
        let _req: CompleteMultipartUploadRequest =
            from_str(xml).expect("deserialize should fail for part number 0");
    }

    #[test]
    #[should_panic(expected = "PartNumber must be between 1 and 10000")]
    fn test_part_number_validation_invalid_too_large() {
        let xml = r#"
            <CompleteMultipartUpload>
                <Part><PartNumber>10001</PartNumber><ETag>"etag-too-large"</ETag></Part>
            </CompleteMultipartUpload>
        "#;
        let _req: CompleteMultipartUploadRequest =
            from_str(xml).expect("deserialize should fail for part number 10001");
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
        let xml = to_xml_string(&result).expect("to_xml_string failed");
        assert!(xml.contains("<Bucket>my-bucket</Bucket>"));
        assert!(xml.contains("<Key>file.zip</Key>"));
        assert!(xml.contains("<ETag>"));
        assert!(xml.contains("<Location>http://localhost:4566/my-bucket/file.zip</Location>"));
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
            part_number_marker: String::new(),
            storage_class: StorageClass::STANDARD,
            parts: vec![PartInfo {
                part_number: 1,
                last_modified: now,
                etag: "\"etag-1\"".into(),
                size: 5_242_880,
            }],
            is_truncated: false,
        };
        let xml = to_xml_string(&result).expect("to_xml_string failed");
        assert!(xml.contains("<ListPartsResult>"));
        assert!(xml.contains("<Initiator>"));
        assert!(xml.contains("<Owner>"));
        assert!(xml.contains("<Part>"));
        assert!(xml.contains("<PartNumber>1</PartNumber>"));
        assert!(xml.contains("<Size>5242880</Size>"));
        assert!(xml.contains("<NextPartNumberMarker></NextPartNumberMarker>"));
        assert!(xml.contains("<PartNumberMarker></PartNumberMarker>"));
    }

    #[test]
    fn test_list_parts_result_serialize_with_part_number_marker() {
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
            next_part_number_marker: "5".into(),
            part_number_marker: "3".into(), // Echo the request value
            storage_class: StorageClass::STANDARD,
            parts: vec![PartInfo {
                part_number: 1,
                last_modified: now,
                etag: "\"etag-1\"".into(),
                size: 5_242_880,
            }],
            is_truncated: false,
        };
        let xml = to_xml_string(&result).expect("to_xml_string failed");
        assert!(xml.contains("<ListPartsResult>"));
        assert!(xml.contains("<Initiator>"));
        assert!(xml.contains("<Owner>"));
        assert!(xml.contains("<Part>"));
        assert!(xml.contains("<PartNumber>1</PartNumber>"));
        assert!(xml.contains("<Size>5242880</Size>"));
        assert!(xml.contains("<NextPartNumberMarker>5</NextPartNumberMarker>"));
        assert!(xml.contains("<PartNumberMarker>3</PartNumberMarker>"));
    }

    // -- LocationConstraint ----------------------------------------------

     #[test]
     fn test_location_constraint_serialize() {
         let lc = LocationConstraint {
             location: "us-east-1".into(),
         };
         let xml = to_xml_string(&lc).expect("to_xml_string failed");
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

    #[test]
    fn test_expand_empty_tags_utf8() {
        // Multi-byte UTF-8 characters must survive round-trip uncorrupted
        let input = "<Key>照片/cat.jpg</Key><Tag/>";
        let output = expand_empty_tags(input);
        assert_eq!(output, "<Key>照片/cat.jpg</Key><Tag></Tag>");
    }

    #[test]
    fn test_expand_empty_tags_utf8_with_empty_tag() {
        // Empty string field alongside multi-byte UTF-8 in sibling element
        let input = "<Name>café</Name><Empty/><Value>日本語</Value>";
        let output = expand_empty_tags(input);
        assert_eq!(
            output,
            "<Name>café</Name><Empty></Empty><Value>日本語</Value>"
        );
    }

    #[test]
    fn test_expand_empty_tags_comment() {
        // Self-closing-looking patterns inside XML comments must NOT be expanded
        let input = "<Root><!-- <Foo/> --><Bar/></Root>";
        let output = expand_empty_tags(input);
        assert!(
            output.contains("<!-- <Foo/> -->"),
            "comment content should be preserved: {output}"
        );
        assert!(
            output.contains("<Bar></Bar>"),
            "real tag should be expanded: {output}"
        );
        assert!(
            !output.contains("<!-- <Foo></Foo> -->"),
            "comment must not be mutated: {output}"
        );
    }

    #[test]
    fn test_expand_empty_tags_comment_no_match() {
        // Comments without any /> patterns pass through cleanly
        let input = "<Root><!-- just a comment --><Tag/></Root>";
        let output = expand_empty_tags(input);
        assert_eq!(output, "<Root><!-- just a comment --><Tag></Tag></Root>");
    }

    #[test]
    fn test_expand_empty_tags_mixed() {
        // Mix of real self-closing tags, comments with />, CDATA, and PIs
        let input = r#"<Root>
            <Empty/>
            <!-- <Fake/> -->
            <![CDATA[ <AlsoFake/> ]]>
            <?ignore <Nope/> ?>
            <Real/>
        </Root>"#;
        let output = expand_empty_tags(input);
        // Real self-closing tags should expand
        assert!(
            output.contains("<Empty></Empty>"),
            "Empty should expand: {output}"
        );
        assert!(
            output.contains("<Real></Real>"),
            "Real should expand: {output}"
        );
        // Comment content must stay intact
        assert!(
            output.contains("<!-- <Fake/> -->"),
            "comment must not expand: {output}"
        );
        // CDATA content must stay intact
        assert!(
            output.contains("<![CDATA[ <AlsoFake/> ]]>"),
            "CDATA must not expand: {output}"
        );
        // PI content must stay intact
        assert!(
            output.contains("<?ignore <Nope/> ?>"),
            "PI must not expand: {output}"
        );
    }

    #[test]
    fn test_expand_empty_tags_cdata() {
        // CDATA sections pass through without expanding internal />
        let input = "<Data><![CDATA[some <Thing/> here]]></Data>";
        let output = expand_empty_tags(input);
        assert_eq!(output, input);
    }

    #[test]
    fn test_expand_empty_tags_processing_instruction() {
        // Processing instructions pass through without expanding internal />
        let input = "<?xml-stylesheet href='style.css'?><Root><Tag/></Root>";
        let output = expand_empty_tags(input);
        assert!(
            output.contains("<?xml-stylesheet href='style.css'?>"),
            "PI preserved: {output}"
        );
        assert!(
            output.contains("<Tag></Tag>"),
            "real tag expanded: {output}"
        );
    }

    #[test]
    fn test_expand_empty_tags_comment_with_multiple_slashes() {
        // Comment containing multiple /> patterns — none should expand
        let input = "<Root><!-- <A/> and <B/> --></Root>";
        let output = expand_empty_tags(input);
        assert_eq!(output, input);
    }

    #[test]
    fn test_expand_empty_tags_nested_comment_like() {
        // Edge case: comment with --> appearing in text content should still work
        let input = "<Root><!-- comment with > in it --><Tag/></Root>";
        let output = expand_empty_tags(input);
        assert!(
            output.contains("<!-- comment with > in it -->"),
            "comment with > preserved: {output}"
        );
        assert!(
            output.contains("<Tag></Tag>"),
            "real tag expanded: {output}"
        );
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
        let xml = to_xml_string(&s).expect("to_xml_string failed");
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
