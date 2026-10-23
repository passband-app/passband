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

1. **Bootstrap.**
   ```sh
   cd deploy/aws/tofu/bootstrap
   tofu init && tofu apply
   ```
   Then uncomment the `backend "s3"` block in `main.tf`, put in the bucket from
   `tofu output state_bucket`, and `tofu init -migrate-state`.

2. **Configure main.** `cp terraform.tfvars.example terraform.tfvars` and fill
   it with the non-secret values (`state_bucket`, `state_kms_key_arn` from step
   1; the rest from `railway variables -s <svc> --json`). `tofu init`.

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
   the rest with every ECS service at `desired_count = 0`.

5. **Secrets.** Put a value into every container in `tofu output secret_names`.
   For the database:
   - read the master password from `tofu output rds_master_secret_arn` (an
     AWS-managed secret RDS rotates itself);
   - set `operator_db_access = true` and apply; open an SSM session on the
     monitor box (`aws ssm start-session --target $(tofu output -raw
     monitor_instance_id)`), `sudo dnf install -y postgresql16`, and connect
     as `passband_admin` with `sslmode=verify-full` (RDS global bundle);
   - create the app role: `CREATE ROLE control LOGIN PASSWORD '...'; GRANT
     control TO passband_admin; ALTER DATABASE control OWNER TO control;` (and
     a `bifrost` database + role if `bifrost_store = "postgres"`);
   - set `operator_db_access = false` and apply;
   - put `postgres://control:...@<rds_endpoint>/control?sslmode=verify-full`
     into `passband/control/SQUELCH_CONTROL_DATABASE_URL`.

6. **Images.** Set the `AWS_DEPLOY_ROLE_ARN` repo variable from
   `tofu output github_deploy_role_arn`, flip `AWS_DEPLOY_ENABLED`, and run the
   deploy workflow once per service so ECR holds real images.

7. **Start.** `services_running = true`, `tofu apply`.

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
  creation; CI redeploys the box through SSM. To re-run bootstrap from scratch,
  `tofu apply -replace=aws_instance.monitor` (the data volume survives).

## Checks

```sh
tofu fmt -check -recursive deploy/aws/tofu
(cd deploy/aws/tofu/main && tofu init -backend=false && tofu validate)
trivy config --ignorefile deploy/aws/tofu/.trivyignore deploy/aws/tofu
```

Every accepted trivy finding is listed with its reason in `.trivyignore`.
