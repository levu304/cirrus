# Cirrus v0.1.0 Technical Specification

## 1. Overview

**Version:** 0.1.0  
**Scope:** HTTP Router + Amazon S3 API emulation  
**Goal:** Statically-linked musl binary exposing wire-compatible S3 endpoint on 4566. Non-S3 services return `501 Not Implemented` with valid AWS XML error bodies.

**Non-Goals:** Persistence across restarts, pre-signed URLs, versioning, ACLs, CORS, event notifications, range requests, `x-amz-checksum-*` header processing (ignored in v0.1.0), any non-S3 service.

---

## 2. System Architecture

### 2.1 Component Diagram

```
┌─────────────────────────────────────────────────────────────┐
│                     cirrus-server (binary)                   │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐  │
│  │   Router    │  │   S3 Core   │  │   Protocol Layer    │  │
│  │  (axum)     │──│  (handlers) │──│  (XML + errors)     │  │
│  └──────┬──────┘  └──────┬──────┘  └─────────────────────┘  │
│         │                │                                   │
│         │         ┌──────┴──────┐                          │
│         │         │   Storage   │                          │
│         │         │  (dashmap)  │                          │
│         │         └─────────────┘                          │
│         │                                                   │
│  ┌──────┴──────────────────────────────────────────────┐  │
│  │              Request Lifecycle                         │  │
│  │  1. TCP accept on :4566                               │  │
│  │  2. Parse HTTP request                                │  │
│  │  3. Extract service from SigV4 Credential scope       │  │
│  │  4. If service != "s3" → 501 XML error               │  │
│  │  5. Parse S3 addressing (path-style vs virtual-host)  │  │
│  │  6. Route to S3 handler based on (method, bucket, key)│  │
│  │  7. Execute storage operation                         │  │
│  │  8. Serialize XML response                            │  │
│  │  9. Return HTTP response with correct headers           │  │
│  └────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
```

### 2.2 Crate Boundaries

| Crate | Responsibility | Public API Surface |
|-------|---------------|-------------------|
| `cirrus-server` | Binary entrypoint, config loading, server startup | `main()` only |
| `cirrus-router` | HTTP server, request routing, SigV4 parsing, S3 addressing | `Router::new()` → `axum::Router` |
| `cirrus-s3` | S3 operation handlers, storage logic, multipart state machine | `S3Service::new()` + `handle(Request) -> Response` |
| `cirrus-protocol` | AWS XML serialization, standard error responses, shared types | `AwsError::to_xml()`, `S3Xml::serialize()` |

---

## 3. HTTP Router Specification

### 3.1 Server Binding

- **Listen address:** `0.0.0.0:4566` (configurable via `CIRRUS_PORT`)
- **Protocol:** HTTP/1.1 only (HTTP/2 not required for v0.1.0)
- **Server implementation:** `axum` with `tokio::net::TcpListener`
- **Concurrency model:** One `tokio` task per connection, unlimited concurrent connections
- **Request body limit:** Enforced via custom middleware wrapping `tower_http::limit::RequestBodyLimitLayer`. Default: **100 MB** (configurable via `CIRRUS_MAX_REQUEST_BYTES`). Exceeding returns AWS XML `EntityTooLarge` error (not bare 413). Custom middleware intercepts the 413 and converts to proper XML error response.
- **IncompleteBody detection:** A custom middleware wraps the body stream to track bytes consumed and compare against Content-Length at EOF. If fewer bytes are received than declared, the middleware returns `IncompleteBody` (400) with AWS XML error response. For chunked transfer encoding (no Content-Length), the body is complete when the final zero-length chunk is received — no `IncompleteBody` error is possible since there's no declared length to compare against.

### 3.2 Request Identification

Classify request before routing.

#### 3.2.1 Service Extraction

Extract priority:

1. **`Authorization` header** — Parse SigV4 `Credential` scope:
   ```
   Authorization: AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260517/us-east-1/s3/aws4_request, ...
   ```
   5th component of scope (`s3` in example) is service name.

2. **`X-Amz-Target` header** — Used by JSON-RPC services:
   ```
   X-Amz-Target: DynamoDB_20120810.ListTables
   ```

3. **Query parameter `Service`** — Fallback for query-protocol requests:
   ```
   ?Service=sqs&Action=CreateQueue
   ```

4. **Default fallback** — Assume `s3` (pre-signed URLs and some SDK calls omit service markers).

#### 3.2.2 S3 Addressing Mode Resolution

Extract `(bucket, key)` from two modes:

**Virtual-Hosted-Style:**
```
Host: my-bucket.localhost:4566
Path: /photos/cat.jpg
→ bucket = "my-bucket", key = "photos/cat.jpg"
```

**Path-Style:**
```
Host: localhost:4566
Path: /my-bucket/photos/cat.jpg
→ bucket = "my-bucket", key = "photos/cat.jpg"
```

**Resolution algorithm:**
```rust
use http::Request;
use std::borrow::Cow;
use urlencoding::decode;

fn resolve_address(req: &Request, base_host: &str) -> (Option<String>, Option<String>) {
    // Host header is trusted for v0.1.0 (no validation)
    let host = req.headers()
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");

    // Strip port if present
    let host = host.split(':').next().unwrap_or(host);

    // Virtual-hosted: bucket.<base-host>
    let vhost_suffix = format!(".{}", base_host);
    if host.ends_with(&vhost_suffix) {
        let bucket = host[..host.len() - vhost_suffix.len()].to_string();
        let key = req.uri().path().trim_start_matches('/').to_string();
        return (Some(bucket), if key.is_empty() { None } else { Some(key) });
    }

    // Path-style
    let path = req.uri().path().trim_start_matches('/');
    let mut segments = path.splitn(2, '/');
    let bucket = segments.next().map(|s| decode(s).unwrap_or(Cow::Borrowed(s)).to_string());
    let key = segments.next().map(|s| decode(s).unwrap_or(Cow::Borrowed(s)).to_string());

    (bucket, key)
}
```

`BASE_HOST` defaults to `localhost`, configurable via `CIRRUS_BASE_HOST`.

> **Note:** v0.1.0 uses the Host header directly. When behind a reverse proxy, ensure the proxy forwards the original Host header (X-Forwarded-Host handling not implemented in v0.1.0).

### 3.3 Routing Table

Single `axum` fallback handler. No per-path routes. Dispatch dynamic based on parsed `(service, method, bucket, key, query_string)`.

