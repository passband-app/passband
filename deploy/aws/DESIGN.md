# Railway → AWS: the locked design

Why: Railway's customer env-var store was breached on 2026-10-21. Every secret
on it has been or is being rotated, and every Railway service moves to AWS.
Tenants (the k3s box `carrier` on Hetzner) stay put for now; they move later as
k3s on EC2, and EKS only when a second node is needed.

This file is the contract. Builders implement it; a change to anything below is
a question for the operator, not a judgement call.

## Decisions (locked 2026-10-23)

| | |
|---|---|
| Account | Fresh AWS account under AWS Organizations, IAM Identity Center for humans, no IAM users, no long-lived access keys anywhere |
| Region | `us-west-2` (carrier is Hetzner Hillsboro, OR) |
| IaC | OpenTofu, in `deploy/aws/tofu/`. State in S3 (versioned, SSE-KMS, `use_lockfile = true`) with OpenTofu state encryption on |
| DNS | Stays where it is today. Tofu does NOT manage DNS; it OUTPUTS every record the operator must create (ACM validation CNAMEs, service CNAMEs) |
| Secrets | AWS Secrets Manager, one secret per value, named `passband/<service>/<ENV_NAME>`. Tofu creates the secret CONTAINERS only, never a version: no secret value ever enters tofu state, a plan, a variable, or a file. The operator sets values with `aws secretsmanager put-secret-value --secret-string file:///dev/stdin` |
| Bifrost | On AWS with the rest, started FRESH (new admin credential, new virtual keys minted for every tenant). The Railway volume is not copied: it may hold sessions minted with the leaked admin credential |

## Topology

```
                         Internet
                            │
          ┌─────────────────┴──────────────────┐
          │ ALB (public subnets, 443 only;      │      CloudFront + S3 (private, OAC)
          │ 80 → 443 redirect; one ACM cert)    │      passband.app landing site
          └──┬────────┬─────────┬────────┬──────┘
   host:     │signup  │relay    │bifrost │grafana / metrics
             ▼        ▼         ▼        ▼
     ECS Fargate (ARM64, PRIVATE subnets, no public IPs)      EC2 "monitor" (t4g.small,
       control   relay(EFS /data)   bifrost(store: see below)  private subnet, SSM only)
          │                                                     docker compose:
          ▼                                                     prometheus+grafana+blackbox
       RDS Postgres 16 (db.t4g.micro, private, encrypted,      EBS gp3, encrypted
       deletion protection, rds.force_ssl=1, 7d backups)
                                    egress → NAT INSTANCE (fck-nat, t4g.nano), NOT a NAT gateway
```

Hostnames (ALB host-based routing, one ACM cert with all SANs):

