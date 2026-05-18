// S3 storage backend.
//
// This module defines the storage layer for the Cirrus S3 server:
// data structures, a Storage trait with 16 methods, and an in-memory
// implementation (DefaultStorage) backed by DashMap + AtomicU64.
//
// Anti-Patterns (MUST FOLLOW):
//   AP-P2: No derive(Clone) on DashMap-containing structs.
//          Manual Clone via Arc::clone() for each field.
//   AP-P3: No RwLock<bool> delete_guard (causes ABBA deadlock).
//          Use DashMap::remove() for atomic isolation.
//   AP-P6: CopyObject does NOT increment total_bytes.
//          Bytes::clone() is O(1) Arc refcount bump.

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use md5::{Digest, Md5};
use uuid::Uuid;

use cirrus_protocol::types::{
    BucketInfo, CommonPrefixes, ObjectInfo, Part, PartInfo, S3Object, StorageClass,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of buckets allowed.
pub const CIRRUS_MAX_BUCKETS: u64 = 1000;

/// Maximum number of objects per bucket.
pub const CIRRUS_MAX_OBJECTS_PER_BUCKET: u64 = 100_000;

/// Maximum object size (5 GB for S3-compatible objects).
pub const CIRRUS_MAX_OBJECT_SIZE: u64 = 5 * 1024 * 1024 * 1024;

/// Maximum multipart upload count per bucket.
pub const CIRRUS_MAX_MULTIPART_UPLOADS: usize = 1000;

/// Maximum parts per upload.
pub const CIRRUS_MAX_PARTS_PER_UPLOAD: u32 = 10000;

/// Maximum total bytes across all buckets (10 TB).
pub const CIRRUS_MAX_TOTAL_BYTES: u64 = 10 * 1024 * 1024 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

/// An in-memory S3 bucket with objects and multipart uploads.
///
/// The inner maps are wrapped in `Arc` so that `Bucket` can implement `Clone`
/// (DashMap does not implement `Clone`). This is needed because the `Storage`
/// trait's `create_bucket` returns an owned `Bucket`.
#[derive(Debug)]
pub struct Bucket {
    pub name: String,
    pub creation_date: DateTime<Utc>,
    pub objects: Arc<DashMap<String, S3Object>>,
    pub multipart_uploads: Arc<DashMap<String, MultipartUpload>>,
}

impl Clone for Bucket {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            creation_date: self.creation_date,
            objects: Arc::clone(&self.objects),
            multipart_uploads: Arc::clone(&self.multipart_uploads),
        }
    }
}

/// An in-progress multipart upload.
#[derive(Debug)]
pub struct MultipartUpload {
    pub upload_id: String,
    pub key: String,
    pub initiated: DateTime<Utc>,
    pub parts: BTreeMap<u32, PartData>,
}

/// Data for one uploaded part.
#[derive(Debug)]
pub struct PartData {
    pub part_number: u32,
    pub data: Bytes,
    pub etag: String,
    pub last_modified: DateTime<Utc>,
    pub size: u64,
}

// ---------------------------------------------------------------------------
// Response types (storage-layer return values)
// ---------------------------------------------------------------------------

/// Result of listing objects in a bucket (ListObjectsV2).
#[derive(Debug)]
pub struct ObjectList {
    pub is_truncated: bool,
    pub next_continuation_token: String,
    pub contents: Vec<ObjectInfo>,
    pub common_prefixes: Vec<CommonPrefixes>,
    pub key_count: u32,
}

/// Result of getting a single object.
#[derive(Debug)]
pub struct GetObjectResult {
    pub object: S3Object,
}

/// Result of listing parts of a multipart upload.
#[derive(Debug)]
pub struct PartsList {
    pub parts: Vec<PartInfo>,
    pub is_truncated: bool,
    pub next_part_number_marker: String,
}

/// S3 errors returned by storage operations.
#[derive(Debug, Clone, thiserror::Error)]
pub enum S3Error {
    #[error("The specified bucket does not exist")]
    NoSuchBucket,

    #[error("The specified key does not exist")]
    NoSuchKey,

    #[error("The specified upload does not exist")]
    NoSuchUpload,

    #[error("The requested bucket already exists")]
    BucketAlreadyExists,

    #[error("The bucket you tried to delete is not empty")]
    BucketNotEmpty,

    #[error("One or more of the specified parts could not be found")]
    InvalidPart,

    #[error("The list of parts was not in ascending order")]
    InvalidPartOrder,

    #[error("Your proposed upload exceeds the maximum allowed object size")]
    EntityTooLarge,

    #[error("The maximum capacity has been exceeded")]
    MaxCapacityExceeded,
}

// ---------------------------------------------------------------------------
// Storage trait
// ---------------------------------------------------------------------------