```rust
use async_trait::async_trait;
use axum::extract::State;
use std::collections::HashMap;
use std::sync::Arc;
use axum::http::{Request, Response};

pub struct ServiceRegistry {
    pub services: HashMap<String, Box<dyn AwsService>>,
}

#[async_trait]
pub trait AwsService: Send + Sync {
    async fn handle(&self, req: Request) -> Response;
}

async fn fallback_handler(
    State(registry): State<Arc<ServiceRegistry>>,
    req: Request,
) -> Response {
    let service = extract_service(&req);

    if let Some(handler) = registry.services.get(&service) {
        handler.handle(req).await
    } else {
        AwsError::NotImplemented {
            message: format!("Service '{}' not implemented in v0.1.0", service),
        }.into_response()
    }
}
```

---

## 4. S3 API Specification

### 4.1 Supported Operations

| # | Operation | HTTP Method | Path Pattern | Query Params | Request Body | Response Body |
|---|-----------|-------------|--------------|--------------|--------------|-----------------|
| 1 | `ListBuckets` | `GET` | `/` | — | Empty | `ListAllMyBucketsResult` XML |
| 2 | `CreateBucket` | `PUT` | `/{bucket}` | — | Optional `CreateBucketConfiguration` XML | Empty (200) with `Location: http://{host}:{port}/{bucket}` header. No response body for us-east-1. |
| 3 | `DeleteBucket` | `DELETE` | `/{bucket}` | — | Empty | Empty (204) |
| 4 | `ListObjectsV2` | `GET` | `/{bucket}` | `list-type=2` | Empty | `ListBucketResult` XML |
| 5 | `PutObject` | `PUT` | `/{bucket}/{key}` | — | Object bytes | Empty (200) with `ETag` response header |
| 6 | `GetObject` | `GET` | `/{bucket}/{key}` | — | Empty | Object bytes |
| 7 | `HeadObject` | `HEAD` | `/{bucket}/{key}` | — | Empty | Empty (headers only) |
| 8 | `DeleteObject` | `DELETE` | `/{bucket}/{key}` | — | Empty | Empty (204) |
| 9 | `DeleteObjects` | `POST` | `/{bucket}` | `delete` | `Delete` XML | `DeleteResult` XML |
| 10 | `CopyObject` | `PUT` | `/{bucket}/{key}` | — | Empty (source in header) | `CopyObjectResult` XML |
| 11 | `CreateMultipartUpload` | `POST` | `/{bucket}/{key}` | `uploads` | Empty | `InitiateMultipartUploadResult` XML |
| 12 | `UploadPart` | `PUT` | `/{bucket}/{key}` | `partNumber={n}&uploadId={id}` | Part bytes | Empty (200) with `ETag` response header |
| 13 | `CompleteMultipartUpload` | `POST` | `/{bucket}/{key}` | `uploadId={id}` | `CompleteMultipartUpload` XML | `CompleteMultipartUploadResult` XML |
| 14 | `AbortMultipartUpload` | `DELETE` | `/{bucket}/{key}` | `uploadId={id}` | Empty | Empty (204) |
| 15 | `ListParts` | `GET` | `/{bucket}/{key}` | `uploadId={id}&max-parts={n}&part-number-marker={n}` | Empty | `ListPartsResult` XML |
| 16 | `GetBucketLocation` | `GET` | `/{bucket}` | `location` | Empty | `LocationConstraint` XML |

> **Note on `DeleteObjects`:** DeleteObjects is implemented as sequential calls to `Storage::delete_object` per key. The Storage trait does not have a dedicated bulk-delete method for v0.1.0.

> **Note:** DeleteObjects is non-transactional. Each key is deleted independently via sequential `delete_object` calls. Failed deletions are reported in `<Error>` elements of the response; successfully deleted keys are not rolled back.

> **Note:** `CopyObject` shares the same HTTP method and path as `PutObject`. Dispatch rule: if `x-amz-copy-source` header present (checked BEFORE URL decoding) → `CopyObject`, else → `PutObject`. URL decoding of the header value happens AFTER dispatch, during source path parsing.

> **Note on `CreateBucketConfiguration`:** For v0.1.0, any provided `CreateBucketConfiguration` XML is parsed but the region value is ignored. All buckets stored in default region (`us-east-1`). Matches LocalStack behavior. Malformed XML returns `MalformedXML` (400). Empty request body is valid and equivalent to no configuration.

> **Note on `GetBucketLocation`:** Stub implementation returns `<LocationConstraint>us-east-1</LocationConstraint>`. Required by AWS CLI after CreateBucket.

> **Note on `GetObject`/`HeadObject` errors:** `GetObject`/`HeadObject` on a non-existent key returns 404 `NoSuchKey`. `GetObject`/`HeadObject` on a non-existent bucket returns 404 `NoSuchBucket`.

> **Note on `ListParts` query params:** Only `uploadId` is required. `max-parts` and `part-number-marker` are optional pagination parameters.

### 4.2 Request/Response XML Schemas

#### 4.2.1 `ListAllMyBucketsResult`

```xml
<?xml version="1.0" encoding="UTF-8"?>
<ListAllMyBucketsResult xmlns="http://storage.amazonaws.com/doc/2006-03-01/">
  <Owner>
    <ID>000000000000</ID>
    <DisplayName>webfile</DisplayName>
  </Owner>
  <Buckets>
    <Bucket>
      <Name>my-bucket</Name>
      <CreationDate>2026-05-17T08:40:00.000Z</CreationDate>
    </Bucket>
  </Buckets>
</ListAllMyBucketsResult>
```

#### 4.2.1a `CreateBucketOutput`

```xml
<?xml version="1.0" encoding="UTF-8"?>
<CreateBucketOutput xmlns="http://storage.amazonaws.com/doc/2006-03-01/">
  <Location>http://localhost:4566/my-bucket</Location>
</CreateBucketOutput>
```

> **Note:** Reserved for future versions when non-default regions are supported. v0.1.0 always returns empty body (all buckets in us-east-1).

#### 4.2.2 `ListBucketResult` (ListObjectsV2)

