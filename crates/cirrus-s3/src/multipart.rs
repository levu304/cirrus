// S3 multipart upload support.

use sha2::{Digest, Sha256};
use base64::Engine;

/// Generate an S3-compatible multipart upload ID.
///
/// Uses SHA-256 of `bucket:key:timestamp:random` encoded as base64url
/// (no padding) for a compact, unique, hard-to-guess upload identifier.
pub fn generate_upload_id(bucket: &str, key: &str) -> String {
    let timestamp = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
    let random = uuid::Uuid::new_v4();
    let input = format!("{}:{}:{}:{}", bucket, key, timestamp, random);
    let hash = Sha256::digest(input.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash)
}
