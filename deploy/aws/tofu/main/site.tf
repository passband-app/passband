# passband.app: private S3 bucket behind CloudFront (OAC). Routing that
# passband-site/server.ts does today is a CloudFront Function on viewer
# request, code in deploy/aws/site/function.js (owned by the site builder).
#
# The function bakes the latest app version from appcast.xml at BUILD time, so
# its code changes with every Mac release. Tofu creates it from function.js and
# then ignores `code`: the site deploy job owns it from there (iam.tf grants the
# deploy role Update/Publish on this one function).
#
# Per-object headers (.woff2 Cache-Control max-age=604800, appcast.xml
# Content-Type application/xml + short Cache-Control) are S3 object metadata
# set at upload; the cache policy below honours the origin's Cache-Control.

locals {
  site_function_path = "${path.module}/../../site/function.js"
  # Only used when function.js is missing, so `tofu validate` works before the
  # site builder lands it. The precondition below stops a real apply.
  site_function_placeholder = <<-EOT
    function handler(event) { return event.request; }
  EOT
}

resource "aws_acm_certificate" "site" {
  provider = aws.us_east_1

  domain_name               = var.site_hosts[0]
  subject_alternative_names = slice(var.site_hosts, 1, length(var.site_hosts))
  validation_method         = "DNS"
  key_algorithm             = "RSA_2048"

  lifecycle {
    create_before_destroy = true
  }
}

resource "aws_acm_certificate_validation" "site" {
  provider = aws.us_east_1

  certificate_arn = aws_acm_certificate.site.arn
  validation_record_fqdns = [
    for o in aws_acm_certificate.site.domain_validation_options : o.resource_record_name
  ]
}

resource "aws_s3_bucket" "site" {
  bucket = "passband-site-${local.account_id}"
}

resource "aws_s3_bucket_public_access_block" "site" {
  bucket                  = aws_s3_bucket.site.id
  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_ownership_controls" "site" {
  bucket = aws_s3_bucket.site.id
  rule {
    object_ownership = "BucketOwnerEnforced"
  }
}

# SSE-S3: public content, and OAC reads SSE-KMS objects only with a key grant
# to CloudFront, which buys nothing here.
resource "aws_s3_bucket_server_side_encryption_configuration" "site" {
  bucket = aws_s3_bucket.site.id
  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm = "AES256"
    }
  }
}

resource "aws_s3_bucket_versioning" "site" {
  bucket = aws_s3_bucket.site.id
  versioning_configuration {
    status = "Enabled"
  }
}

resource "aws_s3_bucket_lifecycle_configuration" "site" {
  bucket = aws_s3_bucket.site.id
  rule {
    id     = "expire-old-versions"
    status = "Enabled"
    filter {}
    noncurrent_version_expiration {
      noncurrent_days = 30
    }
  }
}

resource "aws_cloudfront_origin_access_control" "site" {
  name                              = "passband-site"
  origin_access_control_origin_type = "s3"
  signing_behavior                  = "always"
  signing_protocol                  = "sigv4"
}

resource "aws_cloudfront_function" "site" {
  name    = "passband-site-router"
  runtime = "cloudfront-js-2.0"
  comment = "passband-site/server.ts routing; code owned by the site deploy job after creation"
  publish = true
  code    = fileexists(local.site_function_path) ? file(local.site_function_path) : local.site_function_placeholder

  lifecycle {
    ignore_changes = [code, comment]

    precondition {
      condition     = fileexists(local.site_function_path)
      error_message = "deploy/aws/site/function.js does not exist yet."
    }
  }
}

data "aws_cloudfront_cache_policy" "optimized" {
  name = "Managed-CachingOptimized"
}

# The managed SecurityHeadersPolicy's headers, plus a Content-Security-Policy,
# which the managed policy does not set and a DAST scan reports as Medium. The site loads nothing but
# its own files; the one cross-origin call is the waitlist form's fetch to
# control. Inline styles stay allowed because React's `style` props and the
# static pages' <style> blocks use them. Checked with every page loaded in
# Chromium under this exact policy, waitlist submit included: no violations.
# A new third-party script, font or API call on the site needs a line here.
resource "aws_cloudfront_response_headers_policy" "security" {
  name    = "passband-site-security"
  comment = "passband.app: managed security headers plus CSP"

  security_headers_config {
    content_security_policy {
      content_security_policy = join("; ", [
        "default-src 'self'",
        "script-src 'self'",
        "style-src 'self' 'unsafe-inline'",
        "img-src 'self' data:",
        "font-src 'self'",
        "connect-src 'self' https://signup.passband.app",
        "object-src 'none'",
        "base-uri 'none'",
        "form-action 'self'",
        "frame-ancestors 'none'",
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
      referrer_policy = "strict-origin-when-cross-origin"
      override        = true
    }
  }
}

resource "aws_cloudfront_distribution" "site" {
  enabled             = true
  is_ipv6_enabled     = true
  http_version        = "http2and3"
  price_class         = "PriceClass_100"
  aliases             = var.site_hosts
  default_root_object = "index.html"
  comment             = "passband.app"

  origin {
    origin_id                = "site"
    domain_name              = aws_s3_bucket.site.bucket_regional_domain_name
    origin_access_control_id = aws_cloudfront_origin_access_control.site.id
  }

  default_cache_behavior {
    target_origin_id           = "site"
    viewer_protocol_policy     = "redirect-to-https"
    allowed_methods            = ["GET", "HEAD"]
    cached_methods             = ["GET", "HEAD"]
    compress                   = true
    cache_policy_id            = data.aws_cloudfront_cache_policy.optimized.id
    response_headers_policy_id = aws_cloudfront_response_headers_policy.security.id

    function_association {
      event_type   = "viewer-request"
      function_arn = aws_cloudfront_function.site.arn
    }
  }

  restrictions {
    geo_restriction {
      restriction_type = "none"
    }
  }

  viewer_certificate {
    acm_certificate_arn      = aws_acm_certificate_validation.site.certificate_arn
    ssl_support_method       = "sni-only"
    minimum_protocol_version = "TLSv1.2_2021"
  }
}

data "aws_iam_policy_document" "site_bucket" {
  statement {
    sid       = "CloudFrontReads"
    actions   = ["s3:GetObject"]
    resources = ["${aws_s3_bucket.site.arn}/*"]
    principals {
      type        = "Service"
      identifiers = ["cloudfront.amazonaws.com"]
    }
    condition {
      test     = "StringEquals"
      variable = "AWS:SourceArn"
      values   = [aws_cloudfront_distribution.site.arn]
    }
  }

  statement {
    sid     = "DenyInsecureTransport"
    effect  = "Deny"
    actions = ["s3:*"]
    resources = [
      aws_s3_bucket.site.arn,
      "${aws_s3_bucket.site.arn}/*",
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

resource "aws_s3_bucket_policy" "site" {
  bucket = aws_s3_bucket.site.id
  policy = data.aws_iam_policy_document.site_bucket.json
}
