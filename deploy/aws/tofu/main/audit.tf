# CloudTrail (management events, read AND write, one multi-region trail, the
# free tier), the monthly budget alarm, and the security alert tripwires.

resource "aws_s3_bucket" "cloudtrail" {
  bucket = "passband-cloudtrail-${local.account_id}"
}

resource "aws_s3_bucket_public_access_block" "cloudtrail" {
  bucket                  = aws_s3_bucket.cloudtrail.id
  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_ownership_controls" "cloudtrail" {
  bucket = aws_s3_bucket.cloudtrail.id
  rule {
    object_ownership = "BucketOwnerEnforced"
  }
}

resource "aws_s3_bucket_server_side_encryption_configuration" "cloudtrail" {
  bucket = aws_s3_bucket.cloudtrail.id
  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm     = "aws:kms"
      kms_master_key_id = aws_kms_key.logs.arn
    }
    bucket_key_enabled = true
  }
}

resource "aws_s3_bucket_versioning" "cloudtrail" {
  bucket = aws_s3_bucket.cloudtrail.id
  versioning_configuration {
    status = "Enabled"
  }
}

resource "aws_s3_bucket_lifecycle_configuration" "cloudtrail" {
  bucket = aws_s3_bucket.cloudtrail.id
  rule {
    id     = "expire"
    status = "Enabled"
    filter {}
    expiration {
      days = 365
    }
    noncurrent_version_expiration {
      noncurrent_days = 30
    }
  }
}

locals {
  trail_arn = "arn:${local.partition}:cloudtrail:${var.region}:${local.account_id}:trail/passband"
}

data "aws_iam_policy_document" "cloudtrail_bucket" {
  statement {
    sid       = "AclCheck"
    actions   = ["s3:GetBucketAcl"]
    resources = [aws_s3_bucket.cloudtrail.arn]
    principals {
      type        = "Service"
      identifiers = ["cloudtrail.amazonaws.com"]
    }
    condition {
      test     = "StringEquals"
      variable = "aws:SourceArn"
      values   = [local.trail_arn]
    }
  }

  statement {
    sid       = "Write"
    actions   = ["s3:PutObject"]
    resources = ["${aws_s3_bucket.cloudtrail.arn}/AWSLogs/${local.account_id}/*"]
    principals {
      type        = "Service"
      identifiers = ["cloudtrail.amazonaws.com"]
    }
    condition {
      test     = "StringEquals"
      variable = "aws:SourceArn"
      values   = [local.trail_arn]
    }
  }

  statement {
    sid     = "DenyInsecureTransport"
    effect  = "Deny"
    actions = ["s3:*"]
    resources = [
      aws_s3_bucket.cloudtrail.arn,
      "${aws_s3_bucket.cloudtrail.arn}/*",
    ]
    principals {
      type        = "*"
      identifiers = ["*"]
    }
    condition {
      test     = "Bool"
      variable = "aws:SecureTransport"
      values   = ["false"]
    }
  }
}

resource "aws_s3_bucket_policy" "cloudtrail" {
  bucket = aws_s3_bucket.cloudtrail.id
  policy = data.aws_iam_policy_document.cloudtrail_bucket.json
}

resource "aws_cloudtrail" "main" {
  name                          = "passband"
  s3_bucket_name                = aws_s3_bucket.cloudtrail.id
  kms_key_id                    = aws_kms_key.logs.arn
  is_multi_region_trail         = true
  include_global_service_events = true
  enable_log_file_validation    = true

  event_selector {
    # "All", not "WriteOnly": the secrets alert below needs GetSecretValue,
    # which is a read event.
    read_write_type           = "All"
    include_management_events = true
  }

  depends_on = [aws_s3_bucket_policy.cloudtrail]
}

