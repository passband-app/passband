# The monitor box: prometheus + grafana + blackbox under docker compose
# (project dir /opt/passband-monitor), from the three monitoring ECR repos.
# Prometheus cannot keep its TSDB on NFS, hence EC2 + EBS rather than Fargate.
#
# User data is deploy/aws/monitor/bootstrap.sh, owned by the monitor builder.
# It runs ONCE, on first boot; afterwards CI re-runs `docker compose pull && up
# -d` through SSM, so tofu ignores later changes to user data and to the AMI
# (the SSM parameter moves with every AL2023 release, which would otherwise
# replace the box). Replacing the instance loses nothing: state is on the data
# volume below, which survives it.
#
# Contract for bootstrap.sh:
#   - the data volume is attached as /dev/sdf (AL2023 udev symlinks it to the
#     nvme device), possibly a few seconds AFTER boot: wait for it;
#   - first boot must mkfs only a volume with no filesystem (blkid), never a
#     formatted one;
#   - secrets come from passband/monitor/<NAME> via the instance role; the
#     region from IMDS; the registry is <account>.dkr.ecr.<region>.amazonaws.com.

locals {
  monitor_bootstrap = "${path.module}/../../monitor/bootstrap.sh"
}

data "aws_ssm_parameter" "al2023_arm64" {
  name = "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-arm64"
}

resource "aws_iam_role" "monitor" {
  name               = "passband-monitor"
  assume_role_policy = data.aws_iam_policy_document.ec2_assume.json
}

resource "aws_iam_role_policy_attachment" "monitor_ssm" {
  role       = aws_iam_role.monitor.name
  policy_arn = "arn:${local.partition}:iam::aws:policy/AmazonSSMManagedInstanceCore"
}

data "aws_iam_policy_document" "monitor" {
  statement {
    sid       = "ReadOwnSecrets"
    actions   = ["secretsmanager:GetSecretValue"]
    resources = local.secret_arns["monitor"]
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
      values   = local.secret_arns["monitor"]
    }
  }

  statement {
    sid       = "EcrAuth"
    actions   = ["ecr:GetAuthorizationToken"]
    resources = ["*"]
  }

  statement {
    sid = "PullMonitoringImages"
    actions = [
      "ecr:BatchCheckLayerAvailability",
      "ecr:BatchGetImage",
      "ecr:GetDownloadUrlForLayer",
    ]
    resources = [for r in local.monitor_repos : aws_ecr_repository.this[r].arn]
  }
}

resource "aws_iam_role_policy" "monitor" {
  name   = "monitor"
  role   = aws_iam_role.monitor.id
  policy = data.aws_iam_policy_document.monitor.json
}

resource "aws_iam_instance_profile" "monitor" {
  name = "passband-monitor"
  role = aws_iam_role.monitor.name
}

resource "aws_instance" "monitor" {
  ami                         = data.aws_ssm_parameter.al2023_arm64.insecure_value
  instance_type               = "t4g.small"
  subnet_id                   = aws_subnet.private[0].id
  vpc_security_group_ids      = [aws_security_group.monitor.id]
  iam_instance_profile        = aws_iam_instance_profile.monitor.name
  associate_public_ip_address = false
  # no key_name: no SSH, Session Manager only
  ebs_optimized = true
  monitoring    = false

  user_data                   = fileexists(local.monitor_bootstrap) ? file(local.monitor_bootstrap) : null
  user_data_replace_on_change = false

  metadata_options {
    http_tokens   = "required"
    http_endpoint = "enabled"
    # 1: containers on the bridge network cannot reach IMDS, so they cannot
    # borrow the role that reads every monitor secret. bootstrap.sh runs on the
    # host and is unaffected.
    http_put_response_hop_limit = 1
    instance_metadata_tags      = "disabled"
  }

  root_block_device {
    volume_type           = "gp3"
    volume_size           = 16
    encrypted             = true
    delete_on_termination = true
  }

  tags = { Name = "passband-monitor" }

  lifecycle {
    ignore_changes = [ami, user_data]

    precondition {
      condition     = fileexists(local.monitor_bootstrap)
      error_message = "deploy/aws/monitor/bootstrap.sh does not exist yet; the monitor box would boot with no user data."
    }
  }
}

