# Working in this repo

## Deploys: merge to `main`; production is AWS

Production runs on AWS (account `824648488986`, `us-west-2`). It moved off
Railway after Railway's env-var breach on 2026-10-21, and Railway serves
nothing now. The architecture is `deploy/aws/DESIGN.md`, the infrastructure is
OpenTofu in `deploy/aws/tofu/` (its README says how to apply it), and
`deploy/aws/CUTOVER.md` is the record of the move. Tenants are not on AWS:
they still run on `carrier` (k3s on Hetzner), managed by the warden.

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

Verify what actually shipped rather than trusting the deploy status, from an
SSM session on the box (`aws ssm start-session --target <instance id>`; the id
is the repo variable `AWS_MONITOR_INSTANCE_ID`):

```sh
cd /opt/passband-monitor && sudo docker compose exec -T grafana \
  grep -c last_over_time /etc/grafana/dashboards/passband-health.json
```

## Draft PRs: CI runs when a PR is ready, not on every push

Open every PR as a **draft**, and mark it ready for review only once it is
done. `ci` (Rust) and `ci-passband` (Swift) skip draft PRs, and marking a PR
ready starts a full run. This repo is private, so every CI minute is billed, and
`ci-passband` runs on a macOS runner at 10x the Linux rate. A PR that pushes
ten times while a draft costs one CI run instead of ten.

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

Read state from the real services before believing a doc about them. Two
findings from one afternoon in August 2026, both from docs that were true when
written:

- `deploy/monitoring/README.md` said to redeploy Grafana by hand. Grafana had
  been auto-deploying from `main` for some time.
- `deploy/hosted/SETUP.md` §6b said to set the Bifrost provider key's models to
  `claude-haiku-4-5, claude-sonnet-5`. The fleet had moved to `claude-opus-5`
  and every hosted tenant's triage had been failing for four days.

The AWS CLI works here once you log in (`aws login --profile passband`, then
`export AWS_PROFILE=passband AWS_REGION=us-west-2`; sessions last about 12
hours), so the current answer is usually one command away:

- Config a service is really running with:
  `aws ecs describe-task-definition --task-definition <control|relay|bifrost>`.
- Logs: `aws logs tail /passband/<control|relay|bifrost> --follow`.
- An operator command on control (`squelch-control llm sync --check`,
  `tenants`, `llm mint`, ...) runs over ECS Exec, enabled on control only:

  ```sh
  t=$(aws ecs list-tasks --cluster passband --service-name control \
    --desired-status RUNNING --query 'taskArns[0]' --output text)
  aws ecs execute-command --cluster passband --task "$t" --container control \
    --interactive --command "squelch-control llm sync --check"
  ```

  Exec session output is logged to `/passband/ecs-exec`, so never print a
  secret there (no `env`, no `printenv`).
- The monitor box: `aws ssm start-session --target <AWS_MONITOR_INSTANCE_ID>`.
- A secret: `aws secretsmanager get-secret-value --secret-id
  passband/<svc>/<NAME> --query SecretString --output text`, piped into
  whatever needs it, never printed.

Every Secrets Manager read or write, ECS Exec, SSM session and console sign-in
mails a security alert to the operator. That is the tripwire working, not a
reason to avoid the command: just expect the mail.

## Commits

Plain commit messages. No `Co-Authored-By` trailers.
