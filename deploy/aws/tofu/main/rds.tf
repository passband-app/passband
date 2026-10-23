# One Postgres 16 instance for the control store (and Bifrost's, if
# bifrost_store = "postgres"). Single-AZ (multi-AZ is out of scope this wave).
#
# The master password is generated and held by RDS in its own AWS-managed
# Secrets Manager secret (manage_master_user_password): it never exists in
# tofu. The operator uses it once to create the app roles (README.md) and
# writes the app's URL, with sslmode=verify-full, to
# passband/control/SQUELCH_CONTROL_DATABASE_URL.

resource "aws_db_subnet_group" "main" {
  name       = "passband"
  subnet_ids = aws_subnet.private[*].id
}

resource "aws_db_parameter_group" "main" {
  name   = "passband-postgres16"
  family = "postgres16"

  parameter {
    name  = "rds.force_ssl"
    value = "1"
  }

  parameter {
    name  = "log_min_duration_statement"
    value = "1000" # slow queries only; parameters are not logged
  }
}

resource "aws_db_instance" "main" {
  identifier     = "passband"
  engine         = "postgres"
  engine_version = "16"
  instance_class = "db.t4g.micro"

  allocated_storage     = 20
  max_allocated_storage = 50
  storage_type          = "gp3"
  storage_encrypted     = true

  db_name                     = "control"
  username                    = "passband_admin"
  manage_master_user_password = true

  db_subnet_group_name   = aws_db_subnet_group.main.name
  vpc_security_group_ids = [aws_security_group.rds.id]
  parameter_group_name   = aws_db_parameter_group.main.name
  publicly_accessible    = false
  multi_az               = false
  ca_cert_identifier     = "rds-ca-rsa2048-g1"

  backup_retention_period    = 7
  backup_window              = "10:00-10:30" # 03:00 Pacific
  maintenance_window         = "sun:11:00-sun:12:00"
  copy_tags_to_snapshot      = true
  deletion_protection        = true
  skip_final_snapshot        = false
  final_snapshot_identifier  = "passband-final"
  auto_minor_version_upgrade = true
  apply_immediately          = false

  iam_database_authentication_enabled = false
  enabled_cloudwatch_logs_exports     = ["postgresql"]

  depends_on = [aws_cloudwatch_log_group.rds]

  lifecycle {
    prevent_destroy = true
  }
}

# Created ahead of RDS so retention and encryption are ours, not "never expire".
resource "aws_cloudwatch_log_group" "rds" {
  name              = "/aws/rds/instance/passband/postgresql"
  retention_in_days = var.log_retention_days
  kms_key_id        = aws_kms_key.logs.arn
}