```xml
<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://storage.amazonaws.com/doc/2006-03-01/">
  <Name>my-bucket</Name>
  <ContinuationToken></ContinuationToken>
  <StartAfter></StartAfter>
  <Delimiter></Delimiter>
  <Prefix></Prefix>
  <MaxKeys>1000</MaxKeys>
  <KeyCount>1</KeyCount>
  <IsTruncated>false</IsTruncated>
  <NextContinuationToken></NextContinuationToken>
  <Contents>
    <Key>photos/cat.jpg</Key>
    <LastModified>2026-05-17T08:40:00.000Z</LastModified>
    <ETag>&quot;d41d8cd98f00b204e9800998ecf8427e&quot;</ETag>
    <Size>1024</Size>
    <StorageClass>STANDARD</StorageClass>
  </Contents>
  <CommonPrefixes>
    <Prefix>photos/</Prefix>
  </CommonPrefixes>
</ListBucketResult>
```

**Query parameter handling:**
- `list-type=2` — Required. Without it, return `InvalidArgument` error.
- `prefix` — Filter keys by prefix.
- `max-keys` — Default 1000. Values > 1000 clamped to 1000. Values < 1 clamped to 1.
- `continuation-token` — Opaque token for pagination.
- `delimiter` — If provided, group keys sharing prefix up to delimiter into `CommonPrefixes` instead of `Contents`.
- `start-after` — Return keys lexicographically after this value.

**Pagination logic:**
1. Filter keys by `prefix`.
2. If `start-after` provided, skip keys <= `start-after`.
3. Sort remaining keys lexicographically.
4. If `continuation-token` provided, decode base64 to get the last key. Token format: `base64(key.bytes())`. Skip keys <= decoded value.
5. Take up to `max-keys`.
6. If more keys remain, set `IsTruncated=true` and `NextContinuationToken` to base64 of last returned key. Set `NextContinuationToken` to empty when not truncated.

> **Note:** Empty values are rendered as `<Element></Element>` (explicit open/close tags, not self-closing `<Element/>`) to match AWS S3 XML output.

**Echo request values:** Empty elements in the schema represent the default/empty case. Actual responses echo the request parameter values: `<ContinuationToken>` echoes the request's `continuation-token`, `<StartAfter>` echoes `start-after`, `<Delimiter>` echoes `delimiter`.

#### 4.2.3 `Delete` (DeleteObjects Request)

```xml
<Delete>
  <Quiet>false</Quiet>
  <Object>
    <Key>key1</Key>
  </Object>
  <Object>
    <Key>key2</Key>
    <VersionId>version-id</VersionId>
  </Object>
</Delete>
```

v0.1.0 ignores `<VersionId>` (versioning not supported), parses without error.

#### 4.2.4 `DeleteResult`

```xml
<?xml version="1.0" encoding="UTF-8"?>
<DeleteResult xmlns="http://storage.amazonaws.com/doc/2006-03-01/">
  <Deleted>
    <Key>key1</Key>
  </Deleted>
  <Error>
    <Key>key2</Key>
    <Code>NoSuchKey</Code>
    <Message>The specified key does not exist.</Message>
  </Error>
</DeleteResult>
```

**Quiet mode:** When `<Quiet>true</Quiet>`, omit `<Deleted>` elements from response. Only include `<Error>` elements for failed deletions.

**Empty body:** Empty `<Delete>` body (no `<Object>` children) returns 200 with empty `<DeleteResult>` (no `<Deleted>` or `<Error>` elements).

#### 4.2.5 `CopyObjectResult`

```xml
<?xml version="1.0" encoding="UTF-8"?>
<CopyObjectResult xmlns="http://storage.amazonaws.com/doc/2006-03-01/">
  <ETag>&quot;d41d8cd98f00b204e9800998ecf8427e&quot;</ETag>
  <LastModified>2026-05-17T08:40:00.000Z</LastModified>
</CopyObjectResult>
```

#### 4.2.6 `InitiateMultipartUploadResult`

```xml
<?xml version="1.0" encoding="UTF-8"?>
<InitiateMultipartUploadResult xmlns="http://storage.amazonaws.com/doc/2006-03-01/">
  <Bucket>my-bucket</Bucket>
  <Key>large-file.zip</Key>
  <UploadId>upload-id-string</UploadId>
</InitiateMultipartUploadResult>
```

`UploadId` generation: `base64url(sha256("{bucket}:{key}:{timestamp}:{random_u64}"))`

#### 4.2.7 `CompleteMultipartUpload` (Request)

```xml
<CompleteMultipartUpload>
  <Part>
    <PartNumber>1</PartNumber>
    <ETag>&quot;etag-of-part-1&quot;</ETag>
  </Part>
  <Part>
    <PartNumber>2</PartNumber>
    <ETag>&quot;etag-of-part-2&quot;</ETag>
  </Part>
</CompleteMultipartUpload>
```

#### 4.2.8 `CompleteMultipartUploadResult`

```xml
<?xml version="1.0" encoding="UTF-8"?>
<CompleteMultipartUploadResult xmlns="http://storage.amazonaws.com/doc/2006-03-01/">
  <Location>http://localhost:4566/my-bucket/large-file.zip</Location>
  <Bucket>my-bucket</Bucket>
  <Key>large-file.zip</Key>
  <ETag>&quot;a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6-3&quot;</ETag>
</CompleteMultipartUploadResult>
```

**Composite ETag:** Concatenate all part *raw bytes* in `PartNumber` order, compute MD5 of concatenated bytes, wrap in quotes, append `-<part_count>`. Format: `"<md5(concatenated_raw_bytes)>-<N>"`. This matches real AWS S3 multipart ETag behavior.

#### 4.2.9 `ListPartsResult`

```xml
<?xml version="1.0" encoding="UTF-8"?>
<ListPartsResult xmlns="http://storage.amazonaws.com/doc/2006-03-01/">
  <Bucket>my-bucket</Bucket>
  <Key>large-file.zip</Key>
  <UploadId>upload-id-string</UploadId>
  <Initiator>
    <ID>000000000000</ID>
    <DisplayName>webfile</DisplayName>
  </Initiator>
  <Owner>
    <ID>000000000000</ID>
    <DisplayName>webfile</DisplayName>
  </Owner>
  <MaxParts>1000</MaxParts>
  <NextPartNumberMarker></NextPartNumberMarker>
  <StorageClass>STANDARD</StorageClass>
  <Part>
    <PartNumber>1</PartNumber>
    <LastModified>2026-05-17T08:40:00.000Z</LastModified>
    <ETag>&quot;etag-of-part-1&quot;</ETag>
    <Size>5242880</Size>
  </Part>
  <IsTruncated>false</IsTruncated>
</ListPartsResult>
```

When `IsTruncated=true`, `NextPartNumberMarker` contains the highest `PartNumber` from the returned page. When `IsTruncated=false`, the element is empty. `PartNumberMarker` is omitted when value is 0.

