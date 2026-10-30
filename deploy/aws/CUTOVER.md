# Railway → AWS cutover runbook

> **Historical record.** Phases 0 to 9 ran 2026-10-23 to 2026-10-26 and
> production has served from AWS since. Phase 10 (deleting the Railway
> project, its tokens and the GitHub App) is the only part left. For how
> production works today, read `DESIGN.md` and the root `CLAUDE.md`, not this
> file; the commands below assume a Railway that is going away.

The operator runs this top to bottom. The design is `deploy/aws/DESIGN.md`; how
tofu is laid out is `deploy/aws/tofu/README.md`. This file is the order of
operations, the commands, and how to prove each step on the real system.

Ground rules for the whole run:

- **No secret value in argv, shell history, a file in the repo, or a terminal
  scrollback.** Values travel on stdin (`--secret-string file:///dev/stdin`,
  `--from-file=…=/dev/stdin`, `curl -K -`). Check a stored value by its LENGTH,
  never by printing it.
- **Verify the real system, not a deploy status.** Every phase ends with a
  VERIFY that asks the running thing (curl, ECS running count, ECS Exec,
  Grafana datasource health). A green workflow run is not a VERIFY.
- **Short blips are acceptable, lost data is not.** No paying users yet
  (2026-10-23). The two places data can be lost are the control database
  (Phase 6) and the relay's opens buffer (Phase 7); both have explicit steps.
- Railway keeps serving until each service's DNS flips. Nothing here deletes
  anything on Railway before Phase 10.
- **Expect security-alert mail all the way through.** From Phase 1 on, the
  `passband-security-alerts` topic mails `budget_email` on every Secrets
  Manager read or write by you (each `put`, `putin`, `sget`, the RDS master
  password read), every `cexec` (ECS Exec), every SSM session, every console
  sign-in, and every `tofu apply` that touches IAM. That is the tripwire
  working. Skim each one anyway: a mail you cannot match to a command you just
  ran is the whole point of having it.
- **Two hosts do not answer your laptop.** `bifrost.passband.app` answers only
  the NAT EIP (control, blackbox), carrier, and `operator_cidrs`;
  `metrics.passband.app` answers only carrier. Anyone else, you included, gets
  the ALB's 404, even with `--resolve`. Phase 4 opens Bifrost to your /32 for
  its configuration and closes it again; Prometheus is queried through
  Grafana's datasource proxy (`promq`, Phase 5).

Hosted tenants at the time of writing: `braelyn braelyn-work ellie ellie-atuin
ethan-2 phiilpp we-z`. The authority is `squelch-control tenants` (Phase 6), not
this line.

## Phase order, and why it differs from the obvious one

| # | Phase | Why here |
|---|---|---|
| 0 | AWS account, tools, GitHub OIDC + environment, prep that has lead time | DNS TTLs need a day to age out; the new APNs key id is a required tofu variable; the deploy role's trust names a `sub` that exists only once GitHub is set up |
| 1 | Tofu: bootstrap, certs, full apply (services at 0) | |
| 2 | Every secret, RDS app role | Tasks cannot start without a value in every container; Grafana's admin password only takes on its FIRST boot |
| 3 | Repo variables, first images, services on | |
| 4 | Bifrost fresh, configured privately | Needs a running task; must have auth on before anything public points at it |
| 5 | Monitoring | Before any tenant-touching phase, so the new dashboard watches Phases 7 and 8 roll the fleet |
| 6 | Control: database move, signup DNS | `llm mint` (Phase 8) writes key ids into the control store, so the store must already be on RDS |
| 7 | Relay, `apns-relay-2` | Before Phase 8: an `llm mint` re-applies a tenant's whole Deployment from today's render, so if the warden already names `apns-relay-2`, each mint also delivers the relay Secret and every tenant rolls ONCE, not twice |
| 8 | Bifrost DNS, `llm mint` every tenant | Point of no return for the LLM path |
| 9 | Landing site | Independent; last so a site regression cannot be confused with a backend one |
| 10 | Teardown, docs owed | |

---

## Shell setup (every terminal, every phase)

```sh
cd ~/Developer/squelch            # the main checkout, on an up-to-date main
export AWS_PROFILE=passband AWS_REGION=us-west-2
TF=deploy/aws/tofu/main
WORK=${WORK:-$(mktemp -d)} && chmod 700 "$WORK"   # dumps and scratch; deleted in Phase 10. Reuse the same path in every terminal.

# put <secret-id>: reads a value from the TERMINAL without echo and stores it.
put() { local v; printf 'value for %s (hidden): ' "$1" >&2; IFS= read -rs v; echo >&2
  [ -n "$v" ] || { echo "empty, refusing" >&2; return 1; }
  printf '%s' "$v" | aws secretsmanager put-secret-value --secret-id "$1" \
    --secret-string file:///dev/stdin --query VersionId --output text; unset v; }

# putin <secret-id>: stores whatever arrives on stdin (strip newlines first).
putin() { aws secretsmanager put-secret-value --secret-id "$1" \
    --secret-string file:///dev/stdin --query VersionId --output text; }

# rw <service> <VAR>: one Railway variable, raw, no trailing newline. JSON
# only: the table view TRUNCATES values. Fails (non-zero) if the var is unset.
rw() { railway variables -s "$1" --json | jq -je --arg k "$2" '.[$k] // error("unset: \($k)")'; }

# sget <secret-id>: a stored value on stdout, no newline. Pipe it, never print it.
sget() { aws secretsmanager get-secret-value --secret-id "$1" --query SecretString --output text | tr -d '\n'; }

# cexec "<cmd>": run a command inside the running control task (ECS Exec).
cexec() { local t; t=$(aws ecs list-tasks --cluster passband --service-name control \
    --desired-status RUNNING --query 'taskArns[0]' --output text)
  aws ecs execute-command --cluster passband --task "$t" --container control \
    --interactive --command "$*"; }

alb_ip() { dig +short "$(tofu -chdir=$TF output -raw alb_dns_name)" | head -1; }
myip() { curl -fsS https://checkip.amazonaws.com | tr -d '\n'; }

# carrier_curl <args…>: curl from a throwaway pod on carrier, i.e. from the
# source address the ALB allow-list sees for tenants. Prints the status code.
carrier_curl() { kubectl -n default run "cc-$RANDOM" --rm -i --quiet --restart=Never \
    --image=curlimages/curl:8.10.1 -- -s -o /dev/null -w '%{http_code}\n' "$@"; }
set -o pipefail
```

`rw` and `sget` exist so a value goes from one store to the next through a
pipe. If you ever see a secret on screen, treat it as leaked and rotate it.

---

## Phase 0: AWS account, tools, lead-time prep

**Preconditions.** The `move-clouds` work is merged to `main` and Railway
`control` is healthy after that merge (`curl -s https://signup.passband.app/healthz`
→ `ok`). That merge carries the Postgres TLS change: Railway's private URL has
no `sslmode`, so it must keep dialing plaintext. If control broke on that merge,
stop here.

### 0a. Account

1. Create the management account (a new root email you control; hardware MFA
   on root; no root access keys). Enable **AWS Organizations**.
2. In Organizations, create a member account `passband-prod`. Workloads live
   there, never in the management account.
3. Enable **IAM Identity Center** in the management account, home region
   `us-west-2`. Create your user (MFA required), a permission set
   `AdministratorAccess` (session 4h), and assign it to `passband-prod`.
4. No IAM users, no access keys, anywhere. CI uses OIDC (Phase 3).

### 0b. Tools

```sh
brew install opentofu awscli jq postgresql@17
brew install --cask session-manager-plugin
tofu version            # >= 1.8
session-manager-plugin --version
which htpasswd          # macOS ships /usr/sbin/htpasswd
aws configure sso --profile passband     # start URL from Identity Center; account passband-prod; region us-west-2
aws sso login --profile passband
aws sts get-caller-identity              # Account = passband-prod, Arn = …AWSReservedSSO_AdministratorAccess…
```

### 0c. Lead-time prep (do at least 24h before Phase 6)

1. **Lower DNS TTLs** to the minimum (Namecheap: 1 min) on every record that
   will flip: `signup.passband.app`, `bifrost.passband.app`, `passband.app`,
   `www.passband.app` (Namecheap), and `relay.squelch.sh` (at whichever
   provider hosts `squelch.sh`). The old TTL must age out before the flip
   means anything.
2. **CAA check.** If either zone has CAA records, ACM needs `amazon.com`:
   `dig +short CAA passband.app; dig +short CAA squelch.sh`. Empty is fine.
3. **New APNs key.** Apple Developer → Keys → `+`, enable APNs. Apple allows
   **two APNs keys per team**: if two exist, the second slot must be freed
   first, and NOT by revoking the key Railway's relay uses today. Download the
   `.p8` once into `$WORK`; note the Key ID (it is `relay_apns_key_id` in
   Phase 1). The old key is revoked in Phase 7, after the new relay delivers.
4. **New Slack webhooks** (old ones are in the breach): one for
   `#passband-signups` (control) and one for the Grafana alerts channel. Create
   them now; revoke the old ones in Phases 5 and 6.
5. **carrier's egress address** (it becomes `carrier_egress_cidrs` in 1b, the
   only non-AWS source Bifrost and metrics will answer). From a pod, because
   that is the path tenants take:

   ```sh
   kubectl -n default run "ip-$RANDOM" --rm -i --quiet --restart=Never \
     --image=curlimages/curl:8.10.1 -- -fsS https://checkip.amazonaws.com
   ```

   Believed to be `5.78.205.240`. If it differs from the node's own address
   (`curl -s https://checkip.amazonaws.com` over SSH on carrier), list both.

