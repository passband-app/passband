# app.passband.app: the web client (passband-web/app), a static single-page
# app in a private S3 bucket behind CloudFront (OAC). Same shape as the landing
# site (site.tf), with three differences that all come from what the app is:
#
#   - Its own bucket, distribution, certificate and deploy role. The app holds
#     users' mailbox credentials in the browser, so whatever can write to this
#     bucket can serve code to every signed-in user. Nothing else deploys here,
#     and the role that does can do nothing else.
#   - A strict Content-Security-Policy as a response HEADER. The build writes
#     the same policy as a <meta> (so tests run under it); the header adds what
#     a <meta> cannot carry: frame-ancestors.
#   - SPA routing in a tofu-owned CloudFront Function: an extensionless path
#     (/connect, /t/<id>) is the app and gets /index.html; a path with an
#     extension is a file, and a missing one stays a real 404.
#
# Per-object Cache-Control is S3 object metadata set at upload by the deploy
# job: index.html must revalidate (a deploy reaches everyone at once), and the
# content-hashed /assets/* are immutable. The cache policy honours it.

resource "aws_acm_certificate" "app" {
  provider = aws.us_east_1

  domain_name               = var.app_hosts[0]
  subject_alternative_names = slice(var.app_hosts, 1, length(var.app_hosts))
  validation_method         = "DNS"
  key_algorithm             = "RSA_2048"

  lifecycle {
    create_before_destroy = true
  }
}

resource "aws_acm_certificate_validation" "app" {
  provider = aws.us_east_1

  certificate_arn = aws_acm_certificate.app.arn
  validation_record_fqdns = [
    for o in aws_acm_certificate.app.domain_validation_options : o.resource_record_name
  ]
}

resource "aws_s3_bucket" "app" {
  bucket = "passband-app-${local.account_id}"
}

resource "aws_s3_bucket_public_access_block" "app" {
  bucket                  = aws_s3_bucket.app.id
  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_ownership_controls" "app" {
  bucket = aws_s3_bucket.app.id
  rule {
    object_ownership = "BucketOwnerEnforced"
  }
}

# SSE-S3, as for the site: public code, and OAC would need a key grant for KMS.
resource "aws_s3_bucket_server_side_encryption_configuration" "app" {
  bucket = aws_s3_bucket.app.id
  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm = "AES256"
    }
  }
}

# Versioned, so a bad deploy can be rolled back by restoring the previous
# index.html (the hashed assets it names are still in the bucket until pruned).
resource "aws_s3_bucket_versioning" "app" {
  bucket = aws_s3_bucket.app.id
  versioning_configuration {
    status = "Enabled"
  }
}

resource "aws_s3_bucket_lifecycle_configuration" "app" {
  bucket = aws_s3_bucket.app.id
  rule {
    id     = "expire-old-versions"
    status = "Enabled"
    filter {}
    noncurrent_version_expiration {
      noncurrent_days = 30
    }
  }
}

resource "aws_cloudfront_origin_access_control" "app" {
  name                              = "passband-app"
  origin_access_control_origin_type = "s3"
  signing_behavior                  = "always"
  signing_protocol                  = "sigv4"
}

# Unlike the site's function, this one never changes with a deploy, so tofu
# owns its code outright.
resource "aws_cloudfront_function" "app" {
  name    = "passband-app-router"
  runtime = "cloudfront-js-2.0"
  comment = "app.passband.app: extensionless paths are the SPA"
  publish = true
  code    = <<-EOT
    function handler(event) {
      var request = event.request;
      var uri = request.uri;
      var last = uri.substring(uri.lastIndexOf("/") + 1);
      // No extension in the last segment: an app route, not a file.
      if (last.indexOf(".") === -1) {
        request.uri = "/index.html";
      }
      return request;
    }
  EOT
}

