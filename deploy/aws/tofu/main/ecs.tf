# Fargate ARM64, private subnets, no public IPs, ECS Exec on control only.
#
# CI owns the image. Tofu registers each family's FIRST revision with a
# placeholder tag and then ignores the service's task_definition, so a later
# apply never rolls back a deploy. The deploy job must therefore build each new
# revision from the family's LATEST revision (`aws ecs describe-task-definition
# --task-definition <family>`), swap the image of the container of the same
# name, register, and update the service. That also means an env change made
# here reaches the running service on the next CI deploy of that service (or a
# workflow_dispatch), not on `tofu apply`.

resource "aws_cloudwatch_log_group" "service" {
  for_each = toset(local.ecs_services)

  name              = "/passband/${each.key}"
  retention_in_days = var.log_retention_days
  kms_key_id        = aws_kms_key.logs.arn
}

resource "aws_cloudwatch_log_group" "exec" {
  name              = "/passband/ecs-exec"
  retention_in_days = var.log_retention_days
  kms_key_id        = aws_kms_key.logs.arn
}

resource "aws_ecs_cluster" "main" {
  name = "passband"

  setting {
    name  = "containerInsights"
    value = "disabled" # ~$3+/month for 3 tasks; the monitor box is the dashboard
  }

  configuration {
    execute_command_configuration {
      kms_key_id = aws_kms_key.logs.arn
      logging    = "OVERRIDE"

      log_configuration {
        cloud_watch_log_group_name     = aws_cloudwatch_log_group.exec.name
        cloud_watch_encryption_enabled = true
      }
    }
  }
}

resource "aws_ecs_cluster_capacity_providers" "main" {
  cluster_name       = aws_ecs_cluster.main.name
  capacity_providers = ["FARGATE"]

  default_capacity_provider_strategy {
    capacity_provider = "FARGATE"
    weight            = 1
  }
}

locals {
  # null entries are dropped: the binary's own default applies.
  plain_env = {
    control = {
      SQUELCH_CONTROL_PUBLIC_URL           = var.control_public_url
      SQUELCH_CONTROL_BASE_DOMAIN          = var.control_base_domain
      SQUELCH_CONTROL_CLIENT_ID            = var.control_client_id
      SQUELCH_CONTROL_WARDEN_URL           = var.control_warden_url
      SQUELCH_CONTROL_BIFROST_URL          = var.control_bifrost_url
      SQUELCH_CONTROL_TRUSTED_PROXY_HOPS   = tostring(var.control_trusted_proxy_hops)
      SQUELCH_CONTROL_INVITE_FROM          = var.control_invite_from
      SQUELCH_CONTROL_WAITLIST_ORIGIN      = var.control_waitlist_origin
      SQUELCH_CONTROL_LLM_BUDGET_USD       = var.control_llm_budget_usd
      SQUELCH_CONTROL_LLM_MODELS           = var.control_llm_models
      SQUELCH_CONTROL_ASSISTANT_BUDGET_USD = var.control_assistant_budget_usd
      SQUELCH_CONTROL_ASSISTANT_MODELS     = var.control_assistant_models
      SQUELCH_CONTROL_LOG                  = var.control_log
    }
    relay = {
      SQUELCH_RELAY_APNS_KEY_ID        = var.relay_apns_key_id
      SQUELCH_RELAY_APNS_TEAM_ID       = var.relay_apns_team_id
      SQUELCH_RELAY_APNS_TOPICS        = var.relay_apns_topics
      SQUELCH_RELAY_APNS_ENV           = var.relay_apns_env
      SQUELCH_RELAY_DB_PATH            = "/data/opens.sqlite3"
      SQUELCH_RELAY_TRUSTED_PROXY_HOPS = tostring(var.relay_trusted_proxy_hops)
      SQUELCH_RELAY_LOG                = var.relay_log
    }
    bifrost = {
      APP_HOST = "0.0.0.0"
      APP_PORT = tostring(local.ports.bifrost)
    }
  }

  # Where each service keeps state on EFS (absent = stateless on Fargate).
  efs_mounts = merge(
    { relay = "/data" },
    var.bifrost_store == "efs" ? { bifrost = "/app/data" } : {},
  )

  # Single-writer services: never two tasks at once. min 0 / max 100 means a
  # deploy stops the old task before starting the new one (brief downtime, no
  # two SQLite writers on one EFS file). control is stateless and rolls.
  single_writer = {
    control = false
    relay   = true
    bifrost = var.bifrost_store == "efs"
  }
}