### 0d. GitHub: OIDC subject and the `aws-production` environment

Before the deploy role is ever used. The role's trust (tofu `iam.tf`) pins the
token's `sub` to the CUSTOMIZED form

`repo:passband-app@336248739/passband@1293146964:environment:aws-production:job_workflow_ref:passband-app/passband/.github/workflows/deploy-aws.yml@refs/heads/main`

The repo uses GitHub's IMMUTABLE subjects, so the `repo:` part carries owner
and repo IDs (tofu `github_sub_prefix`, copied from the API's
`sub_claim_prefix`); `job_workflow_ref` stays name-based. If the VERIFY below
shows a different `sub_claim_prefix`, set `github_sub_prefix` to it.

(`tofu -chdir=$TF output -raw github_deploy_oidc_sub` after Phase 1). That
string exists only once the repo's sub template includes those claims. The
environment must exist, with its branch policy, before any run references it:
GitHub auto-creates a missing environment with no protection at all.

```sh
R=passband-app/passband
gh api -X PUT repos/$R/environments/aws-production --input - <<'EOF'
{"deployment_branch_policy": {"protected_branches": false, "custom_branch_policies": true}}
EOF
gh api -X POST repos/$R/environments/aws-production/deployment-branch-policies -f name=main -f type=branch
gh api -X PUT repos/$R/actions/oidc/customization/sub --input - <<'EOF'
{"use_default": false, "include_claim_keys": ["repo", "context", "job_workflow_ref"]}
EOF
```

Two locks, either sufficient on its own: the trust refuses any other branch, a
tag, a PR, and any other workflow file on main (its `job_workflow_ref`
differs), and the environment's branch policy refuses non-main refs. Branch
policies on a private repo's environment need a paid GitHub plan; if the PUT is
refused for that reason, the sub pin still holds. The template is repo-wide:
today `deploy-aws.yml` is the only workflow with `id-token: write`, so nothing
else changes.

**VERIFY.** `aws sts get-caller-identity` returns the member account; `dig`
shows the new TTLs (`dig signup.passband.app | grep -A1 'ANSWER SECTION'`);
GitHub holds both settings:

```sh
gh api repos/$R/actions/oidc/customization/sub                     # use_default false, the three keys, sub_claim_prefix == tofu github_sub_prefix
gh api repos/$R/environments/aws-production/deployment-branch-policies --jq '.branch_policies[].name'   # main
```

**ROLLBACK.** Nothing serves from AWS yet. The sub template can go back with
`{"use_default": true}`; nothing else uses OIDC.

---

## Phase 1: tofu

**Preconditions.** Phase 0. `deploy/aws/monitor/bootstrap.sh` and
`deploy/aws/site/function.js` exist on main (a precondition refuses the plan
otherwise).

### 1a. Bootstrap root, then migrate its state into the bucket it made

```sh
tofu -chdir=deploy/aws/tofu/bootstrap init
tofu -chdir=deploy/aws/tofu/bootstrap apply
tofu -chdir=deploy/aws/tofu/bootstrap output -raw state_bucket; echo
tofu -chdir=deploy/aws/tofu/bootstrap output -raw state_kms_key_arn; echo
```

Uncomment the `backend "s3"` block in `deploy/aws/tofu/bootstrap/main.tf` and
put the bucket name in (an account id is not a secret; commit it so the next
operator does not init into local state). Then:

```sh
tofu -chdir=deploy/aws/tofu/bootstrap init -migrate-state    # answer yes
rm -f deploy/aws/tofu/bootstrap/terraform.tfstate*           # only after the migrate succeeded
tofu -chdir=deploy/aws/tofu/bootstrap plan                   # "No changes" from the S3 state
```

### 1b. Main root: values

```sh
cp $TF/terraform.tfvars.example $TF/terraform.tfvars        # gitignored
railway variables -s control --json | jq '{SQUELCH_CONTROL_BASE_DOMAIN,SQUELCH_CONTROL_CLIENT_ID,SQUELCH_CONTROL_WARDEN_URL,SQUELCH_CONTROL_INVITE_FROM,SQUELCH_CONTROL_LLM_BUDGET_USD,SQUELCH_CONTROL_LLM_MODELS,SQUELCH_CONTROL_ASSISTANT_MODELS,SQUELCH_CONTROL_PUBLIC_URL,SQUELCH_CONTROL_TRUSTED_PROXY_HOPS}'
railway variables -s squelch --json | jq '{SQUELCH_RELAY_APNS_TEAM_ID,SQUELCH_RELAY_APNS_TOPICS}'
```