resource "aws_budgets_budget" "monthly" {
  name         = "passband-monthly"
  budget_type  = "COST"
  limit_amount = tostring(var.budget_limit_usd)
  limit_unit   = "USD"
  time_unit    = "MONTHLY"

  notification {
    comparison_operator        = "GREATER_THAN"
    threshold                  = 80
    threshold_type             = "PERCENTAGE"
    notification_type          = "ACTUAL"
    subscriber_email_addresses = [var.budget_email]
  }

  notification {
    comparison_operator        = "GREATER_THAN"
    threshold                  = 100
    threshold_type             = "PERCENTAGE"
    notification_type          = "ACTUAL"
    subscriber_email_addresses = [var.budget_email]
  }

  notification {
    comparison_operator        = "GREATER_THAN"
    threshold                  = 100
    threshold_type             = "PERCENTAGE"
    notification_type          = "FORECASTED"
    subscriber_email_addresses = [var.budget_email]
  }
}

# --- Security alerts: CloudTrail -> EventBridge -> SNS email -----------------------
#
# Mails budget_email on sensitive management events. EventBridge rules on AWS
# events are free, SNS email is free at this volume, and the topic is
# encrypted with the logs key.
#
#   secrets   Secrets Manager reads and writes by anyone who is not an ECS
#             execution role or the monitor instance role.
#   shells    ecs ExecuteCommand, ssm StartSession / ResumeSession.
#   iam       every IAM write.
#   signin    ConsoleLogin.
#   trail     StopLogging / DeleteTrail / UpdateTrail / PutEventSelectors.
#
# The secrets rule uses ENABLED_WITH_ALL_CLOUDTRAIL_MANAGEMENT_EVENTS because
# its events are read-only management events; IAM and sign-in events arrive
# in us-east-1 and are forwarded to this region.

resource "aws_sns_topic" "security_alerts" {
  name              = "passband-security-alerts"
  kms_master_key_id = aws_kms_key.logs.arn
}

data "aws_iam_policy_document" "security_alerts_topic" {
  statement {
    sid       = "EventBridgePublish"
    actions   = ["sns:Publish"]
    resources = [aws_sns_topic.security_alerts.arn]
    principals {
      type        = "Service"
      identifiers = ["events.amazonaws.com"]
    }
    condition {
      test     = "ArnLike"
      variable = "aws:SourceArn"
      values   = ["arn:${local.partition}:events:${var.region}:${local.account_id}:rule/passband-alert-*"]
    }
  }
}

resource "aws_sns_topic_policy" "security_alerts" {
  arn    = aws_sns_topic.security_alerts.arn
  policy = data.aws_iam_policy_document.security_alerts_topic.json
}

resource "aws_sns_topic_subscription" "security_alerts_email" {
  topic_arn = aws_sns_topic.security_alerts.arn
  protocol  = "email"
  endpoint  = var.budget_email
}

locals {
  # Role ARNs allowed to read secrets without an alert.
  secret_readers = concat(
    [for r in aws_iam_role.execution : r.arn],
    [aws_iam_role.monitor.arn],
  )

  security_alert_rules = {
    secrets = {
      description = "Secrets Manager reads/writes by anyone but the ECS execution roles and the monitor role"
      state       = "ENABLED_WITH_ALL_CLOUDTRAIL_MANAGEMENT_EVENTS"
      pattern = {
        source        = ["aws.secretsmanager"]
        "detail-type" = ["AWS API Call via CloudTrail"]
        detail = {
          eventName = ["GetSecretValue", "BatchGetSecretValue", "PutSecretValue", "UpdateSecret", "DeleteSecret", "PutResourcePolicy"]
          "$or" = [
            { userIdentity = { type = [{ "anything-but" = ["AssumedRole", "AWSService"] }] } },
            { userIdentity = { sessionContext = { sessionIssuer = { arn = [{ "anything-but" = local.secret_readers }] } } } },
          ]
        }
      }
    }
    shells = {
      description = "ECS Exec and SSM sessions"
      state       = "ENABLED"
      pattern = {
        source        = ["aws.ecs", "aws.ssm"]
        "detail-type" = ["AWS API Call via CloudTrail"]
        detail = {
          eventName = ["ExecuteCommand", "StartSession", "ResumeSession"]
        }
      }
    }
    iam = {
      description = "IAM writes (read-only IAM calls never reach an ENABLED rule)"
      state       = "ENABLED"
      pattern = {
        source        = ["aws.iam"]
        "detail-type" = ["AWS API Call via CloudTrail"]
      }
    }
    signin = {
      description = "Console sign-ins"
      state       = "ENABLED"
      pattern = {
        source        = ["aws.signin"]
        "detail-type" = ["AWS Console Sign In via CloudTrail"]
        detail = {
          eventName = ["ConsoleLogin"]
        }
      }
    }
    trail = {
      description = "Anything that turns CloudTrail down"
      state       = "ENABLED"
      pattern = {
        source        = ["aws.cloudtrail"]
        "detail-type" = ["AWS API Call via CloudTrail"]
        detail = {
          eventName = ["StopLogging", "DeleteTrail", "UpdateTrail", "PutEventSelectors"]
        }
      }
    }
  }
}

