# Chained security groups. Every ingress names a source group, never a CIDR,
# except the ALB's public 80/443 and the NAT's VPC-wide ingress.
#
#   internet ─80/443→ alb ─8852→ control ─5432→ rds
#                         ─8850→ relay   ─2049→ efs
#                         ─8080→ bifrost ─2049→ efs (efs store) | ─5432→ rds (postgres store)
#                         ─3000/9090→ monitor
#
# Task and monitor egress is 443 to anywhere (Google, Anthropic, APNs, Resend,
# WorkOS, the warden, Slack, ECR, Secrets Manager, SSM all speak 443) plus the
# one internal port each needs. DNS to the VPC resolver is not subject to SGs.

locals {
  ports = {
    control = 8852 # squelch-control's entrypoint default (0.0.0.0:${PORT:-8852})
    relay   = 8850 # squelch-relay's entrypoint default (0.0.0.0:${PORT:-8850})
    bifrost = 8080 # APP_PORT
    grafana = 3000
    metrics = 9090
  }
}

resource "aws_security_group" "alb" {
  name        = "passband-alb"
  description = "Public ALB"
  vpc_id      = aws_vpc.main.id
}

resource "aws_vpc_security_group_ingress_rule" "alb_https" {
  security_group_id = aws_security_group.alb.id
  description       = "HTTPS from the internet"
  cidr_ipv4         = "0.0.0.0/0"
  ip_protocol       = "tcp"
  from_port         = 443
  to_port           = 443
}

resource "aws_vpc_security_group_ingress_rule" "alb_http" {
  security_group_id = aws_security_group.alb.id
  description       = "HTTP from the internet, redirected to 443 and nothing else"
  cidr_ipv4         = "0.0.0.0/0"
  ip_protocol       = "tcp"
  from_port         = 80
  to_port           = 80
}

resource "aws_security_group" "task" {
  for_each = toset(["control", "relay", "bifrost"])

  name        = "passband-${each.key}"
  description = "ECS task ${each.key}"
  vpc_id      = aws_vpc.main.id
}

resource "aws_security_group" "monitor" {
  name        = "passband-monitor"
  description = "Monitor box: grafana and the prometheus receiver, behind the ALB only"
  vpc_id      = aws_vpc.main.id
}

resource "aws_security_group" "rds" {
  name        = "passband-rds"
  description = "Postgres"
  vpc_id      = aws_vpc.main.id
}

resource "aws_security_group" "efs" {
  name        = "passband-efs"
  description = "EFS mount targets"
  vpc_id      = aws_vpc.main.id
}

locals {
  # ALB -> target: (target SG, port)
  alb_targets = {
    control = { sg = aws_security_group.task["control"].id, port = local.ports.control }
    relay   = { sg = aws_security_group.task["relay"].id, port = local.ports.relay }
    bifrost = { sg = aws_security_group.task["bifrost"].id, port = local.ports.bifrost }
    grafana = { sg = aws_security_group.monitor.id, port = local.ports.grafana }
    metrics = { sg = aws_security_group.monitor.id, port = local.ports.metrics }
  }

  efs_clients = var.bifrost_store == "efs" ? ["relay", "bifrost"] : ["relay"]
  rds_clients = var.bifrost_store == "postgres" ? ["control", "bifrost"] : ["control"]
}

resource "aws_vpc_security_group_egress_rule" "alb_to_target" {
  for_each = local.alb_targets

  security_group_id            = aws_security_group.alb.id
  description                  = "ALB to ${each.key}"
  referenced_security_group_id = each.value.sg
  ip_protocol                  = "tcp"
  from_port                    = each.value.port
  to_port                      = each.value.port
}

resource "aws_vpc_security_group_ingress_rule" "target_from_alb" {
  for_each = local.alb_targets

  security_group_id            = each.value.sg
  description                  = "${each.key} from the ALB"
  referenced_security_group_id = aws_security_group.alb.id
  ip_protocol                  = "tcp"
  from_port                    = each.value.port
  to_port                      = each.value.port
}