/// Abstract S3 storage backend.
///
/// All 16 methods are async. Implementations must be `Send + Sync + Clone +
/// 'static` so they can be shared across Tokio tasks and wrapped in `Arc`.
#[async_trait]
pub trait Storage: Send + Sync + Clone + 'static {
    // -- Bucket operations ------------------------------------------------

    /// Create a new bucket. Returns the bucket on success.
    async fn create_bucket(&self, name: &str) -> Result<Bucket, S3Error>;

    /// Delete an empty bucket.
    async fn delete_bucket(&self, name: &str) -> Result<(), S3Error>;

    /// List all buckets.
    async fn list_buckets(&self) -> Result<Vec<BucketInfo>, S3Error>;

    /// Get the location (region) of a bucket.
    async fn get_bucket_location(&self, name: &str) -> Result<String, S3Error>;

    // -- Object operations ------------------------------------------------

    /// Store an object in a bucket.
    async fn put_object(
        &self,
        bucket: &str,
        key: &str,
        object: S3Object,
    ) -> Result<(), S3Error>;

    /// Retrieve an object from a bucket.
    async fn get_object(
        &self,
        bucket: &str,
        key: &str,
    ) -> Result<GetObjectResult, S3Error>;

    /// Return object metadata (same data as get_object for now).
    async fn head_object(
        &self,
        bucket: &str,
        key: &str,
    ) -> Result<GetObjectResult, S3Error>;

    /// Delete an object from a bucket.
    async fn delete_object(&self, bucket: &str, key: &str) -> Result<(), S3Error>;

    /// Copy an object from one location to another.
    ///
    /// **Anti-Pattern AP-P6:** Bytes::clone() is O(1) — do NOT increment
    /// `total_bytes` here.
    async fn copy_object(
        &self,
        src_bucket: &str,
        src_key: &str,
        dst_bucket: &str,
        dst_key: &str,
    ) -> Result<(), S3Error>;

    /// List objects in a bucket (ListObjectsV2) with prefix, delimiter,
    /// and continuation-token pagination.
    #[allow(clippy::too_many_arguments)]
    async fn list_objects_v2(
        &self,
        bucket: &str,
        prefix: &str,
        delimiter: &str,
        start_after: &str,
        max_keys: u32,
        continuation_token: &str,
    ) -> Result<ObjectList, S3Error>;

    // -- Multipart operations ---------------------------------------------

    /// Initiate a multipart upload. Returns the upload ID.
    async fn create_multipart_upload(
        &self,
        bucket: &str,
        key: &str,
    ) -> Result<String, S3Error>;

    /// Upload a part. Returns the ETag for the part.
    async fn upload_part(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
        part_number: u32,
        data: Bytes,
    ) -> Result<String, S3Error>;

    /// Complete a multipart upload. Returns the final object ETag.
    async fn complete_multipart_upload(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
        parts: &[Part],
    ) -> Result<String, S3Error>;

    /// Abort a multipart upload and discard all uploaded parts.
    async fn abort_multipart_upload(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
    ) -> Result<(), S3Error>;

    /// List parts of an in-progress multipart upload.
    async fn list_parts(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
        max_parts: u32,
        part_number_marker: u32,
    ) -> Result<PartsList, S3Error>;
}

// ---------------------------------------------------------------------------
// DefaultStorage — in-memory implementation
// ---------------------------------------------------------------------------

/// In-memory S3 storage backend.
///
/// All state lives behind `Arc` so the struct can be `Clone` even though
/// `DashMap` is not `Clone` (see AP-P2).
pub struct DefaultStorage {
    buckets: Arc<DashMap<String, Bucket>>,
    total_bytes: Arc<AtomicU64>,
}

impl Clone for DefaultStorage {
    fn clone(&self) -> Self {
        Self {
            buckets: Arc::clone(&self.buckets),
            total_bytes: Arc::clone(&self.total_bytes),
        }
    }
}

