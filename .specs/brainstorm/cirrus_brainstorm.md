# Cirrus

> Rust-native, high-performance AWS local emulator. Single binary. Real Docker integration.

**Target metrics:**
- Startup: **<10 ms**
- Idle memory: **<8 MiB**
- Binary size: **~20-25 MB** (static)
- Docker image: **<50 MB** (distroless)

## Core Philosophy

1. **Single binary** — statically linked, no runtime deps, `cargo install` / download-and-run.
2. **Real where it matters** — containerized (Lambda, RDS, ECS, EC2) = real Docker. In-process (S3, DynamoDB, IAM) = native speed.
3. **Wire-compatible** — AWS SDKs, CLI, Terraform, CDK, Pulumi work unchanged. `http://localhost:4566`.
4. **Zero configuration** — `docker run -p 4566:4566 cirrus/cirrus`.
5. **Free forever** — MIT licensed. No auth tokens. No feature gates.

## Architecture

```
                    ┌─────────────┐
                    │ AWS SDK/CLI │
                    └──────┬──────┘
                           │ HTTP :4566
                           ▼
┌─────────────────────────────────────────────────────────────┐
│                      Axum HTTP Router                        │
│  ┌─────────────────────────────────────────────────────────┐  │
│  │  SigV4 Parser → Service Identification → Account ID      │  │
│  │  (12-digit access key = account isolation)             │  │
│  └─────────────────────────────────────────────────────────┘  │
└──────────────────────────────┬────────────────────────────────┘
                               │
        ┌──────────────────────┼──────────────────────┐
        │                      │                      │
        ▼                      ▼                      ▼
┌───────────────┐    ┌─────────────────┐    ┌──────────────────┐
│   Stateless   │    │    Stateful     │    │  Containerized   │
│   (In-Proc)   │    │   (In-Proc)     │    │   (Docker)       │
├───────────────┤    ├─────────────────┤    ├──────────────────┤
│ SQS · SNS     │    │ S3              │    │ Lambda           │
│ IAM · STS     │    │ DynamoDB        │    │ RDS              │
│ KMS · Cognito │    │ DynamoDB Streams│    │ ElastiCache      │
│ EventBridge   │    │                 │    │ ECS · EC2        │
│ CloudWatch    │    │                 │    │ EKS · MSK        │
│ Step Functions│    │                 │    │ CodeBuild        │
│ API Gateway   │    │                 │    │ OpenSearch       │
│ Route53 · ACM │    │                 │    │                  │
└───────┬───────┘    └────────┬────────┘    └────────┬─────────┘
        │                     │                        │
        └─────────────────────┼────────────────────────┘
                              │
                              ▼
              ┌───────────────────────────────┐
              │     Storage Backend           │
              │  ┌─────┐ ┌─────┐ ┌────────┐  │
              │  │mem  │ │hybrid│ │persist │  │
              │  │(def) │ │     │ │(rocks) │  │
              │  └─────┘ └─────┘ └────────┘  │
              └───────────────────────────────┘
                              │
                              ▼
                     ┌────────────────┐
                     │ Docker Engine  │
                     │ (via bollard)  │
                     └────────────────┘
```

## Service Taxonomy

| Category | Strategy | Services |
|----------|----------|----------|
| **Stateless** | Pure Rust, in-memory or RocksDB | SQS, SNS, IAM, STS, KMS, Secrets Manager, SES, Cognito, EventBridge, Scheduler, CloudWatch, Step Functions, CloudFormation, ACM, Route53, API Gateway, ELB v2, Auto Scaling, CodeDeploy, Backup, AppConfig, Bedrock Runtime |
| **Stateful** | In-memory + pluggable persistence | S3, DynamoDB, DynamoDB Streams |
| **Containerized** | Docker-backed real engines | Lambda, RDS, ElastiCache, MSK, ECS, EC2, EKS, OpenSearch, CodeBuild, Data Firehose (DuckDB sidecar), Athena (DuckDB sidecar) |

### Containerized Service Details

| Service | Real Engine | IAM Integration |
|---------|-------------|-----------------|
| Lambda | `public.ecr.aws/lambda/<runtime>` | Warm pool, SigV4, IMDS credential serving |
| RDS | `postgres:16-alpine`, `mysql:8.0` | IAM auth tokens, JDBC-compatible |
| ElastiCache | `valkey/valkey:8` | Redis protocol + ACL-based IAM auth |
| MSK | `redpandadata/redpanda` | Kafka-compatible with IAM |
| EC2 | `amazonlinux:2023` containers | SSH key injection, UserData, IMDSv1+v2 |
| EKS | `rancher/k3s` | Live Kubernetes API per cluster |
| ECS | User-specified task images | Full container lifecycle |
| CodeBuild | User-specified build images | Real buildspec phases, S3 artifacts |
| Athena | DuckDB sidecar | Glue views, Parquet/JSON/CSV over S3 |