#### 4.2.10 `LocationConstraint` (GetBucketLocation)

```xml
<?xml version="1.0" encoding="UTF-8"?>
<LocationConstraint xmlns="http://storage.amazonaws.com/doc/2006-03-01/">us-east-1</LocationConstraint>
```

### 4.3 Object Metadata

Every stored object metadata:

```rust
#[derive(Debug, Clone)]
pub struct S3Object {
    pub data: Bytes,
    pub etag: String,           // MD5 hex, wrapped in quotes
    pub content_type: String,   // Default: "binary/octet-stream"
    pub content_length: usize,
    pub last_modified: DateTime<Utc>,
    pub metadata: HashMap<String, String>, // x-amz-meta-* headers
}
```

**Header mapping on `PutObject`:**
- `Content-Type` → `content_type`
- `Content-Length` → validated against body length
- `x-amz-meta-{key}` → stored in `metadata` map after stripping `\r` and `\n` characters (prevents HTTP response splitting)
- `x-amz-copy-source` → triggers `CopyObject` logic
- `x-amz-server-side-encryption` → echoed back in response as `AES256` (stub; SSE not enforced in v0.1.0)
- `ETag` → computed MD5 of object data, returned as response header

**Header mapping on `GetObject` / `HeadObject` response:**
- `Content-Type` ← `content_type`
- `Content-Length` ← `content_length`
- `ETag` ← `etag`
- `Last-Modified` ← `last_modified` formatted as IMF-fixdate (RFC 7231)
- `x-amz-meta-{key}` ← from `metadata` map
- `Accept-Ranges: bytes` ← always included
- `x-amz-request-id` ← same UUID generated per-request
- `x-amz-id-2` ← static `"cirrus-v0.1.0"`

### 4.4 CopyObject Semantics

`CopyObject` triggered by `PUT /{dest-bucket}/{dest-key}` + `x-amz-copy-source` header.

**Source parsing:**
URL-decode the `x-amz-copy-source` header value before parsing bucket and key.
```
x-amz-copy-source: /source-bucket/source-key
x-amz-copy-source: /source-bucket/source-key?versionId=xxx
```

v0.1.0 ignores `versionId`.

**Behavior:**
1. Parse source bucket and key.
2. Verify source object exists. If not, return `NoSuchKey`.
3. Create new `S3Object` in destination with cloned `data` and `metadata`.
4. Generate new `ETag` and `Last-Modified`.
5. Return `CopyObjectResult` XML.
6. If `x-amz-metadata-directive: REPLACE` header present, use new `x-amz-meta-*` headers from request instead of cloning source metadata. Default is `COPY`.

> **Note:** `Bytes::clone()` is O(1) — it increments the Arc refcount. CopyObject does not duplicate underlying data.

> **Note (same-source copy):** Same-source copy (source == destination): v0.1.0 allows it — creates a new reference with updated `LastModified` and recomputed `ETag`. This differs from real AWS which returns 400 `InvalidRequest`.

### 4.5 Multipart Upload State Machine

```
CreateMultipartUpload ──► UploadPart (1..N) ──► CompleteMultipartUpload
        │                                        │
        └──────────────────────────────────────────┘
                         AbortMultipartUpload
```

**Storage structure:**
```rust
struct MultipartUpload {
    bucket: String,
    key: String,
    upload_id: String,
    initiated: DateTime<Utc>,
    parts: BTreeMap<u32, S3Object>, // part_number -> part data
}
```

**UploadId format:** `base64url(sha256("{bucket}:{key}:{timestamp}:{random_u64}"))`

**UploadPart response:** Returns `ETag` header containing MD5 of part data. This ETag must match the value provided in subsequent `CompleteMultipartUpload` request.

**CompleteMultipartUpload validation:**
1. Verify all `PartNumber` values in request exist in stored parts.
2. Verify `ETag` of each stored part matches request.
3. Concatenate all part `data` in `PartNumber` order.
4. Compute composite ETag: concatenate the raw `data` bytes of each part (not their ETags), compute MD5 of the full concatenation, format as `"<md5_hex>-<part_count>"`.
5. Store as regular object.
6. Delete multipart upload state.

**AbortMultipartUpload:** Delete all stored parts and multipart upload record. Return 204.

**v0.1.0 limitation:** Abandoned multipart uploads accumulate until server restart. No cleanup mechanism.

---

## 5. Storage Layer Specification

### 5.1 Data Structures

```rust
use dashmap::DashMap;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

pub struct Bucket {
    name: String,
    created_at: DateTime<Utc>,
    objects: DashMap<String, S3Object>,
    multipart_uploads: DashMap<String, MultipartUpload>,
}

#[derive(Debug, Clone)]
pub struct S3Object {
    pub data: Bytes,
    pub etag: String,
    pub content_type: String,
    pub content_length: usize,
    pub last_modified: DateTime<Utc>,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug)]
pub struct MultipartUpload {
    bucket: String,
    key: String,
    upload_id: String,
    initiated: DateTime<Utc>,
    parts: BTreeMap<u32, S3Object>,
}
```

### 5.2 Concurrency Model

- `DashMap` provides sharded read-write locks. No global lock on store.
- Each `Bucket` has its own `DashMap<String, S3Object>` for objects.
- Each `Bucket` has its own `DashMap<String, MultipartUpload>` for uploads.
- **Bucket-level isolation:** Operations on different buckets do not contend.
- **Key-level isolation within bucket:** `DashMap` shards by hash, different keys within same bucket typically do not contend.
- **Multipart uploads:** During `UploadPart`, only specific `MultipartUpload` entry locked. Other uploads to same bucket proceed concurrently.
- **DeleteBucket atomicity:** DeleteBucket atomically removes the bucket from the parent `DashMap` via `buckets.remove(name)`. If removal succeeds, the bucket is now isolated — no other thread can access it. Check `objects.is_empty() && multipart_uploads.is_empty()`. If empty, discard the bucket (deletion complete). If not empty, re-insert via `buckets.insert(name, bucket)` and return `BucketNotEmpty`. This avoids the ABBA deadlock between `delete_guard` and `DashMap` shard locks by using a single lock ordering: parent DashMap lock first, then bucket internals.

### 5.3 Memory Management

