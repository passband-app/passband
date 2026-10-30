# One public ALB, host-routed. 80 redirects to 443; 443 terminates TLS with one
# ACM certificate carrying every hostname as a SAN; an unknown Host gets a 404.

locals {
  alb_hosts = [
    var.control_host,
    var.relay_host,
    var.bifrost_host,
    var.grafana_host,
    var.metrics_host,
  ]
}

resource "aws_acm_certificate" "alb" {
  domain_name               = var.control_host
  subject_alternative_names = slice(local.alb_hosts, 1, length(local.alb_hosts))
  validation_method         = "DNS"
  key_algorithm             = "RSA_2048"

  lifecycle {
    create_before_destroy = true
  }
}

# Waits until the operator has created the validation CNAMEs (README.md, step
# 3). Nothing that needs the certificate is created before this completes.
resource "aws_acm_certificate_validation" "alb" {
  certificate_arn = aws_acm_certificate.alb.arn
  validation_record_fqdns = [
    for o in aws_acm_certificate.alb.domain_validation_options : o.resource_record_name
  ]
}

resource "aws_s3_bucket" "alb_logs" {
  count  = var.alb_access_logs ? 1 : 0
  bucket = "passband-alb-logs-${data.aws_caller_identity.current.account_id}"
}

resource "aws_s3_bucket_public_access_block" "alb_logs" {
  count                   = var.alb_access_logs ? 1 : 0
  bucket                  = aws_s3_bucket.alb_logs[0].id
  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_server_side_encryption_configuration" "alb_logs" {
  count  = var.alb_access_logs ? 1 : 0
  bucket = aws_s3_bucket.alb_logs[0].id
  rule {
    # ALB log delivery supports SSE-S3 only.
    apply_server_side_encryption_by_default {
      sse_algorithm = "AES256"
    }
  }
}

resource "aws_s3_bucket_lifecycle_configuration" "alb_logs" {
  count  = var.alb_access_logs ? 1 : 0
  bucket = aws_s3_bucket.alb_logs[0].id
  rule {
    id     = "expire"
    status = "Enabled"
    filter {}
    expiration {
      days = 30
    }
  }
}

data "aws_iam_policy_document" "alb_logs" {
  count = var.alb_access_logs ? 1 : 0
  statement {
    actions   = ["s3:PutObject"]
    resources = ["${aws_s3_bucket.alb_logs[0].arn}/alb/AWSLogs/${data.aws_caller_identity.current.account_id}/*"]
    principals {
      type        = "Service"
      identifiers = ["logdelivery.elasticloadbalancing.amazonaws.com"]
    }
  }
}

resource "aws_s3_bucket_policy" "alb_logs" {
  count  = var.alb_access_logs ? 1 : 0
  bucket = aws_s3_bucket.alb_logs[0].id
  policy = data.aws_iam_policy_document.alb_logs[0].json
}

resource "aws_lb" "main" {
  name                       = "passband"
  load_balancer_type         = "application"
  internal                   = false
  subnets                    = aws_subnet.public[*].id
  security_groups            = [aws_security_group.alb.id]
  drop_invalid_header_fields = true
  enable_deletion_protection = true
  # Relay and control hold no long-poll connections; 60s is the default and fine.
  idle_timeout = 60

  dynamic "access_logs" {
    for_each = var.alb_access_logs ? [1] : []
    content {
      bucket  = aws_s3_bucket.alb_logs[0].id
      prefix  = "alb"
      enabled = true
    }
  }
}

resource "aws_lb_listener" "http" {
  load_balancer_arn = aws_lb.main.arn
  port              = 80
  protocol          = "HTTP"

  default_action {
    type = "redirect"
    redirect {
      port        = "443"
      protocol    = "HTTPS"
      status_code = "HTTP_301"
    }
  }
}

resource "aws_lb_listener" "https" {
  load_balancer_arn = aws_lb.main.arn
  port              = 443
  protocol          = "HTTPS"
  # TLS 1.3 + TLS 1.2 with forward-secret AEAD ciphers only.
  ssl_policy      = "ELBSecurityPolicy-TLS13-1-2-Res-2021-06"
  certificate_arn = aws_acm_certificate_validation.alb.certificate_arn

  default_action {
    type = "fixed-response"
    fixed_response {
      content_type = "text/plain"
      message_body = "not found"
      status_code  = "404"
    }
  }
}