resource "aws_ecs_task_definition" "this" {
  for_each = toset(local.ecs_services)

  family                   = each.key
  requires_compatibilities = ["FARGATE"]
  network_mode             = "awsvpc"
  cpu                      = var.task_sizes[each.key].cpu
  memory                   = var.task_sizes[each.key].memory
  execution_role_arn       = aws_iam_role.execution[each.key].arn
  task_role_arn            = aws_iam_role.task[each.key].arn

  runtime_platform {
    operating_system_family = "LINUX"
    cpu_architecture        = "ARM64"
  }

  dynamic "volume" {
    for_each = contains(keys(local.efs_mounts), each.key) ? [1] : []
    content {
      name = "data"
      efs_volume_configuration {
        file_system_id     = aws_efs_file_system.main.id
        transit_encryption = "ENABLED"
        authorization_config {
          access_point_id = aws_efs_access_point.this[each.key].id
          iam             = "ENABLED"
        }
      }
    }
  }

  container_definitions = jsonencode([
    {
      name      = each.key
      image     = "${aws_ecr_repository.this[each.key].repository_url}:placeholder"
      essential = true

      portMappings = [{
        containerPort = local.ports[each.key]
        protocol      = "tcp"
      }]

      environment = [
        for k, v in local.plain_env[each.key] : { name = k, value = v } if v != null
      ]

      secrets = [
        for k, v in local.secrets : {
          name      = v.env
          valueFrom = aws_secretsmanager_secret.this[k].arn
        } if v.service == each.key
      ]

      mountPoints = contains(keys(local.efs_mounts), each.key) ? [{
        sourceVolume  = "data"
        containerPath = local.efs_mounts[each.key]
        readOnly      = false
      }] : []

      # ECS Exec needs a writable root filesystem (its agent is bind-mounted
      # and writes there), so readonlyRootFilesystem stays false.
      readonlyRootFilesystem = false
      # The relay and control entrypoints start as root only to chown the data
      # dir and then setpriv down; no extra capabilities are needed.
      linuxParameters = {
        initProcessEnabled = true
        capabilities = {
          drop = ["NET_RAW"]
        }
      }

      logConfiguration = {
        logDriver = "awslogs"
        options = {
          awslogs-group         = aws_cloudwatch_log_group.service[each.key].name
          awslogs-region        = var.region
          awslogs-stream-prefix = each.key
        }
      }
    }
  ])
}

resource "aws_ecs_service" "this" {
  for_each = toset(local.ecs_services)

  name            = each.key
  cluster         = aws_ecs_cluster.main.id
  task_definition = aws_ecs_task_definition.this[each.key].arn
  desired_count   = var.services_running ? 1 : 0
  launch_type     = "FARGATE"

  # DESIGN.md: ECS Exec on control only (the operator's CLI lives there).
  # Exec into bifrost would be a shell holding ANTHROPIC_API_KEY, into relay
  # one holding the APNs key; neither needs it.
  enable_execute_command            = each.key == "control"
  enable_ecs_managed_tags           = true
  propagate_tags                    = "SERVICE"
  health_check_grace_period_seconds = 60
  # When the services are meant to run, an apply (the services_running flip
  # above all) does not return until each one is steady, so the first real
  # start fails loudly instead of going green at 0 tasks.
  wait_for_steady_state = var.services_running

  deployment_minimum_healthy_percent = local.single_writer[each.key] ? 0 : 100
  deployment_maximum_percent         = local.single_writer[each.key] ? 100 : 200

  deployment_circuit_breaker {
    enable   = true
    rollback = true
  }

  network_configuration {
    subnets          = aws_subnet.private[*].id
    security_groups  = [aws_security_group.task[each.key].id]
    assign_public_ip = false
  }

  load_balancer {
    target_group_arn = aws_lb_target_group.this[each.key].arn
    container_name   = each.key
    container_port   = local.ports[each.key]
  }

  lifecycle {
    ignore_changes = [task_definition]
  }

  depends_on = [aws_lb_listener_rule.host, aws_lb_listener_rule.restricted]
}
