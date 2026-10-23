# deploy/aws/tofu

OpenTofu for the AWS side of the Railway move. The contract is
`deploy/aws/DESIGN.md`; this file is how to apply it.

**No secret value ever goes through tofu.** Tofu creates Secrets Manager
*containers* (`passband/<service>/<ENV_NAME>`) and never a version, so no value
can land in state, a plan, a variable or a file. Values go in by hand (CUTOVER.md Phase 2 has the piped forms):

```sh
aws secretsmanager put-secret-value \
  --secret-id passband/control/SQUELCH_CONTROL_COOKIE_KEY \
  --secret-string file:///dev/stdin      # paste, then Ctrl-D
```

`tofu output secret_names` lists every container that needs one.

## Roots

| Root | What | State |
|---|---|---|
| `bootstrap/` | The state bucket (versioned, SSE-KMS, TLS-only, `prevent_destroy`) and the KMS key for both bucket SSE and OpenTofu state encryption | Local on first apply, then migrated into the bucket it made |
| `main/` | Everything else: VPC + fck-nat, ALB + ACM, ECS (control, relay, bifrost), RDS, EFS, the monitor box, the site (S3 + CloudFront), ECR, IAM (task roles, GitHub deploy role), secrets containers, CloudTrail, the budget | S3 backend, `use_lockfile`, client-side state encryption (`aws_kms` key provider, `enforced`) |

Requires OpenTofu >= 1.8 (variables in the backend and encryption blocks).

Files in `main/`: `network.tf` (VPC, subnets, hand-rolled fck-nat),
`security_groups.tf`, `alb.tf`, `ecs.tf`, `ecr.tf`, `rds.tf`, `efs.tf`,
`monitor.tf`, `site.tf`, `iam.tf`, `kms.tf`, `secrets.tf`, `audit.tf`
(CloudTrail + budget), `outputs.tf`.

## Apply order

**The procedure is `deploy/aws/CUTOVER.md`**, and only there, so there are not
two versions to drift apart. In outline, and where each step lives:

| Step | CUTOVER.md |
|---|---|
| GitHub first: the `aws-production` environment (main only) and the repo's customized OIDC sub (`repo`, `context`, `job_workflow_ref`), before the deploy role is used. `tofu output github_deploy_oidc_sub` is the exact string the role trusts | Phase 0d |
| `bootstrap/` with local state, then `-migrate-state` into the bucket it made | Phase 1a |
| `main/terraform.tfvars`, including `fck_nat_ami_name`, `carrier_egress_cidrs`, `operator_cidrs = []` | Phase 1b |
| Certificates (`-target`), validation CNAMEs at the DNS provider | Phase 1c |
| Full apply with services at 0; confirm the security-alerts SNS subscription | Phase 1d |
| Every secret value; the RDS app role (through an SSM port-forward, SCRAM verifier, `operator_db_access` on then off) | Phase 2 |
| Repo variables from `tofu output github_repo_variables`, arm, first CI run | Phase 3a-3c |
| `services_running = true` (the apply waits for steady state) | Phase 3d |
| Bifrost first-run config with `operator_cidrs` set to your /32, then back to `[]` | Phase 4 |

Run as an Identity Center admin session (`aws sso login`), never with access
keys. Requires OpenTofu >= 1.8.

## Who owns what after the first apply

- **Task definitions: CI.** Tofu registers revision 1 with a `:placeholder`
  image and ignores the service's `task_definition`. The deploy job builds each
  new revision from the family's latest one, so an env change made here
  reaches a service on its next CI deploy, not on `tofu apply`.
- **The CloudFront Function's code: the site deploy job.** It bakes the app
  version from `appcast.xml` at build time; tofu creates it from
  `deploy/aws/site/function.js` and ignores `code` afterwards.
- **The monitor box's user data and AMI: nobody.** Both are ignored after
  creation. CI redeploys the box only through the `passband-monitor-deploy`
  SSM document (monitor.tf): it pins `IMAGE_TAG` to a 40-hex commit SHA and
  runs `systemctl reload-or-restart passband-monitor`, nothing else; CI has no
  shell there. `compose.yaml` rides inside the prometheus image at the same
  tag and `bin/up` copies it out, so it changes on an ordinary deploy. A
  bootstrap.sh change lands by `tofu apply -replace=aws_instance.monitor`
  (the data volume survives).

## ECS Exec

On `control` only (`enable_execute_command`), for the operator CLI. Session
output is logged to `/passband/ecs-exec` (KMS, `log_retention_days`), so never
print a secret in an exec session: no `env`, no `printenv`, and treat `llm
mint` output as landing in that log. CloudTrail records every ExecuteCommand
and the `shells` alert mails on each.

## Source-restricted hosts

`bifrost.passband.app` answers only the NAT EIP (control, blackbox),
`carrier_egress_cidrs` (tenants) and `operator_cidrs` (temporary).
`metrics.passband.app` answers only `carrier_egress_cidrs`. Everyone else,
including a forged `Host:` at the ALB's own DNS name, gets the listener's 404.
If carrier's egress address changes, update `carrier_egress_cidrs` and apply
BEFORE (or with) the move, or triage stops fleet-wide. The ALB holds at most 3
source-ip values per rule, so alb.tf cuts each allow-list into rules of 3.

## Detection

`audit.tf` wires CloudTrail to EventBridge to an encrypted SNS topic that
mails `budget_email` on: Secrets Manager reads/writes by anyone but the ECS
execution roles and the monitor role, ECS Exec, SSM sessions, IAM writes,
console sign-ins, and anything that stops or rewrites the trail. Its header
comment lists what it cannot see. GuardDuty is `enable_guardduty` (off; about
$5-15/month).

## Bumping the NAT AMI

`fck_nat_ami_name` is one exact image name; tofu never picks a newer one by
itself. fck-nat republishes its latest minor every six months (only the date
suffix changes). To bump:
```sh
aws ec2 describe-images --region us-west-2 --owners 568608671756   --filters 'Name=name,Values=fck-nat-al2023-hvm-*-arm64-ebs'   --query 'sort_by(Images,&CreationDate)[-3:].[Name,ImageId,CreationDate]' --output table
```
Read the release notes for a version change (github.com/AndrewGuenther/fck-nat),
set the new name, `tofu apply`. The launch template gets a new version and the
NAT instance is replaced: a minute or so with no egress for tasks and the
monitor box, so do it outside a deploy.

## Checks

```sh
tofu fmt -check -recursive deploy/aws/tofu
(cd deploy/aws/tofu/bootstrap && tofu init -backend=false && tofu validate)
(cd deploy/aws/tofu/main && tofu init -backend=false && tofu validate)
trivy config --ignorefile deploy/aws/tofu/.trivyignore deploy/aws/tofu
```

Every accepted trivy finding is listed with its reason in `.trivyignore`.