resource "aws_cloudwatch_event_rule" "security_alert" {
  for_each = local.security_alert_rules

  name          = "passband-alert-${each.key}"
  description   = each.value.description
  state         = each.value.state
  event_pattern = jsonencode(each.value.pattern)
}

resource "aws_cloudwatch_event_target" "security_alert" {
  for_each = local.security_alert_rules

  rule = aws_cloudwatch_event_rule.security_alert[each.key].name
  arn  = aws_sns_topic.security_alerts.arn

  input_transformer {
    input_paths = {
      name   = "$.detail.eventName"
      who    = "$.detail.userIdentity.arn"
      ip     = "$.detail.sourceIPAddress"
      region = "$.region"
      time   = "$.time"
      err    = "$.detail.errorCode"
    }
    input_template = "\"passband security alert (${each.key}): <name> by <who> from <ip> in <region> at <time>. errorCode=<err>. Look it up in CloudTrail event history.\""
  }
}

# us-east-1 -> var.region forwarder for the global-service events above.
data "aws_iam_policy_document" "events_assume" {
  statement {
    actions = ["sts:AssumeRole"]
    principals {
      type        = "Service"
      identifiers = ["events.amazonaws.com"]
    }
    condition {
      test     = "StringEquals"
      variable = "aws:SourceAccount"
      values   = [local.account_id]
    }
  }
}

resource "aws_iam_role" "security_alert_forwarder" {
  name               = "passband-security-alert-forwarder"
  assume_role_policy = data.aws_iam_policy_document.events_assume.json
}

data "aws_iam_policy_document" "security_alert_forwarder" {
  statement {
    actions   = ["events:PutEvents"]
    resources = ["arn:${local.partition}:events:${var.region}:${local.account_id}:event-bus/default"]
  }
}

resource "aws_iam_role_policy" "security_alert_forwarder" {
  name   = "forward"
  role   = aws_iam_role.security_alert_forwarder.id
  policy = data.aws_iam_policy_document.security_alert_forwarder.json
}

resource "aws_cloudwatch_event_rule" "security_alert_forward_global" {
  provider = aws.us_east_1

  name        = "passband-alert-forward-global"
  description = "Forward IAM and console sign-in events to ${var.region}, where the alert rules live"
  event_pattern = jsonencode({
    source        = ["aws.iam", "aws.signin"]
    "detail-type" = ["AWS API Call via CloudTrail", "AWS Console Sign In via CloudTrail"]
  })
}

resource "aws_cloudwatch_event_target" "security_alert_forward_global" {
  provider = aws.us_east_1

  rule     = aws_cloudwatch_event_rule.security_alert_forward_global.name
  arn      = "arn:${local.partition}:events:${var.region}:${local.account_id}:event-bus/default"
  role_arn = aws_iam_role.security_alert_forwarder.arn
}

# --- GuardDuty (opt-in, see var.enable_guardduty) ----------------------------------

resource "aws_guardduty_detector" "main" {
  count  = var.enable_guardduty ? 1 : 0
  enable = true
}