- Object data stored as `Bytes` (reference-counted, cheap to clone for `CopyObject`).
- No streaming for v0.1.0 — entire object loaded into memory on `PutObject`.
- Maximum object size: **100 MB** (configurable via `CIRRUS_MAX_OBJECT_SIZE`)
- Maximum multipart part size: **100 MB** (same limit as single objects)
- Maximum total memory: **512 MB** (configurable via `CIRRUS_MAX_MEMORY`). `PutObject` rejected if exceeded.
- **Memory accounting:** `DefaultStorage` tracks total bytes via `AtomicU64`. Incremented on `PutObject` and `CompleteMultipartUpload`, decremented on `DeleteObject` and `AbortMultipartUpload`. `PutObject` rejected if total would exceed `CIRRUS_MAX_MEMORY`.
- **CopyObject:** Does NOT increment `total_bytes` because `Bytes::clone()` shares the underlying allocation via Arc.
- Maximum parts per multipart upload: **10,000**.

> **Note:** `CIRRUS_MAX_REQUEST_BYTES` must be >= `CIRRUS_MAX_OBJECT_SIZE` to avoid rejecting valid objects. The request body includes headers and multipart boundaries, so set the request limit higher than the object limit (e.g., 110 MB vs 100 MB).

### 5.4 Storage Trait

Storage abstracted behind a trait to enable unit testing with mock stores:

```rust
pub struct BucketInfo {
    pub name: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct ObjectList {
    pub name: String,
    pub prefix: String,
    pub max_keys: u32,
    pub key_count: u32,
    pub objects: Vec<S3ObjectInfo>,
    pub common_prefixes: Vec<String>,
    pub is_truncated: bool,
    pub next_continuation_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct S3ObjectInfo {
    pub key: String,
    pub last_modified: DateTime<Utc>,
    pub etag: String,
    pub size: usize,
    pub storage_class: String,
}

#[derive(Debug, Clone)]
pub struct MultipartUploadInfo {
    pub bucket: String,
    pub key: String,
    pub upload_id: String,
    pub initiated: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct PartsList {
    pub parts: Vec<PartInfo>,
    pub is_truncated: bool,
    pub next_part_number_marker: Option<u32>,
    pub max_parts: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PartInfo {
    pub part_number: u32,
    pub last_modified: DateTime<Utc>,
    pub etag: String,
    pub size: usize,
}

#[derive(thiserror::Error, Debug)]
pub enum S3Error {
    #[error("Bucket not found: {0}")]
    NoSuchBucket(String),
    #[error("Object not found: {0}")]
    NoSuchKey(String),
    #[error("Multipart upload not found: {0}")]
    NoSuchUpload(String),
    #[error("Bucket already exists: {0}")]
    BucketAlreadyExists(String),
    #[error("Bucket not empty: {0}")]
    BucketNotEmpty(String),
    #[error("Invalid argument: {0}")]
    InvalidArgument(String),
    #[error("Method not allowed")]
    MethodNotAllowed,
    #[error("Entity too large")]
    EntityTooLarge,
    #[error("Malformed XML")]
    MalformedXML,
    #[error("Internal error")]
    InternalError,
    #[error("Invalid bucket name: {0}")]
    InvalidBucketName(String),
    #[error("Key too long")]
    KeyTooLong,
    #[error("One or more of the specified parts could not be found. UploadId: {0}, PartNumber: {1}, ETag: {2}")]
    InvalidPart(String, u32, String),
    #[error("Incomplete body")]
    IncompleteBody,
}

pub struct GetObjectResult {
    pub data: Bytes,
    pub info: S3ObjectInfo,
    pub metadata: HashMap<String, String>,
}

use async_trait::async_trait;

#[async_trait]
pub trait Storage: Send + Sync {
    async fn create_bucket(&self, name: &str) -> Result<(), S3Error>;
    async fn delete_bucket(&self, name: &str) -> Result<(), S3Error>;
    async fn list_buckets(&self) -> Result<Vec<BucketInfo>, S3Error>;
    async fn put_object(&self, bucket: &str, key: &str, data: Bytes, metadata: HashMap<String, String>, content_type: String) -> Result<S3ObjectInfo, S3Error>;
    async fn get_object(&self, bucket: &str, key: &str) -> Result<GetObjectResult, S3Error>;
    async fn delete_object(&self, bucket: &str, key: &str) -> Result<(), S3Error>;
    async fn head_object(&self, bucket: &str, key: &str) -> Result<S3ObjectInfo, S3Error>;
    async fn copy_object(&self, src_bucket: &str, src_key: &str, dest_bucket: &str, dest_key: &str, metadata: HashMap<String, String>, directive: MetadataDirective) -> Result<S3ObjectInfo, S3Error>;
    async fn create_multipart_upload(&self, bucket: &str, key: &str) -> Result<MultipartUploadInfo, S3Error>;
    async fn upload_part(&self, bucket: &str, key: &str, upload_id: &str, part_number: u32, data: Bytes) -> Result<PartInfo, S3Error>;
    async fn complete_multipart_upload(&self, bucket: &str, key: &str, upload_id: &str, parts: &[(u32, String)]) -> Result<S3ObjectInfo, S3Error>;
    async fn abort_multipart_upload(&self, bucket: &str, key: &str, upload_id: &str) -> Result<(), S3Error>;
    async fn list_parts(&self, bucket: &str, key: &str, upload_id: &str, max_parts: Option<u32>, part_number_marker: Option<u32>) -> Result<PartsList, S3Error>;
    async fn list_objects(&self, bucket: &str, prefix: &str, delimiter: &str, max_keys: u32, start_after: Option<&str>, continuation_token: Option<&str>) -> Result<ObjectList, S3Error>;
    async fn get_bucket_location(&self, name: &str) -> Result<String, S3Error>;
}

#[derive(Debug, Clone, PartialEq)]
pub enum MetadataDirective {
    Copy,
    Replace,
}

#[derive(Debug)]
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

impl Default for DefaultStorage {
    fn default() -> Self {
        Self {
            buckets: Arc::new(DashMap::new()),
            total_bytes: Arc::new(AtomicU64::new(0)),
        }
    }
}
```

`list_parts` `max_parts` defaults to 1000 when `None`.

> **Note:** `get_bucket_location` is a v0.1.0 stub: verifies bucket exists, returns static `us-east-1`.

`S3Service` is generic over `S: Storage = DefaultStorage`:
```rust
pub struct S3Service<S: Storage = DefaultStorage> {
    store: S,
}
```

```rust
impl Default for S3Service<DefaultStorage> {
    fn default() -> Self { Self { store: DefaultStorage::default() } }
}
impl<S: Storage> S3Service<S> {
    pub fn new(store: S) -> Self { Self { store } }
}
```