impl DefaultStorage {
    /// Create a new empty storage backend.
    pub fn new() -> Self {
        Self {
            buckets: Arc::new(DashMap::new()),
            total_bytes: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl Default for DefaultStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Storage for DefaultStorage {
    // -- Bucket operations ------------------------------------------------

    async fn create_bucket(&self, name: &str) -> Result<Bucket, S3Error> {
        if self.buckets.len() as u64 >= CIRRUS_MAX_BUCKETS {
            return Err(S3Error::MaxCapacityExceeded);
        }
        if self.buckets.contains_key(name) {
            return Err(S3Error::BucketAlreadyExists);
        }

        let bucket = Bucket {
            name: name.to_string(),
            creation_date: Utc::now(),
            objects: Arc::new(DashMap::new()),
            multipart_uploads: Arc::new(DashMap::new()),
        };

        // AP-P2: Insert and return a clone (both share the same Arc-wrapped
        // DashMaps, so modifications through one are visible to the other).
        self.buckets
            .insert(name.to_string(), bucket.clone());
        Ok(bucket)
    }

    async fn delete_bucket(&self, name: &str) -> Result<(), S3Error> {
        // AP-P3: Atomically isolate the bucket via remove(), then check
        // emptiness. This avoids ABBA deadlock from RwLock<bool> guards.
        let (_, bucket) = self
            .buckets
            .remove(name)
            .ok_or(S3Error::NoSuchBucket)?;

        if !bucket.objects.is_empty() || !bucket.multipart_uploads.is_empty() {
            // Re-insert so the bucket isn't silently lost. Use entry().or_insert()
            // to avoid overwriting a bucket a concurrent create_bucket inserted
            // between our remove() and this re-insert (cirrus-5os).
            self.buckets
                .entry(name.to_string())
                .or_insert(bucket);
            return Err(S3Error::BucketNotEmpty);
        }

        Ok(())
    }

    async fn list_buckets(&self) -> Result<Vec<BucketInfo>, S3Error> {
        let mut infos: Vec<BucketInfo> = self
            .buckets
            .iter()
            .map(|entry| BucketInfo {
                name: entry.name.clone(),
                creation_date: entry.creation_date,
            })
            .collect();
        infos.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(infos)
    }

    async fn get_bucket_location(&self, _name: &str) -> Result<String, S3Error> {
        // S3-compatible default region.
        Ok("us-east-1".to_string())
    }

    // -- Object operations ------------------------------------------------

    async fn put_object(
        &self,
        bucket: &str,
        key: &str,
        object: S3Object,
    ) -> Result<(), S3Error> {
        let bkt = self
            .buckets
            .get(bucket)
            .ok_or(S3Error::NoSuchBucket)?;

        if bkt.objects.len() as u64 >= CIRRUS_MAX_OBJECTS_PER_BUCKET {
            return Err(S3Error::MaxCapacityExceeded);
        }

        let obj_size = object.content_length() as u64;
        if obj_size > CIRRUS_MAX_OBJECT_SIZE {
            return Err(S3Error::EntityTooLarge);
        }

        let current_total = self.total_bytes.load(Ordering::Relaxed);
        if current_total.saturating_add(obj_size) > CIRRUS_MAX_TOTAL_BYTES {
            return Err(S3Error::MaxCapacityExceeded);
        }

        // AP-P6: Increment total_bytes — this is new memory.
        self.total_bytes
            .fetch_add(obj_size, Ordering::Relaxed);

        bkt.objects.insert(key.to_string(), object);
        Ok(())
    }

    async fn get_object(
        &self,
        bucket: &str,
        key: &str,
    ) -> Result<GetObjectResult, S3Error> {
        let bkt = self
            .buckets
            .get(bucket)
            .ok_or(S3Error::NoSuchBucket)?;

        let object = bkt
            .objects
            .get(key)
            .ok_or(S3Error::NoSuchKey)?
            .clone();

        Ok(GetObjectResult { object })
    }

    async fn head_object(
        &self,
        bucket: &str,
        key: &str,
    ) -> Result<GetObjectResult, S3Error> {
        // Semantically identical to get_object for the in-memory backend.
        self.get_object(bucket, key).await
    }

    async fn delete_object(
        &self,
        bucket: &str,
        key: &str,
    ) -> Result<(), S3Error> {
        let bkt = self
            .buckets
            .get(bucket)
            .ok_or(S3Error::NoSuchBucket)?;

        let (_, obj) = bkt
            .objects
            .remove(key)
            .ok_or(S3Error::NoSuchKey)?;

        self.total_bytes
            .fetch_sub(obj.content_length() as u64, Ordering::Relaxed);
        Ok(())
    }

    async fn copy_object(
        &self,
        src_bucket: &str,
        src_key: &str,
        dst_bucket: &str,
        dst_key: &str,
    ) -> Result<(), S3Error> {
        // Scope the source-bucket borrow so we drop the Ref before
        // acquiring the destination-bucket Ref (they may be the same bucket).
        let new_obj = {
            let src_bkt = self
                .buckets
                .get(src_bucket)
                .ok_or(S3Error::NoSuchBucket)?;
            let src_obj = src_bkt
                .objects
                .get(src_key)
                .ok_or(S3Error::NoSuchKey)?;
            let mut obj = src_obj.clone();
            obj.last_modified = Utc::now();
            obj
        }; // src_obj and src_bkt dropped here

        let dst_bkt = self
            .buckets
            .get(dst_bucket)
            .ok_or(S3Error::NoSuchBucket)?;

        // AP-P6: Bytes::clone() is O(1) — do NOT increment total_bytes.
        dst_bkt
            .objects
            .insert(dst_key.to_string(), new_obj);
        Ok(())
    }

    async fn list_objects_v2(
        &self,
        bucket: &str,
        prefix: &str,
        delimiter: &str,
        start_after: &str,
        max_keys: u32,
        continuation_token: &str,
    ) -> Result<ObjectList, S3Error> {
        let bkt = self
            .buckets
            .get(bucket)
            .ok_or(S3Error::NoSuchBucket)?;

        // 1. Collect and sort all keys matching the prefix.
        let mut all_keys: Vec<String> = bkt
            .objects
            .iter()
            .map(|entry| entry.key().clone())
            .filter(|k| k.starts_with(prefix))
            .collect();
        all_keys.sort();

        // 2. Determine skip point: continuation_token takes precedence.
        let skip = if !continuation_token.is_empty() {
            continuation_token
        } else {
            start_after
        };

        // 3. Apply skip.
        let keys: Vec<String> = if skip.is_empty() {
            all_keys
        } else {
            let skip_owned = skip.to_string();
            all_keys
                .into_iter()
                .skip_while(|k| k.as_str() <= skip_owned.as_str())
                .collect()
        };

        let max_keys = max_keys.max(1);

        // 4. Build full deduplicated result list, tracking which original
        //    keys are consumed by each entry (for continuation token).
        enum ListEntry {
            Object(String),
            Prefix(String),
        }

        struct EntryGroup {
            entry: ListEntry,
            consumed: Vec<String>,
        }

        let mut groups: Vec<EntryGroup> = Vec::new();
        let mut seen_prefixes: HashSet<String> = HashSet::new();

        for key in &keys {
            // Check if this key falls under an already-emitted common prefix.
            let mut covered = false;
            for p in &seen_prefixes {
                if key.starts_with(p.as_str()) {
                    if let Some(last) = groups.last_mut() {
                        last.consumed.push(key.clone());
                    }
                    covered = true;
                    break;
                }
            }
            if covered {
                continue;
            }

            // Check for delimiter grouping.
            if !delimiter.is_empty() {
                let remaining = key.get(prefix.len()..).unwrap_or("");
                if let Some(pos) = remaining.find(delimiter) {
                    let cp = format!("{}{}", prefix, &remaining[..=pos]);
                    seen_prefixes.insert(cp.clone());
                    groups.push(EntryGroup {
                        entry: ListEntry::Prefix(cp),
                        consumed: vec![key.clone()],
                    });
                    continue;
                }
            }

            // Direct object entry.
            groups.push(EntryGroup {
                entry: ListEntry::Object(key.clone()),
                consumed: vec![key.clone()],
            });
        }

        // 5. Slice to max_keys.
        let is_truncated = groups.len() > max_keys as usize;
        let page: Vec<EntryGroup> = groups.into_iter().take(max_keys as usize).collect();

        // 6. Build continuation token from the last consumed original key.
        let next_continuation_token = if is_truncated {
            page.last()
                .map(|g| g.consumed.last().cloned().unwrap_or_default())
                .unwrap_or_default()
        } else {
            String::new()
        };

        // 7. Build ObjectList response.
        let mut contents: Vec<ObjectInfo> = Vec::new();
        let mut common_prefixes: Vec<CommonPrefixes> = Vec::new();

        for group in &page {
            match &group.entry {
                ListEntry::Prefix(cp) => {
                    common_prefixes.push(CommonPrefixes {
                        prefix: cp.clone(),
                    });
                }
                ListEntry::Object(obj_key) => {
                    if let Some(obj) = bkt.objects.get(obj_key.as_str()) {
                        contents.push(ObjectInfo {
                            key: obj_key.clone(),
                            last_modified: obj.last_modified,
                            etag: obj.etag.clone(),
                            size: obj.content_length() as u64,
                            storage_class: StorageClass::STANDARD,
                            owner: None,
                        });
                    }
                }
            }
        }

        let key_count = (contents.len() + common_prefixes.len()) as u32;

        Ok(ObjectList {
            is_truncated,
            next_continuation_token,
            contents,
            common_prefixes,
            key_count,
        })
    }

    // -- Multipart operations ---------------------------------------------

    async fn create_multipart_upload(
        &self,
        bucket: &str,
        key: &str,
    ) -> Result<String, S3Error> {
        let bkt = self
            .buckets
            .get(bucket)
            .ok_or(S3Error::NoSuchBucket)?;

        if bkt.multipart_uploads.len() >= CIRRUS_MAX_MULTIPART_UPLOADS {
            return Err(S3Error::MaxCapacityExceeded);
        }

        let upload_id = Uuid::new_v4().to_string();
        let upload = MultipartUpload {
            upload_id: upload_id.clone(),
            key: key.to_string(),
            initiated: Utc::now(),
            parts: BTreeMap::new(),
        };

        bkt.multipart_uploads
            .insert(upload_id.clone(), upload);
        Ok(upload_id)
    }

    async fn upload_part(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
        part_number: u32,
        data: Bytes,
    ) -> Result<String, S3Error> {
        // PartNumber must be 1..=10000 (S3 spec).
        if !(1..=CIRRUS_MAX_PARTS_PER_UPLOAD).contains(&part_number) {
            return Err(S3Error::InvalidPart);
        }

        let bkt = self
            .buckets
            .get(bucket)
            .ok_or(S3Error::NoSuchBucket)?;

        let mut upload = bkt
            .multipart_uploads
            .get_mut(upload_id)
            .ok_or(S3Error::NoSuchUpload)?;

        if upload.key != key {
            return Err(S3Error::NoSuchUpload);
        }

        if upload.parts.len() as u32 >= CIRRUS_MAX_PARTS_PER_UPLOAD {
            return Err(S3Error::MaxCapacityExceeded);
        }

        // Generate ETag (MD5 of part data).
        let hash = Md5::digest(&data);
        let etag = format!("\"{:x}\"", hash);

        let part_size = data.len() as u64;

        // AP-P6: Increment total_bytes — this is new memory.
        self.total_bytes
            .fetch_add(part_size, Ordering::Relaxed);

        let part_data = PartData {
            part_number,
            data,
            etag: etag.clone(),
            last_modified: Utc::now(),
            size: part_size,
        };

        upload.parts.insert(part_number, part_data);
        Ok(etag)
    }

    async fn complete_multipart_upload(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
        parts: &[Part],
    ) -> Result<String, S3Error> {
        if parts.is_empty() {
            return Err(S3Error::InvalidPart);
        }

        let bkt = self
            .buckets
            .get(bucket)
            .ok_or(S3Error::NoSuchBucket)?;

        // Validate parts and collect data while holding the RefMut.
        // We drop it before any expensive operations (concatenation, hashing).
        let all_parts: Vec<Bytes> = {
            let upload = bkt
                .multipart_uploads
                .get_mut(upload_id)
                .ok_or(S3Error::NoSuchUpload)?;

            if upload.key != key {
                return Err(S3Error::NoSuchUpload);
            }

            // Validate: parts must be in strictly increasing order, and each
            // must exist with a matching ETag.
            let mut prev_part_number: u32 = 0;
            let mut parts_data: Vec<Bytes> = Vec::with_capacity(parts.len());
            for part in parts {
                if part.part_number <= prev_part_number {
                    return Err(S3Error::InvalidPartOrder);
                }
                let stored = upload
                    .parts
                    .get(&part.part_number)
                    .ok_or(S3Error::InvalidPart)?;
                if stored.etag != part.etag {
                    return Err(S3Error::InvalidPart);
                }
                // Clone is O(1): Bytes is Arc<[u8]>.
                parts_data.push(stored.data.clone());
                prev_part_number = part.part_number;
            }

            // Drop the RefMut before returning, so the lock is not held during
            // concatenation, hashing, or object creation.
            drop(upload);
            parts_data
        };

        // RefMut is now released. No lock contention for other operations
        // on the same upload_id.

        // Concatenate all part data in order.
        let mut all_data: Vec<u8> = Vec::new();
        for part_data in &all_parts {
            all_data.extend_from_slice(part_data);
        }

        // Compute final ETag: MD5 of concatenated data with "-N" suffix.
        let hash = Md5::digest(&all_data);
        let final_etag = format!("\"{:x}-{}\"", hash, parts.len());

        let completed_object = S3Object {
            data: Bytes::from(all_data),
            etag: final_etag.clone(),
            content_type: "binary/octet-stream".to_string(),
            last_modified: Utc::now(),
            metadata: std::collections::HashMap::new(),
        };

        bkt.multipart_uploads.remove(upload_id);

        // AP-P6: Do NOT increment total_bytes here. The individual parts'
        // bytes were already counted during UploadPart. The concatenated
        // object replaces the part data (same total size).
        bkt.objects.insert(key.to_string(), completed_object);

        Ok(final_etag)
    }

    async fn abort_multipart_upload(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
    ) -> Result<(), S3Error> {
        let bkt = self
            .buckets
            .get(bucket)
            .ok_or(S3Error::NoSuchBucket)?;

        let upload = bkt
            .multipart_uploads
            .get_mut(upload_id)
            .ok_or(S3Error::NoSuchUpload)?;

        if upload.key != key {
            return Err(S3Error::NoSuchUpload);
        }

        let total_part_size: u64 = upload.parts.values().map(|p| p.size).sum();

        drop(upload);
        bkt.multipart_uploads.remove(upload_id);

        // Decrement total_bytes — the part data is being freed.
        self.total_bytes
            .fetch_sub(total_part_size, Ordering::Relaxed);

        Ok(())
    }

    async fn list_parts(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
        max_parts: u32,
        part_number_marker: u32,
    ) -> Result<PartsList, S3Error> {
        let bkt = self
            .buckets
            .get(bucket)
            .ok_or(S3Error::NoSuchBucket)?;

        let upload = bkt
            .multipart_uploads
            .get(upload_id)
            .ok_or(S3Error::NoSuchUpload)?;

        if upload.key != key {
            return Err(S3Error::NoSuchUpload);
        }

        // Collect all parts with part_number > marker.
        let all_matching: Vec<(u32, &PartData)> = upload
            .parts
            .iter()
            .filter(|item| *item.0 > part_number_marker)
            .map(|item| (*item.0, item.1))
            .collect();

        let total = all_matching.len();
        let take = (max_parts as usize).min(total);
        let is_truncated = take < total;

        let part_infos: Vec<PartInfo> = all_matching[..take]
            .iter()
                .map(|(pn, p)| PartInfo {
                part_number: *pn,
                last_modified: p.last_modified,
                etag: p.etag.clone(),
                size: p.size,
            })
            .collect();

        let next_part_number_marker = if is_truncated && !part_infos.is_empty() {
            part_infos
                .last()
                .unwrap()
                .part_number
                .to_string()
        } else {
            String::new()
        };

        Ok(PartsList {
            parts: part_infos,
            is_truncated,
            next_part_number_marker,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use cirrus_protocol::types::S3Object;
    use std::collections::HashMap;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;

    // -- Helper: create a test DefaultStorage with one bucket ------------

    fn test_storage() -> DefaultStorage {
        DefaultStorage::new()
    }

    fn test_s3_object(data: &[u8]) -> S3Object {
        let hash = Md5::digest(data);
        S3Object {
            data: Bytes::copy_from_slice(data),
            etag: format!("\"{:x}\"", hash),
            content_type: "binary/octet-stream".to_string(),
            last_modified: Utc::now(),
            metadata: HashMap::new(),
        }
    }

    async fn with_bucket(
        storage: &DefaultStorage,
        name: &str,
    ) -> Bucket {
        storage
            .create_bucket(name)
            .await
            .expect("create_bucket should succeed")
    }

    // -- Bucket operation tests ------------------------------------------

    #[tokio::test]
    async fn test_create_bucket() {
        let storage = test_storage();
        let bucket = with_bucket(&storage, "test-bucket").await;
        assert_eq!(bucket.name, "test-bucket");
        assert!(storage.buckets.contains_key("test-bucket"));
    }

    #[tokio::test]
    async fn test_create_bucket_duplicate() {
        let storage = test_storage();
        with_bucket(&storage, "dup-bucket").await;
        let err = storage.create_bucket("dup-bucket").await.unwrap_err();
        assert!(matches!(err, S3Error::BucketAlreadyExists));
    }

    #[tokio::test]
    async fn test_create_bucket_max_capacity() {
        let storage = test_storage();
        // Fill up to the limit.
        for i in 0..CIRRUS_MAX_BUCKETS {
            let name = format!("bucket-{}", i);
            if storage.buckets.contains_key(&name) {
                continue;
            }
            storage.create_bucket(&name).await.unwrap();
        }
        // One more should fail.
        let err = storage
            .create_bucket("overflow")
            .await
            .unwrap_err();
        assert!(matches!(err, S3Error::MaxCapacityExceeded));
    }

    #[tokio::test]
    async fn test_delete_bucket() {
        let storage = test_storage();
        with_bucket(&storage, "del-bucket").await;
        storage
            .delete_bucket("del-bucket")
            .await
            .expect("delete_bucket should succeed");
        assert!(!storage.buckets.contains_key("del-bucket"));
    }

    #[tokio::test]
    async fn test_delete_bucket_not_empty() {
        let storage = test_storage();
        let bucket = with_bucket(&storage, "nonempty").await;
        let obj = test_s3_object(b"hello");
        bucket.objects.insert("some-key".to_string(), obj);
        let err = storage
            .delete_bucket("nonempty")
            .await
            .unwrap_err();
        assert!(matches!(err, S3Error::BucketNotEmpty));
    }

    #[tokio::test]
    async fn test_delete_bucket_nonexistent() {
        let storage = test_storage();
        let err = storage
            .delete_bucket("nope")
            .await
            .unwrap_err();
        assert!(matches!(err, S3Error::NoSuchBucket));
    }

    #[tokio::test]
    async fn test_list_buckets() {
        let storage = test_storage();
        let names = ["alpha", "beta", "gamma"];
        for name in &names {
            with_bucket(&storage, name).await;
        }
        let buckets = storage
            .list_buckets()
            .await
            .expect("list_buckets should succeed");
        let mut got: Vec<&str> = buckets.iter().map(|b| b.name.as_str()).collect();
        got.sort();
        assert_eq!(got, vec!["alpha", "beta", "gamma"]);
    }

    #[tokio::test]
    async fn test_get_bucket_location() {
        let storage = test_storage();
        let loc = storage
            .get_bucket_location("any-bucket")
            .await
            .expect("get_bucket_location should succeed");
        assert_eq!(loc, "us-east-1");
    }

    // -- Object operation tests ------------------------------------------

    #[tokio::test]
    async fn test_put_and_get_object() {
        let storage = test_storage();
        with_bucket(&storage, "obj-bucket").await;
        let obj = test_s3_object(b"hello world");
        storage
            .put_object("obj-bucket", "hello.txt", obj.clone())
            .await
            .expect("put_object should succeed");
        let result = storage
            .get_object("obj-bucket", "hello.txt")
            .await
            .expect("get_object should succeed");
        assert_eq!(result.object.data, b"hello world"[..]);
        assert_eq!(result.object.etag, obj.etag);
    }

    #[tokio::test]
    async fn test_get_object_nonexistent_key() {
        let storage = test_storage();
        with_bucket(&storage, "obj-bucket").await;
        let err = storage
            .get_object("obj-bucket", "nope")
            .await
            .unwrap_err();
        assert!(matches!(err, S3Error::NoSuchKey));
    }

    #[tokio::test]
    async fn test_head_object() {
        let storage = test_storage();
        with_bucket(&storage, "head-bucket").await;
        let obj = test_s3_object(b"head data");
        storage
            .put_object("head-bucket", "head.txt", obj.clone())
            .await
            .expect("put_object");
        let result = storage
            .head_object("head-bucket", "head.txt")
            .await
            .expect("head_object should succeed");
        assert_eq!(result.object.etag, obj.etag);
    }

    #[tokio::test]
    async fn test_delete_object() {
        let storage = test_storage();
        with_bucket(&storage, "del-obj-bucket").await;
        let obj = test_s3_object(b"delete me");
        storage
            .put_object("del-obj-bucket", "del.txt", obj)
            .await
            .expect("put_object");
        storage
            .delete_object("del-obj-bucket", "del.txt")
            .await
            .expect("delete_object should succeed");
        let err = storage
            .get_object("del-obj-bucket", "del.txt")
            .await
            .unwrap_err();
        assert!(matches!(err, S3Error::NoSuchKey));
    }

    #[tokio::test]
    async fn test_copy_object() {
        let storage = test_storage();
        with_bucket(&storage, "src").await;
        with_bucket(&storage, "dst").await;
        let obj = test_s3_object(b"copy content");
        storage
            .put_object("src", "source.txt", obj)
            .await
            .expect("put_object");
        storage
            .copy_object("src", "source.txt", "dst", "dest.txt")
            .await
            .expect("copy_object should succeed");
        let result = storage
            .get_object("dst", "dest.txt")
            .await
            .expect("get_object of copy");
        assert_eq!(result.object.data, b"copy content"[..]);
    }

    #[tokio::test]
    async fn test_copy_object_same_bucket() {
        let storage = test_storage();
        with_bucket(&storage, "b").await;
        let obj = test_s3_object(b"same bucket copy");
        storage
            .put_object("b", "original.txt", obj)
            .await
            .expect("put_object");
        storage
            .copy_object("b", "original.txt", "b", "copy.txt")
            .await
            .expect("copy_object same bucket");
        let orig = storage.get_object("b", "original.txt").await.unwrap();
        let copy = storage.get_object("b", "copy.txt").await.unwrap();
        assert_eq!(orig.object.data, copy.object.data);
    }

    #[tokio::test]
    async fn test_list_objects_v2_simple() {
        let storage = test_storage();
        with_bucket(&storage, "list-bucket").await;
        let keys = ["a.txt", "b.txt", "c.txt"];
        for key in &keys {
            storage
                .put_object("list-bucket", key, test_s3_object(b"data"))
                .await
                .expect("put_object");
        }
        let list = storage
            .list_objects_v2("list-bucket", "", "", "", 1000, "")
            .await
            .expect("list_objects_v2");
        assert!(!list.is_truncated);
        assert_eq!(list.contents.len(), 3);
        assert_eq!(list.contents[0].key, "a.txt");
    }

    #[tokio::test]
    async fn test_list_objects_v2_prefix() {
        let storage = test_storage();
        with_bucket(&storage, "pref-bucket").await;
        for key in &["photos/sunset.jpg", "photos/vacation.jpg", "notes.txt"] {
            storage
                .put_object("pref-bucket", key, test_s3_object(b"data"))
                .await
                .expect("put_object");
        }
        let list = storage
            .list_objects_v2("pref-bucket", "photos/", "", "", 1000, "")
            .await
            .expect("list_objects_v2");
        assert_eq!(list.contents.len(), 2);
        assert!(list.contents.iter().all(|c| c.key.starts_with("photos/")));
    }

    #[tokio::test]
    async fn test_list_objects_v2_delimiter() {
        let storage = test_storage();
        with_bucket(&storage, "delim-bucket").await;
        for key in &["a/b/c.txt", "a/b/d.txt", "a/e.txt", "f.txt"] {
            storage
                .put_object("delim-bucket", key, test_s3_object(b"data"))
                .await
                .expect("put_object");
        }
        let list = storage
            .list_objects_v2("delim-bucket", "", "/", "", 1000, "")
            .await
            .expect("list_objects_v2");
        // With "" prefix and "/" delimiter: common prefixes should be "a/" and
        // "f.txt" should be a direct object.
        assert_eq!(list.common_prefixes.len(), 1, "expected 1 common prefix");
        assert_eq!(list.common_prefixes[0].prefix, "a/");
        assert_eq!(list.contents.len(), 1);
        assert_eq!(list.contents[0].key, "f.txt");
    }

    #[tokio::test]
    async fn test_list_objects_v2_pagination() {
        let storage = test_storage();
        with_bucket(&storage, "page-bucket").await;
        for i in 0..5u32 {
            let key = format!("key-{:03}", i);
            storage
                .put_object("page-bucket", &key, test_s3_object(b"data"))
                .await
                .expect("put_object");
        }
        // Fetch 2 per page.
        let page1 = storage
            .list_objects_v2("page-bucket", "", "", "", 2, "")
            .await
            .expect("list_objects_v2 page1");
        assert!(page1.is_truncated);
        assert!(!page1.next_continuation_token.is_empty());
        assert_eq!(page1.contents.len(), 2);

        // Page 2 with continuation token.
        let page2 = storage
            .list_objects_v2(
                "page-bucket",
                "",
                "",
                "",
                2,
                &page1.next_continuation_token,
            )
            .await
            .expect("list_objects_v2 page2");
        assert!(page2.is_truncated);
        assert_eq!(page2.contents.len(), 2);

        // Page 3.
        let page3 = storage
            .list_objects_v2(
                "page-bucket",
                "",
                "",
                "",
                2,
                &page2.next_continuation_token,
            )
            .await
            .expect("list_objects_v2 page3");
        assert!(!page3.is_truncated);
        assert_eq!(page3.contents.len(), 1);
        assert_eq!(page3.contents[0].key, "key-004");
    }

    // -- Multipart operation tests ---------------------------------------

    #[tokio::test]
    async fn test_create_multipart_upload() {
        let storage = test_storage();
        with_bucket(&storage, "mp-bucket").await;
        let upload_id = storage
            .create_multipart_upload("mp-bucket", "large-file.zip")
            .await
            .expect("create_multipart_upload");
        assert!(!upload_id.is_empty());
        // Verify it exists in the bucket.
        let bkt = storage.buckets.get("mp-bucket").unwrap();
        assert!(bkt.multipart_uploads.contains_key(&upload_id));
    }

    #[tokio::test]
    async fn test_upload_part() {
        let storage = test_storage();
        with_bucket(&storage, "mp-bucket").await;
        let upload_id = storage
            .create_multipart_upload("mp-bucket", "file.zip")
            .await
            .expect("create_multipart_upload");

        let etag = storage
            .upload_part("mp-bucket", "file.zip", &upload_id, 1, Bytes::from("part1 data"))
            .await
            .expect("upload_part");
        assert!(!etag.is_empty());
        assert!(etag.starts_with('"'));
    }

    #[tokio::test]
    async fn test_upload_part_invalid_number() {
        let storage = test_storage();
        with_bucket(&storage, "mp-bucket").await;
        let upload_id = storage
            .create_multipart_upload("mp-bucket", "f.txt")
            .await
            .expect("create_multipart_upload");
        let err = storage
            .upload_part("mp-bucket", "f.txt", &upload_id, 0, Bytes::from("x"))
            .await
            .unwrap_err();
        assert!(matches!(err, S3Error::InvalidPart));
    }

    #[tokio::test]
    async fn test_complete_multipart_upload() {
        let storage = test_storage();
        with_bucket(&storage, "mp-bucket").await;
        let upload_id = storage
            .create_multipart_upload("mp-bucket", "final.zip")
            .await
            .expect("create_multipart_upload");

        let etag1 = storage
            .upload_part("mp-bucket", "final.zip", &upload_id, 1, Bytes::from("part1"))
            .await
            .expect("upload_part 1");
        let etag2 = storage
            .upload_part("mp-bucket", "final.zip", &upload_id, 2, Bytes::from("part2"))
            .await
            .expect("upload_part 2");

        let parts = vec![
            Part {
                part_number: 1,
                etag: etag1,
            },
            Part {
                part_number: 2,
                etag: etag2,
            },
        ];

        let final_etag = storage
            .complete_multipart_upload("mp-bucket", "final.zip", &upload_id, &parts)
            .await
            .expect("complete_multipart_upload");
        // Final ETag should have "-2" suffix (2 parts).
        assert!(final_etag.ends_with("-2\""), "etag: {}", final_etag);

        // Object should be retrievable.
        let result = storage
            .get_object("mp-bucket", "final.zip")
            .await
            .expect("get_object after complete");
        assert_eq!(result.object.data, b"part1part2"[..]);
    }

    #[tokio::test]
    async fn test_complete_multipart_upload_wrong_order() {
        let storage = test_storage();
        with_bucket(&storage, "mp-bucket").await;
        let upload_id = storage
            .create_multipart_upload("mp-bucket", "f.txt")
            .await
            .expect("create_multipart_upload");

        let etag1 = storage
            .upload_part("mp-bucket", "f.txt", &upload_id, 1, Bytes::from("a"))
            .await
            .expect("upload_part 1");
        storage
            .upload_part("mp-bucket", "f.txt", &upload_id, 2, Bytes::from("b"))
            .await
            .expect("upload_part 2");

        // Duplicate part numbers must fail with InvalidPartOrder.
        let _err = storage
            .complete_multipart_upload(
                "mp-bucket",
                "f.txt",
                &upload_id,
                &[
                    Part {
                        part_number: 1,
                        etag: etag1.clone(),
                    },
                    Part {
                        part_number: 1,
                        etag: etag1,
                    },
                ],
            )
            .await
            .unwrap_err();
        assert!(matches!(_err, S3Error::InvalidPartOrder));
    }

    #[tokio::test]
    async fn test_abort_multipart_upload() {
        let storage = test_storage();
        with_bucket(&storage, "mp-bucket").await;
        let upload_id = storage
            .create_multipart_upload("mp-bucket", "abort-file.zip")
            .await
            .expect("create_multipart_upload");
        storage
            .upload_part(
                "mp-bucket",
                "abort-file.zip",
                &upload_id,
                1,
                Bytes::from("part data"),
            )
            .await
            .expect("upload_part");

        storage
            .abort_multipart_upload("mp-bucket", "abort-file.zip", &upload_id)
            .await
            .expect("abort_multipart_upload");

        // Upload should be gone.
        let bkt = storage.buckets.get("mp-bucket").unwrap();
        assert!(!bkt.multipart_uploads.contains_key(&upload_id));
    }

    #[tokio::test]
    async fn test_list_parts() {
        let storage = test_storage();
        with_bucket(&storage, "mp-bucket").await;
        let upload_id = storage
            .create_multipart_upload("mp-bucket", "parts-file.zip")
            .await
            .expect("create_multipart_upload");

        storage
            .upload_part(
                "mp-bucket",
                "parts-file.zip",
                &upload_id,
                1,
                Bytes::from("part1"),
            )
            .await
            .expect("upload_part 1");
        storage
            .upload_part(
                "mp-bucket",
                "parts-file.zip",
                &upload_id,
                2,
                Bytes::from("part2"),
            )
            .await
            .expect("upload_part 2");

        let list = storage
            .list_parts("mp-bucket", "parts-file.zip", &upload_id, 1000, 0)
            .await
            .expect("list_parts");
        assert!(!list.is_truncated);
        assert_eq!(list.parts.len(), 2);
        assert_eq!(list.parts[0].part_number, 1);
        assert_eq!(list.parts[1].part_number, 2);
    }

    #[tokio::test]
    async fn test_list_parts_pagination() {
        let storage = test_storage();
        with_bucket(&storage, "mp-bucket").await;
        let upload_id = storage
            .create_multipart_upload("mp-bucket", "many-parts.zip")
            .await
            .expect("create_multipart_upload");

        for i in 1..=5u32 {
            storage
                .upload_part(
                    "mp-bucket",
                    "many-parts.zip",
                    &upload_id,
                    i,
                    Bytes::from(format!("part{}", i)),
                )
                .await
                .expect("upload_part");
        }

        let page1 = storage
            .list_parts("mp-bucket", "many-parts.zip", &upload_id, 2, 0)
            .await
            .expect("list_parts page1");
        assert!(page1.is_truncated);
        assert_eq!(page1.parts.len(), 2);
        assert!(!page1.next_part_number_marker.is_empty());

        let marker: u32 = page1.next_part_number_marker.parse().unwrap();
        let page2 = storage
            .list_parts("mp-bucket", "many-parts.zip", &upload_id, 2, marker)
            .await
            .expect("list_parts page2");
        assert!(page2.is_truncated);
        assert_eq!(page2.parts.len(), 2);

        let marker2: u32 = page2.next_part_number_marker.parse().unwrap();
        let page3 = storage
            .list_parts("mp-bucket", "many-parts.zip", &upload_id, 2, marker2)
            .await
            .expect("list_parts page3");
        assert!(!page3.is_truncated);
        assert_eq!(page3.parts.len(), 1);
        assert_eq!(page3.parts[0].part_number, 5);
    }

    // -- Total bytes accounting tests ------------------------------------

    #[tokio::test]
    async fn test_total_bytes_accounting_put_delete() {
        let storage = test_storage();
        with_bucket(&storage, "acct-bucket").await;

        let init = storage.total_bytes.load(Ordering::Relaxed);
        let obj = test_s3_object(b"1234567890");
        let size = obj.content_length() as u64;

        storage
            .put_object("acct-bucket", "key", obj)
            .await
            .expect("put_object");
        assert_eq!(
            storage.total_bytes.load(Ordering::Relaxed),
            init + size
        );

        storage
            .delete_object("acct-bucket", "key")
            .await
            .expect("delete_object");
        assert_eq!(
            storage.total_bytes.load(Ordering::Relaxed),
            init
        );
    }

    #[tokio::test]
    async fn test_total_bytes_copy_does_not_increment() {
        let storage = test_storage();
        with_bucket(&storage, "src").await;
        with_bucket(&storage, "dst").await;

        let obj = test_s3_object(b"some data");

        storage
            .put_object("src", "src-key", obj)
            .await
            .expect("put_object");
        let after_put = storage.total_bytes.load(Ordering::Relaxed);

        storage
            .copy_object("src", "src-key", "dst", "dst-key")
            .await
            .expect("copy_object");
        let after_copy = storage.total_bytes.load(Ordering::Relaxed);

        // AP-P6: copy_object must NOT increment total_bytes.
        assert_eq!(
            after_copy, after_put,
            "copy_object must not increment total_bytes"
        );
    }

    // -- DefaultStorage new / clone / default ----------------------------

    #[test]
    fn test_default_storage_new() {
        let s = DefaultStorage::new();
        assert_eq!(s.total_bytes.load(Ordering::Relaxed), 0);
        assert!(s.buckets.is_empty());
    }

    #[tokio::test]
    async fn test_default_storage_clone() {
        let s1 = DefaultStorage::new();
        let s2 = s1.clone();
        // Both share the same Arc — mutations through one visible to the other.
        let bucket = Bucket {
            name: "shared".to_string(),
            creation_date: Utc::now(),
            objects: Arc::new(DashMap::new()),
            multipart_uploads: Arc::new(DashMap::new()),
        };
        s1.buckets.insert("shared".to_string(), bucket);
        assert!(s2.buckets.contains_key("shared"));
    }

    #[test]
    fn test_default_storage_default() {
        let s = DefaultStorage::default();
        assert_eq!(s.total_bytes.load(Ordering::Relaxed), 0);
    }
}
