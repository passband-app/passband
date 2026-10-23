# Per-service roles. Each ECS service has two:
#   execution role: what the ECS agent uses to start the task. Pulls its own
#                   ECR repo, writes its own log group, reads ONLY its own
#                   passband/<service>/* secrets.
#   task role:      what the running process (and an ECS Exec session) gets.
#                   ECS Exec channels + exec logging, and for EFS users their
#                   own access point. No secrets, no AWS APIs beyond that.

data "aws_iam_policy_document" "ecs_tasks_assume" {
  statement {
    actions = ["sts:AssumeRole"]
    principals {
      type        = "Service"
      identifiers = ["ecs-tasks.amazonaws.com"]
    }
    condition {
      test     = "StringEquals"
      variable = "aws:SourceAccount"
      values   = [local.account_id]
    }
    condition {
      test     = "ArnLike"
      variable = "aws:SourceArn"
      values   = ["arn:${local.partition}:ecs:${var.region}:${local.account_id}:*"]
    }
  }
}

resource "aws_iam_role" "execution" {
  for_each = toset(local.ecs_services)

  name               = "passband-${each.key}-execution"
  assume_role_policy = data.aws_iam_policy_document.ecs_tasks_assume.json
}

data "aws_iam_policy_document" "execution" {
  for_each = toset(local.ecs_services)

  statement {
    sid       = "EcrAuth"
    actions   = ["ecr:GetAuthorizationToken"]
    resources = ["*"] # this action has no resource scoping
  }

  statement {
    sid = "PullOwnImage"
    actions = [
      "ecr:BatchCheckLayerAvailability",
      "ecr:BatchGetImage",
      "ecr:GetDownloadUrlForLayer",
    ]
    resources = [aws_ecr_repository.this[each.key].arn]
  }

  statement {
    sid       = "WriteOwnLogs"
    actions   = ["logs:CreateLogStream", "logs:PutLogEvents"]
    resources = ["${aws_cloudwatch_log_group.service[each.key].arn}:*"]
  }

  statement {
    sid       = "ReadOwnSecrets"
    actions   = ["secretsmanager:GetSecretValue"]
    resources = local.secret_arns[each.key]
  }

  statement {
    sid       = "DecryptOwnSecrets"
    actions   = ["kms:Decrypt"]
    resources = [aws_kms_key.secrets.arn]
    condition {
      test     = "StringEquals"
      variable = "kms:ViaService"
      values   = ["secretsmanager.${var.region}.amazonaws.com"]
    }
    condition {
      test     = "StringEquals"
      variable = "kms:EncryptionContext:SecretARN"
      values   = local.secret_arns[each.key]
    }
  }
}

resource "aws_iam_role_policy" "execution" {
  for_each = toset(local.ecs_services)

  name   = "execution"
  role   = aws_iam_role.execution[each.key].id
  policy = data.aws_iam_policy_document.execution[each.key].json
}

resource "aws_iam_role" "task" {
  for_each = toset(local.ecs_services)

  name               = "passband-${each.key}-task"
  assume_role_policy = data.aws_iam_policy_document.ecs_tasks_assume.json
}

data "aws_iam_policy_document" "task" {
  for_each = toset(local.ecs_services)

  statement {
    sid = "EcsExecChannels"
    actions = [
      "ssmmessages:CreateControlChannel",
      "ssmmessages:CreateDataChannel",
      "ssmmessages:OpenControlChannel",
      "ssmmessages:OpenDataChannel",
    ]
    resources = ["*"] # ssmmessages has no resource-level scoping
  }

  statement {
    sid       = "EcsExecSessionKey"
    actions   = ["kms:Decrypt", "kms:GenerateDataKey"]
    resources = [aws_kms_key.logs.arn]
  }

  statement {
    sid       = "EcsExecLogs"
    actions   = ["logs:CreateLogStream", "logs:PutLogEvents", "logs:DescribeLogStreams"]
    resources = ["${aws_cloudwatch_log_group.exec.arn}:*"]
  }

  statement {
    sid       = "EcsExecLogGroups"
    actions   = ["logs:DescribeLogGroups"]
    resources = ["*"]
  }

  dynamic "statement" {
    for_each = contains(keys(aws_efs_access_point.this), each.key) ? [1] : []
    content {
      sid       = "OwnAccessPoint"
      actions   = ["elasticfilesystem:ClientMount", "elasticfilesystem:ClientWrite"]
      resources = [aws_efs_file_system.main.arn]
      condition {
        test     = "StringEquals"
        variable = "elasticfilesystem:AccessPointArn"
        values   = [aws_efs_access_point.this[each.key].arn]
      }
    }
  }
}

resource "aws_iam_role_policy" "task" {
  for_each = toset(local.ecs_services)

  name   = "task"
  role   = aws_iam_role.task[each.key].id
  policy = data.aws_iam_policy_document.task[each.key].json
}

# --- GitHub Actions deploy role --------------------------------------------------

resource "aws_iam_openid_connect_provider" "github" {
  count = var.create_github_oidc_provider ? 1 : 0

  url            = "https://token.actions.githubusercontent.com"
  client_id_list = ["sts.amazonaws.com"]
  # AWS no longer checks this for GitHub's IdP (it trusts its own CA store),
  # but the API still requires a value.
  thumbprint_list = ["6938fd4d98bab03faadb97b34396831e3780aea1"]
}

data "aws_iam_openid_connect_provider" "github" {
  count = var.create_github_oidc_provider ? 0 : 1
  url   = "https://token.actions.githubusercontent.com"
}

locals {
  github_deploy_sub = "repo:${var.github_repo}:environment:${var.github_environment}:job_workflow_ref:${var.github_repo}/${var.github_workflow_path}@refs/heads/main"

  github_oidc_arn = var.create_github_oidc_provider ? aws_iam_openid_connect_provider.github[0].arn : data.aws_iam_openid_connect_provider.github[0].arn
}

