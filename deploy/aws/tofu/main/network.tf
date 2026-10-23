# Two AZs, a public and a private subnet in each. Tasks, RDS, EFS and the
# monitor box live in the private subnets; only the ALB and the NAT instance
# sit in the public ones.
#
# NAT: a HAND-ROLLED fck-nat (https://github.com/AndrewGuenther/fck-nat), not
# the community module, so there is no third-party tofu code in the apply path.
# It mirrors what RaJiska/terraform-aws-fck-nat does:
#   - a static ENI (source/dest check OFF) that both private route tables
#     target, so the routes survive the instance being replaced;
#   - an ASG of exactly one t4g.nano from the fck-nat AL2023 arm64 AMI; on boot
#     the AMI's fck-nat service reads /etc/fck-nat.conf, attaches the static
#     ENI as eth1, and associates the Elastic IP with its primary interface;
#   - the instance role may attach that ENI and move that EIP, nothing else.
# One NAT in AZ a serves both AZs (no NAT HA: an AZ-a outage takes egress with
# it, which this single-task fleet cannot ride out anyway). A replacement
# instance takes ~1-2 minutes, during which tasks have no egress.

data "aws_availability_zones" "available" {
  state = "available"
}

locals {
  azs = slice(data.aws_availability_zones.available.names, 0, 2)
}

resource "aws_vpc" "main" {
  cidr_block           = var.vpc_cidr
  enable_dns_support   = true
  enable_dns_hostnames = true

  tags = { Name = "passband" }
}

# Lock the default security group down to nothing; every resource here uses
# its own.
resource "aws_default_security_group" "default" {
  vpc_id = aws_vpc.main.id
}

resource "aws_internet_gateway" "main" {
  vpc_id = aws_vpc.main.id
  tags   = { Name = "passband" }
}

resource "aws_subnet" "public" {
  count = 2

  vpc_id            = aws_vpc.main.id
  availability_zone = local.azs[count.index]
  cidr_block        = cidrsubnet(var.vpc_cidr, 8, count.index)
  # The ALB gets its addresses from the load balancer itself and the NAT from
  # its EIP; nothing here should be handed a public IP implicitly.
  map_public_ip_on_launch = false

  tags = { Name = "passband-public-${local.azs[count.index]}" }
}

resource "aws_subnet" "private" {
  count = 2

  vpc_id            = aws_vpc.main.id
  availability_zone = local.azs[count.index]
  cidr_block        = cidrsubnet(var.vpc_cidr, 8, count.index + 10)

  tags = { Name = "passband-private-${local.azs[count.index]}" }
}

resource "aws_route_table" "public" {
  vpc_id = aws_vpc.main.id
  tags   = { Name = "passband-public" }
}

resource "aws_route" "public_internet" {
  route_table_id         = aws_route_table.public.id
  destination_cidr_block = "0.0.0.0/0"
  gateway_id             = aws_internet_gateway.main.id
}

resource "aws_route_table_association" "public" {
  count          = 2
  subnet_id      = aws_subnet.public[count.index].id
  route_table_id = aws_route_table.public.id
}

resource "aws_route_table" "private" {
  count  = 2
  vpc_id = aws_vpc.main.id
  tags   = { Name = "passband-private-${local.azs[count.index]}" }
}

resource "aws_route" "private_nat" {
  count                  = 2
  route_table_id         = aws_route_table.private[count.index].id
  destination_cidr_block = "0.0.0.0/0"
  network_interface_id   = aws_network_interface.nat.id
}

resource "aws_route_table_association" "private" {
  count          = 2
  subnet_id      = aws_subnet.private[count.index].id
  route_table_id = aws_route_table.private[count.index].id
}

# S3 gateway endpoint: free, and keeps ECR layer pulls (which are S3 GETs) off
# the NAT instance.
resource "aws_vpc_endpoint" "s3" {
  vpc_id            = aws_vpc.main.id
  service_name      = "com.amazonaws.${var.region}.s3"
  vpc_endpoint_type = "Gateway"
  route_table_ids   = aws_route_table.private[*].id

  tags = { Name = "passband-s3" }
}

# --- fck-nat --------------------------------------------------------------------

resource "aws_security_group" "nat" {
  name        = "passband-nat"
  description = "fck-nat: forwards anything from inside the VPC to the internet"
  vpc_id      = aws_vpc.main.id
}

resource "aws_vpc_security_group_ingress_rule" "nat_from_vpc" {
  security_group_id = aws_security_group.nat.id
  description       = "All traffic from the VPC (it is the default route)"
  cidr_ipv4         = var.vpc_cidr
  ip_protocol       = "-1"
}

resource "aws_vpc_security_group_egress_rule" "nat_to_internet" {
  security_group_id = aws_security_group.nat.id
  description       = "A NAT egresses to the internet by definition"
  cidr_ipv4         = "0.0.0.0/0"
  ip_protocol       = "-1"
}

resource "aws_network_interface" "nat" {
  description       = "passband fck-nat static ENI (route target)"
  subnet_id         = aws_subnet.public[0].id
  security_groups   = [aws_security_group.nat.id]
  source_dest_check = false

  tags = { Name = "passband-nat" }
}

# A stable egress address: worth having the day something (Hetzner firewall,
# a provider allow-list) needs to name where we come from.
resource "aws_eip" "nat" {
  domain = "vpc"
  tags   = { Name = "passband-nat" }
}