locals {
  target_groups = {
    control = {
      host        = var.control_host
      port        = local.ports.control
      target_type = "ip"
      path        = "/healthz"
      matcher     = "200"
    }
    relay = {
      host        = var.relay_host
      port        = local.ports.relay
      target_type = "ip"
      path        = "/healthz"
      matcher     = "200"
    }
    bifrost = {
      host        = var.bifrost_host
      port        = local.ports.bifrost
      target_type = "ip"
      path        = "/"
      matcher     = "200-499" # /health is behind Bifrost's session auth
    }
    grafana = {
      host        = var.grafana_host
      port        = local.ports.grafana
      target_type = "instance"
      path        = "/api/health"
      matcher     = "200"
    }
    metrics = {
      host        = var.metrics_host
      port        = local.ports.metrics
      target_type = "instance"
      path        = "/-/healthy"
      matcher     = "200,401" # basic auth on every route
    }
  }
}

resource "aws_lb_target_group" "this" {
  for_each = local.target_groups

  name                 = "passband-${each.key}"
  port                 = each.value.port
  protocol             = "HTTP"
  target_type          = each.value.target_type
  vpc_id               = aws_vpc.main.id
  deregistration_delay = 30

  health_check {
    path                = each.value.path
    matcher             = each.value.matcher
    interval            = 15
    timeout             = 5
    healthy_threshold   = 2
    unhealthy_threshold = 3
  }
}

# Open hosts: anyone may reach these (control's signup, relay's opens).
resource "aws_lb_listener_rule" "host" {
  for_each = { for k, v in local.target_groups : k => v if !contains(keys(local.source_restricted), k) }

  listener_arn = aws_lb_listener.https.arn
  priority     = index(keys(local.target_groups), each.key) + 10

  condition {
    host_header {
      values = [each.value.host]
    }
  }

  action {
    type             = "forward"
    target_group_arn = aws_lb_target_group.this[each.key].arn
  }
}

# Source-restricted hosts. A request for one of these hosts from any other
# address matches no rule and gets the listener's default 404, and that
# includes a forged `Host:` sent straight at the ALB's DNS name, so DNS
# cutover is not the gate.
#   bifrost: the LLM proxy AND its admin plane (UI, /api/*, HTTP Basic). Its
#            callers are control and blackbox (both egress from the NAT EIP)
#            and the tenants on carrier. A fresh Bifrost has no admin auth until
#            first-run config, so this rule is what keeps that window closed;
#            operator_cidrs opens it to the operator for that step only.
#   metrics: Prometheus remote-write from carrier's prometheus-agent only.
#   grafana: tailnet-only. Tailnet devices reach it through the monitor box's
#            Tailscale app connector, which egresses from the NAT EIP.
#
# ALB limits: one source-ip condition per rule, at most 3 values per
# condition, at most 5 match evaluations per rule (docs: "Condition types
# for listener rules"). So each allow-list is cut into chunks of 3, one rule
# per chunk, each rule host (1) + source-ip (<= 3) = <= 4 evaluations.
locals {
  source_restricted = {
    bifrost = concat(["${aws_eip.nat.public_ip}/32"], var.carrier_egress_cidrs, var.operator_cidrs)
    metrics = var.carrier_egress_cidrs
    grafana = concat(["${aws_eip.nat.public_ip}/32"], var.operator_cidrs)
  }

  restricted_rules = merge([
    for svc, base in { bifrost = 100, metrics = 200, grafana = 300 } : {
      for i in range(ceil(length(local.source_restricted[svc]) / 3)) : "${svc}-${i}" => {
        service  = svc
        priority = base + i
        chunk    = i
      }
    }
  ]...)
}

resource "aws_lb_listener_rule" "restricted" {
  # Keys are built from list LENGTHS (known at plan time even though the NAT
  # EIP's address is not), never from the CIDR values themselves.
  for_each = local.restricted_rules

  listener_arn = aws_lb_listener.https.arn
  priority     = each.value.priority

  condition {
    host_header {
      values = [local.target_groups[each.value.service].host]
    }
  }

  condition {
    source_ip {
      values = slice(
        local.source_restricted[each.value.service],
        each.value.chunk * 3,
        min(length(local.source_restricted[each.value.service]), each.value.chunk * 3 + 3),
      )
    }
  }

  action {
    type             = "forward"
    target_group_arn = aws_lb_target_group.this[each.value.service].arn
  }
}

resource "aws_lb_target_group_attachment" "monitor" {
  for_each = toset(["grafana", "metrics"])

  target_group_arn = aws_lb_target_group.this[each.key].arn
  target_id        = aws_instance.monitor.id
  port             = local.target_groups[each.key].port
}