# The app's policy. Keep it identical to csp() in passband-web/app/vite.config.ts
# plus frame-ancestors and upgrade-insecure-requests, which only a header can
# carry. Anything the app starts loading from somewhere new needs a line in
# BOTH places.
resource "aws_cloudfront_response_headers_policy" "app" {
  name    = "passband-app-security"
  comment = "app.passband.app: strict CSP with Trusted Types, and the security headers"

  security_headers_config {
    content_security_policy {
      content_security_policy = join("; ", [
        "default-src 'none'",
        "script-src 'self'",
        "style-src 'self' 'unsafe-inline'",
        "img-src 'self' data: blob:",
        "font-src 'self'",
        "connect-src https://*.passband.email",
        "frame-src 'self'",
        "frame-ancestors 'none'",
        "base-uri 'none'",
        "form-action 'none'",
        "object-src 'none'",
        "trusted-types passband-mail",
        "require-trusted-types-for 'script'",
        "upgrade-insecure-requests",
      ])
      override = true
    }
    strict_transport_security {
      access_control_max_age_sec = 31536000
      include_subdomains         = false
      preload                    = false
      override                   = true
    }
    content_type_options {
      override = true
    }
    frame_options {
      frame_option = "DENY"
      override     = true
    }
    referrer_policy {
      referrer_policy = "no-referrer"
      override        = true
    }
  }

  custom_headers_config {
    items {
      header   = "Permissions-Policy"
      value    = "camera=(), microphone=(), geolocation=(), payment=(), usb=(), serial=(), bluetooth=(), hid=(), display-capture=(), interest-cohort=()"
      override = true
    }
    items {
      header   = "Cross-Origin-Opener-Policy"
      value    = "same-origin"
      override = true
    }
  }
}

resource "aws_cloudfront_distribution" "app" {
  enabled             = true
  is_ipv6_enabled     = true
  http_version        = "http2and3"
  price_class         = "PriceClass_100"
  aliases             = var.app_hosts
  default_root_object = "index.html"
  comment             = "app.passband.app"

  origin {
    origin_id                = "app"
    domain_name              = aws_s3_bucket.app.bucket_regional_domain_name
    origin_access_control_id = aws_cloudfront_origin_access_control.app.id
  }

  default_cache_behavior {
    target_origin_id           = "app"
    viewer_protocol_policy     = "redirect-to-https"
    allowed_methods            = ["GET", "HEAD"]
    cached_methods             = ["GET", "HEAD"]
    compress                   = true
    cache_policy_id            = data.aws_cloudfront_cache_policy.optimized.id
    response_headers_policy_id = aws_cloudfront_response_headers_policy.app.id

    function_association {
      event_type   = "viewer-request"
      function_arn = aws_cloudfront_function.app.arn
    }
  }

  restrictions {
    geo_restriction {
      restriction_type = "none"
    }
  }

  viewer_certificate {
    acm_certificate_arn      = aws_acm_certificate_validation.app.certificate_arn
    ssl_support_method       = "sni-only"
    minimum_protocol_version = "TLSv1.2_2021"
  }
}

data "aws_iam_policy_document" "app_bucket" {
  statement {
    sid       = "CloudFrontReads"
    actions   = ["s3:GetObject"]
    resources = ["${aws_s3_bucket.app.arn}/*"]
    principals {
      type        = "Service"
      identifiers = ["cloudfront.amazonaws.com"]
    }
    condition {
      test     = "StringEquals"
      variable = "AWS:SourceArn"
      values   = [aws_cloudfront_distribution.app.arn]
    }
  }

  statement {
    sid     = "DenyInsecureTransport"
    effect  = "Deny"
    actions = ["s3:*"]
    resources = [
      aws_s3_bucket.app.arn,
      "${aws_s3_bucket.app.arn}/*",
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

resource "aws_s3_bucket_policy" "app" {
  bucket = aws_s3_bucket.app.id
  policy = data.aws_iam_policy_document.app_bucket.json
}

# --- The app's own deploy role ----------------------------------------------------
#
# Same trust as passband-github-deploy (iam.tf): GitHub OIDC, this repo, the
# aws-production environment, deploy-aws.yml as it is on main. What differs is
# what it can do: write THIS bucket and invalidate THIS distribution, nothing
# else. The app's deploy job assumes this role and no other, and the main
# deploy role gets no access to the app bucket.

resource "aws_iam_role" "github_deploy_app" {
  name                 = "passband-github-deploy-app"
  assume_role_policy   = data.aws_iam_policy_document.github_assume.json
  max_session_duration = 3600
}

data "aws_iam_policy_document" "github_deploy_app" {
  statement {
    sid       = "AppList"
    actions   = ["s3:ListBucket"]
    resources = [aws_s3_bucket.app.arn]
  }

  statement {
    sid       = "AppObjects"
    actions   = ["s3:GetObject", "s3:PutObject", "s3:DeleteObject"]
    resources = ["${aws_s3_bucket.app.arn}/*"]
  }

  statement {
    sid       = "AppInvalidate"
    actions   = ["cloudfront:CreateInvalidation", "cloudfront:GetInvalidation"]
    resources = [aws_cloudfront_distribution.app.arn]
  }
}

resource "aws_iam_role_policy" "github_deploy_app" {
  name   = "deploy-app"
  role   = aws_iam_role.github_deploy_app.id
  policy = data.aws_iam_policy_document.github_deploy_app.json
}