---

## 6. Error Handling Specification

### 6.1 Error Response Format

All errors return XML with `Content-Type: application/xml`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<Error>
  <Code>{ErrorCode}</Code>
  <Message>{ErrorMessage}</Message>
  <RequestId>{UUID}</RequestId>
  <HostId>{HostId}</HostId>
</Error>
```

- `RequestId`: UUID v4, unique per request.
- `HostId`: Static value `"cirrus-v0.1.0"` for v0.1.0.
- Every response (success and error) includes `x-amz-request-id` header matching the `RequestId` element in the XML error body.
- Every response (success and error) includes `x-amz-id-2` header with static value `"cirrus-v0.1.0"`.

### 6.2 Error Code Mapping

| Error Code | HTTP Status | Trigger Condition |
|------------|-------------|-------------------|
| `NotImplemented` | 501 | Service != "s3", or S3 operation not in v0.1.0 scope |
| `NoSuchBucket` | 404 | Bucket does not exist |
| `NoSuchKey` | 404 | Object key does not exist in bucket |
| `NoSuchUpload` | 404 | Multipart upload ID does not exist |
| `BucketAlreadyExists` | 409 | `CreateBucket` on existing bucket |
| `BucketNotEmpty` | 409 | `DeleteBucket` on bucket with objects or bucket has active multipart uploads |
| `InvalidArgument` | 400 | `list-type` missing on ListObjectsV2, invalid `partNumber`, etc. |
| `MethodNotAllowed` | 405 | HTTP method not supported for resource |
| `EntityTooLarge` | 400 | Object exceeds 100 MB limit (matches AWS S3 behavior — EntityTooLarge returns HTTP 400, not 413) |
| `MalformedXML` | 400 | Invalid XML in request body |
| `InternalError` | 500 | Unexpected panic or storage failure |
| `InvalidBucketName` | 400 | Bucket name fails validation (3-63 chars, lowercase alphanumeric + hyphens) |
| `KeyTooLong` | 400 | Object key exceeds 1024 bytes |
| `InvalidPart` | 400 | `CompleteMultipartUpload` references a part that doesn't exist or has wrong ETag |
| `IncompleteBody` | 400 | Request body shorter than declared Content-Length. Detected by comparing actual bytes received against Content-Length header. Only applies when Content-Length is set. For chunked transfer encoding (no Content-Length), the body completes at the final zero-length chunk — no `IncompleteBody` error is possible since there's no declared length to compare against. |

> **Note:** Some error codes include additional context-specific XML elements beyond the standard `<Code>`, `<Message>`, `<RequestId>`, `<HostId>`. See Section 6.3 for per-error schemas.

### 6.3 Specific Error Behaviors

**ListObjectsV2 without `list-type=2`:**
```xml
<Error>
  <Code>InvalidArgument</Code>
  <Message>Invalid Argument</Message>
  <ArgumentName>list-type</ArgumentName>
  <ArgumentValue></ArgumentValue>
</Error>
```

**DeleteBucket with objects:**
```xml
<Error>
  <Code>BucketNotEmpty</Code>
  <Message>The bucket you tried to delete is not empty</Message>
  <BucketName>my-bucket</BucketName>
</Error>
```

**CompleteMultipartUpload with missing part:**
```xml
<Error>
  <Code>InvalidPart</Code>
  <Message>One or more of the specified parts could not be found.</Message>
  <UploadId>upload-id</UploadId>
  <PartNumber>3</PartNumber>
  <ETag>expected-etag</ETag>
</Error>
```

---

## 7. Protocol Layer Specification

### 7.1 XML Serialization

Use `quick-xml` + `serde` for structured XML. Hand-builders for simple responses to avoid serde overhead on hot paths.

**Performance rule:** Responses <1 KB hand-built with `format!()`. Use `xml_escape()` utility for all dynamic values (object keys, metadata values) before insertion:

```rust
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
     .replace('<', "&lt;")
     .replace('>', "&gt;")
     .replace('"', "&quot;")
     .replace('\'', "&apos;")
}
```

Complex responses (ListBuckets, ListObjectsV2) use `quick-xml`.

### 7.2 Date Formatting

- **XML elements:** `2026-05-17T08:40:00.000Z` (ISO 8601 with milliseconds)
- **HTTP headers:** `Sun, 17 May 2026 08:40:00 GMT` (IMF-fixdate per RFC 7231, format: `"%a, %d %b %Y %H:%M:%S GMT"`)

### 7.3 ETag Format

Always double-quoted hex MD5:
```
ETag: "d41d8cd98f00b204e9800998ecf8427e"
```

In XML, quotes escaped as `&quot;`:
```xml
<ETag>&quot;d41d8cd98f00b204e9800998ecf8427e&quot;</ETag>
```

---

## 8. Configuration Specification

### 8.1 Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `CIRRUS_PORT` | `4566` | HTTP listen port |
| `CIRRUS_BIND_ADDRESS` | `0.0.0.0` | HTTP bind address |
| `CIRRUS_BASE_HOST` | `localhost` | Base hostname for virtual-hosted-style addressing |
| `CIRRUS_DEFAULT_ACCOUNT_ID` | `000000000000` | Default AWS account ID. v0.1.0 uses this value for all requests regardless of access key format. Access key is parsed but ignored for account resolution. |
| `CIRRUS_DEFAULT_REGION` | `us-east-1` | Default region (informational only in v0.1.0) |
| `CIRRUS_LOG_LEVEL` | `info` | `tracing` log level: `error`, `warn`, `info`, `debug`, `trace` |
| `CIRRUS_MAX_REQUEST_BYTES` | `104857600` (100 MB) | Maximum HTTP request body size |
| `CIRRUS_MAX_OBJECT_SIZE` | `104857600` (100 MB) | Maximum single object size |
| `CIRRUS_MAX_MEMORY` | `536870912` (512 MB) | Maximum total memory for stored objects |

### 8.2 CLI Arguments

```
cirrus-server [OPTIONS]

Options:
  -p, --port <PORT>          HTTP port [default: 4566]
  -b, --bind <ADDRESS>       Bind address [default: 0.0.0.0]
  -l, --log-level <LEVEL>    Log level [default: info]
  -h, --help                 Print help
  -V, --version              Print version
```

**Priority order:** CLI args override env vars, env vars override defaults. If using figment, ensure CLI args are merged AFTER env vars to maintain this priority:
```rust
Figment::from(Env::prefixed("CIRRUS_"))
    .merge(Clap::default())
