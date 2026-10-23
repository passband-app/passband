# Image tags are immutable except `buildcache*`, which the cargo-chef registry
# cache rewrites on every build (release-daemon.yml pattern, PR #534).

locals {
  ecr_repos     = ["control", "relay", "bifrost", "prometheus", "grafana", "blackbox"]
  ecs_services  = ["control", "relay", "bifrost"]
  monitor_repos = ["prometheus", "grafana", "blackbox"]
}

resource "aws_ecr_repository" "this" {
  for_each = toset(local.ecr_repos)

  name                 = "passband-${each.key}"
  image_tag_mutability = "IMMUTABLE_WITH_EXCLUSION"

  image_tag_mutability_exclusion_filter {
    filter      = "buildcache*"
    filter_type = "WILDCARD"
  }

  # The monitor images also carry `:latest`, which a freshly bootstrapped box
  # pulls before its first deploy pins IMAGE_TAG (deploy-aws.yml).
  image_tag_mutability_exclusion_filter {
    filter      = "latest"
    filter_type = "WILDCARD"
  }

  image_scanning_configuration {
    scan_on_push = true
  }

  encryption_configuration {
    encryption_type = "KMS" # AWS-managed aws/ecr key
  }
}

resource "aws_ecr_lifecycle_policy" "this" {
  for_each = aws_ecr_repository.this

  repository = each.value.name
  policy = jsonencode({
    rules = [
      {
        # Highest priority, so the newest buildcache image is never counted
        # against (or expired by) the rule below. Older cache images lose
        # their tag when it is rewritten and fall to rule 2.
        rulePriority = 1
        description  = "Keep the current buildcache image"
        selection = {
          tagStatus      = "tagged"
          tagPatternList = ["buildcache*"]
          countType      = "imageCountMoreThan"
          countNumber    = 1
        }
        action = { type = "expire" }
      },
      {
        rulePriority = 2
        description  = "Keep the last 20 images"
        selection = {
          tagStatus   = "any"
          countType   = "imageCountMoreThan"
          countNumber = 20
        }
        action = { type = "expire" }
      },
    ]
  })
}