resource "aws_vpc_security_group_egress_rule" "task_https" {
  for_each = aws_security_group.task

  security_group_id = each.value.id
  description       = "HTTPS out via the NAT"
  cidr_ipv4         = "0.0.0.0/0"
  ip_protocol       = "tcp"
  from_port         = 443
  to_port           = 443
}

resource "aws_vpc_security_group_egress_rule" "monitor_https" {
  security_group_id = aws_security_group.monitor.id
  description       = "HTTPS out via the NAT (ECR, SSM, Secrets Manager, Slack, blackbox probes)"
  cidr_ipv4         = "0.0.0.0/0"
  ip_protocol       = "tcp"
  from_port         = 443
  to_port           = 443
}

# Tailscale: STUN and WireGuard out, so tailnet clients get direct paths
# instead of DERP relays over 443. Outbound only; nothing listens publicly.
resource "aws_vpc_security_group_egress_rule" "monitor_tailscale" {
  for_each          = { stun = 3478, wireguard = 41641 }
  security_group_id = aws_security_group.monitor.id
  description       = "Tailscale ${each.key} out"
  cidr_ipv4         = "0.0.0.0/0"
  ip_protocol       = "udp"
  from_port         = each.value
  to_port           = each.value
}

resource "aws_vpc_security_group_egress_rule" "monitor_http" {
  security_group_id = aws_security_group.monitor.id
  description       = "HTTP out (blackbox http to https redirect probes)"
  cidr_ipv4         = "0.0.0.0/0"
  ip_protocol       = "tcp"
  from_port         = 80
  to_port           = 80
}

resource "aws_vpc_security_group_egress_rule" "to_rds" {
  for_each = toset(local.rds_clients)

  security_group_id            = aws_security_group.task[each.key].id
  description                  = "${each.key} to Postgres"
  referenced_security_group_id = aws_security_group.rds.id
  ip_protocol                  = "tcp"
  from_port                    = 5432
  to_port                      = 5432
}

resource "aws_vpc_security_group_ingress_rule" "rds_from" {
  for_each = toset(local.rds_clients)

  security_group_id            = aws_security_group.rds.id
  description                  = "Postgres from ${each.key}"
  referenced_security_group_id = aws_security_group.task[each.key].id
  ip_protocol                  = "tcp"
  from_port                    = 5432
  to_port                      = 5432
}

resource "aws_vpc_security_group_egress_rule" "monitor_to_rds" {
  count = var.operator_db_access ? 1 : 0

  security_group_id            = aws_security_group.monitor.id
  description                  = "TEMPORARY operator psql from the monitor box"
  referenced_security_group_id = aws_security_group.rds.id
  ip_protocol                  = "tcp"
  from_port                    = 5432
  to_port                      = 5432
}

resource "aws_vpc_security_group_ingress_rule" "rds_from_monitor" {
  count = var.operator_db_access ? 1 : 0

  security_group_id            = aws_security_group.rds.id
  description                  = "TEMPORARY operator psql from the monitor box"
  referenced_security_group_id = aws_security_group.monitor.id
  ip_protocol                  = "tcp"
  from_port                    = 5432
  to_port                      = 5432
}

resource "aws_vpc_security_group_egress_rule" "to_efs" {
  for_each = toset(local.efs_clients)

  security_group_id            = aws_security_group.task[each.key].id
  description                  = "${each.key} to EFS"
  referenced_security_group_id = aws_security_group.efs.id
  ip_protocol                  = "tcp"
  from_port                    = 2049
  to_port                      = 2049
}

resource "aws_vpc_security_group_ingress_rule" "efs_from" {
  for_each = toset(local.efs_clients)

  security_group_id            = aws_security_group.efs.id
  description                  = "NFS from ${each.key}"
  referenced_security_group_id = aws_security_group.task[each.key].id
  ip_protocol                  = "tcp"
  from_port                    = 2049
  to_port                      = 2049
}
