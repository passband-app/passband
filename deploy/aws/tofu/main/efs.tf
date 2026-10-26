# One encrypted file system, one access point per writer. The access point
# enforces a POSIX identity, so every NFS operation from the task happens as
# that uid/gid regardless of what the process runs as:
#   relay   -> /relay   as var.relay_uid (999, the image's `relay` user). The
#              entrypoint's root-time `chown -R relay:relay /data` becomes a
#              no-op chown to the id it already is, and the relay (dropped to
#              999 by setpriv) owns the directory it writes.
#   bifrost -> /bifrost as var.bifrost_uid. Bifrost runs as root; the access
#              point maps it down, so root in the container is not root on EFS.

resource "aws_efs_file_system" "main" {
  creation_token   = "passband"
  encrypted        = true
  performance_mode = "generalPurpose"
  throughput_mode  = "elastic"

  lifecycle_policy {
    transition_to_ia = "AFTER_30_DAYS"
  }

  lifecycle {
    prevent_destroy = true
  }

  tags = { Name = "passband" }
}

resource "aws_efs_backup_policy" "main" {
  file_system_id = aws_efs_file_system.main.id
  backup_policy {
    status = "ENABLED"
  }
}

resource "aws_efs_mount_target" "main" {
  count = 2

  file_system_id  = aws_efs_file_system.main.id
  subnet_id       = aws_subnet.private[count.index].id
  security_groups = [aws_security_group.efs.id]
}

locals {
  efs_access_points = merge(
    { relay = var.relay_uid },
    var.bifrost_store == "efs" ? { bifrost = var.bifrost_uid } : {},
  )
}

resource "aws_efs_access_point" "this" {
  for_each = local.efs_access_points

  file_system_id = aws_efs_file_system.main.id

  posix_user {
    uid = each.value
    gid = each.value
  }

  root_directory {
    path = "/${each.key}"
    creation_info {
      owner_uid   = each.value
      owner_gid   = each.value
      permissions = "0750"
    }
  }

  tags = { Name = "passband-${each.key}" }
}

# With a file system policy present, EFS's default "anyone who can reach a
# mount target" grant is gone: only these task roles, only through their own
# access point, only over TLS.
data "aws_iam_policy_document" "efs" {
  dynamic "statement" {
    for_each = aws_efs_access_point.this
    content {
      sid       = "Mount${title(statement.key)}"
      actions   = ["elasticfilesystem:ClientMount", "elasticfilesystem:ClientWrite"]
      resources = [aws_efs_file_system.main.arn]
      principals {
        type        = "AWS"
        identifiers = [aws_iam_role.task[statement.key].arn]
      }
      condition {
        test     = "StringEquals"
        variable = "elasticfilesystem:AccessPointArn"
        values   = [statement.value.arn]
      }
    }
  }

  statement {
    sid       = "DenyInsecureTransport"
    effect    = "Deny"
    actions   = ["*"]
    resources = [aws_efs_file_system.main.arn]
    principals {
      type        = "AWS"
      identifiers = ["*"]
    }
    condition {
      test     = "Bool"
      variable = "aws:SecureTransport"
      values   = ["false"]
    }
  }
}

resource "aws_efs_file_system_policy" "main" {
  file_system_id = aws_efs_file_system.main.id
  policy         = data.aws_iam_policy_document.efs.json
}
