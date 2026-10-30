# Every value here is NON-secret. Secret values never pass through tofu: see
# secrets.tf and README.md.

# --- State (from the bootstrap root's outputs) -------------------------------

variable "state_bucket" {
  description = "State bucket, `tofu -chdir=../bootstrap output -raw state_bucket`."
  type        = string
}

variable "state_kms_key_arn" {
  description = "State KMS key, `tofu -chdir=../bootstrap output -raw state_kms_key_arn`."
  type        = string
}

# --- Account-wide -------------------------------------------------------------

variable "region" {
  type    = string
  default = "us-west-2"
}

variable "budget_email" {
  description = "Operator address for the $150/month AWS Budgets alarm AND the security alerts topic (audit.tf). Confirm the SNS subscription email after the first apply."
  type        = string
}

variable "enable_guardduty" {
  description = <<-EOT
    Turn on GuardDuty (account detector, us-west-2). Off by default for cost:
    roughly $5-15/month at this size, mostly CloudTrail management-event and
    VPC flow/DNS analysis. Worth it the day anything here holds real users: it
    catches instance credentials used from outside AWS and odd API callers,
    which the EventBridge rules in audit.tf do not.
  EOT
  type        = bool
  default     = false
}

variable "budget_limit_usd" {
  type    = number
  default = 150
}

variable "github_repo" {
  description = "owner/name the deploy role trusts."
  type        = string
  default     = "passband-app/passband"
}

variable "github_sub_prefix" {
  description = <<-EOT
    The leading `repo:` part of the OIDC sub, exactly as GitHub reports it in
    `gh api repos/<owner>/<repo>/actions/oidc/customization/sub` (field
    sub_claim_prefix). This repo uses IMMUTABLE subjects, so the prefix carries
    the owner and repo IDs, not just names; a name-based prefix is refused at
    AssumeRoleWithWebIdentity. job_workflow_ref stays name-based.
  EOT
  type        = string
  default     = "repo:passband-app@336248739/passband@1293146964"
  validation {
    condition     = startswith(var.github_sub_prefix, "repo:")
    error_message = "Must start with repo: (copy sub_claim_prefix verbatim)."
  }
}

variable "github_environment" {
  description = "GitHub environment the workflow_dispatch jobs run in."
  type        = string
  default     = "aws-production"
}

variable "github_workflow_path" {
  description = "The workflow file the deploy role trusts (job_workflow_ref, pinned to refs/heads/main)."
  type        = string
  default     = ".github/workflows/deploy-aws.yml"
}

variable "create_github_oidc_provider" {
  description = "false if the account already has the token.actions.githubusercontent.com provider."
  type        = bool
  default     = true
}

# --- Network ------------------------------------------------------------------

variable "vpc_cidr" {
  type    = string
  default = "10.40.0.0/16"
}

variable "fck_nat_ami_name" {
  description = <<-EOT
    EXACT name of the fck-nat AMI (owner 568608671756) the NAT runs, e.g.
    fck-nat-al2023-hvm-1.4.0-20260201-arm64-ebs. No wildcard: a new upstream
    AMI must never roll the NAT on its own. Find the current one with
      aws ec2 describe-images --region us-west-2 --owners 568608671756 \
        --filters 'Name=name,Values=fck-nat-al2023-hvm-*-arm64-ebs' \
        --query 'sort_by(Images,&CreationDate)[-3:].[Name,ImageId,CreationDate]' --output table
    and bump deliberately (README.md, "Bumping the NAT AMI").
  EOT
  type        = string

  validation {
    condition     = can(regex("^fck-nat-al2023-hvm-[0-9]+\\.[0-9]+\\.[0-9]+-[0-9]{8}-arm64-ebs$", var.fck_nat_ami_name))
    error_message = "fck_nat_ami_name must be one exact arm64 AL2023 fck-nat image name (fck-nat-al2023-hvm-<x.y.z>-<YYYYMMDD>-arm64-ebs), no wildcards."
  }
}

# --- Who may reach the source-restricted hosts (alb.tf) -----------------------