resource "aws_ebs_volume" "monitor_data" {
  availability_zone = aws_subnet.private[0].availability_zone
  size              = 30
  type              = "gp3"
  encrypted         = true

  tags = { Name = "passband-monitor-data" }

  lifecycle {
    prevent_destroy = true
  }
}

resource "aws_volume_attachment" "monitor_data" {
  device_name = "/dev/sdf"
  volume_id   = aws_ebs_volume.monitor_data.id
  instance_id = aws_instance.monitor.id
  # On instance replacement, detach cleanly instead of force-detaching a
  # mounted Prometheus TSDB.
  stop_instance_before_detaching = true
}

# Snapshots of the data volume: Grafana state and 30d of metrics. DLM is free;
# snapshot storage is pennies at this size.
data "aws_iam_policy_document" "dlm_assume" {
  statement {
    actions = ["sts:AssumeRole"]
    principals {
      type        = "Service"
      identifiers = ["dlm.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "dlm" {
  name               = "passband-dlm"
  assume_role_policy = data.aws_iam_policy_document.dlm_assume.json
}

resource "aws_iam_role_policy_attachment" "dlm" {
  role       = aws_iam_role.dlm.name
  policy_arn = "arn:${local.partition}:iam::aws:policy/service-role/AWSDataLifecycleManagerServiceRole"
}

resource "aws_dlm_lifecycle_policy" "monitor_data" {
  description        = "passband monitor data daily keep 7"
  execution_role_arn = aws_iam_role.dlm.arn
  state              = "ENABLED"

  policy_details {
    resource_types = ["VOLUME"]
    target_tags    = { Name = "passband-monitor-data" }

    schedule {
      name      = "daily"
      copy_tags = true
      create_rule {
        interval      = 24
        interval_unit = "HOURS"
        times         = ["11:00"]
      }
      retain_rule {
        count = 7
      }
    }
  }
}

# --- CI's only way onto the box ---------------------------------------------------
# The deploy role may SendCommand with THIS document and no other (iam.tf). It
# takes one parameter, a 40-hex commit SHA (SSM rejects anything else before it
# reaches the box), pins it as IMAGE_TAG in the compose .env bootstrap.sh
# wrote, and runs the unit's ExecReload (bin/up: fetch-secrets, compose pull,
# up -d, health wait). reload-or-restart so a unit that is not active (a fresh
# box still waiting on its first images) is started instead of erroring.
#
# compose.yaml is NOT shipped through this document: it travels inside the
# passband-prometheus image at the same tag, and bin/up copies it out before
# pulling (bootstrap.sh). So a compose.yaml change is an ordinary monitor
# deploy, and CI still never gets an arbitrary shell here.
resource "aws_ssm_document" "monitor_deploy" {
  name            = "passband-monitor-deploy"
  document_type   = "Command"
  document_format = "JSON"
  target_type     = "/AWS::EC2::Instance"

  content = jsonencode({
    schemaVersion = "2.2"
    description   = "Pin the monitor stack's IMAGE_TAG to a commit SHA and reload passband-monitor."
    parameters = {
      ImageTag = {
        type           = "String"
        description    = "Commit SHA the deploy built (the ECR image tag)."
        allowedPattern = "^[0-9a-f]{40}$"
      }
    }
    mainSteps = [{
      action = "aws:runShellScript"
      name   = "deploy"
      inputs = {
        timeoutSeconds = "1200"
        runCommand = [
          "set -eu",
          "cd /opt/passband-monitor",
          "sed -i \"s/^IMAGE_TAG=.*/IMAGE_TAG={{ImageTag}}/\" .env",
          "grep -qx 'IMAGE_TAG={{ImageTag}}' .env",
          "systemctl reload-or-restart passband-monitor.service",
        ]
      }
    }]
  })
}