data "aws_iam_policy_document" "github_assume" {
  statement {
    actions = ["sts:AssumeRoleWithWebIdentity"]
    principals {
      type        = "Federated"
      identifiers = [local.github_oidc_arn]
    }
    condition {
      test     = "StringEquals"
      variable = "token.actions.githubusercontent.com:aud"
      values   = ["sts.amazonaws.com"]
    }
    condition {
      test     = "StringEquals"
      variable = "token.actions.githubusercontent.com:sub"
      # The CUSTOMIZED sub (README.md step 1 sets the repo's OIDC template to
      # include_claim_keys ["repo", "context", "job_workflow_ref"]). GitHub
      # renders that as
      #   repo:<owner/name>:environment:<env>:job_workflow_ref:<owner/name>/<path>@<ref>
      # (docs.github.com/en/actions/reference/security/oidc, "Customizing the
      # subject claims"). So the trust itself requires the deploy-aws.yml
      # file AS IT IS ON main, running in the environment: a workflow on
      # another branch, or a different workflow on main, gets a different sub
      # and is refused even if the environment's branch policy is missing.
      # The environment's main-only branch policy is the second lock.
      values = [local.github_deploy_sub]
    }
  }
}

resource "aws_iam_role" "github_deploy" {
  name                 = "passband-github-deploy"
  assume_role_policy   = data.aws_iam_policy_document.github_assume.json
  max_session_duration = 3600
}

data "aws_iam_policy_document" "github_deploy" {
  # --- ECR: push to (and read back from) the six repos -------------------------
  statement {
    sid       = "EcrAuth"
    actions   = ["ecr:GetAuthorizationToken"]
    resources = ["*"]
  }

  statement {
    sid = "EcrPush"
    actions = [
      "ecr:BatchCheckLayerAvailability",
      "ecr:BatchGetImage",
      "ecr:CompleteLayerUpload",
      "ecr:DescribeImages",
      "ecr:GetDownloadUrlForLayer",
      "ecr:InitiateLayerUpload",
      "ecr:PutImage",
      "ecr:UploadLayerPart",
    ]
    resources = [for r in aws_ecr_repository.this : r.arn]
  }

  # --- ECS: new task definition revision + update the three services -----------
  statement {
    sid = "EcsTaskDefinitions"
    # Neither action supports resource-level permissions.
    actions   = ["ecs:DescribeTaskDefinition", "ecs:RegisterTaskDefinition"]
    resources = ["*"]
  }

  # The render action copies the family's tags (default_tags) into the new
  # revision, and RegisterTaskDefinition with tags is authorized as
  # ecs:TagResource too (tag-on-create). Scoped to that call only.
  statement {
    sid       = "EcsTagOnRegister"
    actions   = ["ecs:TagResource"]
    resources = ["*"]
    condition {
      test     = "StringEquals"
      variable = "ecs:CreateAction"
      values   = ["RegisterTaskDefinition"]
    }
  }

  statement {
    sid       = "EcsServices"
    actions   = ["ecs:DescribeServices", "ecs:UpdateService"]
    resources = [for s in aws_ecs_service.this : s.id]
  }

  statement {
    sid     = "PassOnlyTheTaskRoles"
    actions = ["iam:PassRole"]
    resources = concat(
      [for r in aws_iam_role.execution : r.arn],
      [for r in aws_iam_role.task : r.arn],
    )
    condition {
      test     = "StringEquals"
      variable = "iam:PassedToService"
      values   = ["ecs-tasks.amazonaws.com"]
    }
  }

  # --- SSM: pin IMAGE_TAG and reload the stack on the monitor box --------------
  # Only through passband-monitor-deploy (monitor.tf), which takes a 40-hex
  # commit SHA and nothing else. No AWS-RunShellScript: CI gets no shell on
  # the box (and so none of the monitor secrets, nor RDS).
  statement {
    sid     = "SsmSendCommandToMonitor"
    actions = ["ssm:SendCommand"]
    resources = [
      aws_instance.monitor.arn,
      aws_ssm_document.monitor_deploy.arn,
    ]
  }

  statement {
    sid       = "SsmReadCommandResult"
    actions   = ["ssm:GetCommandInvocation", "ssm:ListCommandInvocations"]
    resources = ["*"] # no resource-level scoping for these
  }

  # --- Site: S3 sync + CloudFront invalidation (+ the routing function) --------
  statement {
    sid       = "SiteList"
    actions   = ["s3:ListBucket"]
    resources = [aws_s3_bucket.site.arn]
  }

  statement {
    sid       = "SiteObjects"
    actions   = ["s3:GetObject", "s3:PutObject", "s3:DeleteObject"]
    resources = ["${aws_s3_bucket.site.arn}/*"]
  }

  statement {
    sid       = "SiteInvalidate"
    actions   = ["cloudfront:CreateInvalidation", "cloudfront:GetInvalidation"]
    resources = [aws_cloudfront_distribution.site.arn]
  }

  # The function bakes the latest app version at BUILD time (DESIGN.md
  # "landing site"), so each site deploy must replace its code. See site.tf.
  statement {
    sid = "SiteRoutingFunction"
    actions = [
      "cloudfront:DescribeFunction",
      "cloudfront:GetFunction",
      "cloudfront:UpdateFunction",
      "cloudfront:TestFunction",
      "cloudfront:PublishFunction",
    ]
    resources = [aws_cloudfront_function.site.arn]
  }
}

resource "aws_iam_role_policy" "github_deploy" {
  name   = "deploy"
  role   = aws_iam_role.github_deploy.id
  policy = data.aws_iam_policy_document.github_deploy.json
}
