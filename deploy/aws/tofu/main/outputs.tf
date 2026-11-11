# Everything the operator (DNS, runbook) and the other builders (CI, monitor
# bootstrap) need. `tofu output -json` for the lot.

output "acm_validation_records" {
  description = "CNAMEs to create at the DNS provider BEFORE the full apply can finish (both certificates)."
  value = {
    for o in concat(
      tolist(aws_acm_certificate.alb.domain_validation_options),
      tolist(aws_acm_certificate.site.domain_validation_options),
      ) : o.domain_name => {
      type  = o.resource_record_type
      name  = o.resource_record_name
      value = o.resource_record_value
    }
  }
}

output "service_dns_records" {
  description = "Cutover records. Apex note: a CNAME at passband.app needs the provider's CNAME flattening / ALIAS / ANAME."
  value = merge(
    {
      for h in local.alb_hosts : h => {
        type  = "CNAME"
        value = aws_lb.main.dns_name
      }
    },
    {
      for h in var.site_hosts : h => {
        type  = h == var.site_hosts[0] ? "ALIAS (CNAME flattening) - apex cannot hold a plain CNAME" : "CNAME"
        value = aws_cloudfront_distribution.site.domain_name
      }
    },
  )
}

output "alb_dns_name" {
  value = aws_lb.main.dns_name
}

output "nat_egress_ip" {
  description = "Every outbound connection from tasks and the monitor box comes from here."
  value       = aws_eip.nat.public_ip
}

output "ecr_repository_urls" {
  description = "passband-<name> => repository URL."
  value       = { for k, r in aws_ecr_repository.this : r.name => r.repository_url }
}

output "ecr_registry" {
  value = "${local.account_id}.dkr.ecr.${var.region}.amazonaws.com"
}

output "github_deploy_role_arn" {
  description = "For reference only: the workflow derives this from AWS_ACCOUNT_ID (arn:aws:iam::<id>:role/passband-github-deploy). There is no ARN repo variable."
  value       = aws_iam_role.github_deploy.arn
}

output "github_deploy_oidc_sub" {
  description = "The exact OIDC sub the deploy role trusts. Needs the repo's customized sub template."
  value       = local.github_deploy_sub
}

output "github_repo_variables" {
  description = "Repo variables deploy-aws.yml reads, with their values (none is a secret). AWS_ARM64_RUNNER is optional and not tofu's to know."
  value = {
    AWS_ACCOUNT_ID           = local.account_id
    AWS_SITE_BUCKET          = aws_s3_bucket.site.bucket
    AWS_SITE_DISTRIBUTION_ID = aws_cloudfront_distribution.site.id
    AWS_SITE_FUNCTION_NAME   = aws_cloudfront_function.site.name
    AWS_MONITOR_INSTANCE_ID  = aws_instance.monitor.id
  }
}

output "monitor_deploy_document" {
  description = "The only SSM document CI may run on the monitor box (one ImageTag parameter, a 40-hex SHA)."
  value       = aws_ssm_document.monitor_deploy.name
}

output "security_alerts_topic_arn" {
  description = "SNS topic the CloudTrail tripwires publish to. Confirm the email subscription after the first apply."
  value       = aws_sns_topic.security_alerts.arn
}

output "ecs_cluster_name" {
  value = aws_ecs_cluster.main.name
}

output "ecs_service_names" {
  value = { for k, s in aws_ecs_service.this : k => s.name }
}

output "ecs_task_families" {
  value = { for k, t in aws_ecs_task_definition.this : k => t.family }
}

output "monitor_instance_id" {
  value = aws_instance.monitor.id
}

output "site_bucket" {
  value = aws_s3_bucket.site.bucket
}

output "site_distribution_id" {
  value = aws_cloudfront_distribution.site.id
}

output "site_function_name" {
  value = aws_cloudfront_function.site.name
}

output "rds_endpoint" {
  description = "host:port. Use sslmode=verify-full with this hostname."
  value       = aws_db_instance.main.endpoint
}

output "rds_master_secret_arn" {
  description = "AWS-managed secret holding the RDS master password (for creating app roles only)."
  value       = aws_db_instance.main.master_user_secret[0].secret_arn
}

output "secret_names" {
  description = "Every container the operator must put a value into."
  value       = sort([for s in aws_secretsmanager_secret.this : s.name])
}

output "efs_file_system_id" {
  value = aws_efs_file_system.main.id
}
