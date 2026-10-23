# deploy/aws/tofu

OpenTofu for the AWS side of the Railway move. The contract is
`deploy/aws/DESIGN.md`; this file is how to apply it.

**No secret value ever goes through tofu.** Tofu creates Secrets Manager
*containers* (`passband/<service>/<ENV_NAME>`) and never a version, so no value
can land in state, a plan, a variable or a file. Values go in by hand:

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

Run as an Identity Center admin session (`aws sso login`), never with access keys.

0. **GitHub first, before the deploy role exists.** The role's trust pins the
   token's `sub` to
   `repo:passband-app/passband:environment:aws-production:job_workflow_ref:passband-app/passband/.github/workflows/deploy-aws.yml@refs/heads/main`
   (`tofu output github_deploy_oidc_sub`). That string only exists once the
   repo's OIDC sub template is customized, and the environment must exist with
   its branch policy BEFORE any run references it, because GitHub auto-creates
   a missing environment with no protection at all.
   ```sh
   R=passband-app/passband
   # The environment, deployable from main only.
   gh api -X PUT repos/$R/environments/aws-production --input - <<'EOF'
   {"deployment_branch_policy": {"protected_branches": false, "custom_branch_policies": true}}
   EOF
   gh api -X POST repos/$R/environments/aws-production/deployment-branch-policies \
     -f name=main -f type=branch
   # Optional, recommended: a required reviewer (add to the PUT body above)
   #   "reviewers": [{"type": "User", "id": <gh api user --jq .id>}]

   # The repo's OIDC sub template: repo + context (environment:<name>) + job_workflow_ref.
   gh api -X PUT repos/$R/actions/oidc/customization/sub --input - <<'EOF'
   {"use_default": false, "include_claim_keys": ["repo", "context", "job_workflow_ref"]}
   EOF
   # Check both.
   gh api repos/$R/actions/oidc/customization/sub
   gh api repos/$R/environments/aws-production/deployment-branch-policies --jq '.branch_policies[].name'
   ```
   The rendered format is GitHub's: "repo:ORG-NAME/REPO-NAME:environment:ENVIRONMENT-NAME:job_workflow_ref:REUSABLE-WORKFLOW-PATH"
   (docs.github.com/en/actions/reference/security/oidc, "Customizing the
   subject claims"). For a workflow that is not reusable, `job_workflow_ref` is
   the file itself at the ref it ran from, so the trust alone refuses another
   branch, a tag, a PR, and every other workflow on main. The template is
   repo-wide: today `deploy-aws.yml` is the only workflow with `id-token:
   write`; a future OIDC workflow gets this sub format too.

   Environment branch policies on a private repo need a paid GitHub plan. If
   the PUT is refused for that reason, the sub pin above still holds by itself.

1. **Bootstrap.**
   ```sh
   cd deploy/aws/tofu/bootstrap
   tofu init && tofu apply
   ```
   Then uncomment the `backend "s3"` block in `main.tf`, put in the bucket and
   key ARN from `tofu output`, and `tofu init -migrate-state`. The bootstrap
   root has no client-side `encryption` block (its own key cannot encrypt the
   state that creates it); its state is SSE-KMS in the bucket and holds no
   secret, only the bucket and key. The `main` root's state is client-side
   encrypted and `enforced`.

2. **Configure main.** `cp terraform.tfvars.example terraform.tfvars` and fill
   it with the non-secret values (`state_bucket`, `state_kms_key_arn` from step
   1; the rest from `railway variables -s <svc> --json`). Three need care:
   - `fck_nat_ami_name`: the exact current name (see "Bumping the NAT AMI").
   - `carrier_egress_cidrs`: carrier's egress address. Believed to be
     `5.78.205.240/32`; confirm by running `curl -s ifconfig.me` ON carrier
     (and from a pod, in case pod egress leaves differently). Wrong here = every
     tenant loses Bifrost and metrics stops arriving.
   - `operator_cidrs`: leave `[]` for now (step 7).

   `tofu init`.

3. **Certificates first.** The ALB listener and CloudFront need validated
   certificates, and DNS is not ours to write:
   ```sh
   tofu apply -target=aws_acm_certificate.alb -target=aws_acm_certificate.site
   tofu output acm_validation_records
   ```
   Create those CNAMEs at the DNS provider.

4. **Everything else.** `deploy/aws/monitor/bootstrap.sh` and
   `deploy/aws/site/function.js` must exist (a precondition refuses the plan
   otherwise). `tofu apply`. It waits on certificate validation, then builds
   the rest with every ECS service at `desired_count = 0`. Then **confirm the
   SNS subscription** mailed to `budget_email` (security alerts, see
   "Detection"); until you click it, the tripwires mail nobody.

5. **Secrets.** Put a value into every container in `tofu output secret_names`.
   Each `put-secret-value` mails a security alert; that is expected now.
   For the database:
   - set `operator_db_access = true` and apply;
   - on your laptop, read the master password (an AWS-managed secret RDS
     rotates itself; this read mails an alert too):
     `aws secretsmanager get-secret-value --secret-id "$(tofu output -raw rds_master_secret_arn)" --query SecretString --output text | jq -r .password`
   - open an SSM session on the monitor box (`aws ssm start-session --target
     $(tofu output -raw monitor_instance_id)`), then:
     ```sh
     sudo -i
     export PSQL_HISTORY=/dev/null          # nothing typed lands on the root volume
     dnf install -y postgresql16
     # The RDS CA bundle, for verify-full. Same file control embeds (pg_tls.rs):
     curl -fsSo /root/rds-global-bundle.pem https://truststore.pki.rds.amazonaws.com/global/global-bundle.pem
     sha256sum /root/rds-global-bundle.pem   # compare with the sha256 in squelch-control/src/pg_tls.rs
                                             # (a mismatch can just mean AWS added a CA; check grep -c BEGIN)
     psql "host=<rds_endpoint WITHOUT :5432> port=5432 dbname=control user=passband_admin sslmode=verify-full sslrootcert=/root/rds-global-bundle.pem"
     ```
     (paste the master password at the prompt);
   - generate the app password on your laptop with `openssl rand -hex 32`. It
     MUST be hex (or otherwise URL-safe): control's URL parser takes
     everything up to the first `@` as credentials, so `@`, `/`, `:` or `%`
     in a password break the URL;
   - create the role without the password ever appearing in a statement
     (`\password` sends a SCRAM hash, so neither psql history nor RDS's
     error-statement log sees it):
     ```sql
     CREATE ROLE control LOGIN;
     \password control
     GRANT control TO passband_admin;
     ALTER DATABASE control OWNER TO control;
     ```
     (and a `bifrost` database + role the same way if `bifrost_store = "postgres"`);
   - `exit` the session, set `operator_db_access = false` and apply;
   - put `postgres://control:<hex>@<rds_endpoint host>:5432/control?sslmode=verify-full`
     into `passband/control/SQUELCH_CONTROL_DATABASE_URL` (via
     `file:///dev/stdin`, as above, never on the command line).

6. **Images.** Set the repo variables the workflow reads (none is a secret)
   straight from tofu, then arm it:
   ```sh
   tofu output -json github_repo_variables \
     | jq -r 'to_entries[] | "\(.key) \(.value)"' \
     | while read -r k v; do gh variable set "$k" --repo passband-app/passband --body "$v"; done
   gh variable set AWS_DEPLOY_ENABLED --repo passband-app/passband --body true
   gh workflow run deploy-aws.yml --repo passband-app/passband --ref main -f service=all
   ```
   That sets `AWS_ACCOUNT_ID`, `AWS_SITE_BUCKET`, `AWS_SITE_DISTRIBUTION_ID`,
   `AWS_SITE_FUNCTION_NAME` and `AWS_MONITOR_INSTANCE_ID`. There is no role-ARN
   variable: the workflow builds `arn:aws:iam::<AWS_ACCOUNT_ID>:role/passband-github-deploy`.

   **arm64 runners.** The Rust images build natively on `ubuntu-24.04-arm`.
   GitHub's hosted arm64 runners may not be available to this private repo on
   its plan; if the ecs jobs sit in "Waiting for a runner", create an arm64
   larger runner (or register a self-hosted arm64 one) and set its label in
   the optional `AWS_ARM64_RUNNER` variable.

   With `desired_count = 0` these ECS deploys go green vacuously (0 running ==
   0 desired); they only prove the images exist. Step 7 is the real check.

7. **Start.** In `terraform.tfvars` set `services_running = true` and, for
   Bifrost's first-run config only, `operator_cidrs = ["<curl -s ifconfig.me>/32"]`.
   `tofu apply`: with `services_running` on, it waits for each ECS service to
   reach steady state and fails if one does not.
   - **Bifrost first run** (the store is fresh, so it has NO admin auth until
     you set it; the ALB rule admits only the NAT EIP, carrier and
     `operator_cidrs`, which is what keeps this window closed). Reach it at
     `https://bifrost.passband.app` (before DNS cutover, point that name at
     one of the ALB's addresses in `/etc/hosts`). Then: set the NEW admin
     credential and turn auth on; turn virtual-key enforcement on; add the
     Anthropic provider key with the model list the fleet uses TODAY (read it
     from Railway, do not copy an old doc); turn content logging OFF.
   - Set `operator_cidrs = []` and `tofu apply` again.
   - **Re-run the workflow for every service** (`gh workflow run deploy-aws.yml
     --ref main -f service=all`). This time the deploy action waits for a
     real task to go healthy behind the ALB, which is the first deploy that
     proves anything.

8. **Cutover DNS** from `tofu output service_dns_records` (`deploy/aws/CUTOVER.md`).
   The apex `passband.app` needs CNAME flattening / ALIAS at the provider.

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