```

---

## 9. Build & Deployment Specification

### 9.1 Cargo Workspace

```toml
# Cargo.toml (workspace root)
[workspace]
members = ["crates/*"]
resolver = "2"

[workspace.package]
version = "0.1.0"
edition = "2024"
rust-version = "1.85"
authors = ["Cirrus Contributors"]
license = "MIT"
repository = "https://github.com/cirrus-io/cirrus"

[workspace.dependencies]
tokio = { version = "1.43", features = ["rt", "rt-multi-thread", "net", "macros", "signal", "time", "fs"] }
axum = "0.8"
# Note: axum 0.8 is stable as of 2026. This spec targets axum 0.8 APIs: `axum::serve`, `with_graceful_shutdown`, `RequestBodyLimitLayer`. Verify each API against the chosen version's documentation before implementation.
hyper = { version = "1.6", features = ["http1", "server"] }
http = "1.3"
http-body = "1.0"
http-body-util = "0.1"
bytes = "1.10"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
serde_urlencoded = "0.1"
quick-xml = { version = "0.37", features = ["serialize", "deserialize"] }
chrono = { version = "0.4", features = ["serde"] }
dashmap = "6.1"
md-5 = "0.10"
sha2 = "0.10"
base64 = "0.22"
uuid = { version = "1.15", features = ["v4"] }
thiserror = "2.0"
async-trait = "0.1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
clap = { version = "4.5", features = ["derive"] }
figment = { version = "0.10", features = ["env"] }
tower = "0.5"
tower-http = { version = "0.6", features = ["trace", "limit"] }
urlencoding = "2.6"

# Dev dependencies
criterion = { version = "0.5", features = ["async_tokio"] }
proptest = "1.5"
```

> **Note:** Alternatively, use native `async fn` in traits (Rust 1.75+). The `#[async_trait]` attribute is shown for compatibility with older patterns.

> **Note:** v0.1.0 uses `multi_thread` runtime for concurrent connection handling. Adds ~2ms startup, ~2 MiB memory — still within targets.

### 9.2 Crate Structure

```
cirrus/
├── Cargo.toml
├── crates/
│   ├── cirrus-server/
│   │   ├── Cargo.toml
│   │   └── src/main.rs
│   ├── cirrus-router/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── service.rs      # SigV4 + service extraction
│   │       └── address.rs      # S3 addressing resolution
│   ├── cirrus-s3/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── service.rs      # S3Service + routing logic
│   │       ├── handlers.rs     # Per-operation handlers
│   │       ├── storage.rs      # In-memory store + data structures
│   │       └── multipart.rs    # Multipart upload state machine
│   └── cirrus-protocol/
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs
│           ├── error.rs        # AwsError enum + XML generation
│           ├── xml.rs          # Shared XML builders
│           └── types.rs        # Common AWS types (Owner, etc.)
```

### 9.3 Docker Build

**Dockerfile:**
```dockerfile
# Build stage
FROM rust:1.85-alpine AS builder
RUN apk add --no-cache musl-dev
WORKDIR /app
COPY . .
ARG RUST_TARGET=x86_64-unknown-linux-musl
RUN rustup target add "$RUST_TARGET"
RUN cargo build --release --target "$RUST_TARGET" --bin cirrus-server

# Runtime stage
FROM gcr.io/distroless/static-debian12
ARG RUST_TARGET=x86_64-unknown-linux-musl
COPY --from=builder /app/target/$RUST_TARGET/release/cirrus-server /cirrus-server
EXPOSE 4566
ENTRYPOINT ["/cirrus-server"]
```

**Image targets:**
- Size: **<30 MB** (distroless/static ~2 MB + ~20 MB musl static binary)
- Startup in container: **<20 ms**

### 9.4 CI/CD Pipeline

**GitHub Actions workflow:**

| Job | Trigger | Steps |
|-----|---------|-------|
| `test` | PR, push to main | `cargo test --workspace`, `cargo clippy -- -D warnings`, `cargo fmt --check` |
| `integration` | PR, push to main | Start server, run AWS CLI + boto3 + Rust SDK integration tests |
| `build` | Tag push | `cargo build --release`, build Docker image, push to GHCR |

---

## 10. Testing Specification

### 10.1 Unit Tests

**Coverage targets:**
- `cirrus-router`: 90%+ line coverage
- `cirrus-s3`: 85%+ line coverage
- `cirrus-protocol`: 90%+ line coverage

**Test categories:**

| Component | Test Cases |
|-----------|-----------|
| SigV4 parser | Valid scope extraction, missing header, malformed header, pre-signed URL without auth |
| Address parser | Path-style, virtual-hosted-style, empty path, URL-encoded keys, Unicode keys |
| S3 handlers | Create bucket, put/get/delete object, list buckets, list objects with all query params |
| Storage | Concurrent put/get on same key, concurrent operations on different buckets, multipart lifecycle |
| XML builders | Round-trip serialization/deserialization for all response types |

### 10.2 Integration Tests

**Test harness:** `crates/cirrus-server/tests/integration.rs`

**Test matrix:**

| Client | Test Command | Validation |
|--------|-------------|------------|
| AWS CLI v2 | `aws s3api create-bucket` | HTTP 200, bucket exists in ListBuckets |
| AWS CLI v2 | `aws s3 cp local.file s3://bucket/remote.file` | Round-trip: put then get returns identical bytes |
| AWS CLI v2 | `aws s3 sync local-dir s3://bucket/` | Multiple files uploaded, listed correctly |
| boto3 | `client.create_bucket()` + `client.put_object()` + `client.get_object()` | SDK-level assertions |
| boto3 | `client.create_multipart_upload()` + upload 3 parts + `client.complete_multipart_upload()` | Composite object retrievable |
| AWS SDK Rust | `aws_sdk_s3` with `endpoint_url` | Full Rust SDK compatibility |

**Integration test server lifecycle:**
```rust
async fn with_server<F, Fut>(test: F)
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = ()>,
{
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let router = cirrus_router::Router::new();
    let handle = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async { shutdown_rx.await.ok(); })
            .await.unwrap();
    });

    test(format!("http://127.0.0.1:{}", port)).await;

    let _ = shutdown_tx.send(());
    handle.await.ok();
}
```

### 10.3 Compatibility Tests

**Automated compatibility suite:**
- 50+ test cases covering all 16 supported operations
- Each test verified against real AWS S3 behavior (documented, not executed against real AWS in CI)
- Error case validation: verify exact XML structure and HTTP status codes

