# Working in this repo

## Deploys: merge to `main`; production is AWS

Production runs on AWS (`us-west-2`). The architecture is
`deploy/aws/DESIGN.md` and the infrastructure is OpenTofu in `deploy/aws/tofu/`
(its README says how to apply it). Tenants are not on AWS: they run on
`carrier` (k3s), managed by the warden.

**A merge to `main` is the deploy.** `.github/workflows/deploy-aws.yml` runs
on every push to `main`, path-filtered per service, and deploys whatever the
push touched:

| Service | Runs on | Host | Built from |
|---|---|---|---|
| `control` | ECS Fargate | `signup.passband.app` | `Dockerfile.control` |
| `relay` | ECS Fargate (`/data` on EFS) | `relay.squelch.sh` | `Dockerfile` |
| `bifrost` | ECS Fargate | `bifrost.passband.app` (source-restricted) | `deploy/aws/docker/Dockerfile.bifrost` |
| `monitor` (prometheus, grafana, blackbox) | one EC2 box, docker compose | Grafana over the tailnet (`passband-monitor`:8443); `metrics.passband.app` admits carrier only | `deploy/aws/monitor/` + `deploy/monitoring/` |
| `site` | S3 + CloudFront | `passband.app` | `passband-site/` + `deploy/aws/site/` |

Control's Postgres is RDS. Redeploy one service by hand with
`gh workflow run deploy-aws.yml -f service=<name>` (or `all`).

Two things worth knowing before you push:

- **Deploys do not wait for CI.** A merge to `main` starts a deploy
  immediately, whether or not the Rust job has passed. Green CI on the PR
  before merging is the gate; there is no second one.
- **A green deploy job means the new task reached steady state**, not that the
  change did what you wanted. Check the thing itself.

Infra (env vars, sizes, routes, allow-lists) is a change under
`deploy/aws/tofu/main/` and a `tofu apply`. Task definitions are CI-owned
after the first apply, so an env change in tofu reaches a running service on
its next CI deploy, not on the apply. Secrets live in Secrets Manager as
`passband/<service>/<ENV_NAME>`; tofu holds the containers, never the values.

### Grafana specifically

The dashboard is not editable in the UI in any way that survives. It is baked
into the image (`deploy/aws/monitor/Dockerfile.grafana` copies
`deploy/monitoring/grafana/dashboards/` to `/etc/grafana/dashboards`) and
provisioned with `allowUiUpdates: false`. So a dashboard change is a change to
`passband-health.json`, merged to `main`, and the monitor box pulls the new
image itself.

Verify what actually shipped on the monitor box rather than trusting the deploy
status. The access steps are in the operator's private notes, not in this repo.

## Draft PRs: CI runs when a PR is ready, not on every push

Open every PR as a **draft**, and mark it ready for review only once it is
done. `ci` (Rust) and `ci-passband` (Swift) skip draft PRs, and marking a PR
ready starts a full run. `ci-passband` runs on a macOS runner and is slow, so a
PR that pushes ten times while a draft costs one CI run instead of ten.

That makes the draft stage your own responsibility to keep green:

- **Run the checks yourself before marking a PR ready**, the same ones CI runs:
  `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D
  warnings`, and `cargo test --workspace` (or the changed crates' tests while
  iterating, then the workspace once at the end). For client changes, run
  `./passband/test.sh` where a Swift toolchain exists.
- **Mark ready once, when the work is finished**, not as a way to get CI to
  check work in progress. If CI then fails, fix it on the ready PR. Do not flip
  it back to draft to dodge runs.
- Pushes to `main` always run CI. Only PR runs are gated on draft state.
- The checks the `main` ruleset requires are the two gate jobs, `ci-gate` and
  `ci-passband-gate`. Their names must stay unique, because a ruleset matches a
  check by job name alone, not by workflow. They always report, pass when the
  real jobs passed or were correctly skipped, and fail otherwise. `deny`
  (cargo-deny, `deny.toml`) runs with the Rust job: a new advisory or license
  fails it, and an ignore needs a written reason.

Human contributors can open non-draft PRs whenever they like; the rule exists
for agents, which push often.

## Verifying against production

Read state from the real services before believing a doc about them. Docs
drift: a deploy step that went automatic, or a model list that moved on, still
reads as true in the doc that described it.

The AWS CLI works here once you log in (`aws login --profile passband`, then
`export AWS_PROFILE=passband AWS_REGION=us-west-2`; sessions last about 12
hours), so the current answer is usually one command away:

- Config a service is really running with:
  `aws ecs describe-task-definition --task-definition <control|relay|bifrost>`.
- Logs: `aws logs tail /passband/<control|relay|bifrost> --follow`.
- Operator commands on control, the monitor box and secret reads: the access
  steps are in the operator's private notes, not in this repo. Never print a
  secret.

## Commits

Plain commit messages. No `Co-Authored-By` trailers.

## The repo is public: what never goes in it

Everything committed here is public, including history. Before writing a
comment, doc, fixture, test, commit message or PR description, check it
against both lists below. A rewrite of git history is the only fix after the
fact, so the check happens before the commit.

**Never personal data.**

- No real people: not the owner's name, personal email, devices, contacts,
  friends, beta users or tenant labels.
- Nothing from a real mailbox or calendar: no subjects, bodies, senders,
  orders, tracking or confirmation numbers, reservations, bills, searches or
  mailbox statistics.
- Fixtures use invented people at `.example` domains and fake numbers. When a
  real message is the bug report, reduce it to a synthetic one with the same
  shape.

**Never infrastructure or security detail.**

- No AWS account IDs, ARNs, IP addresses, instance IDs, tailnet or machine
  names, auth-provider tenant IDs or cloud project IDs. Real values live in
  gitignored config (`terraform.tfvars`, `backend.hcl`) or Secrets Manager.
  Committed files carry placeholders.
- No security gaps or open-weakness lists.
- No descriptions of how alerting or tripwires work, or what they miss.
- No access recipes specific to this install.
- No incident or outage stories: no dates of what broke, how long it was
  down, or who noticed. Keep the rule the incident taught, without the story.
- No tenant or user counts, and no spend figures.

Private operational notes belong outside this repo.