## Tech Stack

| Concern | Crates |
|---------|--------|
| HTTP Server | `axum`, `hyper` |
| Async Runtime | `tokio` |
| AWS SigV4 | `aws-sigv4` |
| Serialization | `serde`, `serde_json`, `quick-xml`, `serde_urlencoded` |
| Concurrent Collections | `dashmap` |
| Storage | `rocksdb` (persistent), `dashmap` (in-memory) |
| Docker API | `bollard` |
| Time | `chrono` |
| Hashing | `sha2`, `md5` |
| CLI | `clap` |
| Config | `figment` |
| Observability | `tracing`, `opentelemetry` |
| Testing | `tokio-test`, `testcontainers` |

## Storage Backends

| Mode | Behavior | Durability | Use Case |
|------|----------|------------|----------|
| `memory` | Entirely in-RAM. Lost on stop. | ❌ None | CI pipelines, ephemeral tests |
| `hybrid` | RAM + async flush every 5s | ⚠️ Good | Local dev with crash tolerance |
| `persistent` | Load on start, flush on shutdown | ⚠️ Medium | Simple state preservation |
| `wal` | Write-ahead log per mutation | 💎 Highest | Critical state, audit trails |

## Multi-Account Isolation

Zero-config multi-tenancy. `AWS_ACCESS_KEY_ID` = 12 digits → account ID. Resources fully isolated per account.

```bash
AWS_ACCESS_KEY_ID=111111111111 aws s3 mb s3://orders --endpoint-url http://localhost:4566
AWS_ACCESS_KEY_ID=222222222222 aws s3 mb s3://orders --endpoint-url http://localhost:4566
```

Non-12-digit keys → `CIRRUS_DEFAULT_ACCOUNT_ID` (`000000000000`).

## SDK Integration

Point AWS SDK at `http://localhost:4566`. No code changes.

**Rust example:**
```rust
let config = aws_config::defaults(BehaviorVersion::latest())
    .region(Region::new("us-east-1"))
    .credentials_provider(Credentials::new("test", "test", None, None, "cirrus"))
    .endpoint_url("http://localhost:4566")
    .load().await;

let s3 = aws_sdk_s3::Client::new(&config);
s3.create_bucket().bucket("demo").send().await?;
```

**Testcontainers support:**
```rust
use testcontainers_cirrus::CirrusContainer;

#[tokio::test]
async fn test_s3() {
    let cirrus = CirrusContainer::new().start().await;
    let s3 = aws_sdk_s3::Client::new(
        &aws_config::load_from_env().await
            .to_builder()
            .endpoint_url(cirrus.endpoint())
            .build()
    );
    s3.create_bucket().bucket("test").send().await.unwrap();
}
```

## Roadmap

| Version | Focus |
|---------|-------|
| **v0.1.0** | S3 only. Router + in-memory storage. Wire-compatible with AWS CLI, boto3, Rust SDK. |
| **v0.2.0** | S3 persistence (RocksDB), pre-signed URLs, DynamoDB |
| **v0.3.0** | SQS, SNS, IAM, STS |
| **v0.4.0** | Lambda (Docker-backed, warm pool, all runtimes) |
| **v0.5.0** | API Gateway (REST + HTTPv2), EventBridge, Step Functions |
| **v0.6.0** | RDS, ElastiCache, MSK (Docker-backed) |
| **v0.7.0** | ECS, EC2, EKS (Docker-backed with IMDS) |
| **v0.8.0** | CloudFormation, CodeBuild, CodeDeploy |
| **v0.9.0** | CloudWatch, Route53, ACM, SES |
| **v1.0.0** | 40+ services. Full LocalStack drop-in replacement. Testcontainers modules for Java, Node, Python, Go, Rust. |

## Differentiators

| Dimension | LocalStack Community | Floci | Cirrus (target) |
|-----------|----------------------|-------|-----------------|
| Auth token | Required (2026+) | ❌ No | ❌ No |
| Startup | ~3.3 s | ~24 ms | **<10 ms** |
| Idle memory | ~143 MiB | ~13 MiB | **<8 MiB** |
| Binary size | N/A (JAR) | ~40 MB native | **~20-25 MB** |
| Docker image | ~1.0 GB | ~90 MB | **<50 MB** |
| Runtime | JVM | GraalVM native | **Static Rust binary** |
| Memory safety | GC-managed | GC-managed | **Compile-time guarantees** |
| Async I/O | Vert.x | Vert.x | **Zero-overhead (Tokio)** |

## License

MIT — free forever.
