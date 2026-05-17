# Cirrus Roadmap

> Living doc. Updated: 2026-05-17.

## Legend

| Status | Icon |
|--------|------|
| Released | ✅ |
| In Progress | 🚧 |
| Planned | ⬜ |
| Stretch | 🔮 |

---

## v0.1.0 — S3 Foundation
**Target: Q2 2026**

- ✅ HTTP router on `:4566` with AWS SigV4 service identification
- ✅ Path-style & virtual-hosted-style S3 addressing
- ✅ S3 core operations: `ListBuckets`, `CreateBucket`, `DeleteBucket`
- ✅ S3 object operations: `PutObject`, `GetObject`, `HeadObject`, `DeleteObject`
- ✅ S3 batch: `DeleteObjects`
- ✅ S3 copy: `CopyObject` via `x-amz-copy-source`
- ✅ S3 listing: `ListObjectsV2` with pagination, prefix, delimiter, CommonPrefixes
- ✅ S3 multipart: `CreateMultipartUpload`, `UploadPart`, `CompleteMultipartUpload`, `AbortMultipartUpload`, `ListParts`
- ✅ In-memory storage (`DashMap`)
- ✅ Proper AWS XML error responses for all operations
- ✅ `501 Not Implemented` for all non-S3 services
- ✅ Static binary <30 MB, Docker image <50 MB
- ✅ Integration tests: AWS CLI v2, boto3, AWS SDK Rust

**Excluded:** Pre-signed URLs, versioning, ACLs, CORS, event notifications, range requests, persistence.

---

## v0.2.0 — Persistence & DynamoDB
**Target: Q3 2026**

- 🚧 **S3 persistence** — RocksDB backend (`CIRRUS_STORAGE_MODE=persistent|hybrid|wal`)
- 🚧 **S3 pre-signed URLs** — SigV4 query-string auth for GET/PUT
- 🚧 **DynamoDB** — In-process, full query/scan
  - Tables, items, attributes
  - Primary keys (HASH, HASH+RANGE)
  - Global Secondary Indexes (GSI)
  - Local Secondary Indexes (LSI)
  - `Query`, `Scan`, `BatchGetItem`, `BatchWriteItem`
  - `TransactWriteItems`, `TransactGetItems`
  - Conditional expressions
- 🚧 **DynamoDB Streams** — Stream records on table changes, shard iterators

---

## v0.3.0 — Messaging & Identity
**Target: Q3 2026**

- ⬜ **SQS** — Standard & FIFO queues
  - `SendMessage`, `ReceiveMessage`, `DeleteMessage`, `ChangeMessageVisibility`
  - `SendMessageBatch`, `DeleteMessageBatch`, `ChangeMessageVisibilityBatch`
  - Dead-letter queues (DLQ)
  - Visibility timeout
  - Message attributes
  - Queue tags
- ⬜ **SNS** — Topics & subscriptions
  - `CreateTopic`, `Publish`, `Subscribe`, `Unsubscribe`
  - Protocols: `sqs`, `lambda`, `http`, `https`
  - Message filtering
  - Topic attributes & tags
- ⬜ **IAM** — Identity & access management
  - Users, groups, roles, policies
  - `CreateUser`, `CreateRole`, `CreatePolicy`, `AttachRolePolicy`
  - `GetRole`, `GetPolicy`, `ListAttachedRolePolicies`
  - Policy document parsing (basic JSON validation)
  - Instance profiles
- ⬜ **STS** — Security token service
  - `GetSessionToken`, `AssumeRole`, `GetCallerIdentity`
  - `AssumeRoleWithWebIdentity` (basic stub)

---

## v0.4.0 — Lambda
**Target: Q4 2026**

- ⬜ **Lambda** — Docker-backed execution
  - All runtimes via `public.ecr.aws/lambda/<runtime>` images
  - `CreateFunction`, `UpdateFunctionCode`, `Invoke`, `DeleteFunction`
  - Function aliases & versions
  - `FunctionUrl` support
  - Warm container pool (configurable size)
  - `ListFunctions`, `GetFunction`, `GetFunctionConfiguration`
  - Environment variables
  - IAM role assumption for execution
  - CloudWatch Logs integration (logs → Cirrus CloudWatch)
- ⬜ **CloudWatch Logs** — Basic log groups & streams
  - `CreateLogGroup`, `CreateLogStream`, `PutLogEvents`
  - `DescribeLogGroups`, `DescribeLogStreams`
  - `FilterLogEvents`

---

