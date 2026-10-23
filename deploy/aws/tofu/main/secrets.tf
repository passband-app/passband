# Secret CONTAINERS only. There is deliberately no aws_secretsmanager_secret_version
# anywhere in this repo: a value set through tofu would sit in state and in
# every plan. The operator sets each value out of band:
#
#   aws secretsmanager put-secret-value \
#     --secret-id passband/control/SQUELCH_CONTROL_COOKIE_KEY \
#     --secret-string file:///dev/stdin
#
# A container with no current version makes its ECS task fail to start, which
# is why services_running defaults to false.

locals {
  secret_names = {
    control = [
      "SQUELCH_CONTROL_CLIENT_SECRET",
      "SQUELCH_CONTROL_COOKIE_KEY",
      "SQUELCH_CONTROL_WARDEN_TOKEN",
      "SQUELCH_CONTROL_BIFROST_ADMIN_TOKEN",
      "SQUELCH_CONTROL_ADMIN_TOKEN",
      "SQUELCH_CONTROL_RESEND_API_KEY",
      "SQUELCH_CONTROL_WORKOS_API_KEY",
      "SQUELCH_CONTROL_ALERT_WEBHOOK_URL",
      "SQUELCH_CONTROL_DATABASE_URL",
    ]
    relay = [
      "SQUELCH_RELAY_APNS_KEY",
      "SQUELCH_RELAY_AUTH_TOKEN",
      "SQUELCH_RELAY_PUSH_TOKEN",
    ]
    bifrost = [
      "ANTHROPIC_API_KEY",
    ]
    monitor = [
      "PROM_REMOTE_WRITE_PASSWORD",
      "PROM_WEB_BCRYPT",
      "GF_SECURITY_ADMIN_PASSWORD",
      "PROM_PASSWORD",
      "SLACK_ALERTS_WEBHOOK_URL",
      "BLACKBOX_BIFROST_CANARY_KEY",
      "BLACKBOX_CANARY_MODEL",
    ]
  }

  secrets = merge([
    for svc, names in local.secret_names : {
      for n in names : "${svc}/${n}" => { service = svc, env = n }
    }
  ]...)
}

resource "aws_secretsmanager_secret" "this" {
  for_each = local.secrets

  name                    = "passband/${each.key}"
  description             = "${each.value.env} for ${each.value.service}. Value set by the operator, never by tofu."
  kms_key_id              = aws_kms_key.secrets.arn
  recovery_window_in_days = 30

  tags = { service = each.value.service }
}

locals {
  # service -> list of its secret ARNs, for the least-privilege policies.
  secret_arns = {
    for svc in keys(local.secret_names) : svc => [
      for k, v in local.secrets : aws_secretsmanager_secret.this[k].arn if v.service == svc
    ]
  }
}
