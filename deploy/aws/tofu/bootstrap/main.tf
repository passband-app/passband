# The state bucket and the KMS key that encrypts both the bucket and OpenTofu's
# own state encryption. Applied ONCE with local state, then migrated into the
# bucket it created (README.md, "Apply order"). Nothing here holds a secret.

terraform {
  required_version = ">= 1.8.0"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 6.0"
    }
  }

  # Step 1 runs with no backend (local state): comment this block out for the
  # very first apply. Then restore it, copy backend.hcl.example to backend.hcl
  # (gitignored), fill in the bucket and key ARN from `tofu output`, and run
  # `tofu init -backend-config=backend.hcl -migrate-state`. SSE-KMS with the
  # state key; no client-side encryption block here (the key cannot encrypt
  # the state that creates it), and this state holds no secret.
  backend "s3" {}
}

provider "aws" {
  region = var.region

  default_tags {
    tags = {
      project    = "passband"
      managed-by = "opentofu"
      tofu-root  = "bootstrap"
    }
  }
}

variable "region" {
  type    = string
  default = "us-west-2"
}

data "aws_caller_identity" "current" {}

locals {
  bucket = "passband-tofu-state-${data.aws_caller_identity.current.account_id}"
}

resource "aws_kms_key" "state" {
  description             = "passband: OpenTofu state bucket SSE and state encryption"
  enable_key_rotation     = true
  deletion_window_in_days = 30

  lifecycle {
    prevent_destroy = true
  }
}

resource "aws_kms_alias" "state" {
  name          = "alias/passband-tofu-state"
  target_key_id = aws_kms_key.state.key_id
}

resource "aws_s3_bucket" "state" {
  bucket = local.bucket

  lifecycle {
    prevent_destroy = true
  }
}

resource "aws_s3_bucket_versioning" "state" {
  bucket = aws_s3_bucket.state.id
  versioning_configuration {
    status = "Enabled"
  }
}

resource "aws_s3_bucket_server_side_encryption_configuration" "state" {
  bucket = aws_s3_bucket.state.id
  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm     = "aws:kms"
      kms_master_key_id = aws_kms_key.state.arn
    }
    bucket_key_enabled = true
  }
}

resource "aws_s3_bucket_public_access_block" "state" {
  bucket                  = aws_s3_bucket.state.id
  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_ownership_controls" "state" {
  bucket = aws_s3_bucket.state.id
  rule {
    object_ownership = "BucketOwnerEnforced"
  }
}

# Old state versions are the undo button; keep a year of them.
resource "aws_s3_bucket_lifecycle_configuration" "state" {
  bucket = aws_s3_bucket.state.id
  rule {
    id     = "expire-old-versions"
    status = "Enabled"
    filter {}
    noncurrent_version_expiration {
      noncurrent_days = 365
    }
    abort_incomplete_multipart_upload {
      days_after_initiation = 7
    }
  }
}

data "aws_iam_policy_document" "state_bucket" {
  statement {
    sid     = "DenyInsecureTransport"
    effect  = "Deny"
    actions = ["s3:*"]
    resources = [
      aws_s3_bucket.state.arn,
      "${aws_s3_bucket.state.arn}/*",
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

resource "aws_s3_bucket_policy" "state" {
  bucket = aws_s3_bucket.state.id
  policy = data.aws_iam_policy_document.state_bucket.json
}

output "state_bucket" {
  description = "Bucket for every root's backend block."
  value       = aws_s3_bucket.state.bucket
}

output "state_kms_key_arn" {
  description = "KMS key for the main root's state_kms_key_arn variable (OpenTofu state encryption)."
  value       = aws_kms_key.state.arn
}