## v0.5.0 — API Gateway & Orchestration
**Target: Q4 2026**

- ⬜ **API Gateway REST** — Full REST API emulation
  - Resources, methods, integrations
  - Lambda proxy integration
  - MOCK integrations
  - Stages & deployment
  - API keys & usage plans (basic)
- ⬜ **API Gateway HTTP (v2)** — HTTP APIs
  - Routes, integrations
  - JWT authorizers
  - `$default` stage
- ⬜ **EventBridge** — Event bus & rules
  - Custom event buses
  - Rules with event patterns
  - Targets: SQS, SNS, Lambda
  - `PutEvents`, `PutRule`, `PutTargets`
- ⬜ **Step Functions** — State machine execution
  - ASL (Amazon States Language) parser
  - `Pass`, `Task`, `Choice`, `Wait`, `Succeed`, `Fail`, `Parallel`, `Map`
  - Lambda task integration
  - Execution history
  - `StartExecution`, `DescribeExecution`, `GetExecutionHistory`

---

## v0.6.0 — Data Services
**Target: Q1 2027**

- ⬜ **RDS** — Docker-backed relational databases
  - PostgreSQL (`postgres:16-alpine`)
  - MySQL (`mysql:8.0`)
  - IAM database authentication (auth tokens)
  - `CreateDBInstance`, `DescribeDBInstances`, `DeleteDBInstance`
  - `ModifyDBInstance`, `RebootDBInstance`
- ⬜ **ElastiCache** — Docker-backed cache
  - Valkey/Redis (`valkey/valkey:8`)
  - Redis protocol compatibility
  - IAM auth via ACL + SigV4
  - `CreateCacheCluster`, `DescribeCacheClusters`
- ⬜ **MSK** — Kafka-compatible streaming
  - Redpanda (`redpandadata/redpanda`)
  - `CreateCluster`, `DescribeCluster`, `DeleteCluster`
  - Topic management via Kafka protocol

---

## v0.7.0 — Container & Compute Platform
**Target: Q1 2027**

- ⬜ **ECS** — Container orchestration
  - Clusters, task definitions, tasks, services
  - `CreateCluster`, `RegisterTaskDefinition`, `RunTask`, `StartTask`, `StopTask`
  - Service scheduler with desired count
  - Capacity providers
  - Fargate-style launch (Docker-backed)
- ⬜ **EC2** — Virtual machines as containers
  - `RunInstances` launches real Docker containers
  - AMI mapping to container images (`amazonlinux:2023`)
  - SSH key pair injection
  - UserData execution on startup
  - VPCs, subnets, security groups (in-process metadata)
  - Elastic IPs
  - **IMDS** — Instance Metadata Service on `169.254.169.254`
    - IMDSv1 & IMDSv2
    - IAM credential serving
- ⬜ **EKS** — Kubernetes clusters
  - `CreateCluster`, `DescribeCluster`, `DeleteCluster`
  - k3s per cluster (`rancher/k3s`)
  - Live Kubernetes API server
  - `kubeconfig` generation

---

## v0.8.0 — DevOps & IaC
**Target: Q2 2027**

- ⬜ **CloudFormation** — Stack management
  - `CreateStack`, `UpdateStack`, `DeleteStack`
  - `DescribeStacks`, `ListStacks`
  - Change sets (`CreateChangeSet`, `ExecuteChangeSet`)
  - Resource type handlers for supported services
  - Template parsing (YAML/JSON)
- ⬜ **CodeBuild** — CI/CD builds
  - `CreateProject`, `StartBuild`, `BatchGetBuilds`
  - Real `buildspec.yml` execution in Docker
  - `install`, `pre_build`, `build`, `post_build` phases
  - S3 artifact upload
  - CloudWatch log streaming
- ⬜ **CodeDeploy** — Deployment orchestration
  - Applications, deployment groups, deployment configs
  - Lambda traffic shifting
  - Lifecycle hooks
  - Auto-rollback on failure
- ⬜ **Auto Scaling** — Elastic capacity
  - Launch configurations & templates
  - Auto Scaling Groups with min/max/desired
  - Background reconciler (10s loop)
  - ELB v2 target group auto-registration
  - Lifecycle hooks
  - Scaling policies (target tracking stubs)

---

## v0.9.0 — Edge & Operations
**Target: Q2 2027**

- ⬜ **CloudWatch Metrics** — Custom metrics & alarms
  - `PutMetricData`, `GetMetricStatistics`
  - `ListMetrics`, `DescribeAlarms`
  - Basic alarm evaluation (stub)