variable "carrier_egress_cidrs" {
  description = <<-EOT
    carrier's (the Hetzner k3s tenant box) public egress address(es), as /32s.
    The only sources besides the NAT EIP that may reach Bifrost, and the only
    ones that may reach metrics (remote-write). Currently believed to be
    5.78.205.240/32; CONFIRM before applying by running `curl -s ifconfig.me`
    ON carrier (from a pod, if pod egress is SNATed differently). A wrong value
    here cuts every tenant off from LLM triage.
  EOT
  type        = list(string)

  validation {
    condition     = length(var.carrier_egress_cidrs) >= 1 && length(var.carrier_egress_cidrs) <= 5
    error_message = "carrier_egress_cidrs needs 1 to 5 CIDRs (each becomes an ALB source-ip value; rules hold 3, see alb.tf)."
  }

  validation {
    condition     = alltrue([for c in var.carrier_egress_cidrs : can(cidrhost(c, 0)) && !startswith(c, "0.0.0.0/")])
    error_message = "carrier_egress_cidrs entries must be CIDRs (e.g. 5.78.205.240/32), never 0.0.0.0/0."
  }
}

variable "operator_cidrs" {
  description = <<-EOT
    Extra sources allowed to reach Bifrost (UI included). Empty in steady
    state. Set it TEMPORARILY to the operator's own /32 for Bifrost first-run
    config (CUTOVER.md Phase 4), apply, configure, then set it back to [] and
    apply again.
  EOT
  type        = list(string)
  default     = []

  validation {
    condition     = length(var.operator_cidrs) <= 3
    error_message = "operator_cidrs holds at most 3 CIDRs."
  }

  validation {
    condition     = alltrue([for c in var.operator_cidrs : can(cidrhost(c, 0)) && !startswith(c, "0.0.0.0/")])
    error_message = "operator_cidrs entries must be CIDRs (your /32), never 0.0.0.0/0."
  }
}

# --- Hostnames (DNS stays at the current provider; tofu only outputs records) -

variable "control_host" {
  type    = string
  default = "signup.passband.app"
}

variable "relay_host" {
  type    = string
  default = "relay.squelch.sh"
}

variable "bifrost_host" {
  type    = string
  default = "bifrost.passband.app"
}

variable "grafana_host" {
  type    = string
  default = "grafana.passband.app"
}

variable "metrics_host" {
  type    = string
  default = "metrics.passband.app"
}

variable "site_hosts" {
  description = "CloudFront aliases. The first is the apex."
  type        = list(string)
  default     = ["passband.app", "www.passband.app"]
}

# --- Rollout switch -----------------------------------------------------------

variable "services_running" {
  description = <<-EOT
    false keeps every ECS service at desired_count 0. A task cannot start until
    each of its secrets has a value and CI has pushed a real image, so the first
    apply leaves them at 0; flip to true after both (README.md).
  EOT
  type        = bool
  default     = false
}

# --- control: plain env -------------------------------------------------------

variable "control_public_url" {
  description = "SQUELCH_CONTROL_PUBLIC_URL"
  type        = string
  default     = "https://signup.passband.app"
}

variable "control_base_domain" {
  description = "SQUELCH_CONTROL_BASE_DOMAIN"
  type        = string
}

variable "control_client_id" {
  description = "SQUELCH_CONTROL_CLIENT_ID (the web OAuth client id; an id, not a secret)."
  type        = string
}

variable "control_microsoft_client_id" {
  description = "SQUELCH_CONTROL_MICROSOFT_CLIENT_ID: the Passband Hosted Entra app's Application (client) id, an id, not a secret. Null = Gmail-only signup. Setting it also creates the passband/control/SQUELCH_CONTROL_MICROSOFT_CLIENT_SECRET container, which needs a value before control's next deploy (SETUP.md §6a)."
  type        = string
  default     = null
}

variable "control_warden_url" {
  description = "SQUELCH_CONTROL_WARDEN_URL"
  type        = string
}

variable "control_bifrost_url" {
  description = "SQUELCH_CONTROL_BIFROST_URL"
  type        = string
  default     = "https://bifrost.passband.app"
}

variable "control_trusted_proxy_hops" {
  description = <<-EOT
    SQUELCH_CONTROL_TRUSTED_PROXY_HOPS. The ALB appends the TCP peer it saw to
    X-Forwarded-For, and control reads the hops-th entry from the right
    (ratelimit.rs client_ip), so 1 is the real client.
  EOT
  type        = number
  default     = 1
}

variable "control_invite_from" {
  description = "SQUELCH_CONTROL_INVITE_FROM, e.g. \"Passband <invites@passband.app>\"."
  type        = string
}