| Host | Target | Health check |
|---|---|---|
| `signup.passband.app` | control :8852 (its entrypoint default, `0.0.0.0:${PORT:-8852}`) | `GET /healthz` |
| `relay.squelch.sh` | relay :8850 (its entrypoint default) | `GET /healthz` |
| `bifrost.passband.app` | bifrost :8080 | `GET /`, matcher `200-499` (Bifrost's /health is behind auth) |
| `grafana.passband.app` | monitor :3000 | `GET /api/health` |
| `metrics.passband.app` | monitor :9090 (remote-write receiver, basic auth on every route) | `GET /-/healthy`, matcher `200,401` |
| `passband.app` (+ `www`) | CloudFront | n/a |

Blackbox has NO public route, ever (same rule as on Railway).

## Per-service contract

### control
- Image built from the existing `Dockerfile.control` (unchanged), pushed to ECR.
- Secrets (Secrets Manager → ECS `secrets`): `SQUELCH_CONTROL_CLIENT_SECRET`,
  `_COOKIE_KEY`, `_WARDEN_TOKEN`, `_BIFROST_ADMIN_TOKEN`, `_ADMIN_TOKEN`,
  `_RESEND_API_KEY`, `_WORKOS_API_KEY`, `_ALERT_WEBHOOK_URL`, `_DATABASE_URL`.
- Plain env (tofu variables): `_PUBLIC_URL`, `_BASE_DOMAIN`, `_CLIENT_ID`,
  `_WARDEN_URL`, `_BIFROST_URL`, `_TRUSTED_PROXY_HOPS=1` (the ALB appends one
  X-Forwarded-For hop; builder must confirm this matches how squelch-control
  counts hops), `_INVITE_FROM`, `_LLM_BUDGET_USD`, `_LLM_MODELS`,
  `_ASSISTANT_MODELS`. Read `squelch-control/README.md` for the full table and
  include anything else it marks required.
- NOT carried over: `SQUELCH_CONTROL_ALERT_WEBHOOK` (legacy name),
  `BIFROST_ADMIN_USERNAME`/`_PASSWORD` (read by nothing).
- **Postgres TLS (code change).** The store dials with `NoTls` today
  (`squelch-control/src/store.rs`, `connect`). Add TLS via
  `tokio-postgres-rustls`, chosen by the URL's `sslmode`: absent/`disable` keeps
  today's `NoTls` exactly (Railway keeps working unchanged until cutover);
  `require` and `verify-full` use rustls with the **AWS RDS global CA bundle**
  checked into the repo (document its source URL and fetch date beside it);
  `verify-full` also checks the hostname. Any other value is a startup error
  that names the value, never the URL (the URL carries the password).
- ECS Exec ON (the replacement for `railway ssh` when running operator commands
  like `squelch-control llm mint`).

### relay
- Image from the existing root `Dockerfile`, unchanged.
- Secrets: `SQUELCH_RELAY_APNS_KEY`, `_AUTH_TOKEN`, `_PUSH_TOKEN`.
  Env: `_APNS_KEY_ID`, `_APNS_TEAM_ID`, `_APNS_TOPICS`, `_DB_PATH=/data/opens.sqlite3`,
  plus anything else `squelch-relay/README.md` lists that Railway had.
- `/data` on EFS (encrypted, access point, single task, `desired_count = 1`,
  deployment min 0 / max 100 so two writers never overlap).

### bifrost
- Image from `Dockerfile.bifrost` (FROM-only, pinned `maximhq/bifrost:v1.6.9`),
  pushed to ECR.
- Secret: `ANTHROPIC_API_KEY`. Env: `APP_HOST`, `APP_PORT`.
- **Store:** if Bifrost v1.6.9 supports a Postgres config store AND log store
  (check its source/docs at that tag, cite what you found), use the same RDS
  instance with a separate database `bifrost` and its own role. Otherwise EFS at
  `/app/data`, single task, same min 0 / max 100 rule. Either way content
  logging is OFF (it is a UI/config setting, see `deploy/hosted/PRODUCTION.md`
  "The LLM gateway"); the runbook must include checking it.
- `desired_count = 1`.

### monitoring (EC2 `monitor`)
- t4g.small, Amazon Linux 2023 ARM, private subnet, no key pair, no SSH, SSM
  Session Manager only, IMDSv2 required, encrypted gp3 data volume for
  Prometheus (30d) and Grafana state.
- docker compose running prometheus, grafana, blackbox from ECR images. A boot
  script fetches its secrets from Secrets Manager via the instance role into an
  env file on tmpfs: `PROM_REMOTE_WRITE_PASSWORD`, `PROM_WEB_BCRYPT`,
  `GF_SECURITY_ADMIN_PASSWORD`, `PROM_PASSWORD`, `SLACK_ALERTS_WEBHOOK_URL`,
  `BLACKBOX_BIFROST_CANARY_KEY`, `BLACKBOX_CANARY_MODEL`.
- Why not Fargate: Prometheus does not support NFS (EFS) storage.

### landing site
- S3 (private, OAC) + CloudFront. Build `passband-site` (`bun run build`) and
  upload the static files `server.ts` serves today.
- `passband-site/server.ts` routing becomes a **CloudFront Function**: the
  static path map, `/download` and `/download/latest` → 302 to the latest
  version's zip (version read from `appcast.xml` AT BUILD TIME and baked into
  the function), `/download/Passband-X.Y.Z.zip` → 302 to the GitHub release
  asset on `passband-app/passband-releases`, everything else → `index.html`.
  The `.woff2` keeps `Cache-Control: public, max-age=604800`; `appcast.xml`
  must be served `application/xml` and short-cached (Sparkle polls it).
- Must produce the same status codes and Location headers as `server.ts` for
  every route; a test asserts that table.

## Deploys

- `.github/workflows/deploy-aws.yml`: on push to `main` (path-filtered per
  service) and `workflow_dispatch` (per service). Every job is gated on the repo
  variable `AWS_DEPLOY_ENABLED == 'true'`, so the workflow is INERT until
  cutover.
- GitHub → AWS via OIDC, one role, trust restricted to
  `repo:passband-app/passband:ref:refs/heads/main` (and the `workflow_dispatch`
  environment). Permissions: push to these ECR repos, update these ECS
  services, SSM SendCommand to the monitor instance, S3 sync + CloudFront
  invalidation for the site. Nothing else.
- Rust images reuse the cargo-chef + registry build-cache pattern in
  `release-daemon.yml` (PR #534), with the cache in ECR.
- ECS services deploy by registering a task definition with the new image digest
  and updating the service; the job waits for steady state and fails if it
  never arrives (no green deploy that is not serving).
- The monitor box deploys via SSM Run Command: `docker compose pull && up -d`.

## Standing invariants

1. **Railway keeps running off `main` until cutover.** Nothing merged may change
   what any Railway service builds or serves. Do not edit `Dockerfile*`,
   `railway*.toml`, or `deploy/monitoring/**` in a way that changes Railway's
   behaviour. AWS-only configs live under `deploy/aws/`. The one exception is
   `.dockerignore`, which may gain `!deploy/aws/...` re-include lines.
2. **No secret value in the repo, in tofu state, or in CI logs.** Not even
   examples that look real.
3. **No NAT gateway, no public IPs on tasks or the monitor box.**
4. **Every stateful resource has deletion protection or `prevent_destroy`**: RDS,
   EFS, the monitor data volume, the state bucket.
5. **Least privilege per task**: each ECS task role/execution role reads only
   its own `passband/<service>/*` secrets.
6. **No new `SQUELCH_*` env vars** (operator rule) except where this design
   names one. The TLS change rides on the existing `DATABASE_URL`'s `sslmode`.
7. Plain commit messages, no Co-Authored-By trailers. Builders do NOT commit.

## Cost target

Roughly $70–100/month: ALB, 3 small Fargate ARM tasks, db.t4g.micro, t4g.small,
t4g.nano NAT, EFS/EBS pennies. Add an AWS Budgets alarm at $150/month to the
operator's email. Flag anything that pushes past this.

## Out of scope for this wave

The cutover itself (the operator runs it from `deploy/aws/CUTOVER.md`), moving
tenants off Hetzner, WAF, multi-AZ RDS.
