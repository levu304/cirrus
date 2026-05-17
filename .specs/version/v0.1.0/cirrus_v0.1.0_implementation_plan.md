# Cirrus v0.1.0 Implementation Plan

> **Source specification:** `cirrus_v0.1.0_spec.md` (1270 lines)
> **Target:** Wire-compatible Amazon S3 API emulator on `:4566`, in-memory storage, statically-linked musl binary

---

## Table of Contents

1. [Build Order (Phases)](#1-build-order-phases)
2. [Dependency Graph](#2-dependency-graph)
3. [Phase Breakdowns](#3-phase-breakdowns)
   - [Phase 1: Project Scaffold & Crate Structure](#phase-1-project-scaffold--crate-structure)
   - [Phase 2: Protocol Layer (cirrus-protocol)](#phase-2-protocol-layer-cirrus-protocol)
   - [Phase 3: Storage Layer (cirrus-s3 — trait & DefaultStorage)](#phase-3-storage-layer-cirrus-s3)
   - [Phase 4: Router Layer (cirrus-router)](#phase-4-router-layer-cirrus-router)
   - [Phase 5: S3 Handlers (cirrus-s3 — 16 operations)](#phase-5-s3-handlers-cirrus-s3)
   - [Phase 6: Error Handling & Response Standardization](#phase-6-error-handling--response-standardization)
   - [Phase 7: Binary Entry Point (cirrus-server)](#phase-7-binary-entry-point-cirrus-server)
   - [Phase 8: Testing Suite](#phase-8-testing-suite)
   - [Phase 9: Docker & CI/CD](#phase-9-docker--cicd)
   - [Phase 10: Documentation & Polish](#phase-10-documentation--polish)
4. [Parallelism Opportunities](#4-parallelism-opportunities)
5. [Anti-Pattern Catalog](#5-anti-pattern-catalog)
6. [Verification Gate (Definition of Done)](#6-verification-gate)
7. [Parallel Step Detection Summary](#7-parallel-step-detection-summary)

---

## 1. Build Order (Phases)

| # | Phase | Crate(s) | Estimated Effort | Dependencies |
|---|-------|----------|-----------------|--------------|
| 1 | Project Scaffold & Crate Structure | (workspace) | Small | None |
| 2 | Protocol Layer | `cirrus-protocol` | Medium | Phase 1 |
| 3 | Storage Layer | `cirrus-s3` (storage.rs) | Medium | Phase 1, 2 |
| 4 | Router Layer | `cirrus-router` | Medium | Phase 1, 2 |
| 5 | S3 Handlers | `cirrus-s3` (handlers, service, multipart) | Large | Phase 1, 2, 3, 4 |
| 6 | Error Handling & Response Standardization | Cross-crate | Small | Phase 2, 3, 4, 5 |
| 7 | Binary Entry Point | `cirrus-server` | Small | Phase 4, 5, 6 |
| 8 | Testing Suite | All crates | Large | Phase 5, 6, 7 |
| 9 | Docker & CI/CD | (infra) | Medium | Phase 7 |
| 10 | Documentation & Polish | (docs) | Small | Phase 7, 8 |

---

## 2. Dependency Graph

```
Phase 1 (Scaffold)
  │
  ├──────────────────┐
  ▼                  ▼
Phase 2 (Protocol)  │ (can start immediately after Phase 1)
  │                  │
  ├──────┬───────────┘
  ▼      ▼
Phase 3  Phase 4
(Storage)(Router)
  │      │
  └──┬───┘
     ▼
  Phase 5 (S3 Handlers)
     │
     ▼
  Phase 6 (Error Standardization)
     │
     ▼
  Phase 7 (Binary Entry Point)
     │
     ├──────────┐
     ▼          ▼
  Phase 8     Phase 9
  (Testing)   (Docker/CI)
     │          │
     └────┬─────┘
          ▼
     Phase 10 (Docs/Polish)
```

**File-level dependencies:**

| Edge | Rationale |
|------|-----------|
| Phase 2 → Phase 1 | Uses workspace Cargo.toml dependency inheritance |
| Phase 3 → Phase 2 | `S3Object` type (§4.3) used in storage data structures; `AwsError` used for error mapping in `S3Error` (§6.2) |
| Phase 4 → Phase 2 | `AwsError::NotImplemented` used in `fallback_handler` for non-S3 services |
| Phase 5 → Phase 2 | All handler responses use `cirrus-protocol` XML types and `AwsError` |
| Phase 5 → Phase 3 | Handlers call `Storage` trait methods defined in Phase 3 |
| Phase 5 → Phase 4 | Handler dispatch logic depends on address resolution from Phase 4 |
| Phase 6 → Phase 5 | Error standardization requires all handlers to be built first |
| Phase 7 → Phase 4 | `cirrus-router::Router::new()` used to assemble server |
| Phase 7 → Phase 5 | `S3Service::<DefaultStorage>` registered in `ServiceRegistry` |
| Phase 8 → Phase 5, 6, 7 | Tests exercise handlers, errors, and the binary entry point |
| Phase 9 → Phase 7 | Dockerfile builds the `cirrus-server` binary |
| Phase 10 → Phase 7, 8 | Docs describe what was built and verified |

---

## 3. Phase Breakdowns

---

### Phase 1: Project Scaffold & Crate Structure

**Objective:** Create the Cargo workspace with all 4 crates, dependency declarations, linter/format configuration, and project hygiene files. After this phase, `cargo build` compiles (with lib crates returning empty stubs).

**Files to create/modify:**

| File | Action |
|------|--------|
| `cirrus/Cargo.toml` | Create — workspace root |
| `cirrus/crates/cirrus-protocol/Cargo.toml` | Create — lib crate |
| `cirrus/crates/cirrus-protocol/src/lib.rs` | Create — empty, `// Phase 2` |
| `cirrus/crates/cirrus-s3/Cargo.toml` | Create — lib crate |
| `cirrus/crates/cirrus-s3/src/lib.rs` | Create — empty, `// Phase 3` |
| `cirrus/crates/cirrus-router/Cargo.toml` | Create — lib crate |
| `cirrus/crates/cirrus-router/src/lib.rs` | Create — empty, `// Phase 4` |
| `cirrus/crates/cirrus-server/Cargo.toml` | Create — bin crate |
| `cirrus/crates/cirrus-server/src/main.rs` | Create — `fn main() { println!("Cirrus v0.1.0"); }` |
| `cirrus/.gitignore` | Create |
| `cirrus/rust-toolchain.toml` | Create |
| `cirrus/.editorconfig` | Create (optional but good practice) |

**Implementation steps:**

1. **Create workspace root `Cargo.toml`** with all dependencies from §9.1 of the spec. Use workspace inheritance for `version`, `edition`, `rust-version`, `authors`, `license`, `repository`. Set `members = ["crates/*"]`, `resolver = "2"`, `edition = "2024"`, `rust-version = "1.85"`.

2. **Create `cirrus-protocol/Cargo.toml`**: Inherits workspace package fields. Dependencies: `serde` (with derive), `quick-xml` (with serialize/deserialize), `chrono` (with serde), `thiserror`, `tracing`, `bytes`, `http`, `uuid` (with v4), `sha2`, `base64`, `md-5`, `urlencoding`. All use workspace inheritance.

3. **Create `cirrus-s3/Cargo.toml`**: Dependencies: `cirrus-protocol`, `tokio` (with full features), `dashmap`, `bytes`, `chrono`, `serde`, `quick-xml`, `thiserror`, `async-trait`, `sha2`, `base64`, `md-5`, `http`, `uuid`, `tracing`, `urlencoding`. Dev-deps: `proptest`. All via workspace inheritance.

4. **Create `cirrus-router/Cargo.toml`**: Dependencies: `cirrus-protocol`, `tokio` (with rt, rt-multi-thread, net, macros, signal), `axum`, `hyper`, `http`, `http-body`, `http-body-util`, `bytes`, `tower`, `tower-http` (with trace, limit), `tracing`, `urlencoding`, `serde`, `uuid`.

5. **Create `cirrus-server/Cargo.toml`**: Dependencies: `cirrus-router`, `cirrus-s3`, `cirrus-protocol`, `tokio` (with full), `axum`, `tracing`, `tracing-subscriber` (with env-filter, json), `clap` (with derive), `figment` (with env), `http`, `bytes`.

6. **Create `.gitignore`**: `target/`, `.DS_Store`, `*.swp`, `.env`, `*.log`, `benchmark_results/`.

7. **Create `rust-toolchain.toml`**: `[toolchain] channel = "1.85" components = ["rustfmt", "clippy"]`.

8. **Write stub `lib.rs`** files: Each crate lib.rs should declare a public module structure matching the planned source files (e.g., `pub mod types;` in `cirrus-protocol`) but with empty module bodies for now. This lets `cargo build` succeed.

9. **Write stub `main.rs`**: `fn main() { println!("Cirrus v0.1.0"); }`.

10. **Verify**: `cargo build --workspace` succeeds. `cargo clippy --workspace` passes with no warnings.

**Key design decisions:**
- Use **workspace dependency inheritance** for all shared deps — each crate's Cargo.toml references `{ workspace = true }`.
- Edition 2024, resolver 2 — modern Rust.
- `circus-protocol` has NO runtime deps beyond `quick-xml` + `serde` — it's a pure type/XML crate.

**Verification criteria:**
- [ ] `cargo build --workspace` compiles without errors
- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo fmt --check` passes
- [ ] `./target/debug/cirrus-server` prints banner and exits
- [ ] Directory structure matches §9.2 exactly

---

### Phase 2: Protocol Layer (cirrus-protocol)

**Objective:** Build the shared type library that all other crates depend on. This includes AWS XML response types, the `AwsError` enum with XML serialization, shared utilities (XML escaping, date formatting, ETag formatting), and all 10 XML schemas from §4.2.

**Files to create/modify:**

| File | Action |
|------|--------|
| `crates/cirrus-protocol/src/lib.rs` | Modify — declare modules |
| `crates/cirrus-protocol/src/types.rs` | Create — Owner, S3Object, XML response types |
| `crates/cirrus-protocol/src/error.rs` | Create — AwsError enum, XML serialization, error code mapping |
| `crates/cirrus-protocol/src/xml.rs` | Create — xml_escape(), date formatting, ETag helpers |

**Implementation steps:**

1. **`lib.rs`**: Declare `pub mod types; pub mod error; pub mod xml;`.

2. **`types.rs` — Define shared types:**
   - `Owner { id: String, display_name: String }` — used in ListBuckets, ListParts.
   - `S3Object` (§4.3) — full struct with `data: Bytes`, `etag`, `content_type`, `content_length`, `last_modified`, `metadata`.
   - XML response structs for all 10 schemas (§4.2):
     - `ListAllMyBucketsResult { owner: Owner, buckets: Vec<BucketInfo> }`
     - `BucketInfo { name: String, creation_date: DateTime<Utc> }`
     - `CreateBucketOutput { location: String }`
     - `ListBucketResult` (for ListObjectsV2)
     - `Delete` (request for DeleteObjects) + `DeleteResult`
     - `CopyObjectResult { etag: String, last_modified: DateTime<Utc> }`
     - `InitiateMultipartUploadResult { bucket, key, upload_id }`
     - `CompleteMultipartUpload` (request) + `CompleteMultipartUploadResult`
     - `ListPartsResult`
     - `LocationConstraint`
   - Each response struct derives `Serialize` from `quick-xml` (via `serde`). Use `#[serde(rename = "ElementName")]` to match AWS exact XML element names.
   - **IMPORTANT:** Empty elements must serialize as `<E></E>` not `<E/>`. `quick-xml` with `serde` may emit self-closing for `Option::None` and empty strings. Test this. The fix is to use `#[serde(skip_serializing_if = "Option::is_none")]` on optional fields, and ensure default-empty strings are written as explicit open/close tags. You may need custom serialization functions that emit `Some("")` for empty values to force explicit tags.

3. **`error.rs` — AwsError enum:**
   ```rust
   pub enum AwsError {
       NotImplemented { message: String },
       NoSuchBucket { bucket_name: String },
       NoSuchKey { key: String },
       NoSuchUpload { upload_id: String },
       BucketAlreadyExists { bucket_name: String },
       BucketNotEmpty { bucket_name: String },
       InvalidArgument { message: String, argument_name: Option<String>, argument_value: Option<String> },
       MethodNotAllowed,
       EntityTooLarge,
       MalformedXML { message: String },
       InternalError { message: String },
       InvalidBucketName { bucket_name: String },
       KeyTooLong,
       InvalidPart { upload_id: String, part_number: u32, etag: String },
       IncompleteBody,
   }
   ```
   - Implement `AwsError::to_xml(&self) -> String` that produces the XML from §6.1.
   - Map each variant to its HTTP status code per §6.2.
   - Implement specific error XML schemas from §6.3 (InvalidArgument with `list-type`, BucketNotEmpty with BucketName, InvalidPart with UploadId/PartNumber/ETag).
   - Implement `IntoResponse for AwsError` (using `axum::http::StatusCode` + `(StatusCode, [(header, value)], body_string)`).
   - Each `AwsError` includes `request_id: String` and `host_id: String` fields (set per-request, not at construction time — use a builder or setter).

4. **`error.rs` — Error code mapping helper:**
   - `fn status_code(&self) -> StatusCode` mapping each variant per §6.2.
   - `fn error_code(&self) -> &str` returning the AWS error code string (e.g., `"NoSuchBucket"`).

5. **`xml.rs` — Utility functions:**
   - `xml_escape(s: &str) -> String` (§7.1): replaces `&`, `<`, `>`, `"`, `'` with XML entities.
   - `format_timestamp(dt: &DateTime<Utc>) -> String` (§7.2): ISO 8601 with milliseconds (`2026-05-17T08:40:00.000Z`).
   - `format_http_date(dt: &DateTime<Utc>) -> String` (§7.2): IMF-fixdate RFC 7231 (`Sun, 17 May 2026 08:40:00 GMT`).
   - `format_etag(digest: &[u8]) -> String` (§7.3): wraps hex MD5 in quotes: `"d41d8cd98f00b204e9800998ecf8427e"`.
   - `consistency_check()` test: verify XML escape round-trips, date format produces expected strings.

6. **Hand-builders for small responses** (§7.1 performance rule):
   - For responses <1 KB (error responses, CopyObjectResult, etc.), use `format!()` with `xml_escape()` instead of `quick-xml` serde. This avoids serde overhead on hot paths.
   - Mark which responses use hand-builders vs. quick-xml in comments.

7. **Tests** (inline in each module):
   - `xml_escape` does not double-escape already-escaped strings.
   - Date formats match expected patterns (use regex test).
   - ETag format is exactly two quotes around 32 hex chars.
   - Each AwsError variant produces valid XML.
   - Round-trip: parse known-good XML → struct → serialize → matches original.

**Key design decisions:**
- **Error responses are hand-built with `format!()` + `xml_escape()`** for performance. Complex responses (ListBuckets, ListObjectsV2) use quick-xml serde.
- **Empty XML elements** must be `<E></E>` not `<E/>`. Handle this explicitly. For hand-built responses, always write open+close tags. For quick-xml serde, test with empty strings and ensure they produce `<E></E>` — if not, write custom serializers.
- **AwsError is the "last mile" type** — it directly implements `IntoResponse`. Storage errors (`S3Error`) are mapped to `AwsError` at the handler layer.
- **No runtime dependencies** beyond quick-xml + serde + chrono + bytes. This crate is lightweight and fast to compile.

**Verification criteria:**
- [ ] `cargo build -p cirrus-protocol` succeeds
- [ ] `cargo test -p cirrus-protocol` passes all unit tests
- [ ] `cargo clippy -p cirrus-protocol -- -D warnings` passes
- [ ] `AwsError::to_xml()` for each variant produces valid XML
- [ ] XML escape handles all 5 characters correctly (including `'` which is often missed)
- [ ] Date format produces `2026-05-17T08:40:00.000Z` format for XML
- [ ] HTTP date format produces `Sun, 17 May 2026 08:40:00 GMT` format

---

### Phase 3: Storage Layer (cirrus-s3)

**Objective:** Implement the in-memory storage backend. Define the `Storage` trait, all data structures (Bucket, S3Object, MultipartUpload), the `DefaultStorage` implementation using DashMap, memory management with `AtomicU64`, and the concurrency model. This is the data plane — no HTTP, no handlers, just storage operations.

**Files to create/modify:**

| File | Action |
|------|--------|
| `crates/cirrus-s3/src/lib.rs` | Modify — declare modules |
| `crates/cirrus-s3/src/storage.rs` | Create — Storage trait, data structures, DefaultStorage, S3Error |

**Implementation steps:**

1. **`lib.rs`**: Declare `pub mod storage;`.

2. **`storage.rs` — Data structures** (§5.1):
   ```rust
   pub struct Bucket {
       name: String,
       created_at: DateTime<Utc>,
       objects: DashMap<String, S3Object>,
       multipart_uploads: DashMap<String, MultipartUpload>,
   }
   ```
   - `S3Object` — import from `cirrus_protocol::types`. Add `Clone` derive.
   - `MultipartUpload { bucket, key, upload_id, initiated, parts: BTreeMap<u32, S3Object> }` — `#[derive(Debug)]`, manual Clone not needed.
   - `BucketInfo { name, created_at }` — response type.
   - `S3ObjectInfo { key, last_modified, etag, size, storage_class }` — response type.
   - `ObjectList { name, prefix, max_keys, key_count, objects, common_prefixes, is_truncated, next_continuation_token }` — response type.
   - `GetObjectResult { data, info, metadata }` — response type.
   - `PartsList { parts, is_truncated, next_part_number_marker, max_parts }` — response type.
   - `PartInfo { part_number, last_modified, etag, size }` — response type.
   - `MultipartUploadInfo { bucket, key, upload_id, initiated }` — response type.
   - `MetadataDirective { Copy, Replace }` enum with `Default = Copy`.
   - `Default for MetadataDirective` -> `MetadataDirective::Copy`.

3. **`storage.rs` — S3Error enum** (§5.4):
   - Define `#[derive(thiserror::Error, Debug)] pub enum S3Error` with all variants from §5.4:
     `NoSuchBucket(String)`, `NoSuchKey(String)`, `NoSuchUpload(String)`, `BucketAlreadyExists(String)`, `BucketNotEmpty(String)`, `InvalidArgument(String)`, `MethodNotAllowed`, `EntityTooLarge`, `MalformedXML`, `InternalError`, `InvalidBucketName(String)`, `KeyTooLong`, `InvalidPart(String, u32, String)`, `IncompleteBody`.
   - Implement `Display` via `thiserror`.

4. **`storage.rs` — Storage trait** (§5.4):
   ```rust
   #[async_trait]
   pub trait Storage: Send + Sync {
       async fn create_bucket(&self, name: &str) -> Result<(), S3Error>;
       async fn delete_bucket(&self, name: &str) -> Result<(), S3Error>;
       async fn list_buckets(&self) -> Result<Vec<BucketInfo>, S3Error>;
       async fn put_object(&self, bucket: &str, key: &str, data: Bytes,
           metadata: HashMap<String, String>, content_type: String) -> Result<S3ObjectInfo, S3Error>;
       async fn get_object(&self, bucket: &str, key: &str) -> Result<GetObjectResult, S3Error>;
       async fn delete_object(&self, bucket: &str, key: &str) -> Result<(), S3Error>;
       async fn head_object(&self, bucket: &str, key: &str) -> Result<S3ObjectInfo, S3Error>;
       async fn copy_object(&self, src_bucket: &str, src_key: &str,
           dest_bucket: &str, dest_key: &str, metadata: HashMap<String, String>,
           directive: MetadataDirective) -> Result<S3ObjectInfo, S3Error>;
       async fn create_multipart_upload(&self, bucket: &str, key: &str)
           -> Result<MultipartUploadInfo, S3Error>;
       async fn upload_part(&self, bucket: &str, key: &str, upload_id: &str,
           part_number: u32, data: Bytes) -> Result<PartInfo, S3Error>;
       async fn complete_multipart_upload(&self, bucket: &str, key: &str,
           upload_id: &str, parts: &[(u32, String)]) -> Result<S3ObjectInfo, S3Error>;
       async fn abort_multipart_upload(&self, bucket: &str, key: &str,
           upload_id: &str) -> Result<(), S3Error>;
       async fn list_parts(&self, bucket: &str, key: &str, upload_id: &str,
           max_parts: Option<u32>, part_number_marker: Option<u32>) -> Result<PartsList, S3Error>;
       async fn list_objects(&self, bucket: &str, prefix: &str, delimiter: &str,
           max_keys: u32, start_after: Option<&str>, continuation_token: Option<&str>)
           -> Result<ObjectList, S3Error>;
       async fn get_bucket_location(&self, name: &str) -> Result<String, S3Error>;
   }
   ```

5. **`storage.rs` — DefaultStorage struct** (§5.4):
   ```rust
   pub struct DefaultStorage {
       buckets: Arc<DashMap<String, Bucket>>,
       total_bytes: Arc<AtomicU64>,
   }
   ```
   - **Manual `Clone` impl** (DashMap doesn't impl Clone):
     ```rust
     impl Clone for DefaultStorage {
         fn clone(&self) -> Self {
             Self { buckets: Arc::clone(&self.buckets), total_bytes: Arc::clone(&self.total_bytes) }
         }
     }
     ```
   - **`Default` impl** that creates empty DashMap and zero AtomicU64.

6. **`storage.rs` — S3Service generic wrapper** (§5.4):
   ```rust
   pub struct S3Service<S: Storage = DefaultStorage> {
       pub store: S,
   }
   ```
   - `impl Default for S3Service<DefaultStorage>` using `DefaultStorage::default()`.
   - `impl<S: Storage> S3Service<S> { pub fn new(store: S) -> Self }`
   - `impl<S: Storage> AwsService for S3Service<S>` — delegates `handle()` to internal dispatch.

7. **Implement each Storage trait method on DefaultStorage** following §5.2 concurrency model and §5.3 memory management:

   - **`create_bucket`**: Check if bucket `name` exists in `buckets`. If so, return `BucketAlreadyExists`. Otherwise insert new Bucket with empty DashMaps.
   - **`delete_bucket`**: Use atomic `buckets.remove(name)` (§5.2). Check removed bucket's `objects.is_empty() && multipart_uploads.is_empty()`. If empty, drop it. If not empty, re-insert and return `BucketNotEmpty`.
   - **`list_buckets`**: Iterate all buckets, collect `BucketInfo` vec.
   - **`put_object`**: ETag = md5 hex of data. Check `total_bytes + data.len() <= CIRRUS_MAX_MEMORY` (access via const or config). Increment `total_bytes`. Insert object into bucket's objects DashMap.
   - **`get_object`**: Look up bucket, then key. Return data + info + metadata.
   - **`delete_object`**: Remove from objects DashMap. Decrement `total_bytes` by object size.
   - **`head_object`**: Same as get_object but return only info (no data).
   - **`copy_object`**: Source lookup, clone Bytes (O(1) Arc bump), handle metadata directive. IMPORTANT: Do NOT increment `total_bytes` (§5.3).
   - **`create_multipart_upload`**: Generate UploadId via `base64url(sha256("{bucket}:{key}:{timestamp}:{random_u64}"))`. Insert into multipart_uploads DashMap.
   - **`upload_part`**: ETag = md5 hex. Insert into parts BTreeMap at `part_number`.
   - **`complete_multipart_upload`**: Validate all part numbers exist with matching ETags (§4.5). Concatenate data in order. Compute composite ETag: `md5(all_concatenated_data)` + `-N`. Store as regular object. Delete multipart state.
   - **`abort_multipart_upload`**: Delete parts (decrement total_bytes). Remove multipart entry.
   - **`list_parts`**: Query parts, paginate with part-number-marker and max-parts.
   - **`list_objects`**: Full pagination logic from §4.2.2: filter by prefix, start-after, continuation-token (base64 decode), sort, take max-keys, calculate is_truncated + next_continuation_token. Handle delimiter → CommonPrefixes.
   - **`get_bucket_location`**: Verify bucket exists, return static `"us-east-1"`.

8. **Memory management helpers** (§5.3):
   - Define constants: `CIRRUS_MAX_OBJECT_SIZE = 100 * 1024 * 1024`, `CIRRUS_MAX_MEMORY = 512 * 1024 * 1024`, `MAX_PARTS = 10_000`.
   - Helper: `check_memory(total_bytes: &AtomicU64, size: u64) -> bool` - checks if adding `size` exceeds limit.

**Key design decisions:**
- **DashMap does NOT implement Clone.** Wrap in `Arc` and implement manual Clone.
- **DeleteBucket atomic pattern** (§5.2): `remove()` then check emptiness, re-insert on failure. This avoids ABBA deadlock between a hypothetical `delete_guard` RwLock and DashMap shard locks.
- **`Bytes::clone()` is O(1)** — CopyObject shares underlying allocation. Do NOT count copied bytes in `total_bytes`.
- **S3Service is generic** over `S: Storage` to allow `MockStorage` in tests.
- **Pagination tokens**: ContinuationToken is `base64(key_bytes)`. NextContinuationToken is `base64(last_returned_key_bytes)`.

**Verification criteria:**
- [ ] `cargo build -p cirrus-s3` succeeds
- [ ] `DefaultStorage::default()` creates empty store
- [ ] `DefaultStorage::clone()` compiles and produces independent reference to same data
- [ ] All 16 Storage trait methods compile
- [ ] Manual `Clone` impl works correctly (both fields Arc::clone)
- [ ] `total_bytes` increments/decrements correctly across put/delete

---

### Phase 4: Router Layer (cirrus-router)

**Objective:** Build the HTTP router layer. This handles TCP binding, request identification (SigV4 service extraction), S3 address resolution (path-style vs virtual-hosted), body size limiting, incomplete body detection, and the fallback dispatch mechanism with `ServiceRegistry`.

**Files to create/modify:**

| File | Action |
|------|--------|
| `crates/cirrus-router/src/lib.rs` | Modify — declare modules, Router builder |
| `crates/cirrus-router/src/service.rs` | Create — SigV4 extraction, service identification |
| `crates/cirrus-router/src/address.rs` | Create — S3 address resolution |
| `crates/cirrus-router/src/middleware.rs` | Create — body limit, incomplete body detection |

**Implementation steps:**

1. **`lib.rs` — Public API:**
   ```rust
   pub fn build_router(registry: Arc<ServiceRegistry>) -> axum::Router { ... }
   ```
   This constructs the full axum Router with middleware layers and the fallback handler.

2. **`lib.rs` — ServiceRegistry + AwsService trait** (§3.3):
   ```rust
   pub struct ServiceRegistry {
       pub services: HashMap<String, Box<dyn AwsService>>,
   }
   impl ServiceRegistry {
       pub fn new() -> Self { Self { services: HashMap::new() } }
       pub fn register<S: AwsService + 'static>(&mut self, name: &str, service: S) {
           self.services.insert(name.to_string(), Box::new(service));
       }
   }
   #[async_trait]
   pub trait AwsService: Send + Sync {
       async fn handle(&self, req: Request<Body>) -> Response<Body>;
   }
   ```

3. **`lib.rs` — fallback_handler** (§3.3):
   - Extract service via `extract_service(&req)`.
   - Look up in registry.services.
   - If found, delegate to `handler.handle(req).await`.
   - If not found, return `AwsError::NotImplemented { message: format!("Service '{}' not implemented in v0.1.0", service) }` via `IntoResponse`. Error response MUST be valid AWS XML with `Content-Type: application/xml`.

4. **`lib.rs` — Router assembly** (§3.1):
   - Use `axum::Router::new()`.
   - Add `.layer(axum::middleware::from_fn(incomplete_body_detection))` wrapping the body tracking middleware.
   - Add `.layer(tower_http::limit::RequestBodyLimitLayer::new(max_request_bytes))` — configure from constant or config.
   - Add `.with_state(Arc::<ServiceRegistry>::clone(&registry))`.
   - Add `.fallback(fallback_handler)` — single fallback, no per-path routes.
   - Use `tower::ServiceBuilder` to compose middleware layers in correct order.

5. **`service.rs` — SigV4 service extraction** (§3.2.1):
   - `extract_service(req: &Request) -> String`:
     1. Check `Authorization` header for `Credential=` scope. Extract 5th `/`-separated component. Return it.
     2. If no Auth header, check `X-Amz-Target` header. Extract first part before `.` (protocol/service).
     3. If no target, check `?Service=` query parameter.
     4. Default fallback: `"s3"`.
   - Use regex or manual string parsing for SigV4 credential extraction (manual parsing is preferred to avoid regex dependency — split on `/`, find `Credential=`).
   - Test with valid AWS SigV4 format, missing header, malformed credential.

6. **`address.rs` — S3 address resolution** (§3.2.2):
   - `resolve_address(req: &Request, base_host: &str) -> (Option<String>, Option<String>)`:
     1. Get Host header, strip port.
     2. Check if host ends with `.{base_host}` → virtual-hosted mode: bucket = host without suffix, key = URI path.
     3. Else path-style: split path on first `/`, decode each segment with `urlencoding::decode`.
   - The `urlencoding` crate handles percent-decoding. Use `urlencoding::decode()` on path-style segments.
   - Test: path-style, virtual-hosted, empty path (root `/`), URL-encoded keys, Unicode keys.
   - **CRITICAL:** URL-decode path segments to handle `%20`, `%2F`, etc. This was a known bug (P1-6 fix) that must NOT be skipped.

7. **`middleware.rs` — Request body limit interceptor** (§3.1):
   - `tower_http::limit::RequestBodyLimitLayer` returns bare 413. We need to intercept that and return AWS XML `EntityTooLarge`.
   - Implementation: A middleware that wraps the body stream. If body exceeds limit, catch the error and convert to `AwsError::EntityTooLarge.into_response()`. Use `axum::middleware::from_fn` pattern.
   - See pattern: wrap inner service, check response status for 413, replace with error body.

8. **`middleware.rs` — IncompleteBody detection** (§3.1):
   - Custom middleware that wraps the request body stream to count bytes consumed.
   - Compare against `Content-Length` header at EOF.
   - If fewer bytes received than declared, return `AwsError::IncompleteBody` (400) with AWS XML.
   - For chunked transfer encoding (no Content-Length), skip this check.
   - Implementation approach: wrap `hyper::Body` in a custom `Body` that tracks bytes read and compares at EOF.

**Key design decisions:**
- **No global/static SERVICE_REGISTRY** — use `axum::State` extractor with `Arc<ServiceRegistry>`. This is a critical fix (§15).
- **Single fallback handler** — no per-path axum routes. All dispatch is manual based on parsed (service, method, bucket, key, query).
- **Body limit middleware is custom** — `tower_http::limit` returns bare 413, we must convert to AWS XML `EntityTooLarge`.
- **Host header trusted directly** in v0.1.0 — no X-Forwarded-Host handling. Document this as a known limitation.
- **Default fallback is `"s3"`** — for pre-signed URLs and SDK calls that omit service markers.

**Verification criteria:**
- [ ] `cargo build -p cirrus-router` succeeds
- [ ] `extract_service` correctly identifies `"s3"` from SigV4 Authorization header
- [ ] `extract_service` returns `"s3"` as default when no auth header present
- [ ] `resolve_address` correctly handles path-style and virtual-hosted
- [ ] `resolve_address` URL-decodes `%20` → space in keys
- [ ] `fallback_handler` returns 501 for unknown service with valid AWS XML
- [ ] Router compiles as an `axum::Router` ready for serving
- [ ] Middleware layers compose correctly

---

### Phase 5: S3 Handlers (cirrus-s3)

**Objective:** Implement all 16 S3 operations as handler functions. Each handler parses the request, calls the Storage trait, serializes the response XML, and sets correct HTTP headers. This is the largest phase.

**Files to create/modify:**

| File | Action |
|------|--------|
| `crates/cirrus-s3/src/lib.rs` | Modify — add modules |
| `crates/cirrus-s3/src/handlers.rs` | Create — per-operation handler functions |
| `crates/cirrus-s3/src/service.rs` | Create — S3Service dispatch, routing logic |
| `crates/cirrus-s3/src/multipart.rs` | Create — multipart upload helpers |

**Implementation steps:**

1. **`lib.rs`**: Add `pub mod handlers; pub mod service; pub mod multipart;`. Re-export `S3Service`, `DefaultStorage`, `Storage` from `storage`.

2. **`service.rs` — S3 dispatch**:
   - `impl<S: Storage> AwsService for S3Service<S>`:
     ```rust
     #[async_trait]
     impl<S: Storage> AwsService for S3Service<S> {
         async fn handle(&self, req: Request<Body>) -> Response<Body> { ... }
     }
     ```
   - Inside `handle()`:
     1. Call `resolve_address(&req, BASE_HOST)` to get (bucket, key).
     2. Match on `(req.method(), &bucket, &key, req.uri().query())` to determine operation.
     3. Delegate to the appropriate handler function.
   - Dispatch rules from §4.1:
     - `GET /` → `handle_list_buckets`
     - `PUT /{bucket}` → `handle_create_bucket`
     - `DELETE /{bucket}` → `handle_delete_bucket`
     - `GET /{bucket}?list-type=2` → `handle_list_objects_v2`
     - `PUT /{bucket}/{key}` + `x-amz-copy-source` header → `handle_copy_object`
     - `PUT /{bucket}/{key}` (no copy-source) → `handle_put_object`
     - `GET /{bucket}/{key}` → `handle_get_object`
     - `HEAD /{bucket}/{key}` → `handle_head_object`
     - `DELETE /{bucket}/{key}` → `handle_delete_object`
     - `POST /{bucket}?delete` → `handle_delete_objects`
     - `POST /{bucket}/{key}?uploads` → `handle_create_multipart_upload`
     - `PUT /{bucket}/{key}?partNumber=N&uploadId=ID` → `handle_upload_part`
     - `POST /{bucket}/{key}?uploadId=ID` → `handle_complete_multipart_upload`
     - `DELETE /{bucket}/{key}?uploadId=ID` → `handle_abort_multipart_upload`
     - `GET /{bucket}/{key}?uploadId=ID&max-parts=M&part-number-marker=N` → `handle_list_parts`
     - `GET /{bucket}?location` → `handle_get_bucket_location`
     - Default → `AwsError::MethodNotAllowed`
   - **IMPORTANT:** The dispatch must check `x-amz-copy-source` BEFORE URL decoding the header (CopyObject vs PutObject distinction — §4.1 note). URL decoding happens after dispatch in the handler.

3. **`handlers.rs` — Response header helpers:**
   - `fn response_headers(request_id: &str) -> [(HeaderName, HeaderValue); 2]`
     - `x-amz-request-id`: UUID per request.
     - `x-amz-id-2`: static `"cirrus-v0.1.0"`.
   - **Every response must include these**. Apply them centrally.

4. **Handlers — implement each of 16 operations:**

   - **`handle_list_buckets`** (§4.1 #1, §4.2.1):
     - Call `storage.list_buckets().await`
     - Build `ListAllMyBucketsResult` XML with `Owner { ID: "000000000000", DisplayName: "webfile" }`
     - Return 200 with `Content-Type: application/xml`

   - **`handle_create_bucket`** (§4.1 #2, §4.2.1a):
     - Validate bucket name (3-63 chars, lowercase alphanumeric + hyphens, start/end with alphanumeric per §12.1). Return `InvalidBucketName` if invalid.
     - Parse optional `CreateBucketConfiguration` XML body. If present and malformed → `MalformedXML`. Ignore region value per spec note.
     - Call `storage.create_bucket(name).await`
     - Return 200 (not 201!) with `Location: http://{host}:{port}/{bucket}` header. Empty body for us-east-1.

   - **`handle_delete_bucket`** (§4.1 #3):
     - Call `storage.delete_bucket(name).await`
     - Return 204 No Content (no body).
     - Handle `BucketNotEmpty` → AwsError::BucketNotEmpty.

   - **`handle_list_objects_v2`** (§4.1 #4, §4.2.2):
     - **Must check `list-type=2`** query param first. Missing → return `AwsError::InvalidArgument` with `list-type` argument field (§6.3).
     - Parse query params: `prefix`, `delimiter`, `max-keys` (default 1000, clamp 1-1000), `continuation-token`, `start-after`.
     - Call `storage.list_objects(...).await`
     - Build `ListBucketResult` XML. Echo request params in response elements (empty for defaults).
     - Pagination: is_truncated + next_continuation_token.

   - **`handle_put_object`** (§4.1 #5, §4.3):
     - Extract `Content-Type` (default `binary/octet-stream`), `Content-Length` (validate), `x-amz-meta-*` headers (strip `\r\n`).
     - Compute `Content-Length` from body bytes; if mismatch → `IncompleteBody`.
     - Read entire body into `Bytes`. Check size against `MAX_OBJECT_SIZE`. Exceeded → `EntityTooLarge`.
     - Compute ETag: md5 of body bytes.
     - Call `storage.put_object(...).await`.
     - Return 200 with `ETag` header (quoted hex), `x-amz-server-side-encryption: AES256` echo.

   - **`handle_get_object`** (§4.1 #6, §4.3):
     - Call `storage.get_object(...).await`.
     - Return 200 with object bytes as body. Set headers: `Content-Type`, `Content-Length`, `ETag`, `Last-Modified`, `Accept-Ranges: bytes`, `x-amz-meta-*`.

   - **`handle_head_object`** (§4.1 #7, §4.3):
     - Call `storage.head_object(...).await`.
     - Return 200 with same headers as GetObject but EMPTY body.

   - **`handle_delete_object`** (§4.1 #8):
     - Call `storage.delete_object(...).await`.
     - Return 204 No Content. (Note: S3 returns 204 even if key doesn't exist — it's idempotent. However, if bucket doesn't exist, return 404.)

   - **`handle_delete_objects`** (§4.1 #9, §4.2.3, §4.2.4):
     - Parse `Delete` XML from request body.
     - Extract `<Object><Key>` elements. Ignore `<VersionId>`.
     - Check `<Quiet>` flag.
     - Sequentially call `storage.delete_object(...)` per key (non-transactional per §4.1 note).
     - Collect results: deleted keys in `<Deleted>`, failed keys in `<Error>`.
     - Build `DeleteResult` XML. If quiet mode, omit `<Deleted>` elements.
     - Return 200.

   - **`handle_copy_object`** (§4.1 #10, §4.2.5, §4.4):
     - Check for `x-amz-copy-source` header. URL-decode it AFTER dispatch identification.
     - Parse source: strip leading `/`, split into (bucket, key).
     - Check `x-amz-metadata-directive` header (`COPY` default, `REPLACE` override).
     - If REPLACE, extract metadata from current request headers.
     - Call `storage.copy_object(...).await`.
     - Build `CopyObjectResult` XML.
     - Return 200.

   - **`handle_create_multipart_upload`** (§4.1 #11, §4.2.6):
     - Call `storage.create_multipart_upload(...).await`.
     - Build `InitiateMultipartUploadResult` XML.
     - Return 200.

   - **`handle_upload_part`** (§4.1 #12):
     - Parse `partNumber` (u32, 1-10000) and `uploadId` from query params.
     - Validate part number range.
     - Read part body into `Bytes`. Compute ETag.
     - Call `storage.upload_part(...).await`.
     - Return 200 with `ETag` header.

   - **`handle_complete_multipart_upload`** (§4.1 #13, §4.2.7, §4.2.8):
     - Parse `CompleteMultipartUpload` XML from body (list of Part {PartNumber, ETag}).
     - Call `storage.complete_multipart_upload(...).await`.
     - Build `CompleteMultipartUploadResult` XML with Location, Bucket, Key, composite ETag.
     - Return 200.

   - **`handle_abort_multipart_upload`** (§4.1 #14):
     - Parse `uploadId` from query params.
     - Call `storage.abort_multipart_upload(...).await`.
     - Return 204 No Content.

   - **`handle_list_parts`** (§4.1 #15, §4.2.9):
     - Parse `uploadId`, optional `max-parts` (default 1000), optional `part-number-marker` from query params.
     - Call `storage.list_parts(...).await`.
     - Build `ListPartsResult` XML with Initiator, Owner, Part entries.
     - Return 200.

   - **`handle_get_bucket_location`** (§4.1 #16, §4.2.10):
     - Call `storage.get_bucket_location(...).await`.
     - Build `LocationConstraint` XML with `us-east-1`.
     - Return 200.

5. **`multipart.rs` — Multipart UploadId generation** (§4.2.6):
   - `fn generate_upload_id(bucket: &str, key: &str) -> String`:
     Concatenate `bucket:key:{Utc::now().timestamp_nanos()}:{random_u64}`, SHA-256 hash, base64url-encode.
   - Use `sha2::Sha256`, `base64::Engine::general_purpose::URL_SAFE_NO_PAD`.

6. **Inline tests for each handler** (basic smoke tests):
   - Can construct handler, call with valid params, get 200.
   - Can construct handler, call with invalid params, get expected error.
   - Use `MockStorage` (will be implemented in Phase 8, but define a basic one here for handler development).

**Key design decisions:**
- **Dispatch happens in `S3Service::handle()`** — the router calls `handle()`, which internally dispatches to per-operation handlers. This keeps the router crate agnostic about S3 semantics.
- **URL-decode `x-amz-copy-source` AFTER dispatch** — the dispatch checks for header PRESENCE (not value), then the handler URL-decodes the value.
- **Empty `<Deleted>` for DeleteObjects** when all deletions fail or none requested.
- **ETag is computed as md5 hex of raw bytes**, wrapped in double quotes.
- **Composite ETag** for CompleteMultipartUpload: md5 of concatenated raw part bytes, not md5 of concatenated ETag hex strings.
- **No range request handling** in v0.1.0 — GetObject always returns entire object.

**Verification criteria:**
- [ ] `cargo build -p cirrus-s3` succeeds (with all modules)
- [ ] Each of 16 handlers compiles and produces correct HTTP response type
- [ ] `S3Service::handle()` correctly dispatches to all 16 operations
- [ ] PutObject computes correct ETag matching `md5sum` command
- [ ] CopyObject triggers correctly when x-amz-copy-source is present
- [ ] CopyObject URL-decodes the source header value
- [ ] ListObjectsV2 pagination logic produces correct tokens
- [ ] DeleteObjects correctly handles quiet mode
- [ ] Multipart composite ETag format matches AWS spec (`"<md5_hex>-<N>"`)
- [ ] Every response has `x-amz-request-id` and `x-amz-id-2` headers

---

### Phase 6: Error Handling & Response Standardization

**Objective:** Ensure error mapping from `S3Error` to `AwsError` is complete and consistent across all handlers. Standardize response headers. Verify all specific error XML schemas from §6.3. Implement the `NotImplemented` handler for non-S3 services.

**Files to create/modify:**

| File | Action |
|------|--------|
| `crates/cirrus-s3/src/handlers.rs` | Modify — add error mapping helper |
| `crates/cirrus-protocol/src/error.rs` | Modify — verify/completeness of AwsError variants |
| `crates/cirrus-router/src/lib.rs` | Modify — verify 501 returns correct XML |

**Implementation steps:**

1. **Error mapping function** in `cirrus-s3/src/handlers.rs`:
   - `fn s3_error_to_aws_error(err: S3Error, request_id: &str) -> AwsError`:
     Map each `S3Error` variant to the corresponding `AwsError` variant with correct HTTP status (§6.2):
     - `S3Error::NoSuchBucket(b)` → `AwsError::NoSuchBucket { bucket_name: b }` (404)
     - `S3Error::NoSuchKey(k)` → `AwsError::NoSuchKey { key: k }` (404)
     - `S3Error::NoSuchUpload(id)` → `AwsError::NoSuchUpload { upload_id: id }` (404)
     - `S3Error::BucketAlreadyExists(b)` → `AwsError::BucketAlreadyExists { bucket_name: b }` (409)
     - `S3Error::BucketNotEmpty(b)` → `AwsError::BucketNotEmpty { bucket_name: b }` (409) — includes extra `<BucketName>` element per §6.3
     - `S3Error::InvalidArgument(msg)` → `AwsError::InvalidArgument` with optional argument fields
     - `S3Error::MethodNotAllowed` → `AwsError::MethodNotAllowed` (405)
     - `S3Error::EntityTooLarge` → `AwsError::EntityTooLarge` (400, not 413 — §6.2 note)
     - `S3Error::MalformedXML` → `AwsError::MalformedXML`
     - `S3Error::InternalError` → `AwsError::InternalError` (500)
     - `S3Error::InvalidBucketName(b)` → `AwsError::InvalidBucketName { bucket_name: b }` (400)
     - `S3Error::KeyTooLong` → `AwsError::KeyTooLong` (400)
     - `S3Error::InvalidPart(id, pn, etag)` → `AwsError::InvalidPart` (400) — includes extra `<UploadId>`, `<PartNumber>`, `<ETag>` elements per §6.3
     - `S3Error::IncompleteBody` → `AwsError::IncompleteBody` (400)

2. **Standard per-request ID generation:**
   - Generate `request_id` (UUID v4) once per request in the dispatch function (`S3Service::handle`).
   - Thread it through to all handler functions.
   - Every handler response includes `x-amz-request-id: {uuid}` and `x-amz-id-2: cirrus-v0.1.0`.
   - The `AwsError.into_response()` must also include these headers.

3. **Verify 501 NotImplemented for non-S3 services:**
   - The `fallback_handler` in `cirrus-router` already returns `AwsError::NotImplemented` for unregistered services.
   - Verify the XML body matches AWS format and SDKs can parse it.
   - Integration test: send request with `Authorization: .../dynamodb/...`, verify 501 with valid AWS XML.

4. **Specific error schemas from §6.3** — verify each:
   - **InvalidArgument (list-type):** Extra `<ArgumentName>list-type</ArgumentName>` and `<ArgumentValue></ArgumentValue>` in XML.
   - **BucketNotEmpty:** Extra `<BucketName>{name}</BucketName>` in XML.
   - **InvalidPart:** Extra `<UploadId>`, `<PartNumber>`, `<ETag>` in XML.

5. **Edge case verification:**
   - `GetObject`/`HeadObject` on non-existent key returns 404 `NoSuchKey` (§4.1 notes).
   - `GetObject`/`HeadObject` on non-existent bucket returns 404 `NoSuchBucket`.
   - `CopyObject` on non-existent source returns `NoSuchKey`.
   - Empty `Delete` body (no `<Object>` children) returns 200 with empty `<DeleteResult>` (§4.2.4).
   - Empty `CreateBucketConfiguration` body is valid and ignored (§4.1 note).

**Key design decisions:**
- **Error mapping is centralized** in `s3_error_to_aws_error()` — every handler calls this instead of mapping manually.
- **request_id is generated once per request** — thread it as a parameter rather than generating in each handler.
- **400 for EntityTooLarge** (not 413) matches AWS S3 behavior exactly.
- **501 responses** must be valid AWS XML that SDKs can parse gracefully (not a bare HTTP error).

**Verification criteria:**
- [ ] Every `S3Error` variant maps to correct `AwsError`
- [ ] Every `AwsError` variant maps to correct HTTP status code
- [ ] Error XML for InvalidArgument (list-type) contains `<ArgumentName>` and `<ArgumentValue>`
- [ ] Error XML for BucketNotEmpty contains `<BucketName>`
- [ ] Error XML for InvalidPart contains `<UploadId>`, `<PartNumber>`, `<ETag>`
- [ ] 501 responses for non-S3 services return valid AWS XML
- [ ] `x-amz-request-id` and `x-amz-id-2` present on EVERY response (success and error)

---

### Phase 7: Binary Entry Point (cirrus-server)

**Objective:** Build the binary crate that wires everything together: config loading (CLI + env), logging setup, Router assembly with S3Service registered, and server startup with graceful shutdown.

**Files to create/modify:**

| File | Action |
|------|--------|
| `crates/cirrus-server/src/main.rs` | Create — full binary entry point |
| `crates/cirrus-server/Cargo.toml` | Create (or verify if created in Phase 1) |

**Implementation steps:**

1. **`main.rs` — CLI argument parsing** (§8.2):
   ```rust
   #[derive(Parser, Debug, Clone)]
   struct Args {
       #[arg(short, long, default_value = "4566")]
       port: u16,
       #[arg(short, long, default_value = "0.0.0.0")]
       bind: String,
       #[arg(short = 'l', long, default_value = "info")]
       log_level: String,
   }
   ```
   Use `clap::Parser` derive macro.

2. **`main.rs` — Config loading** (§8.2 priority):
   ```rust
   let config: Config = Figment::from(Env::prefixed("CIRRUS_"))
       .merge(Clap::from(Args::parse()))
       .extract()?;
   ```
   CLI args override env vars, env vars override defaults. This ordering matters: `Figment` merge priority is last-wins, so CLI (merged after env) takes precedence. Verify this behavior.

3. **`main.rs` — Logging/tracing setup** (§13.1, §13.2):
   ```rust
   use tracing_subscriber::prelude::*;
   let fmt = tracing_subscriber::fmt()
       .json()
       .with_env_filter(format!("cirrus={}", config.log_level))
       .init();
   ```
   Set up JSON logging. Target filter: trace everything in `cirrus_*` crates at configured level.

4. **`main.rs` — Server assembly:**
   ```rust
   #[tokio::main]
   async fn main() -> Result<(), Box<dyn std::error::Error>> {
       // 1. Parse config
       // 2. Init logging
       // 3. Create storage
       let storage = DefaultStorage::default();
       // 4. Create S3Service
       let s3_service = cirrus_s3::S3Service::new(storage);
       // 5. Build service registry
       let mut registry = ServiceRegistry::new();
       registry.register("s3", s3_service);
       let registry = Arc::new(registry);
       // 6. Build router
       let app = cirrus_router::build_router(registry);
       // 7. Bind and serve
       let addr = format!("{}:{}", config.bind, config.port);
       let listener = tokio::net::TcpListener::bind(&addr).await?;
       tracing::info!("Cirrus v0.1.0 starting on {}", addr);
       axum::serve(listener, app)
           .with_graceful_shutdown(shutdown_signal())
           .await?;
       Ok(())
   }
   ```

5. **`main.rs` — Graceful shutdown** (§3.1):
   ```rust
   async fn shutdown_signal() {
       let ctrl_c = async {
           tokio::signal::ctrl_c().await.expect("failed to install Ctrl+C handler");
       };
       #[cfg(unix)]
       let terminate = async {
           tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
               .expect("failed to install signal handler")
               .recv().await;
       };
       #[cfg(not(unix))]
       let terminate = std::future::pending::<()>();
       tokio::select! {
           _ = ctrl_c => {},
           _ = terminate => {},
       }
       tracing::info!("Shutdown signal received, gracefully stopping");
   }
   ```

6. **`main.rs` — CIRRUS_DEFAULT_ACCOUNT_ID handling** (§8.1):
   - In v0.1.0, all requests use `"000000000000"` as the owner ID regardless of access key.
   - Config option exists but is informational. Document this.

**Key design decisions:**
- **No global state.** Everything is constructed in `main()` and passed via axum's State extractor.
- **`ServiceRegistry`** is built from scratch in main — easy to add more services in future versions.
- **Graceful shutdown handles both SIGTERM and Ctrl+C** — essential for Docker/container environments.
- **`multi_thread` tokio runtime** (§9.1 note) for concurrent connection handling.

**Verification criteria:**
- [ ] `cargo build -p cirrus-server --release` produces a binary
- [ ] `cirrus-server --help` shows correct CLI args
- [ ] `cirrus-server --port 4570` binds on port 4570
- [ ] `CIRRUS_PORT=4570 cirrus-server` also binds on port 4570
- [ ] `CIRRUS_PORT=4570 cirrus-server --port 4566` overrides to 4566 (CLI > env)
- [ ] Server starts in <50 ms (bare metal)
- [ ] Ctrl+C triggers graceful shutdown
- [ ] `curl http://localhost:4566/` returns ListBuckets response (empty, but valid XML)
- [ ] `curl http://localhost:4566/ -H "Authorization: AWS4-HMAC-SHA256 Credential=AKID/20260517/us-east-1/dynamodb/aws4_request"` returns 501 with valid AWS XML

---

### Phase 8: Testing Suite

**Objective:** Build comprehensive test coverage: unit tests with `MockStorage`, integration tests with the `with_server()` harness, property-based tests for URL/key encoding, and validation against AWS SDKs (CLI, boto3, Rust SDK).

**Files to create/modify:**

| File | Action |
|------|--------|
| `crates/cirrus-s3/tests/storage_tests.rs` | Create — storage unit tests |
| `crates/cirrus-s3/src/storage.rs` | Add inline `#[cfg(test)] mod tests` |
| `crates/cirrus-protocol/src/xml.rs` | Add inline tests |
| `crates/cirrus-router/tests/router_tests.rs` | Create — router unit tests |
| `crates/cirrus-server/tests/integration.rs` | Create — full integration test suite |
| `crates/cirrus-server/tests/common/mod.rs` | Create — test harness helpers |
| `tests/proptest_keys.rs` | Create — property-based key encoding tests |
| `benches/s3_benchmark.rs` | Create — criterion benchmarks |

**Implementation steps:**

1. **`MockStorage` — in `cirrus-s3/src/storage.rs`** (under `#[cfg(test)]`):
   - Implement the `Storage` trait with a `HashMap<String, HashMap<String, S3Object>>` (single-threaded, wrapped in `Arc<Mutex<...>>`).
   - Simulate all storage operations for unit testing handlers.
   - No DashMap dependency — simple `HashMap`.

2. **Storage unit tests** (`storage_tests.rs`):
   - Bucket lifecycle: create, list, delete, list (empty).
   - Object CRUD: put, get, head, delete on same key.
   - Concurrent put/get on different keys.
   - Concurrent operations on different buckets.
   - Multipart lifecycle: create → upload 3 parts → complete → get object.
   - Abort multipart: create → upload 1 part → abort → list parts (empty).
   - ListObjectsV2 pagination: insert 100 keys, list with max-keys=10, verify tokens.
   - ListObjectsV2 prefix + delimiter: insert "photos/cat.jpg", "photos/dog.jpg", "docs/file.txt", list with prefix="photos/" and delimiter="/" → expect 2 Contents under "photos/" and 0 CommonPrefixes. Actually, with delimiter="/": "photos/cat.jpg" and "photos/dog.jpg" share prefix "photos/" and neither contains "/" after the prefix, so they appear as Contents. If we had "photos/subdir/file.jpg", that would appear as CommonPrefix "photos/subdir/".
   - CopyObject: same-bucket and cross-bucket, verify data matches source.
   - CopyObject with REPLACE metadata directive.
   - Memory management: put objects until memory limit, verify EntityTooLarge.
   - DeleteBucket with objects → BucketNotEmpty. DeleteBucket after emptying → success.
   - Bucket name validation.

3. **Handler unit tests** (using MockStorage):
   - Each handler tested in isolation with MockStorage.
   - Verify correct HTTP status codes and XML response bodies.
   - Verify headers: `ETag`, `Last-Modified`, `Content-Type`, `x-amz-request-id`, `x-amz-id-2`.

4. **Protocol unit tests** (inline in cirrus-protocol):
   - Round-trip XML serialization/deserialization for each response type.
   - AwsError XML output matches expected format.
   - XML escaping round-trips.
   - Date formatting matches ISO 8601 and IMF-fixdate.

5. **Router unit tests:**
   - `extract_service` with valid SigV4 auth header → "s3".
   - `extract_service` with X-Amz-Target → appropriate service.
   - `extract_service` with no headers → default "s3".
   - `resolve_address` path-style: `/bucket/key` → (Some("bucket"), Some("key")).
   - `resolve_address` virtual-hosted: `bucket.localhost:4566/key` → (Some("bucket"), Some("key")).
   - `resolve_address` URL-encoded: `/bucket/hello%20world` → key = "hello world".
   - `resolve_address` root path: `/` → (None, None).
   - `resolve_address` bucket only: `/bucket` → (Some("bucket"), None).
   - `fallback_handler` with unknown service → 501 XML.

6. **Integration test harness** (`cirrus-server/tests/common/mod.rs`):
   ```rust
   pub async fn with_server<F, Fut>(test: F) where
       F: FnOnce(String) -> Fut,
       Fut: Future<Output = ()>,
   {
       let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
       let port = listener.local_addr().unwrap().port();
       let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
       let router = cirrus_router::build_router(/* registry */);
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
   This spawns a real server on a random port for each test.

7. **Integration tests** (`integration.rs`):
   - Create bucket → verify in ListBuckets.
   - Put object → Get object → verify bytes round-trip.
   - Put object → Head object → verify ETag matches.
   - Delete object → Get object → 404.
   - Delete objects (batch) → verify result.
   - Multipart upload: create → upload 3 parts → complete → get whole object.
   - Multipart upload: create → upload 1 part → abort → verify object doesn't exist.
   - List objects with prefix, delimiter, max-keys.
   - Copy object → verify in destination.
   - Non-existent bucket → 404 XML.
   - Non-existent key → 404 XML.
   - Missing list-type → 400 InvalidArgument with proper XML.
   - EntityTooLarge → 400 with XML.
   - Non-S3 service → 501 with XML.
   - **AWS CLI v2**: `aws s3api create-bucket`, `aws s3 cp`, `aws s3 sync` if CLI available in CI.
   - **boto3**: `client.create_bucket()`, `client.put_object()`, `client.get_object()`, `client.create_multipart_upload()`.
   - **AWS SDK Rust**: use `aws_sdk_s3` with `endpoint_url`.

8. **Property-based tests** with `proptest`:
   - Generate random Unicode strings, encode them as URL-path segments, resolve via `resolve_address`, verify decoded value matches original.
   - Generate random keys, put and get them, verify keys are stored literally (§12.1: S3 treats keys as opaque strings; `../` and `./` are valid).
   - Generate random metadata key/value pairs (with embedded special chars), put and retrieve, verify metadata round-trips (with `\r\n` stripped from keys per §4.3).

9. **Benchmarks** (`benches/s3_benchmark.rs` — §11.2):
   ```rust
   use criterion::{black_box, criterion_group, criterion_main, Criterion};
   ```
   - `bench_put_object_1mb`: Pre-create bucket, put 1 MB object.
   - `bench_get_object_1mb`: Pre-populate 1 MB object, get it.
   - `bench_list_objects_100`: Pre-populate 100 objects, list them.
   - Use `tokio::runtime::Builder::new_multi_thread()` — shared runtime, NOT one per benchmark.
   - Use `black_box()` to prevent compiler optimization of unused results.
   - Store benchmark results for regression tracking.

**Key design decisions:**
- **MockStorage** is simple `HashMap`-based for determinism in unit tests.
- **Integration tests** use `with_server()` on random ports — no port conflicts in CI.
- **Property-based tests** focus on URL encoding/decoding edge cases (identified as risk §15).
- **Benchmarks** use `multi_thread` runtime (not `current_thread`) to match production.
- **AWS SDK integration tests** are gated behind `#[cfg(feature = "integration_tests")]` or an env var `CIRRUS_INTEGRATION_TESTS=1` — they require external SDKs installed. Not run in regular `cargo test`.

**Verification criteria:**
- [ ] `cargo test --workspace` passes all unit tests
- [ ] Line coverage: cirrus-protocol >90%, cirrus-router >90%, cirrus-s3 >85%
- [ ] Integration tests pass for all 16 operations
- [ ] Proptest runs 10,000+ test cases for key encoding
- [ ] Benchmarks compile and run (results logged, not failing CI)
- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] All error cases from §6.2 tested

---

### Phase 9: Docker & CI/CD

**Objective:** Create Dockerfile for musl static binary build, set up GitHub Actions CI pipeline with test, integration, and build jobs.

**Files to create/modify:**

| File | Action |
|------|--------|
| `cirrus/Dockerfile` | Create — multi-stage build |
| `cirrus/.dockerignore` | Create |
| `.github/workflows/ci.yml` | Create — CI/CD pipeline |

**Implementation steps:**

1. **`Dockerfile`** (§9.3):
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
   - Support `ARG RUST_TARGET` for multi-platform (arm64, amd64).
   - Build time optimization: `--mount=type=cache` for cargo registry.

2. **`.dockerignore`**:
   ```
   target/
   .git/
   .gitignore
   *.md
   !README.md
   benches/
   tests/
   ```

3. **`.github/workflows/ci.yml`** (§9.4):
   ```yaml
   name: Cirrus CI
   on:
     push:
       branches: [main]
     pull_request:
       branches: [main]
   
   jobs:
     test:
       runs-on: ubuntu-latest
       steps:
         - uses: actions/checkout@v4
         - uses: actions-rust-lang/setup-rust-toolchain@v1
         - run: cargo test --workspace
         - run: cargo clippy --workspace -- -D warnings
         - run: cargo fmt --check
     
     integration:
       needs: test
       runs-on: ubuntu-latest
       steps:
         - uses: actions/checkout@v4
         - uses: actions-rust-lang/setup-rust-toolchain@v1
         # Install AWS CLI v2
         - run: pip install awscli boto3
         - run: cargo build
         - name: Run integration tests
           run: CIRRUS_INTEGRATION_TESTS=1 cargo test --test integration
     
     build:
       if: startsWith(github.ref, 'refs/tags/')
       needs: test
       runs-on: ubuntu-latest
       steps:
         - uses: actions/checkout@v4
         - uses: actions-rust-lang/setup-rust-toolchain@v1
         - run: cargo build --release --bin cirrus-server
         - name: Build Docker image
           run: docker build -t cirrus:${GITHUB_REF##*/} .
         - name: Push to GHCR
           run: |
             # docker push steps (requires secrets)
   ```

4. **Docker build verification:**
   - Verify binary size <25 MB (release, musl).
   - Verify Docker image size <50 MB.
   - Verify startup <20 ms in container.
   - Verify idle RSS <8 MiB.

**Key design decisions:**
- **`gcr.io/distroless/static-debian12`** for minimal image — no shell, no package manager.
- **`ARG RUST_TARGET`** for multi-platform builds (x86_64, aarch64).
- **CI has 3 independent jobs**: `test` (fast), `integration` (needs test to pass), `build` (tag push only).
- **Integration tests require AWS CLI + boto3** — installed via pip in CI.

**Verification criteria:**
- [ ] `docker build -t cirrus .` succeeds
- [ ] Docker image size <50 MB
- [ ] `docker run cirrus` starts and listens on 4566
- [ ] CI pipeline workflows documented and passing
- [ ] `cargo build --release --target x86_64-unknown-linux-musl` produces binary <25 MB
- [ ] Startup <20 ms in Docker container

---

### Phase 10: Documentation & Polish

**Objective:** Create README, CHANGELOG, verify against Definition of Done checklist from §16. Final polish before tagging v0.1.0.

**Files to create/modify:**

| File | Action |
|------|--------|
| `cirrus/README.md` | Create |
| `cirrus/CHANGELOG.md` | Create |

**Implementation steps:**

1. **`README.md`** — Installation, Quick Start, Supported Operations, Configuration:
   ```markdown
   # Cirrus — Lightweight AWS S3 Emulator
   
   Cirrus is a minimal, fast Amazon S3 API emulator. Single binary, zero config.
   
   ## Quick Start
   ```bash
   cargo install cirrus-server
   cirrus-server
   # aws s3api create-bucket --bucket test --endpoint-url http://localhost:4566
   ```
   
   ## Supported Operations
   (table of 16 operations from §4.1)
   
   ## Configuration
   (table of env vars from §8.1)
   ```

2. **`CHANGELOG.md`**:
   ```markdown
   # Changelog
   
   ## [0.1.0] - 2026-05-17
   ### Added
   - S3 API emulation with 16 operations
   - In-memory storage with DashMap
   - SigV4-based service routing
   - AWS XML error responses
   - 501 NotImplemented for non-S3 services
   - Docker image (distroless, <30 MB)
   ```

3. **Definition of Done verification** — run through §16 checklist:
   - [ ] All 16 S3 operations pass unit tests with >85% coverage.
   - [ ] All integration tests pass against AWS CLI v2, boto3, and AWS SDK Rust.
   - [ ] `cargo build --release` produces a musl static binary <25 MB.
   - [ ] Docker image builds successfully and is <50 MB.
   - [ ] Server starts in <50 ms on bare metal, <20 ms in Docker.
   - [ ] Idle RSS memory is <8 MiB.
   - [ ] Non-S3 requests return valid 501 NotImplemented XML that SDKs handle gracefully.
   - [ ] CI passes: `cargo test`, `cargo clippy`, `cargo fmt --check`, integration tests.
   - [ ] README documents installation, quick start, and supported operations.
   - [ ] CHANGELOG.md created with v0.1.0 entry.

**Key design decisions:**
- README doubles as both user-facing docs and developer onboarding.
- CHANGELOG follows Keep a Changelog format.
- No autogenerated docs (no `cargo doc` publishing in v0.1.0).

**Verification criteria:**
- [ ] `Definition of Done` checklist fully verified
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo fmt --check` passes
- [ ] `cargo build --release` completes
- [ ] All integration tests pass
- [ ] Docker build succeeds
- [ ] README documents all 16 operations with examples

---

## 4. Parallelism Opportunities

### Phase Groups That Can Run in Parallel

**Group A: Phase 1 → Phase 2 → (Phase 3, Phase 4)**
Phase 1 must come first. Phase 2 must come next (both Phase 3 and Phase 4 depend on cirrus-protocol types). After Phase 2 completes, **Phase 3 and Phase 4 can run in parallel** by different sub-agents since they depend on different parts of Phase 2:
- Phase 3 (Storage) depends on: `S3Object` type from `cirrus_protocol::types`
- Phase 4 (Router) depends on: `AwsError` from `cirrus_protocol::error`

These have no file overlap — they write to different crates (`cirrus-s3` vs `cirrus-router`).

**Group B: Phase 5 (S3 Handlers)** must wait for both Phase 3 and Phase 4.

**Group C: Phase 6 (Error Standardization) and Phase 7 (Binary Entry Point)** can run in parallel after Phase 5:
- Phase 6 is a cross-cutting review/modification of error handling.
- Phase 7 wires together the binary.
- They touch different files.

**Group D: Phase 8 (Testing), Phase 9 (Docker/CI), Phase 10 (Docs)** — Phase 10 can be done any time after Phase 7. Phase 9 can be done in parallel with Phase 8 (Dockerfile doesn't depend on test files). But practically, Phase 9 should wait until Phase 7 produces a working binary.

### Recommended Parallel Workstreams

| Workstream | Phases | Est. Sequential Time | Blocked By |
|-----------|--------|---------------------|------------|
| **Core** | 1 → 2 → (3+4) → 5 → 6 → 7 | ~6 sequential | Nothing |
| **Testing** | 8 (starts after 7) | ~1 sequential | Phase 7 |
| **Infra** | 9 (starts after 7) | ~1 sequential | Phase 7 |
| **Docs** | 10 (starts after 7) | ~1 sequential | Phase 7 |

With 3 parallel agents:
- Agent 1: Phase 1 → Phase 2 → Phase 3 → Phase 5 → Phase 6 → Phase 7
- Agent 2: Phase 4 (starts after Phase 2, parallel with Agent 1 on Phase 3)
- Agent 3: Phase 8 + Phase 9 + Phase 10 (starts after Phase 7)

---

## 5. Anti-Pattern Catalog

### P1: Global/Static Service Registry
**❌ Don't:**
```rust
static SERVICE_REGISTRY: Lazy<ServiceRegistry> = Lazy::new(ServiceRegistry::new);
```
This prevents multiple server instances in tests and makes testing impossible.

**✅ Do:** Pass via axum's State extractor:
```rust
let registry = Arc::new(ServiceRegistry::new());
let app = Router::new()
    .with_state(registry)
    .fallback(fallback_handler);
```

### P2: Deriving Clone on Structs Containing DashMap
**❌ Don't:**
```rust
#[derive(Clone)] // COMPILE ERROR: DashMap doesn't impl Clone
pub struct DefaultStorage {
    buckets: DashMap<String, Bucket>,
}
```

**✅ Do:** Wrap DashMap in Arc, implement Clone manually:
```rust
pub struct DefaultStorage {
    buckets: Arc<DashMap<String, Bucket>>,
    total_bytes: Arc<AtomicU64>,
}
impl Clone for DefaultStorage {
    fn clone(&self) -> Self {
        Self { buckets: Arc::clone(&self.buckets), total_bytes: Arc::clone(&self.total_bytes) }
    }
}
```

### P3: RwLock Delete Guard for DeleteBucket
**❌ Don't:**
```rust
struct Bucket {
    delete_guard: RwLock<bool>, // Can cause ABBA deadlock
    objects: DashMap<String, S3Object>,
}
```
Locking `delete_guard` (a RwLock) then accessing `objects` (DashMap's internal shard locks) creates potential ABBA deadlock if another thread locks a DashMap shard then tries to acquire delete_guard.

**✅ Do:** Use DashMap's atomic `remove()` to isolate the bucket, then check emptiness:
```rust
fn delete_bucket(&self, name: &str) -> Result<(), S3Error> {
    let bucket = self.buckets.remove(name).ok_or(S3Error::NoSuchBucket(name.to_string()))?;
    let bucket = bucket.1;
    if bucket.objects.is_empty() && bucket.multipart_uploads.is_empty() {
        Ok(())
    } else {
        self.buckets.insert(name.to_string(), bucket); // Re-insert
        Err(S3Error::BucketNotEmpty(name.to_string()))
    }
}
```
This uses a single lock ordering: parent DashMap lock first, then bucket internals.

### P4: Creating a New Tokio Runtime Per Benchmark
**❌ Don't:**
```rust
fn bench_foo(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap(); // New runtime every benchmark
    c.bench_function("foo", |b| b.to_async(&rt).iter(|| ...));
}
```
Creates runtime creation overhead in measured times and wastes memory.

**✅ Do:** Use a single multi_thread runtime:
```rust
fn bench_foo(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all().build().unwrap();
    // Reuse rt across benchmarks
    c.bench_function("foo", |b| b.to_async(&rt).iter(|| ...));
}
```

### P5: Skipping URL Decoding on Path Segments
**❌ Don't:**
```rust
let bucket = segments.next(); // "hello%20world" stays encoded
```

**✅ Do:** Always URL-decode path segments:
```rust
let bucket = segments.next()
    .map(|s| urlencoding::decode(s).unwrap_or(Cow::Borrowed(s)).to_string());
```
This was a known bug pattern (P1-6 fix) that breaks Unicode keys and URL-encoded characters.

### P6: Bytes::clone() Memory Accounting for CopyObject
**⚠️ Watch out:**
```rust
fn copy_object(&self, ...) {
    let data = source.data.clone(); // O(1) - Arc refcount bump
    self.total_bytes.fetch_add(data.len() as u64, Ordering::Relaxed);
    // WRONG: Bytes::clone() shares the allocation, doesn't duplicate memory
}
```

**✅ Do:** DO NOT increment `total_bytes` for CopyObject. The underlying memory is shared. Only increment on `PutObject`, `UploadPart`, and `CompleteMultipartUpload`.

### P7: EntityTooLarge Returning HTTP 413
**❌ Don't:** Return HTTP 413 for EntityTooLarge. The standard `tower_http::limit::RequestBodyLimitLayer` returns bare 413, but AWS S3 returns EntityTooLarge as HTTP 400 with an XML body.

**✅ Do:** Intercept the 413 from tower_http's body limit middleware and convert to `AwsError::EntityTooLarge` which returns HTTP 400 with proper XML.

### P8: Self-Closing XML Elements
**❌ Don't:** Let quick-xml emit `<Element/>` for empty string values. AWS S3 always uses explicit open/close tags: `<Element></Element>`.

**✅ Do:** For hand-built responses, always write both open and close tags. For quick-xml serde, test with empty strings and ensure the output is `<E></E>` — if not, write custom serialization functions that handle this.

### P9: Per-Request State in Static Variables
**❌ Don't:**
```rust
static REQUEST_ID: AtomicU64 = AtomicU64::new(0); // Not thread-safe for unique IDs
```

**✅ Do:** Use `uuid::Uuid::new_v4()` for each request. Generate once in dispatch, thread through handlers. UUID v4 is stateless and doesn't need atomics.

### P10: Blocking I/O in Async Handlers
**❌ Don't:**
```rust
async fn handle_put_object(...) {
    let md5 = std::process::Command::new("md5sum").output(); // Blocking!
}
```

**✅ Do:** Use pure-Rust crypto crates (`md-5`, `sha2`) that are non-blocking by nature. All cryptographic operations are CPU-bound but small enough to run on async threads.

---

## 6. Verification Gate

Run this checklist after Phase 10 before tagging v0.1.0:

### Build Verification
- [ ] `cargo build --release --target x86_64-unknown-linux-musl` produces binary <25 MB
- [ ] `docker build -t cirrus:v0.1.0 .` succeeds
- [ ] Docker image size <50 MB (`docker images cirrus:v0.1.0`)

### Performance Verification
- [ ] Server starts in <50 ms on bare metal: `time cargo run --release` cold start
- [ ] Server starts in <20 ms in Docker
- [ ] Idle RSS <8 MiB: `docker run -d cirrus:v0.1.0 && ps -o rss= -p $(docker inspect -f '{{.State.Pid}}' <container>)`
- [ ] HTTP response latency <1 ms p99 for empty bucket list: `wrk -t4 -c100 -d30s http://localhost:4566/`

### Test Verification
- [ ] `cargo test --workspace` passes (all unit tests)
- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo fmt --check` passes
- [ ] Integration tests pass: `CIRRUS_INTEGRATION_TESTS=1 cargo test --test integration`

### Functional Verification
- [ ] `curl http://localhost:4566/` returns ListBuckets XML
- [ ] `curl -X PUT http://localhost:4566/test-bucket` returns 200 with Location header
- [ ] `curl -X PUT http://localhost:4566/test-bucket/test-key -d "hello"` returns ETag header
- [ ] `curl http://localhost:4566/test-bucket/test-key` returns "hello"
- [ ] `curl -I http://localhost:4566/test-bucket/test-key` returns 200 with headers
- [ ] `curl -X DELETE http://localhost:4566/test-bucket/test-key` returns 204
- [ ] `aws s3api create-bucket --bucket test --endpoint-url http://localhost:4566` succeeds
- [ ] `aws s3 cp /etc/hosts s3://test/hosts --endpoint-url http://localhost:4566` succeeds
- [ ] `aws s3 sync . s3://test/ --endpoint-url http://localhost:4566` works
- [ ] `aws s3api list-objects-v2 --bucket test --endpoint-url http://localhost:4566` works
- [ ] Non-S3 request returns 501 with valid AWS XML

### Code Quality
- [ ] Line coverage: cirrus-protocol >90%, cirrus-router >90%, cirrus-s3 >85%
- [ ] No dead code, no `unwrap()` or `expect()` in production code (use proper error handling)
- [ ] All `todo!()` or `unimplemented!()` removed
- [ ] No global/static mutable state

### Documentation
- [ ] README documents installation, quick start, configuration, all 16 operations
- [ ] CHANGELOG.md created with v0.1.0 entry
- [ ] CI pipeline passing

---

## 7. Parallel Step Detection Summary

### Phase Output File Dependency Matrix

| Phase | Output Files | Read By |
|-------|-------------|---------|
| 1 | Cargo.toml files, directory structure | All subsequent phases |
| 2 | `cirrus-protocol/src/*.rs` | Phase 3, Phase 4, Phase 5, Phase 6 |
| 3 | `cirrus-s3/src/storage.rs` | Phase 5 |
| 4 | `cirrus-router/src/*.rs` | Phase 5, Phase 7 |
| 5 | `cirrus-s3/src/{handlers,service,multipart}.rs` | Phase 6, Phase 7 |
| 6 | Modifications to `protocol`, `s3`, `router` | Phase 7 |
| 7 | `cirrus-server/src/main.rs` | Phase 8, Phase 9 |
| 8 | `tests/*.rs`, `benches/*.rs` | None (leaf) |
| 9 | `Dockerfile`, `.github/workflows/ci.yml` | None (leaf) |
| 10 | `README.md`, `CHANGELOG.md` | None (leaf) |

### Parallel Workstreams

**Workstream Alpha (Core Logic) — 4 agents max:**
```
Agent A: Phase 1 → Phase 2 → Phase 3 → Phase 5 → Phase 6 → Phase 7
Agent B: Phase 4 (after Phase 2, in parallel with Agent A's Phase 3)
```
- Agent B starts Phase 4 after Phase 2 completes.
- Agent A and Agent B work in parallel on Phase 3 and Phase 4.

**Workstream Beta (Infrastructure & Testing) — 2 agents max:**
```
Agent C: Phase 8 (after Phase 7)
Agent D: Phase 9 (after Phase 7, in parallel with Phase 8)
```
- Both start after Phase 7 completes.
- They have NO file overlap (tests vs Docker/CI).
- Phase 10 (Docs) can be done by either Agent C or D after their main work.

### Optimal Team Configuration

| Wave | Agent 1 | Agent 2 | Agent 3 | Agent 4 |
|------|---------|---------|---------|---------|
| Wave 1 | Phase 1 | — | — | — |
| Wave 2 | Phase 2 | — | — | — |
| Wave 3 | Phase 3 | Phase 4 | — | — |
| Wave 4 | Phase 5 | Phase 5 | — | — |
| Wave 5 | Phase 6 | Phase 6/7 | — | — |
| Wave 6 | Phase 7 | Phase 8 | Phase 9 | — |
| Wave 7 | Phase 10 | Phase 8 | Phase 9 | — |

**Minimum sequential depth (critical path):** 6 waves (Phase 1 → 2 → (3 or 4) → 5 → 6 → 7)

**With 2 agents (reduced parallelism):** Same sequential depth but Phase 3+4 and Phase 8+9 run in parallel, collapsing Wave 3 into 1 unit and Wave 6 into 2 units.

---

## Appendix: Quick Reference

### Crate->Crate Dependency Quick Reference

```
cirrus-protocol (no deps)
    ↑
cirrus-s3 ──── depends on cirrus-protocol
    ↑
cirrus-router ── depends on cirrus-protocol
    ↑
cirrus-server ── depends on cirrus-s3 + cirrus-router
```

### Key Constants

| Constant | Value | Configurable |
|----------|-------|-------------|
| Default port | 4566 | `CIRRUS_PORT` / `--port` |
| Max request body | 100 MB (104,857,600) | `CIRRUS_MAX_REQUEST_BYTES` |
| Max object size | 100 MB (104,857,600) | `CIRRUS_MAX_OBJECT_SIZE` |
| Max total memory | 512 MB (536,870,912) | `CIRRUS_MAX_MEMORY` |
| Max parts per upload | 10,000 | Not configurable |
| Max keys per list | 1,000 | `max-keys` param |
| Bucket name length | 3-63 chars | Not configurable |
| Max key length | 1,024 bytes | Not configurable |
| Default account ID | `000000000000` | `CIRRUS_DEFAULT_ACCOUNT_ID` |
| Default region | `us-east-1` | `CIRRUS_DEFAULT_REGION` |

### HTTP Status Code Quick Reference

| Status | Meaning |
|--------|---------|
| 200 | Success (most operations) |
| 204 | Delete success (DeleteBucket, DeleteObject, AbortMultipartUpload) |
| 400 | Client error (InvalidArgument, EntityTooLarge, MalformedXML, etc.) |
| 404 | Not found (NoSuchBucket, NoSuchKey, NoSuchUpload) |
| 405 | Method Not Allowed |
| 409 | Conflict (BucketAlreadyExists, BucketNotEmpty) |
| 500 | Internal Error |
| 501 | Not Implemented (non-S3 services) |