variable "control_waitlist_origin" {
  description = "SQUELCH_CONTROL_WAITLIST_ORIGIN; null leaves control's default (https://passband.app)."
  type        = string
  default     = null
}

variable "control_llm_budget_usd" {
  description = "SQUELCH_CONTROL_LLM_BUDGET_USD; null leaves control's default."
  type        = string
  default     = null
}

variable "control_llm_models" {
  description = "SQUELCH_CONTROL_LLM_MODELS; null leaves control's default."
  type        = string
  default     = null
}

variable "control_assistant_budget_usd" {
  description = "SQUELCH_CONTROL_ASSISTANT_BUDGET_USD; null leaves control's default."
  type        = string
  default     = null
}

variable "control_assistant_models" {
  description = "SQUELCH_CONTROL_ASSISTANT_MODELS; null leaves control's default."
  type        = string
  default     = null
}

variable "control_log" {
  description = "SQUELCH_CONTROL_LOG; null leaves the default (info)."
  type        = string
  default     = null
}

# --- relay: plain env ---------------------------------------------------------

variable "relay_apns_key_id" {
  description = "SQUELCH_RELAY_APNS_KEY_ID (JWT kid; public)."
  type        = string
}

variable "relay_apns_team_id" {
  description = "SQUELCH_RELAY_APNS_TEAM_ID (JWT iss; public)."
  type        = string
}

variable "relay_apns_topics" {
  description = "SQUELCH_RELAY_APNS_TOPICS; the first is the default."
  type        = string
  default     = "app.passband.client,app.passband.client.ios"
}

variable "relay_apns_env" {
  description = "SQUELCH_RELAY_APNS_ENV; null leaves the default (production)."
  type        = string
  default     = null
}

variable "relay_trusted_proxy_hops" {
  description = "SQUELCH_RELAY_TRUSTED_PROXY_HOPS. 1 = the ALB; its private addresses fall inside the relay's default RFC1918 trusted CIDRs."
  type        = number
  default     = 1
}

variable "relay_log" {
  description = "SQUELCH_RELAY_LOG; null leaves the default (info)."
  type        = string
  default     = null
}

# --- bifrost ------------------------------------------------------------------

variable "bifrost_store" {
  description = <<-EOT
    Where Bifrost keeps its config and log stores. "efs": SQLite under
    /app/data on EFS, single writer. "postgres": the shared RDS instance
    (database `bifrost`, own role, created by the operator); no EFS mount.
  EOT
  type        = string
  default     = "efs"

  validation {
    condition     = contains(["efs", "postgres"], var.bifrost_store)
    error_message = "bifrost_store must be \"efs\" or \"postgres\"."
  }
}

# --- EFS identities -----------------------------------------------------------

variable "relay_uid" {
  description = <<-EOT
    POSIX uid/gid the relay's EFS access point enforces. The image creates
    `relay` with `useradd --system` on debian:bookworm-slim, which allocates the
    first free system id from 999 down, so relay is 999:999. Verify with
    `docker run --rm --entrypoint id <relay image> relay`.
  EOT
  type        = number
  default     = 999
}

variable "bifrost_uid" {
  description = "POSIX uid/gid Bifrost's EFS access point enforces. Keep it 1000: deploy/aws/docker/Dockerfile.bifrost runs as the upstream image's appuser (1000:0), and its entrypoint checks that /app/data is writable by that uid."
  type        = number
  default     = 1000
}

# --- Sizing -------------------------------------------------------------------

variable "task_sizes" {
  description = "Fargate cpu/memory per service."
  type = map(object({
    cpu    = number
    memory = number
  }))
  default = {
    control = { cpu = 256, memory = 512 }
    relay   = { cpu = 256, memory = 512 }
    bifrost = { cpu = 512, memory = 1024 }
  }
}

variable "log_retention_days" {
  type    = number
  default = 30
}

variable "alb_access_logs" {
  description = "Ship ALB access logs to S3. Off by default: pennies in S3, but every request line includes client IPs we otherwise do not keep."
  type        = bool
  default     = false
}

variable "operator_db_access" {
  description = <<-EOT
    Temporarily let the monitor box reach Postgres (5432), so the operator can
    create the app roles from an SSM session with psql. No ECS image carries a
    SQL client. Turn on for the setup step, then back off.
  EOT
  type        = bool
  default     = false
}