- ⬜ **Route53** — DNS management
  - Hosted zones with auto-created SOA + NS
  - `ChangeResourceRecordSets` (CREATE/UPSERT/DELETE)
  - `ListResourceRecordSets`
  - Health checks (basic stubs)
- ⬜ **ACM** — Certificate management
  - `RequestCertificate`, `DescribeCertificate`
  - Validation lifecycle (DNS/email stubs)
- ⬜ **SES** — Email sending
  - `SendEmail`, `SendRawEmail`
  - Identity verification (stub)
  - DKIM attributes
  - Email templates with `{{var}}` substitution
- ⬜ **Secrets Manager** — Secret storage
  - `CreateSecret`, `GetSecretValue`, `PutSecretValue`
  - Versioning, resource policies
  - Rotation (stub)
- ⬜ **KMS** — Key management
  - `CreateKey`, `DescribeKey`, `ListKeys`
  - `Encrypt`, `Decrypt`, `GenerateDataKey`
  - `Sign`, `Verify`, `ReEncrypt`
  - Key aliases

---

## v1.0.0 — Production-Ready Local AWS
**Target: Q3 2027**

- ⬜ **40+ services** — Full coverage core AWS services
- ⬜ **Testcontainers modules** — First-class testing support
  - Java: `org.testcontainers:cirrus`
  - Node.js: `@cirrus/testcontainers`
  - Python: `testcontainers-cirrus`
  - Go: `testcontainers-cirrus-go`
  - Rust: `cirrus-testcontainers`
- ⬜ **Compatibility test suite** — 2,000+ automated tests
  - AWS CLI v2, boto3, AWS SDKs (Rust, Java, Node, Python, Go)
  - Terraform & OpenTofu providers
  - AWS CDK v2
- ⬜ **Multi-account isolation** — Full per-account resource namespaces
- ⬜ **Migration guide** — Drop-in LocalStack replacement
  - Environment variable translation (`LOCALSTACK_*` → `CIRRUS_*`)
  - Init script compatibility (`/etc/localstack/init/`)
  - Health check endpoint (`/_localstack/health`)
- ⬜ **Performance guarantees**
  - Startup <10 ms
  - Idle memory <8 MiB
  - Docker image <50 MB
- ⬜ **Stable API** — SemVer commitment, deprecation policy

---

## Post-v1.0 — Advanced Services
**Target: 2028+**

| Service | Description | Priority |
|---------|-------------|----------|
| **Athena** | SQL over S3 via DuckDB sidecar. Glue Data Catalog views. | High |
| **Glue** | Data Catalog, Schema Registry (Avro/JSON Schema/Protobuf) | High |
| **Data Firehose** | Streaming delivery to S3 as NDJSON/Parquet | Medium |
| **Kinesis** | Streams, shards, enhanced fan-out, split/merge | Medium |
| **OpenSearch** | Domain CRUD, real OpenSearch engine in Docker | Medium |
| **Cognito** | User pools, app clients, auth flows, JWKS endpoints | Medium |
| **AppConfig** | Applications, environments, hosted configurations | Low |
| **Bedrock Runtime** | Stub responses for local AI development | Low |
| **Backup** | Vaults, backup plans, on-demand jobs | Low |
| **Transfer Family** | SFTP server management | Low |
| **Textract** | API-compatible stubs | Low |
| **Pricing / Cost Explorer** | Static snapshot + synthesized cost data | Low |
| **SSM Run Command** | `SendCommand` with real `amazon-ssm-agent` polling | Low |

---

## Release Cadence

| Channel | Trigger | Audience |
|---------|---------|----------|
| **Nightly** | Every night at 22:00 UTC from `main` | Early adopters, CI pipelines |
| **Release** | Monthly or bi-monthly from stable tag | General use |
| **LTS** | Every 6 months, supported for 12 months | Enterprise, conservative users |

---

## How We Prioritize

1. **SDK compatibility** — Services used by most AWS SDK test suites first.
2. **Local dev pain** — What developers mock most (S3, DynamoDB, Lambda, SQS).
3. **Container necessity** — Real-engine services (RDS, ElastiCache, MSK) after in-memory.
4. **IaC support** — CloudFormation resource types added as services mature.
5. **Community demand** — GitHub issues, discussions, Slack votes.

---

## Contributing

- Open GitHub Discussion for feature proposals.
- Vote existing proposals with 👍.
- Submit PRs against `main` for in-progress milestones.
- Milestone assignments flexible — community contributions accelerate any version.