---

## 11. Performance Specification

### 11.1 Targets

| Metric | Target | Measurement Method |
|--------|--------|-------------------|
| Binary startup | <50 ms | `time cargo run` from cold start to first log line |
| HTTP response latency (empty bucket list) | <1 ms p99 | `wrk -t4 -c100 -d30s http://localhost:4566/` |
| HTTP response latency (1 MB object GET) | <5 ms p99 | `wrk` with pre-populated object |
| Concurrent connections | 10,000+ | `wrk` or custom load generator |
| Memory at idle | <8 MiB RSS | `ps -o rss= -p <pid>` immediately after startup |
| Memory per 1 MB object | ~1.05 MB | `Bytes` overhead + DashMap entry overhead |

### 11.2 Benchmarks

```rust
// benches/s3_benchmark.rs
use std::collections::HashMap;
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn bench_put_object(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let storage = DefaultStorage::default();
    // Pre-create bucket
    rt.block_on(storage.create_bucket("bench-bucket")).unwrap();
    let data = Bytes::from(vec![0u8; 1024 * 1024]); // 1 MB

    c.bench_function("put_object_1mb", |b| {
        b.to_async(&rt)
         .iter(|| async {
             storage.put_object("bench-bucket", "key", black_box(data.clone()),
                 HashMap::new(), "binary/octet-stream".to_string()).await;
         });
    });
}

fn bench_get_object(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let storage = DefaultStorage::default();
    let data = Bytes::from(vec![0u8; 1024 * 1024]); // 1 MB
    rt.block_on(async {
        storage.create_bucket("bench-bucket").await.unwrap();
        storage.put_object("bench-bucket", "key", black_box(data.clone()), HashMap::new(), "binary/octet-stream".to_string()).await.unwrap();
    });

    c.bench_function("get_object_1mb", |b| {
        b.to_async(&rt).iter(|| async {
            storage.get_object("bench-bucket", "key").await.unwrap();
        });
    });
}
```

> **Note:** `Bytes::clone()` is O(1) — only Arc refcount increment.

---

## 12. Security & Limits

### 12.1 Input Validation

| Input | Validation | Rejection |
|-------|-----------|-----------|
| Bucket name | 3-63 chars, lowercase alphanumeric + hyphens, start/end with alphanumeric | `InvalidBucketName` (400) |
| Object key | Max 1024 bytes, any UTF-8 | `KeyTooLong` (400) if exceeded |
| Object key (path chars) | `../` and `./` sequences valid — S3 treats keys as opaque strings | None (stored literally) |
| Request body | Max 100 MB | `EntityTooLarge` (400) |
| Part number | 1-10,000 | `InvalidArgument` (400) |
| Max parts | 10,000 per upload | `EntityTooLarge` (400) |
| XML depth | Max 50 levels | `MalformedXML` (400) |

### 12.2 Authentication

v0.1.0 does **not** validate SigV4 signatures. Any parseable `Authorization` header accepted. Credentials ignored.
- **Authorization header length:** Parsed headers exceeding 2 KB are rejected with `400 Bad Request`.

Pre-signed URL validation (if in scope) would verify:
- `X-Amz-Algorithm` = `AWS4-HMAC-SHA256`
- `X-Amz-Credential` scope matches service
- `X-Amz-Date` within 15 minutes of server time
- `X-Amz-Signature` matches computed signature

**v0.1.0:** Pre-signed URLs out of scope, signature validation deferred.

---

## 13. Logging & Observability

### 13.1 Log Format

JSON logging via `tracing-subscriber` (optional `json` feature; plain text default):

```json
{
  "timestamp": "2026-05-17T08:40:00.123Z",
  "level": "INFO",
  "target": "cirrus_router",
  "fields": {
    "method": "PUT",
    "path": "/my-bucket/my-key",
    "service": "s3",
    "status": 200,
    "duration_ms": 0.42
  }
}
```

### 13.2 Trace Spans

```rust
#[tracing::instrument(
    skip(self, req),
    fields(
        bucket = %bucket.as_deref().unwrap_or(""),
        key = %key.as_deref().unwrap_or(""),
        method = %req.method(),
    )
)]
async fn handle(&self, req: Request, bucket: Option<String>, key: Option<String>) -> Response {
    // ...
}
```

---

## 14. Versioning & Compatibility

### 14.1 API Version

Targets **Amazon S3 API version 2006-03-01** (canonical S3 API version used by all AWS SDKs).

### 14.2 SDK Compatibility Matrix

| SDK | Minimum Version | Test Coverage |
|-----|----------------|---------------|
| AWS CLI v2 | 2.15+ | Full test suite |
| boto3 | 1.34+ | Full test suite |
| AWS SDK for Rust | 1.0+ | Full test suite |
| AWS SDK for JavaScript v3 | 3.450+ | Smoke tests |
| AWS SDK for Go v2 | 1.25+ | Smoke tests |
| AWS SDK for Java v2 | 2.25+ | Smoke tests |

---

## 15. Open Questions & Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| AWS SDK strict XML parsing | High | Exhaustive compatibility testing against real SDKs; use SDK test vectors |
| Large object memory pressure | Medium | Enforce 100 MB limit; document memory-only constraint |
| Unicode/URL-encoded key edge cases | Medium | Property-based tests with `proptest` for key encoding |
| Abandoned multipart uploads | Low | No cleanup in v0.1.0; accumulates until restart |
| Concurrent multipart part upload ordering | Low | `BTreeMap<u32, S3Object>` ensures order; validate in tests |
| Axum fallback performance vs. explicit routes | Low | Benchmark; if fallback is slow, switch to explicit `any` route with manual dispatch |

---

## 16. Definition of Done

Done when:

- [ ] All 16 S3 operations pass unit tests with >85% coverage.
- [ ] All integration tests pass against AWS CLI v2, boto3, and AWS SDK Rust.
- [ ] `cargo build --release` produces a musl static binary <25 MB.
- [ ] Docker image builds successfully and is <50 MB.
- [ ] Server starts in <50 ms on bare metal, <20 ms in Docker.
- [ ] Idle RSS memory is <8 MiB.
- [ ] Non-S3 requests return valid `501 Not Implemented` XML that SDKs handle gracefully.
- [ ] CI passes: `cargo test`, `cargo clippy`, `cargo fmt --check`, integration tests.
- [ ] README documents installation, quick start, and supported operations.
- [ ] CHANGELOG.md created with v0.1.0 entry.
