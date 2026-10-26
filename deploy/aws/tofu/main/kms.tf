# Two customer-managed keys ($1/month each):
#   secrets: every passband/* Secrets Manager secret. Each reader is granted
#            Decrypt only through Secrets Manager and only for its own secret
#            ARNs (iam.tf), so the key does not widen anybody's reach.
#   logs:    CloudWatch log groups, ECS Exec session encryption + its logs,
#            the CloudTrail trail, and the security alerts SNS topic.
# RDS, EFS and EBS use their AWS-managed default keys: still encrypted at rest,
# and nothing outside the account ever needs a grant on them.

locals {
  account_id = data.aws_caller_identity.current.account_id
  partition  = data.aws_partition.current.partition
  root_arn   = "arn:${data.aws_partition.current.partition}:iam::${data.aws_caller_identity.current.account_id}:root"
}

data "aws_iam_policy_document" "kms_secrets" {
  statement {
    sid       = "AccountAdministersViaIam"
    actions   = ["kms:*"]
    resources = ["*"]
    principals {
      type        = "AWS"
      identifiers = [local.root_arn]
    }
  }
}

resource "aws_kms_key" "secrets" {
  description             = "passband: Secrets Manager values"
  enable_key_rotation     = true
  deletion_window_in_days = 30
  policy                  = data.aws_iam_policy_document.kms_secrets.json
}

resource "aws_kms_alias" "secrets" {
  name          = "alias/passband-secrets"
  target_key_id = aws_kms_key.secrets.key_id
}

data "aws_iam_policy_document" "kms_logs" {
  statement {
    sid       = "AccountAdministersViaIam"
    actions   = ["kms:*"]
    resources = ["*"]
    principals {
      type        = "AWS"
      identifiers = [local.root_arn]
    }
  }

  statement {
    sid = "CloudWatchLogs"
    actions = [
      "kms:Encrypt*",
      "kms:Decrypt*",
      "kms:ReEncrypt*",
      "kms:GenerateDataKey*",
      "kms:Describe*",
    ]
    resources = ["*"]
    principals {
      type        = "Service"
      identifiers = ["logs.${var.region}.amazonaws.com"]
    }
    condition {
      test     = "ArnLike"
      variable = "kms:EncryptionContext:aws:logs:arn"
      values   = ["arn:${local.partition}:logs:${var.region}:${local.account_id}:log-group:*"]
    }
  }

  statement {
    sid       = "CloudTrailEncrypt"
    actions   = ["kms:GenerateDataKey*"]
    resources = ["*"]
    principals {
      type        = "Service"
      identifiers = ["cloudtrail.amazonaws.com"]
    }
    condition {
      test     = "StringLike"
      variable = "kms:EncryptionContext:aws:cloudtrail:arn"
      values   = ["arn:${local.partition}:cloudtrail:*:${local.account_id}:trail/*"]
    }
    condition {
      test     = "StringEquals"
      variable = "aws:SourceArn"
      values   = ["arn:${local.partition}:cloudtrail:${var.region}:${local.account_id}:trail/passband"]
    }
  }

  # EventBridge publishing to the encrypted passband-security-alerts topic
  # (audit.tf). SNS calls KMS with the topic ARN as encryption context.
  statement {
    sid       = "EventBridgeToAlertsTopic"
    actions   = ["kms:Decrypt", "kms:GenerateDataKey*"]
    resources = ["*"]
    principals {
      type        = "Service"
      identifiers = ["events.amazonaws.com"]
    }
    condition {
      test     = "StringEquals"
      variable = "kms:EncryptionContext:aws:sns:topicArn"
      values   = ["arn:${local.partition}:sns:${var.region}:${local.account_id}:passband-security-alerts"]
    }
  }

  statement {
    sid       = "CloudTrailDescribe"
    actions   = ["kms:DescribeKey"]
    resources = ["*"]
    principals {
      type        = "Service"
      identifiers = ["cloudtrail.amazonaws.com"]
    }
  }
}

resource "aws_kms_key" "logs" {
  description             = "passband: CloudWatch logs, ECS Exec, CloudTrail"
  enable_key_rotation     = true
  deletion_window_in_days = 30
  policy                  = data.aws_iam_policy_document.kms_logs.json
}

resource "aws_kms_alias" "logs" {
  name          = "alias/passband-logs"
  target_key_id = aws_kms_key.logs.key_id
}
