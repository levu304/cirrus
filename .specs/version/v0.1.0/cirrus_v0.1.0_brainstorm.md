# Cirrus

Rust-native AWS local emulator. Single binary. Zero config.

## v0.1.0 Scope

**Goal:** Fast in-memory S3 on :4566. AWS SDKs/CLI see real S3. Non-S3 returns `501`.

### In Scope

| Feature | Detail |
|---------|--------|
| **HTTP Router** | Axum-based fallback router on `:4566`. Identifies S3 via AWS SigV4 `Credential` scope. |
| **Addressing** | Path-style (`/bucket/key`) and virtual-hosted-style (`bucket.localhost:4566/key`). |
| **Bucket Ops** | `CreateBucket`, `DeleteBucket`, `ListBuckets` |
| **Object Ops** | `PutObject`, `GetObject`, `HeadObject`, `DeleteObject` |
| **Batch Ops** | `DeleteObjects` (XML multi-delete) |
| **Multipart** | `CreateMultipartUpload`, `UploadPart`, `CompleteMultipartUpload`, `AbortMultipartUpload`, `ListParts` |
| **Listing** | `ListObjectsV2` with `prefix`, `max-keys`, `continuation-token`, `CommonPrefixes` |
| **Copy** | `CopyObject` via `x-amz-copy-source` |
| **Metadata** | `Content-Type`, `Content-Length`, `ETag`, `Last-Modified`, custom `x-amz-meta-*` |
| **Errors** | Proper AWS XML error bodies: `NoSuchBucket`, `NoSuchKey`, `MethodNotAllowed`, `NotImplemented` |

### Out of Scope

- Pre-signed URLs
- Versioning / Object Lock
- ACLs / Bucket Policies / CORS
- Event notifications
- Range requests
- Persistence (memory-only; restart = empty)
- Any non-S3 service

## Architecture

```
┌─────────────────────────────────────┐
│  Axum Router (:4566)                │
│  ├── SigV4 parser (service=?)       │
│  ├── Address parser (bucket, key)   │
│  └── Fallback → S3 handler          │
│       └── DashMap<bucket, objects>  │
└─────────────────────────────────────┘
```

## Tech Stack

| Layer | Crate |
|-------|-------|
| HTTP server | `axum` + `tokio` |
| Concurrent store | `dashmap` |
| XML | `quick-xml` |
| Content hashing | `md5` (ETag) |
| Time | `chrono` |
| Bytes | `bytes` |

## Workspace

```
cirrus/
├── crates/
│   ├── server/      # Binary entrypoint
│   ├── router/      # Axum routing + SigV4 + addressing
│   ├── s3/          # S3 handlers + in-memory storage
│   └── protocol/    # Shared XML errors + AWS types
```

## Success Criteria

- [ ] `cargo run` starts in <50 ms
- [ ] `aws s3api create-bucket --bucket test --endpoint-url http://localhost:4566` succeeds
- [ ] `aws s3 cp ./file s3://test/file --endpoint-url http://localhost:4566` round-trips
- [ ] `aws s3 sync` works against Cirrus
- [ ] `aws-sdk-s3` (Rust) and `boto3` pass integration tests
- [ ] Non-S3 calls return valid `501` XML that SDKs handle gracefully
- [ ] Docker image <30 MB (distroless)

## Roadmap Hint

v0.2.0: Pre-signed URLs, S3 persistence (RocksDB), `501` → DynamoDB.
