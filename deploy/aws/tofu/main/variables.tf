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
  description = "Operator address for the $150/month AWS Budgets alarm."
  type        = string
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

variable "github_environment" {
  description = "GitHub environment the workflow_dispatch jobs run in."
  type        = string
  default     = "aws-production"
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
    (ratelimit.rs client_ip), so 1 is the real client, as on Railway.
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
  description = "SQUELCH_CONTROL_LLM_MODELS; null leaves control's default. Copy the live Railway value."
  type        = string
  default     = null
}

variable "control_assistant_budget_usd" {
  description = "SQUELCH_CONTROL_ASSISTANT_BUDGET_USD; null leaves control's default."
  type        = string
  default     = null
}

variable "control_assistant_models" {
  description = "SQUELCH_CONTROL_ASSISTANT_MODELS; null leaves control's default. Copy the live Railway value."
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
  description = "POSIX uid/gid Bifrost's EFS access point enforces. Bifrost runs as root (Dockerfile.bifrost), and the access point maps every operation to this id regardless, so any non-zero id works."
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