Those are non-secret (ids, URLs, model lists). Fill `terraform.tfvars`:
`state_bucket`, `state_kms_key_arn` (1a), `budget_email`,
`control_base_domain`, `control_client_id` (the `google-oauth-client-2` web
client id), `control_warden_url`, `control_invite_from`,
`control_llm_budget_usd`, `control_llm_models`, `control_assistant_models`
(copy verbatim: these become the provider key's allow-list via `llm sync`),
`relay_apns_team_id`, `relay_apns_key_id` (the **new** key from 0c, not
Railway's), `services_running = false`, `bifrost_store = "efs"`, and three
that have no Railway counterpart:

- `fck_nat_ami_name`: one EXACT image name, no wildcard (tofu never picks a
  newer NAT AMI by itself). Take the newest from:

  ```sh
  aws ec2 describe-images --owners 568608671756 \
    --filters 'Name=name,Values=fck-nat-al2023-hvm-*-arm64-ebs' \
    --query 'sort_by(Images,&CreationDate)[-3:].[Name,ImageId,CreationDate]' --output table
  ```

- `carrier_egress_cidrs`: the address(es) from 0c step 5, as `/32`s. A wrong
  value here cuts every tenant off from LLM triage the moment Phase 8 flips
  DNS (Phase 8 checks it first).
- `operator_cidrs = []`. Phase 4 sets it to your /32 for Bifrost's
  configuration and puts it back.

`budget_email` also receives the security alerts; `enable_guardduty` stays
`false` unless you want it (about $5-15/month).

Railway carries no `SQUELCH_CONTROL_ASSISTANT_BUDGET_USD`,
`_WAITLIST_ORIGIN` or `_LOG`; leave those unset (binary defaults, as today).

### 1c. Certificates first (DNS is not tofu's)

```sh
tofu -chdir=$TF init
tofu -chdir=$TF apply -target=aws_acm_certificate.alb -target=aws_acm_certificate.site
tofu -chdir=$TF output -json acm_validation_records | jq .
```

Create each CNAME at the DNS provider. **Namecheap's Host field takes the name
WITHOUT the zone**: for `_abc123.signup.passband.app.` enter `_abc123.signup`;
for the apex's `_def456.passband.app.` enter `_def456`. Pasting the full name
creates `…passband.app.passband.app` and the cert never validates. The
`relay.squelch.sh` record goes in the `squelch.sh` zone.

### 1d. Everything else

```sh
tofu -chdir=$TF apply          # waits on validation, then builds the rest; services at desired_count 0
tofu -chdir=$TF output secret_names
```

Then **confirm the SNS subscription**: AWS mails `budget_email` a
"Subscription Confirmation" for `passband-security-alerts`. Until you click
it, the tripwires mail nobody. From this apply on, the alerts described under
the ground rules start arriving.

The monitor box boots now and its unit retries every minute: no images in ECR
and no monitor secrets yet. That is expected until Phase 3.

**VERIFY.**

```sh
aws acm list-certificates --region us-west-2 --query 'CertificateSummaryList[].[DomainName,Status]' --output table   # ISSUED
aws acm list-certificates --region us-east-1 --query 'CertificateSummaryList[].[DomainName,Status]' --output table   # ISSUED (CloudFront)
aws ecs describe-services --cluster passband --services control relay bifrost \
  --query 'services[].[serviceName,desiredCount,runningCount]' --output table                                      # all 0 0
aws rds describe-db-instances --db-instance-identifier passband \
  --query 'DBInstances[0].[DBInstanceStatus,StorageEncrypted,DeletionProtection,Endpoint.Address]' --output text   # available True True …
aws ssm describe-instance-information --query 'InstanceInformationList[].[InstanceId,PingStatus]' --output text    # monitor: Online
aws ec2 describe-nat-gateways --query 'NatGateways[?State==`available`]' --output text                             # empty (fck-nat instance, not a gateway)
aws sns list-subscriptions-by-topic --topic-arn "$(tofu -chdir=$TF output -raw security_alerts_topic_arn)" \
  --query 'Subscriptions[].SubscriptionArn' --output text                                                          # an ARN, not PendingConfirmation
tofu -chdir=$TF output -raw nat_egress_ip; echo                                                                    # the address control and blackbox egress from
aws elbv2 describe-rules --listener-arn "$(aws elbv2 describe-listeners --load-balancer-arn \
    "$(aws elbv2 describe-load-balancers --names passband --query 'LoadBalancers[0].LoadBalancerArn' --output text)" \
    --query 'Listeners[?Port==`443`].ListenerArn' --output text)" \
  --query 'Rules[].[Priority,join(`,`,Conditions[?Field==`source-ip`].SourceIpConfig.Values[])]' --output text       # 100+: NAT EIP + carrier; 200+: carrier
```

**ROLLBACK.** Nothing is public. Leave it; stateful resources refuse
`destroy` by design.

---

## Phase 2: secrets and the database role

**Preconditions.** Phase 1. You have: the new APNs `.p8` in `$WORK`, the two
new Slack webhook URLs, Railway CLI access.

Every container must hold a value before Phase 3 except
`passband/monitor/BLACKBOX_BIFROST_CANARY_KEY` (Phase 4; blackbox renders a
dead placeholder and the canary fails visibly until then) and
`passband/control/SQUELCH_CONTROL_DATABASE_URL` (2d, still this phase).

### 2a. Copied from Railway (already rotated by the operator)

```sh
rw control SQUELCH_CONTROL_CLIENT_SECRET  | putin passband/control/SQUELCH_CONTROL_CLIENT_SECRET
rw control SQUELCH_CONTROL_WARDEN_TOKEN   | putin passband/control/SQUELCH_CONTROL_WARDEN_TOKEN
rw control SQUELCH_CONTROL_ADMIN_TOKEN    | putin passband/control/SQUELCH_CONTROL_ADMIN_TOKEN
rw control SQUELCH_CONTROL_RESEND_API_KEY | putin passband/control/SQUELCH_CONTROL_RESEND_API_KEY
rw control SQUELCH_CONTROL_WORKOS_API_KEY | putin passband/control/SQUELCH_CONTROL_WORKOS_API_KEY
rw blackbox BLACKBOX_CANARY_MODEL         | putin passband/monitor/BLACKBOX_CANARY_MODEL
```

Not carried: `BIFROST_ADMIN_USERNAME`/`_PASSWORD` on Railway control (read by
nothing, and leaked), `SQUELCH_CONTROL_ALERT_WEBHOOK` (legacy name).

### 2b. Generated fresh

```sh
openssl rand -base64 48 | tr -d '\n' | putin passband/control/SQUELCH_CONTROL_COOKIE_KEY

# Bifrost admin credential: username:password, one colon, >= 32 chars. Hex keeps ':' out of the halves.
# Bifrost's password policy wants an uppercase letter and a special character;
# plain hex is refused at "enable auth". No ':' in the password half.
printf 'passband-ops:Pb-%s' "$(openssl rand -hex 24)" | putin passband/control/SQUELCH_CONTROL_BIFROST_ADMIN_TOKEN

# Relay: two different bearers, 64 hex chars each.
openssl rand -hex 32 | tr -d '\n' | putin passband/relay/SQUELCH_RELAY_AUTH_TOKEN
openssl rand -hex 32 | tr -d '\n' | putin passband/relay/SQUELCH_RELAY_PUSH_TOKEN
aws secretsmanager put-secret-value --secret-id passband/relay/SQUELCH_RELAY_APNS_KEY \
  --secret-string "file://$WORK/AuthKey_<KEYID>.p8" --query VersionId --output text
rm -P "$WORK"/AuthKey_*.p8      # keep the only other copy in your password manager, not on disk

# Monitoring: ONE remote-write password under two names, plus its bcrypt.
PW=$(openssl rand -hex 32)
printf '%s' "$PW" | putin passband/monitor/PROM_REMOTE_WRITE_PASSWORD
printf '%s' "$PW" | putin passband/monitor/PROM_PASSWORD
printf '%s\n' "$PW" | htpasswd -niBC 10 remote | cut -d: -f2- | tr -d '\n' | putin passband/monitor/PROM_WEB_BCRYPT
unset PW
openssl rand -hex 24 | tr -d '\n' | putin passband/monitor/GF_SECURITY_ADMIN_PASSWORD

# Bifrost's provider key: a PLACEHOLDER until auth is on (Phase 4). Bifrost
# starts with auth OFF. The ALB already admits only the NAT EIP, carrier and
# operator_cidrs, so the window is small; the placeholder makes it worth
# nothing too (carrier runs tenant pods, and the NAT EIP is shared by every
# task), for the price of one extra put in Phase 4.
printf 'placeholder-until-bifrost-auth-is-on' | putin passband/bifrost/ANTHROPIC_API_KEY
```

Monitor values must not contain `'` or a newline (the box's `fetch-secrets`
refuses them by name); hex never does.

### 2c. Pasted (new Slack webhooks)

```sh
put passband/control/SQUELCH_CONTROL_ALERT_WEBHOOK_URL    # new #passband-signups webhook
put passband/monitor/SLACK_ALERTS_WEBHOOK_URL             # new alerts webhook. REQUIRED: Grafana exits at boot without it
```

### 2d. RDS app role and DATABASE_URL

This is the one procedure for the database role (the tofu README defers to
it). It tunnels through the monitor box rather than running psql on it, so the
Phase 6 dump file never has to reach the box.

```sh
grep -q '^operator_db_access' $TF/terraform.tfvars || echo 'operator_db_access = false' >> $TF/terraform.tfvars
sed -i '' 's/^operator_db_access.*/operator_db_access = true/' $TF/terraform.tfvars
tofu -chdir=$TF apply
RDS_HOST=$(tofu -chdir=$TF output -raw rds_endpoint | cut -d: -f1)
MON=$(tofu -chdir=$TF output -raw monitor_instance_id)
```

In a **second terminal** (same shell setup), keep this open:

```sh
aws ssm start-session --target "$MON" --document-name AWS-StartPortForwardingSessionToRemoteHost \
  --parameters "{\"host\":[\"$RDS_HOST\"],\"portNumber\":[\"5432\"],\"localPortNumber\":[\"15432\"]}"
```

Back in the first. `hostaddr` dials the tunnel while `host` is what
`verify-full` checks the certificate against:

```sh
export PGSSLMODE=verify-full PGSSLROOTCERT=$PWD/squelch-control/certs/rds-global-bundle.pem
admin() { PGPASSWORD=$(aws secretsmanager get-secret-value \
    --secret-id "$(tofu -chdir=$TF output -raw rds_master_secret_arn)" \
    --query SecretString --output text | jq -r .password) \
  psql -X "host=$RDS_HOST hostaddr=127.0.0.1 port=15432 dbname=passband user=passband_admin" "$@"; }
admin -c 'select version()'                    # PostgreSQL 16.x, over TLS

CTRL_PW=$(openssl rand -hex 32)
# The server gets a SCRAM verifier, never the password: a failed statement is
# logged WITH its text (log_min_error_statement), and that log is CloudWatch.
scram() { python3 -I -c 'import sys,os,hashlib,hmac,base64 as b
p=sys.stdin.buffer.read(); s=os.urandom(16); k=hashlib.pbkdf2_hmac("sha256",p,s,4096)
c=hmac.new(k,b"Client Key","sha256").digest(); v=hmac.new(k,b"Server Key","sha256").digest()
e=lambda x:b.b64encode(x).decode()
sys.stdout.write("SCRAM-SHA-256$4096:%s$%s:%s"%(e(s),e(hashlib.sha256(c).digest()),e(v)))'; }
printf "CREATE ROLE control LOGIN PASSWORD '%s';\nGRANT control TO passband_admin;\nALTER DATABASE passband OWNER TO control;\n" \
  "$(printf '%s' "$CTRL_PW" | scram)" | admin -v ON_ERROR_STOP=1
printf 'postgres://control:%s@%s:5432/passband?sslmode=verify-full' "$CTRL_PW" "$RDS_HOST" \
  | putin passband/control/SQUELCH_CONTROL_DATABASE_URL
unset CTRL_PW
```

Leave `operator_db_access = true` and the tunnel available until Phase 6 is
done (the restore uses it); turn it off at the end of Phase 6.

**VERIFY.** Every container has a current version. `describe-secret` reads
metadata only, so this loop neither pulls every value onto the laptop nor mails
an alert per secret:

```sh
for s in $(tofu -chdir=$TF output -json secret_names | jq -r '.[]'); do
  v=$(aws secretsmanager describe-secret --secret-id "$s" \
    --query 'VersionIdsToStages && contains(values(VersionIdsToStages)[], `AWSCURRENT`)' --output text 2>/dev/null)
  printf '%-55s %s\n' "$s" "$([ "$v" = True ] && echo set || echo MISSING)"
done
```

Expect `set` on every row except `BLACKBOX_BIFROST_CANARY_KEY`. The monitor
box refuses to start while any of `PROM_REMOTE_WRITE_PASSWORD`,
`PROM_WEB_BCRYPT`, `GF_SECURITY_ADMIN_PASSWORD` or `PROM_PASSWORD` is missing
(its `fetch-secrets` fails closed on those), and Grafana itself exits without
`SLACK_ALERTS_WEBHOOK_URL`. The bcrypt
starts `$2y$` (`sget passband/monitor/PROM_WEB_BCRYPT | cut -c1-4`, four
characters, safe to show). The DATABASE_URL works as the app will use it:

```sh
PGPASSWORD=$(sget passband/control/SQUELCH_CONTROL_DATABASE_URL | sed -E 's#^postgres://control:([^@]+)@.*#\1#') \
  psql -X "host=$RDS_HOST hostaddr=127.0.0.1 port=15432 dbname=passband user=control" -c 'select current_user'   # control
```

**ROLLBACK.** A wrong value is fixed by another `put-secret-value`; the
previous version stays as `AWSPREVIOUS`.

---

## Phase 3: repo variables, first images, services on

**Preconditions.** Phase 0d (GitHub OIDC sub + environment) and Phase 2
complete, including `SLACK_ALERTS_WEBHOOK_URL` and `GF_SECURITY_ADMIN_PASSWORD`.
The monitor box pulls `:latest` the moment this phase pushes it. Its
`fetch-secrets` refuses to start the stack while a required monitor secret is
missing, so Grafana can no longer first-boot as `admin/admin`; but Grafana still
exits without the Slack URL, and a box that keeps failing just wastes the phase.

### 3a. GitHub, as set in 0d

```sh
R=passband-app/passband
gh api repos/$R/actions/oidc/customization/sub --jq '.include_claim_keys'           # ["repo","context","job_workflow_ref"]
# Immutable subjects (0d): the repo part comes from GitHub's sub_claim_prefix.
diff <(tofu -chdir=$TF output -raw github_deploy_oidc_sub; echo) \
     <(echo "$(gh api repos/$R/actions/oidc/customization/sub --jq .sub_claim_prefix):environment:aws-production:job_workflow_ref:$R/.github/workflows/deploy-aws.yml@refs/heads/main") && echo SUB-MATCHES
```

If the template is not customized, every deploy job fails at
`configure-aws-credentials` with `Not authorized to perform
sts:AssumeRoleWithWebIdentity`: the default `sub` is not the one the role
trusts. Fix 0d; do not loosen the trust.

### 3b. Repo variables (none are secrets)

Straight from tofu, so the names cannot drift from what the workflow reads:

```sh
tofu -chdir=$TF output -json github_repo_variables \
  | jq -r 'to_entries[] | "\(.key) \(.value)"' \
  | while read -r k v; do gh variable set "$k" -R $R --body "$v"; done
gh variable list -R $R | grep '^AWS_'
# AWS_ARM64_RUNNER: leave unset (ubuntu-24.04-arm) unless the org uses another label.
```

That sets `AWS_ACCOUNT_ID`, `AWS_SITE_BUCKET`, `AWS_SITE_DISTRIBUTION_ID`,
`AWS_SITE_FUNCTION_NAME` (`passband-site-router`) and
`AWS_MONITOR_INSTANCE_ID`. There is no role-ARN variable: the workflow builds
`arn:aws:iam::<AWS_ACCOUNT_ID>:role/passband-github-deploy`.

### 3c. Arm and build

```sh
gh variable set AWS_DEPLOY_ENABLED -R $R --body true
gh workflow run deploy-aws.yml -R $R --ref main -f service=all
gh run watch -R $R "$(gh run list -R $R --workflow deploy-aws.yml --limit 1 --json databaseId -q '.[0].databaseId')"
```

Every job must be green: `ecs (control|relay|bifrost)`, `monitor`,
`site-build` and `site`. `site-build` runs bun with no AWS credentials and hands
`out/` and `function.js` to `site`, which alone assumes the role. `monitor`
reaches the box only through the SSM document `passband-monitor-deploy`
(one parameter, the commit SHA); the box's `bin/up` copies `compose.yaml` out of
the new prometheus image and fails the job unless all three containers answer.

With `desired_count = 0` the ECS deploys go green vacuously (0 running == 0
desired); they prove the images exist and register CI's task definition
revisions. **3d needs those revisions**: tofu ignores the services'
`task_definition`, so flipping them on before this run leaves them on
`:placeholder`, which never becomes healthy, and the apply hangs about 20
minutes before failing.

From here every push to `main` deploys to AWS as well as Railway. That is
harmless while AWS serves nothing public.

### 3d. Services on

```sh
aws ecs describe-services --cluster passband --services control relay bifrost \
  --query 'services[].taskDefinition' --output text                          # every revision > 1
sed -i '' 's/^services_running.*/services_running = true/' $TF/terraform.tfvars
tofu -chdir=$TF apply           # waits for each service to reach steady state, and fails if one does not
```

**VERIFY.**

```sh
aws ecs describe-services --cluster passband --services control relay bifrost \
  --query 'services[].[serviceName,desiredCount,runningCount,taskDefinition]' --output table   # 1 1, revision > 1 (CI's, not :placeholder)
for tg in $(aws elbv2 describe-target-groups --query 'TargetGroups[].TargetGroupArn' --output text); do
  aws elbv2 describe-target-health --target-group-arn "$tg" --query 'TargetHealthDescriptions[].TargetHealth.State' --output text; done   # healthy x5
IP=$(alb_ip)
curl -sS --resolve signup.passband.app:443:$IP https://signup.passband.app/healthz; echo     # ok
curl -sS --resolve relay.squelch.sh:443:$IP    https://relay.squelch.sh/healthz;    echo     # ok
curl -s -o /dev/null -w '%{http_code}\n' --resolve grafana.passband.app:443:$IP https://grafana.passband.app/api/health   # 200
# Source-restricted: your laptop is not on either list, so these are the ALB's 404.
curl -s -o /dev/null -w '%{http_code}\n' --resolve metrics.passband.app:443:$IP https://metrics.passband.app/-/healthy    # 404
curl -s -o /dev/null -w '%{http_code}\n' --resolve bifrost.passband.app:443:$IP https://bifrost.passband.app/            # 404
# The same two hosts from carrier, i.e. the tenants' source address: admitted.
carrier_curl --resolve metrics.passband.app:443:$IP https://metrics.passband.app/-/healthy    # 401 (basic auth), NOT 404
carrier_curl --resolve bifrost.passband.app:443:$IP -X POST -d '{}' https://bifrost.passband.app/anthropic/v1/messages   # Bifrost's own 4xx/2xx, NOT 404
aws logs tail /passband/relay --since 15m | grep serving       # push_token=true
cexec "sh -c 'env | grep -c ^SQUELCH_CONTROL_'"                 # a count, never values
cexec "squelch-control tenants"                                  # empty: RDS is fresh until Phase 6
```

If a task will not start, `aws ecs describe-tasks` on a stopped task shows
`stoppedReason`; `ResourceInitializationError … secret` is a container with no
value.

The ALB's refusal is `404` with body `not found`; Bifrost answers an empty
POST to `/anthropic/v1/messages` with its own error, never 404. A carrier `404`
means `carrier_egress_cidrs` is wrong: fix it in
`terraform.tfvars` and apply now, long before Phase 8 depends on it.

ECS Exec exists on `control` only (`cexec`). There is no exec into `relay` or
`bifrost`; read their state from CloudWatch logs and the ALB.

**ROLLBACK.** `services_running = false` and apply. `gh variable set
AWS_DEPLOY_ENABLED --body false` re-inerts the workflow. Railway is untouched.

---

## Phase 4: Bifrost, fresh and private

**Preconditions.** Phase 3; `bifrost` running 1/1. Do this immediately after
3d: until auth is on, Bifrost answers anyone on its allow-list (the NAT EIP,
carrier) with no credential. The ALB rule keeps everyone else out; the
placeholder key is why the window costs nothing even from inside it.

Open Bifrost to this laptop, for this phase only, and pin the hostname to the
ALB here. Nothing public changes.

```sh
OPIP=$(myip); echo "$OPIP"
sed -i '' "s#^operator_cidrs.*#operator_cidrs = [\"$OPIP/32\"]#" $TF/terraform.tfvars
tofu -chdir=$TF apply           # one more listener rule (priority 100+) carrying your /32
echo "$(alb_ip) bifrost.passband.app" | sudo tee -a /etc/hosts
curl -s -o /dev/null -w '%{http_code}\n' https://bifrost.passband.app/      # NOT 404 now
```

If your address changes mid-phase (VPN, Wi-Fi hop), everything below starts
answering `404 not found`: redo the three lines above.

**Done 2026-10-26 through Bifrost's API instead of the UI** (a browser with a
cached DNS answer lands on Railway's Bifrost, which already has auth, and
asks for a login that is not this one). The API route: `GET /api/config`,
then `PUT /api/config` with `client_config` (`disable_content_logging: true`,
`log_retention_days: 7`) and `auth_config` (`is_enabled`, `admin_username`,
`admin_password`). Give curl its credentials with `-K <(printf ...)`, never
`-K -`: stdin is also where `--data-binary @-` reads the body, and an empty
body comes back as `Invalid JSON`. Virtual keys can be minted the same way
(`POST /api/governance/virtual-keys`, the shape in `squelch-control/src/bifrost.rs`).
`llm sync --check` exits 1 on a gateway with ZERO virtual keys (Bifrost
answers `"virtual_keys": null`); the provider-key half still reports `in sync`.

1. Open `https://bifrost.passband.app`. Enable auth with the admin credential:
   username and password are the two halves of
   `passband/control/SQUELCH_CONTROL_BIFROST_ADMIN_TOKEN` (copy it with
   `sget passband/control/SQUELCH_CONTROL_BIFROST_ADMIN_TOKEN | pbcopy`, split
   at the colon, then clear the clipboard).
2. Turn on **enforce_auth_on_inference** (Settings → Client Settings).
3. **Config → Logs: content logging DISABLED.** Bifrost's default logs every
   request body (our users' mail) for 365 days. `/trust` and `/privacy`
   promise it does not. Metadata logging may stay on. Set retention low.
4. Real Anthropic key. Create a **fresh** key in the Anthropic console (name it
   `passband-bifrost-aws`; not the bridge key on Railway), then:

   ```sh
   put passband/bifrost/ANTHROPIC_API_KEY
   aws ecs update-service --cluster passband --service bifrost --force-new-deployment --query 'service.deployments[0].id'
   aws ecs wait services-stable --cluster passband --services bifrost
   ```

   The provider key Bifrost auto-created on first boot stores a reference to
   the env var, not its value, so the new task resolves the real key.
5. `llm sync`, against THIS Bifrost. The control task cannot do it yet (it
   resolves `bifrost.passband.app` to Railway until Phase 8), so run the same
   image locally with the hostname pinned:

   ```sh
   REG=$(tofu -chdir=$TF output -raw ecr_registry)
   aws ecr get-login-password | docker login --username AWS --password-stdin "$REG"
   IMG=$(aws ecs describe-task-definition --task-definition control --query 'taskDefinition.containerDefinitions[0].image' --output text)
   export SQUELCH_CONTROL_BIFROST_ADMIN_TOKEN="$(sget passband/control/SQUELCH_CONTROL_BIFROST_ADMIN_TOKEN)"
   sync() { docker run --rm --platform linux/arm64 --add-host "bifrost.passband.app:$(alb_ip)" \
     -e SQUELCH_CONTROL_BIFROST_URL=https://bifrost.passband.app -e SQUELCH_CONTROL_BIFROST_ADMIN_TOKEN \
     -e SQUELCH_CONTROL_LLM_MODELS="$(grep -E '^control_llm_models' $TF/terraform.tfvars | cut -d'"' -f2)" \
     -e SQUELCH_CONTROL_ASSISTANT_MODELS="$(grep -E '^control_assistant_models' $TF/terraform.tfvars | cut -d'"' -f2)" \
     --entrypoint squelch-control "$IMG" llm sync "$@"; }
   sync && sync --check; echo "check exit $?"         # 0
   unset SQUELCH_CONTROL_BIFROST_ADMIN_TOKEN
   ```

   `-e NAME` with no `=` passes the value from the environment, so it never
   reaches argv.
6. **Test virtual key** in the UI: name `cutover-test`, budget $1, provider
   Anthropic with its key attached, allowed models typed exactly
   `anthropic/claude-opus-5-5,claude-opus-5-5` (NO spaces: the gateway matches
   exactly and a trailing space renders invisibly). Then:

   ```sh
   IFS= read -rs VK; echo      # paste the test VK
   printf 'header = "x-api-key: %s"\n' "$VK" | curl -sS -K - https://bifrost.passband.app/anthropic/v1/messages \
     -H 'anthropic-version: 2023-06-01' -H 'content-type: application/json' \
     -d '{"model":"anthropic/claude-opus-5-5","max_tokens":16,"messages":[{"role":"user","content":"say ok"}]}' | jq '.type, .content[0].text // .error'
   unset VK
   ```

   Expect `"message"` and text. A `400 no keys found that support model` means
   step 5 did not converge. Then open Logs in the UI: the request is there
   with token counts and **no request or response body**. Delete `cutover-test`.
7. **Canary key**: same shape, name `blackbox-canary`, a few dollars, allowed
   models = the value in `passband/monitor/BLACKBOX_CANARY_MODEL` plus its
   bare spelling, no spaces. Store it and roll the box:

   ```sh
   put passband/monitor/BLACKBOX_BIFROST_CANARY_KEY
   aws ssm send-command --instance-ids "$MON" --document-name AWS-RunShellScript \
     --parameters 'commands=["systemctl reload passband-monitor.service"]' --query Command.CommandId --output text
   ```

   (You, as admin, may run `AWS-RunShellScript`; CI may not. Its only door is
   `passband-monitor-deploy`, which takes a commit SHA and nothing else.)

**VERIFY.**

```sh
curl -s -o /dev/null -w '%{http_code}\n' https://bifrost.passband.app/api/providers                 # 401: governance refuses anonymous
curl -s -o /dev/null -w '%{http_code}\n' -X POST https://bifrost.passband.app/anthropic/v1/messages \
  -H 'content-type: application/json' -d '{}'                                                        # 401/403: inference refuses a request with no VK
```

Plus step 5's `--check` exit 0, step 6's 200 with no body in Logs, and the
content-logging toggle seen OFF in the UI.

Then **close it again** and prove it closed:

```sh
sed -i '' 's#^operator_cidrs.*#operator_cidrs = []#' $TF/terraform.tfvars
tofu -chdir=$TF apply
curl -s -w ' %{http_code}\n' https://bifrost.passband.app/              # not found 404: the ALB, not Bifrost
```

**ROLLBACK.** Nothing public points here. Fix in the UI (reopen with
`operator_cidrs` as above, then close it). Keep the `/etc/hosts` line until
Phase 8; with `operator_cidrs = []` it only ever reaches the ALB's 404.

---

### Operator access to the Bifrost and Grafana UIs (tailnet)

Added 2026-10-26, after the cutover. The monitor box joins the Passband
tailnet as `tag:passband-aws` (key: `passband/monitor/TS_AUTHKEY`, reusable,
tagged) and serves, tailnet-only:

- `https://passband-monitor.<tailnet>.ts.net/`: the Bifrost UI, through a
  loopback nginx that presents `Host: bifrost.passband.app` to the ALB
- `https://passband-monitor.<tailnet>.ts.net:8443/`: Grafana

So `operator_cidrs` stays `[]`; nobody's laptop goes on the ALB allow-list.
A new tailnet needs the tag in `tagOwners` before a key can carry it, and
Serve/HTTPS enabled once (DNS → HTTPS Certificates), or `tailscale serve`
stops at a consent link and leaves no config.

---

## Phase 5: monitoring

**Preconditions.** Phase 3's monitor deploy succeeded; Phase 4 step 7 done.

1. DNS (new names, no conflict with Railway): at Namecheap, CNAME
   `grafana` → and `metrics` → the ALB name
   (`tofu -chdir=$TF output -json service_dns_records`).
2. Grafana: log in at `https://grafana.passband.app` as `admin` with
   `GF_SECURITY_ADMIN_PASSWORD` (`sget … | pbcopy`).
3. Carrier: change `remote_write.url` in `deploy/hosted/80-monitoring.yaml`
   to `https://metrics.passband.app/api/v1/write` (a reviewed PR, merged),
   then replace the password and **restart the agent**. Prometheus never
   re-reads its ConfigMap on its own (no `--web.enable-lifecycle`); an apply
   without a restart leaves the old URL running indefinitely.

   ```sh
   sget passband/monitor/PROM_REMOTE_WRITE_PASSWORD \
     | kubectl -n monitoring create secret generic remote-write-auth --from-file=password=/dev/stdin --dry-run=client -o yaml \
     | kubectl apply -f -
   kubectl apply -f deploy/hosted/80-monitoring.yaml
   kubectl -n monitoring rollout restart deploy/prometheus-agent
   kubectl -n monitoring rollout status deploy/prometheus-agent
   ```

4. Revoke the OLD alerts Slack webhook (Railway Grafana is about to go quiet
   and noisy at once; it should not reach the channel).

**VERIFY.**

metrics.passband.app answers carrier only, so from the laptop Prometheus is
queried through Grafana's datasource proxy (admin login, public host). `promq`
is reused in Phase 8.

```sh
gfq() { printf 'user = "admin:%s"\n' "$(sget passband/monitor/GF_SECURITY_ADMIN_PASSWORD)" | curl -sS -K - "https://grafana.passband.app$1"; }
promq() { gfq "/api/datasources/proxy/uid/prometheus/api/v1/query?query=$(jq -rn --arg q "$1" '$q|@uri')" \
  | jq -c '.data.result[] | [.metric, .value[1]]'; }
gfq /api/datasources/uid/prometheus/health | jq .status     # "OK"
kubectl -n monitoring logs deploy/prometheus-agent --since=10m | grep -iE 'remote|401|404|error' | tail   # nothing new
promq 'count by (tenant) (squelchd_build_info)'            # one row per hosted tenant
promq 'probe_success'                                       # blackbox jobs report
curl -s -w ' %{http_code}\n' https://metrics.passband.app/-/healthy    # not found 404 from here: restricted
```

Each `gfq`/`promq` reads the Grafana password from Secrets Manager, so each
one mails an alert. Batch your queries.

A `404` in the agent's log means carrier's egress is not in
`carrier_egress_cidrs` (3d's `carrier_curl` should have caught it).

Open `passband-health`: per-tenant panels populated. The LLM canary panel is
**expected red until Phase 8** (it probes `bifrost.passband.app`, still
Railway, where the new canary key does not exist). Silence that one alert in
Grafana until Phase 8, and confirm red actually renders as red, not
`NO DATA`.

Metrics history: the Railway Prometheus 30 days are not copied. They stay
readable on Railway Grafana until Phase 10.

**ROLLBACK.** Revert the `80-monitoring.yaml` URL, recreate `remote-write-auth`
from `rw prometheus PROM_REMOTE_WRITE_PASSWORD`, apply, restart the agent.

---

## Phase 6: control (database move, signup DNS)

**Preconditions.** Phases 2 to 5. The RDS tunnel from 2d available (second
terminal). TTL on `signup.passband.app` already lowered (0c).

Signups are offline from step 1 to step 6, roughly 15 minutes. That is the
price of a dump that nothing can write behind.

1. **Freeze writes on both sides.**

   ```sh
   railway down -s control -y          # Railway control stops; its Postgres keeps running
   aws ecs update-service --cluster passband --service control --desired-count 0 --query service.desiredCount
   aws ecs wait services-stable --cluster passband --services control
   ```

   (Tofu still says 1; the next `tofu apply` puts it back, which is step 5.)
2. **Open a path to Railway Postgres.** It has no public URL (private
   networking only; there is no `DATABASE_PUBLIC_URL` today). Railway
   dashboard → `Postgres` → Settings → Networking → **enable TCP Proxy**
   (target port 5432). Then, without putting the URL in argv:

   ```sh
   eval "$(railway variables -s Postgres --json | jq -r '@sh "export RPGHOST=\(.RAILWAY_TCP_PROXY_DOMAIN) RPGPORT=\(.RAILWAY_TCP_PROXY_PORT) RPGUSER=\(.PGUSER) RPGPASSWORD=\(.PGPASSWORD) RPGDATABASE=\(.PGDATABASE)"')"
   rpg() { PGHOST=$RPGHOST PGPORT=$RPGPORT PGUSER=$RPGUSER PGPASSWORD=$RPGPASSWORD PGDATABASE=$RPGDATABASE PGSSLMODE=require PGSSLROOTCERT= "$@"; }
   rpg psql -X -tAc 'show server_version'
   ```

   If the two `RAILWAY_TCP_PROXY_*` keys are absent, read host and port from
   the proxy shown in the dashboard. Use a local `pg_dump` whose major version
   is >= the server's (`postgresql@17` covers 16 and 17).
3. **Dump and count.**

   ```sh
   cat > "$WORK/counts.sql" <<'SQL'
   SELECT string_agg(format('SELECT %L AS t, count(*) AS n FROM public.%I', tablename, tablename), ' UNION ALL ' ORDER BY tablename)
     || ' ORDER BY t' FROM pg_tables WHERE schemaname = 'public'
   \gexec
   SQL
   rpg pg_dump -Fc --no-owner --no-acl -f "$WORK/control.dump"
   rpg psql -X -At -f "$WORK/counts.sql" > "$WORK/counts.railway"
   cat "$WORK/counts.railway"          # table|rows; no secrets
   ```

   Then **turn the TCP proxy off** in the dashboard and `unset RPGPASSWORD`.
4. **Restore into RDS** as `control`, into a clean schema (control's
   `CREATE TABLE IF NOT EXISTS` ran on its Phase 3 boot; those empty tables
   must go):

   ```sh
   # ctl <tool> <args…>: run a libpq tool as the app role over the tunnel.
   ctl() { local tool=$1; shift
     PGHOST=$RDS_HOST PGHOSTADDR=127.0.0.1 PGPORT=15432 PGDATABASE=passband PGUSER=control \
     PGPASSWORD=$(sget passband/control/SQUELCH_CONTROL_DATABASE_URL | sed -E 's#^postgres://control:([^@]+)@.*#\1#') \
     "$tool" "$@"; }
   printf 'DROP SCHEMA public CASCADE;\nCREATE SCHEMA public AUTHORIZATION control;\n' | ctl psql -X -v ON_ERROR_STOP=1
   ctl pg_restore --no-owner --no-acl --exit-on-error -d passband "$WORK/control.dump"
   ctl psql -X -At -f "$WORK/counts.sql" > "$WORK/counts.rds"
   diff "$WORK/counts.railway" "$WORK/counts.rds" && echo IDENTICAL
   ```

   If the source was PG17 and the restore stops on `transaction_timeout`, rerun
   it without `--exit-on-error` and read every error. The row-count diff is
   the gate either way: do not continue unless it prints `IDENTICAL`.
5. **Start control on AWS.**

   ```sh
   tofu -chdir=$TF apply           # desired_count back to 1
   aws ecs wait services-stable --cluster passband --services control
   cexec "squelch-control tenants"           # the hosted tenants, matching Railway
   cexec "squelch-control invite list"
   cexec "squelch-control drift"             # proves the warden pair: exit 0 or a drift report, never an auth error
   ```

6. **DNS.** At Namecheap, `signup` CNAME → the ALB name. Then remove the
   custom domain from Railway `control` so a stale resolver gets an error,
   not a stopped service's edge page.
7. Revoke the OLD `#passband-signups` Slack webhook.
8. Close the DB path:

   ```sh
   sed -i '' 's/^operator_db_access.*/operator_db_access = false/' $TF/terraform.tfvars
   tofu -chdir=$TF apply           # and close the tunnel terminal
   ```

**VERIFY.**

```sh
dig +short signup.passband.app                                     # the ALB name
curl -sI https://signup.passband.app/healthz | grep -iE '^HTTP|railway'   # HTTP/2 200 and NO railway header
curl -s -o /dev/null -w '%{http_code}\n' https://signup.passband.app/agent/auth   # anything but 404 (WorkOS door mounted)
aws logs tail /passband/control --since 10m | grep -iE 'error|refus' | tail
```

Then a real end-to-end: issue one invite (`cexec "squelch-control invite issue
--count 1"`) and open `/admin` with the admin token; a waitlist post from
`passband.app` lands (control logs a 2xx on the waitlist route).

A signup on AWS before Phase 8 cannot mint its LLM key (control resolves
`bifrost.passband.app` to Railway's gateway, which refuses the new admin
credential). Signup is fail-soft about that by design; Phase 8's mint loop
picks the tenant up.

**ROLLBACK** (until the first AWS-only write you care about): Railway
dashboard → `control` → Deployments → latest → Redeploy; re-add its custom
domain; `signup` CNAME back to the Railway target. Rows written on AWS after
step 5 do not exist on Railway; copy them by hand. Keep Railway Postgres
running until Phase 10 for exactly this.

---

## Phase 7: relay

**Preconditions.** Relay 1/1 on AWS, `serving … push_token=true` in
`/passband/relay` logs. The new APNs key id is in the running task
(`aws ecs describe-task-definition --task-definition relay --query 'taskDefinition.containerDefinitions[0].environment'`).
The OLD APNs key is still valid (do not revoke yet).

**Why not "drain the old relay".** `GET /v1/opens?cursor=N` deletes every row
at or below N, and those rows belong to self-hosted daemons. An operator drain
destroys them. `cursor=0` is the non-destructive peek; the daemons drain
themselves every 60 seconds.

**The cursor trap.** A self-hosted daemon persists the highest relay row id it
has collected (`sync_state`, mailbox `opens_poll_cursor`). The AWS relay starts
its ids at 1. A daemon that polls it with its old cursor N acks (deletes) every
new row up to N and receives none, silently, until the new relay's ids pass N.
Every self-hosted daemon gets its cursor reset in step 5.

1. **Peek** at the old queue:

   ```sh
   # Railway's own domain, so the peek reaches the OLD relay even after the DNS flip.
   OLDRELAY=https://$(rw squelch RAILWAY_PUBLIC_DOMAIN)
   peek() { rw squelch SQUELCH_RELAY_AUTH_TOKEN | { IFS= read -r T; printf 'header = "Authorization: Bearer %s"\n' "$T" \
     | curl -sS -K - "$OLDRELAY/v1/opens?cursor=0" | jq '.opens | length'; unset T; }; }
   peek            # rows waiting for self-hosted daemons; wait for 0 if it is not
   ```

2. **The tenants' Secret, under a new name** (never `kubectl rollout restart`
   the tenants):

   ```sh
   sget passband/relay/SQUELCH_RELAY_PUSH_TOKEN | kubectl -n tenants create secret generic apns-relay-2 --from-file=token=/dev/stdin
   kubectl -n tenants get secret apns-relay-2 -o jsonpath='{.data.token}' | base64 -d | wc -c     # 64
   ```

3. **DNS**: `relay.squelch.sh` CNAME → the ALB name.
4. **Warden** (same sitting as 3): set
   `SQUELCH_WARDEN_RELAY_SECRET_NAME: "apns-relay-2"` in
   `deploy/hosted/15-warden-config.yaml` (reviewed PR, merged), then apply and
   restart the warden in the same minute:

   ```sh
   kubectl apply -f deploy/hosted/15-warden-config.yaml
   kubectl -n warden rollout restart deploy/squelch-warden
   kubectl -n warden get configmap squelch-warden-config -o jsonpath='{.data.SQUELCH_WARDEN_RELAY_SECRET_NAME}'; echo   # apns-relay-2
   ```

   Between 3 and a tenant's roll, that tenant pushes the old token to the new
   relay and gets 401. Its cursor holds and it bursts the backlog after the
   roll. Mail is unaffected. Run Phase 8 straight after this: each mint rolls
   the tenant onto today's render, which now includes `apns-relay-2`.
5. **Self-hosted daemons** (the ones that set `SQUELCH_RELAY_TOKEN`; the old
   relay's logs show who polls `/v1/opens`: `railway logs -s squelch | grep opens`).
   On each: stop the daemon, set `SQUELCH_RELAY_TOKEN` to the new
   `passband/relay/SQUELCH_RELAY_AUTH_TOKEN` (pipe `sget` into wherever that
   daemon reads its env: launchd plist, env file), reset the cursor, start:

   ```sh
   sqlite3 <daemon store path> "DELETE FROM sync_state WHERE mailbox = 'opens_poll_cursor';"
   ```

6. After the old TTL plus an hour: `peek` again (expect 0), then
   `railway down -s squelch -y`.
7. After a push is seen delivered through the new relay (VERIFY): **revoke the
   old APNs key** in the Apple portal. Delete the old `apns-relay` Secret only
   once every tenant is on `apns-relay-2`.

**VERIFY.**

```sh
curl -sI https://relay.squelch.sh/healthz | grep -iE '^HTTP|railway'     # 200, no railway header
aws logs tail /passband/relay --since 30m | grep -E 'push' | tail        # push lines, topic=app.passband.client.ios, delivered
for t in braelyn braelyn-work ellie ellie-atuin ethan-2 phiilpp we-z; do
  printf '%s: ' $t; kubectl -n tenants get deploy $t -o json | jq -r '[.spec.template.spec.containers[].env[]? | select(.name=="SQUELCH_RELAY_TOKEN") | .valueFrom.secretKeyRef.name] | join(",")'; done   # apns-relay-2 once rolled
kubectl -n tenants logs deploy/braelyn -c squelchd --since=30m | grep -iE 'apns|relay' | tail      # no 401 after its roll
```

And on a phone: trigger a notify-eligible mail on your own tenant and see the
push arrive.

**ROLLBACK** (only while the old APNs key is unrevoked): `relay.squelch.sh`
back to Railway, `railway redeploy -s squelch` (or Redeploy in the
dashboard), warden secret name back to `apns-relay`, self-hosted daemons back
to the old token. The old tokens are in the breach; this is a stopgap, not a
state.

---

## Phase 8: Bifrost DNS and a fresh key for every tenant

**Preconditions.** Phase 4 verified; Phase 6 done (the store is on RDS and
control reaches the warden); Phase 7 step 4 applied.

The warden needs **no LLM change**: `SQUELCH_WARDEN_LLM_BASE_URL` is already
`https://bifrost.passband.app/anthropic` and the hostname does not change.
Confirm on the live object, not the file:

```sh
kubectl -n warden get configmap squelch-warden-config -o jsonpath='{.data.SQUELCH_WARDEN_LLM_BASE_URL}'; echo
```

The control → warden bearer was rotated before this cutover and copied in 2a;
Phase 6's `drift` already proved it.

**Order: DNS first, then mint.** A mint creates the key on whichever gateway
control resolves. Against Railway's it fails loudly (the new admin credential
is refused there), never silently, but it fails; so wait until control
resolves the ALB. **There IS a gap, so mint everyone straight away:** tenants
reach Bifrost by hostname, so the moment the CNAME flips, a tenant still on
its Railway key calls the AWS gateway, which does not know that key, and its
LLM calls fail (`llm_config_failures`) until its own mint. On 2026-10-26 the
seven mints took about two minutes and the failures stopped with them.

0. **Last check that tenants will be admitted.** After the flip, every
   tenant's triage reaches the ALB from carrier's egress address, and a wrong
   `carrier_egress_cidrs` is a fleet-wide `404`:

   ```sh
   carrier_curl --resolve bifrost.passband.app:443:$(alb_ip) -X POST -d '{}' \
     https://bifrost.passband.app/anthropic/v1/messages        # 401/403 (no VK), NOT 404
   ```

   Do not continue on a `404`.
1. DNS: `bifrost` CNAME → the ALB name. Remove the `/etc/hosts` line from
   Phase 4. From now on the laptop gets `404 not found` from Bifrost; that is
   the allow-list, not an outage.
2. Wait for the VPC to see it:

   ```sh
   dig +short "$(tofu -chdir=$TF output -raw alb_dns_name)" | sort
   cexec "getent ahostsv4 bifrost.passband.app"      # the same addresses
   cexec "squelch-control llm sync --check"          # exit 0, now through the real path
   ```

3. Mint, one tenant at a time. `llm mint` mints two virtual keys (triage and
   assistant), installs them through the warden, and the warden re-applies the
   Deployment with a changed key hash, which **rolls that tenant's pod**:

   ```sh
   cexec "squelch-control tenants"     # reconcile this against the loop's list before running it
   for t in braelyn braelyn-work ellie ellie-atuin ethan-2 phiilpp we-z; do
     echo "== $t"; cexec "squelch-control llm mint $t" || break
     kubectl -n tenants rollout status deploy/$t --timeout=10m || break
   done
   ```

   A cancelled account is refused; that is correct. Mint prints the OLD key
   ids "to revoke": ignore them, the old gateway is being shut down whole.
4. Lift the canary silence from Phase 5.
5. `railway down -s bifrost -y` once every tenant is minted.

**VERIFY.**

```sh
cexec "squelch-control llm sync --check"; echo $?                 # 0: provider key and every VK agree, no stray whitespace
cexec "squelch-control drift"                                       # clean, or only drift you can explain
for t in braelyn braelyn-work ellie ellie-atuin ethan-2 phiilpp we-z; do
  printf '%s: ' $t; kubectl -n tenants logs deploy/$t -c squelchd --since=15m | grep -ciE 'llm.*(401|403|400)'; done   # 0 each
promq 'last_over_time(probe_success{job="blackbox-bifrost-llm"}[20m])'   # 1
```

The canary going to `1` is also the proof that the monitor box (egress via
the NAT EIP) is admitted.

In the Bifrost UI: two keys per tenant plus `blackbox-canary`, and spend
appearing on tenant keys within the hour. The UI needs your /32 in
`operator_cidrs` again (Phase 4's three lines, minus `/etc/hosts`); set it back
to `[]` and apply when done. Dashboard "LLM config failures"
flat.

**ROLLBACK.** There is none past step 3 for a minted tenant: its new key
exists only on the AWS gateway. Fix forward (`llm mint <label>` again,
`llm sync`). Before step 3, point the `bifrost` CNAME back at Railway. A
fleet-wide `404` at the gateway right after step 1 is the allow-list: fix
`carrier_egress_cidrs` and apply (about a minute), or flip the CNAME back
while you do.

---

## Phase 9: landing site

**Preconditions.** Phase 3's site job succeeded (`aws s3 ls s3://$(tofu -chdir=$TF output -raw site_bucket)/`
lists `index.html`, `appcast.xml`).

1. Compare every route, Railway vs CloudFront, before touching DNS:

   ```sh
   OLD=https://$(rw landing-site RAILWAY_PUBLIC_DOMAIN)
   NEW=https://$(tofu -chdir=$TF output -json service_dns_records | jq -r '.["passband.app"].value')
   V=$(curl -s $OLD/appcast.xml | grep -o 'sparkle:shortVersionString="[^"]*"' | head -1 | cut -d'"' -f2)
   for p in / /about /privacy /security /self-host /terms /trust /appcast.xml /site.css /main.js /og.png \
            /icon.svg /icon-dark.svg /mark.svg /mark.png /mark-light.png /icon-180.png /icon-512.png \
            /newsreader-var.woff2 /download /download/latest /download/Passband-$V.zip /no-such-page; do
     for o in $OLD $NEW; do
       curl -s -o /dev/null -D - "$o$p" | awk -v p="$p" -v o="${o:8:12}" '{sub(/\r$/,""); h=tolower($1)}
         h ~ /^http\//{s=$2} h=="content-type:"{c=$2} h=="location:"{l=$2} h=="cache-control:"{k=$2}
         END{printf "%-28s %-14s %s %-24s %s %s\n",p,o,s,c,l,k}'
     done
   done
   ```

   Every pair must match on status, content type and `Location`. `appcast.xml`
   is `application/xml` and short-cached; the `.woff2` carries
   `max-age=604800`; `/download` 302s to the zip for `$V`.
2. DNS at Namecheap: `www` CNAME → the CloudFront name; apex `@` → **ALIAS
   record** to the CloudFront name (an apex cannot hold a CNAME). Delete the
   old apex record pointing at Railway in the same edit.
3. Remove the custom domains from Railway `landing-site`.

**VERIFY.**

```sh
curl -sI https://passband.app/ | grep -iE '^HTTP|^server|railway|x-cache'        # 200, server: CloudFront, no railway
curl -sI https://www.passband.app/ | grep -iE '^HTTP|server'
curl -sI https://passband.app/appcast.xml | grep -iE 'content-type|cache-control'  # application/xml, short max-age
curl -s https://passband.app/appcast.xml | grep -c '<item>'                         # > 0
curl -sI https://passband.app/download | grep -i '^location'                         # …/Passband-$V.zip on passband-releases
curl -sIL https://passband.app/download | grep -E '^HTTP' | tail -1                  # 200 at the end of the redirect chain
```

Then the Mac app: Check for Updates from a build one version behind sees the
current version (Sparkle reads the appcast through the new path).

**ROLLBACK.** Apex and `www` back to Railway's targets (re-add the custom
domains there first).

---

## Phase 10: teardown

**Preconditions.** Phases 5 to 9 verified, and a soak: at least one real
signup on AWS end to end, LLM canary green for 24h, one push delivered. An RDS
snapshot taken by hand:

```sh
aws rds create-db-snapshot --db-instance-identifier passband --db-snapshot-identifier passband-post-cutover
aws rds wait db-snapshot-available --db-snapshot-identifier passband-post-cutover
```

1. **Revoke the bridge Anthropic key** (the one on Railway `bifrost`) in the
   Anthropic console. Confirm the AWS key is the only active one used:
   Anthropic console usage shows traffic on `passband-bifrost-aws` only.
2. **Railway**: delete every service (`control`, `squelch`, `bifrost`,
   `Postgres`, `prometheus`, `grafana`, `blackbox`, `landing-site`), then the
   project (dashboard → Settings → Danger). This destroys the volumes,
   including the old Postgres and the old Bifrost store. Take the decision on
   metrics history first: the old 30 days die here.
3. **Railway tokens**: delete every account and project token
   (railway.com/account/tokens). Log the CLI out on every machine
   (`railway logout`).
4. **GitHub**: uninstall the Railway GitHub App from the `passband-app` org
   (Org settings → GitHub Apps). Then clean what it left:

   ```sh
   gh api repos/$R/hooks --jq '.[] | [.id, .config.url] | @tsv'            # delete any railway.app hook: gh api -X DELETE repos/$R/hooks/<id>
   gh api repos/$R/environments --jq '.environments[].name'                 # delete Railway-created ones; keep aws-production
   ```

   Service-level deployment triggers die with the services in step 2.
5. Delete `$WORK` (`rm -rf "$WORK"`; it holds the control dump).
6. Old Kubernetes Secret: `kubectl -n tenants delete secret apns-relay` once
   every tenant shows `apns-relay-2` (Phase 7 VERIFY loop).
7. **Close the warden API to everyone but control.** Control now calls the
   warden only from the NAT EIP, so the allowlist in
   `deploy/hosted/20-warden.yaml` can go on. On carrier, in this order:

   ```sh
   tofu -chdir=$TF output -raw nat_egress_ip        # -> the /32 for 20-warden.yaml's sourceRange
   kubectl apply -f deploy/hosted/05-traefik-metrics.yaml   # externalTrafficPolicy: Local; Traefik restarts once
   kubectl -n kube-system get svc traefik -o jsonpath='{.spec.externalTrafficPolicy}'   # Local
   kubectl apply -f deploy/hosted/20-warden.yaml            # with the EIP filled in
   ```

   **VERIFY.** From your laptop, `curl -s -o /dev/null -w '%{http_code}\n'
   https://warden.passband.app/v1/tenants` says `403` (Traefik's allowlist,
   before the bearer is even read), and `/healthz` says `200`. Then a real
   signup end to end, which is control calling the warden: if it fails at
   provisioning, the EIP in `sourceRange` is wrong or Traefik is still
   seeing cluster addresses (check the policy line above).

**VERIFY.** `railway list` shows no project; `dig` for every product host
resolves to AWS (ALB or CloudFront); no `railway` header on any of them. `grep -E '^(operator_cidrs|operator_db_access)' $TF/terraform.tfvars` shows
`[]` and `false`, and `tofu -chdir=$TF plan` says "No changes".

### Docs and code owed (list only; each is its own PR)

**Done 2026-10-30** (the `update-docs` branch): every item below, including
the `railway*.toml` and Railway-only Dockerfiles. The NoTls branch stays, for
local dev and tests.

- Root `CLAUDE.md`: the deploy table (every row says Railway; `bifrost` is no
  longer a `railway up` exception) and the Grafana `railway ssh` verification.
- `deploy/hosted/PRODUCTION.md`: "## Railway", "### The Postgres cutover"
  (delete), "### The LLM gateway" (service, volume, health-check rows),
  "### The APNs relay" (`railway logs -s squelch` in the verify line), "## DNS"
  (`signup.passband.app` row), "## Monitoring" (Railway services and URLs).
- `deploy/monitoring/README.md`: the service table and every `railway`
  command; the AWS box is `deploy/aws/monitor/`.
- `deploy/hosted/SETUP.md`: the `signup.passband.app` CNAME line (§DNS table,
  line ~76 and ~94), §6b "on Railway" steps, §6c relay location.
- `deploy/hosted/80-monitoring.yaml` and `remote-write-auth.example.yaml`
  comments that name the Railway receiver.
- `deploy/hosted/apns-relay.example.yaml`: the `railway variable set` /
  `railway variable list` creation steps (the push token now lives in Secrets
  Manager).
- `squelch-control/README.md` "## Deploying to Railway" and the
  `${{Postgres.DATABASE_URL}}` text in the env table; `squelch-relay/README.md`
  deployment sketch; `deploy/DEPLOY.md` §8; `docs/HOSTED.md`.
- `docs/VERIFICATION.md` and `passband-site/privacy.html` name
  `bifrost.passband.app`; neither may tell a reader to probe it (it now answers
  only our own egress and carrier).
- Code, after the soak: `railway*.toml`, `passband-site/Dockerfile` (Railway's
  build), the `release-daemon.yml` comment about Railway, and the NoTls branch
  in the control store if nothing dials plaintext any more.

---

## What can go wrong

| Symptom | Cause | Fix |
|---|---|---|
| ACM cert stuck `PENDING_VALIDATION` | Namecheap Host field given the full name (zone appended twice), or a CAA record without `amazon.com` | Enter the name minus the zone; add CAA `0 issue "amazon.com"` |
| ECS task stops with `ResourceInitializationError` | A secret container with no value | `put` it; the service retries |
| Monitor unit never starts; journal says `REQUIRED passband/monitor/<X> has no value` | `fetch-secrets` fails closed on the Prometheus passwords and the Grafana admin password | `put` it; the unit retries every minute |
| Grafana login `admin/admin` works | Should be impossible (fail-closed above); means `grafana.db` was created some other way | `grafana cli admin reset-admin-password` over an SSM session with the stored value piped in, or wipe `/data/grafana` and reload |
| Grafana container exits at boot; monitor deploy fails "not healthy after 3 minutes" | `SLACK_ALERTS_WEBHOOK_URL` unset (fetch-secrets treats it as optional, Grafana 12.2 does not) | Set it, `systemctl reload passband-monitor` |
| Monitor deploy job fails `AccessDenied` on `ssm:SendCommand` | Something sent `AWS-RunShellScript` as the deploy role | CI's only document is `passband-monitor-deploy` (`--parameters ImageTag=<40-hex sha>`) |
| `404 not found` from `bifrost.` or `metrics.passband.app` | Your address is not on the allow-list (correct), or carrier's egress changed | Laptop: `operator_cidrs` for Bifrost, `promq` for metrics. Carrier: fix `carrier_egress_cidrs`, apply |
| Every deploy job fails `Not authorized to perform sts:AssumeRoleWithWebIdentity` | Repo OIDC sub template not customized, or the run is not deploy-aws.yml on main in `aws-production` | Phase 0d; never widen the trust |
| A flood of security-alert mail | Expected during the cutover: every secret read/write, exec, SSM session, IAM apply | Match each to a command you ran; an unmatched one is an incident |
| `tofu apply` with `services_running = true` hangs ~20 min then fails | A service still on `:placeholder` (CI never deployed it) or a task that cannot pass its health check | Run the workflow for that service, then apply again; `stoppedReason` on the stopped task |
| Prometheus rejects every login | `PROM_WEB_BCRYPT` mangled (`$` interpolated) or not the hash of the same password | Regenerate both from one `PW` as in 2b; `fetch-secrets` single-quotes to prevent the first |
| Carrier still pushing to Railway after the apply | prometheus-agent never re-reads its ConfigMap | `kubectl -n monitoring rollout restart deploy/prometheus-agent` |
| Every tenant `400 no keys found that support model` | Provider key `models` not converged (the August 2026 outage) | `squelch-control llm sync`, then `--check` |
| Canary 403 while the key "looks right" | Allow-list typed with spaces; gateway matches exactly | Retype without spaces; `llm sync --check` flags whitespace |
| Canary shows `NO DATA` | Probe not scraping, or an instant query over a 15m job | Fix the probe; the panel must render red when red |
| Every tenant 401 at the gateway after a "read-modify-write" of the provider key | A read returns the key masked; writing it back stores asterisks as the Anthropic key | Only `llm sync` writes that key; it sends the env reference |
| `llm mint` fails with an auth error | Control still resolves `bifrost.passband.app` to Railway | Wait for TTL; `cexec "getent ahostsv4 bifrost.passband.app"` |
| Self-hosted daemon gets no opens after the relay move | Its persisted `opens_poll_cursor` is above the new relay's ids, so every poll deletes the new rows | Stop daemon, delete the cursor row, start (Phase 7 step 5) |
| Opens lost on the old relay | Someone "drained" it with the full token | Peek with `cursor=0` only; let daemons drain |
| Burst of stale phone notifications | A tenant 401'd at the relay between DNS and its roll, cursor held | Expected, once; shrink the window by running Phase 8 straight after Phase 7 |
| Whole fleet skipped by the roller | Someone ran `kubectl rollout restart` on tenants (foreign field manager) | `squelch-control reconcile <label>` per tenant; never restart tenants by hand |
| Railway deploy shows SKIPPED after a `railway up` | `railway up` from the repo root is skipped on several services | Never `railway up` during this cutover; Railway deploys from `main` |
| Control refuses to boot naming an `sslmode` | Anything other than absent/`disable`/`require`/`verify-full` | Use `verify-full` with the RDS hostname, never an IP |
| `pg_restore` stops on `transaction_timeout` | PG17 dump into PG16 | Rerun without `--exit-on-error`, read each error; the row-count diff is the gate |
| A branch run assumes the deploy role | Should be impossible: the trust pins `job_workflow_ref` to deploy-aws.yml `@refs/heads/main` | Check `tofu output github_deploy_oidc_sub` and the role's trust policy; treat as an incident |
| Site deploy never picks up a new release | Release pushes the appcast with `GITHUB_TOKEN`, which starts no workflow | The `workflow_run` trigger covers it; a manual `-f service=site` dispatch otherwise |
| `squelch-control` env change in tofu "did nothing" | Task definitions are CI-owned after the first apply | `gh workflow run deploy-aws.yml -f service=control` after the apply |