# Pinned to ONE exact image name from fck-nat's publishing account, never
# `most_recent` over a wildcard: a new third-party AMI would otherwise replace
# the box that carries all egress (with an SSM role) on whatever apply happens
# next. Bumping is a deliberate edit of var.fck_nat_ami_name (README.md,
# "Bumping the NAT AMI"). include_deprecated: AWS hides public AMIs from
# DescribeImages two years after publish, and a pin must keep resolving.
data "aws_ami" "fck_nat" {
  owners             = ["568608671756"] # fck-nat's publishing account
  include_deprecated = true

  filter {
    name   = "name"
    values = [var.fck_nat_ami_name]
  }

  filter {
    name   = "architecture"
    values = ["arm64"]
  }
}

data "aws_iam_policy_document" "ec2_assume" {
  statement {
    actions = ["sts:AssumeRole"]
    principals {
      type        = "Service"
      identifiers = ["ec2.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "nat" {
  name               = "passband-nat"
  assume_role_policy = data.aws_iam_policy_document.ec2_assume.json
}

data "aws_iam_policy_document" "nat" {
  statement {
    sid = "AttachStaticEni"
    actions = [
      "ec2:AttachNetworkInterface",
      "ec2:ModifyNetworkInterfaceAttribute",
    ]
    resources = [
      aws_network_interface.nat.arn,
      "arn:${data.aws_partition.current.partition}:ec2:${var.region}:${data.aws_caller_identity.current.account_id}:instance/*",
    ]
  }

  # The instance's own primary ENI (for --no-source-dest-check) is not known
  # ahead of time; restrict by tag instead of resource.
  statement {
    sid       = "ModifyOwnEni"
    actions   = ["ec2:ModifyNetworkInterfaceAttribute"]
    resources = ["arn:${data.aws_partition.current.partition}:ec2:${var.region}:${data.aws_caller_identity.current.account_id}:network-interface/*"]
    condition {
      test     = "StringEquals"
      variable = "aws:ResourceTag/Name"
      values   = ["passband-nat"]
    }
  }

  statement {
    sid     = "MoveEip"
    actions = ["ec2:AssociateAddress", "ec2:DisassociateAddress"]
    resources = [
      "arn:${data.aws_partition.current.partition}:ec2:${var.region}:${data.aws_caller_identity.current.account_id}:elastic-ip/${aws_eip.nat.allocation_id}",
      "arn:${data.aws_partition.current.partition}:ec2:${var.region}:${data.aws_caller_identity.current.account_id}:network-interface/*",
      "arn:${data.aws_partition.current.partition}:ec2:${var.region}:${data.aws_caller_identity.current.account_id}:instance/*",
    ]
  }
}

resource "aws_iam_role_policy" "nat" {
  name   = "fck-nat"
  role   = aws_iam_role.nat.id
  policy = data.aws_iam_policy_document.nat.json
}

# SSM so the NAT can be inspected without SSH, like the monitor box.
resource "aws_iam_role_policy_attachment" "nat_ssm" {
  role       = aws_iam_role.nat.name
  policy_arn = "arn:${data.aws_partition.current.partition}:iam::aws:policy/AmazonSSMManagedInstanceCore"
}

resource "aws_iam_instance_profile" "nat" {
  name = "passband-nat"
  role = aws_iam_role.nat.name
}

resource "aws_launch_template" "nat" {
  name          = "passband-nat"
  image_id      = data.aws_ami.fck_nat.id
  instance_type = "t4g.nano"

  iam_instance_profile {
    arn = aws_iam_instance_profile.nat.arn
  }

  metadata_options {
    http_tokens                 = "required"
    http_endpoint               = "enabled"
    http_put_response_hop_limit = 1
  }

  # The primary interface needs a public address only until fck-nat moves the
  # EIP onto it; the auto-assigned one is released when that happens.
  network_interfaces {
    device_index                = 0
    associate_public_ip_address = true
    security_groups             = [aws_security_group.nat.id]
    delete_on_termination       = true
  }

  block_device_mappings {
    device_name = "/dev/xvda"
    ebs {
      volume_type           = "gp3"
      volume_size           = 8
      encrypted             = true
      delete_on_termination = true
    }
  }

  user_data = base64encode(<<-EOT
    #!/bin/sh
    : > /etc/fck-nat.conf
    echo "eni_id=${aws_network_interface.nat.id}" >> /etc/fck-nat.conf
    echo "eip_id=${aws_eip.nat.allocation_id}" >> /etc/fck-nat.conf
    service fck-nat restart
  EOT
  )

  tag_specifications {
    resource_type = "instance"
    tags          = { Name = "passband-nat" }
  }

  tag_specifications {
    resource_type = "network-interface"
    tags          = { Name = "passband-nat" }
  }

  tag_specifications {
    resource_type = "volume"
    tags          = { Name = "passband-nat" }
  }
}

resource "aws_autoscaling_group" "nat" {
  name                = "passband-nat"
  min_size            = 1
  max_size            = 1
  desired_capacity    = 1
  vpc_zone_identifier = [aws_subnet.public[0].id] # same AZ as the static ENI
  health_check_type   = "EC2"

  launch_template {
    id      = aws_launch_template.nat.id
    version = aws_launch_template.nat.latest_version
  }

  instance_refresh {
    strategy = "Rolling"
    preferences {
      min_healthy_percentage = 0
    }
  }

  tag {
    key                 = "Name"
    value               = "passband-nat"
    propagate_at_launch = true
  }
}
