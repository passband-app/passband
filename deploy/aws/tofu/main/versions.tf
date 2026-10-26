terraform {
  # 1.8 is the first release that evaluates variables in the backend and
  # encryption blocks, which both of these use.
  required_version = ">= 1.8.0"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 6.0"
    }
  }

  backend "s3" {
    bucket       = var.state_bucket
    key          = "main/terraform.tfstate"
    region       = var.region
    encrypt      = true
    kms_key_id   = var.state_kms_key_arn
    use_lockfile = true
  }

  # OpenTofu client-side state encryption: the state object is AES-GCM
  # encrypted with a data key from KMS before it reaches S3, so the bucket's
  # own SSE-KMS is the second layer, not the only one. Plans are encrypted the
  # same way. `enforced` refuses to ever write an unencrypted state.
  encryption {
    key_provider "aws_kms" "state" {
      kms_key_id = var.state_kms_key_arn
      region     = var.region
      key_spec   = "AES_256"
    }

    method "aes_gcm" "state" {
      keys = key_provider.aws_kms.state
    }

    state {
      method   = method.aes_gcm.state
      enforced = true
    }

    plan {
      method   = method.aes_gcm.state
      enforced = true
    }
  }
}

provider "aws" {
  region = var.region

  default_tags {
    tags = {
      project    = "passband"
      managed-by = "opentofu"
      tofu-root  = "main"
    }
  }
}

# CloudFront only accepts ACM certificates from us-east-1.
provider "aws" {
  alias  = "us_east_1"
  region = "us-east-1"

  default_tags {
    tags = {
      project    = "passband"
      managed-by = "opentofu"
      tofu-root  = "main"
    }
  }
}

data "aws_caller_identity" "current" {}
data "aws_partition" "current" {}
data "aws_region" "current" {}
