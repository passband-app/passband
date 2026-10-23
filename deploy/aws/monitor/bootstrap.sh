#!/bin/bash
# User data for the AWS `monitor` box (Amazon Linux 2023, arm64). OpenTofu
# passes this file verbatim as user_data; cloud-init runs it ONCE, on first
# boot, and logs to /var/log/cloud-init-output.log. Everything that must
# happen on EVERY boot (secrets onto tmpfs, compose up) lives in the
# passband-monitor.service unit this script installs, not here.
#
# Safe to re-run by hand (sudo bash bootstrap.sh): every step checks before
# it acts, and the data volume is formatted ONLY if it carries no signature.
#
# Coordinates with deploy/aws/tofu: the data EBS volume is attached with
# device_name = "/dev/sdf" (DATA_DEVICE_NAME below), the instance role can
# read passband/monitor/* and pull the passband-{prometheus,grafana,blackbox}
# ECR repos, and IMDSv2 is required (hop limit 1 keeps containers off IMDS).
#
# NOTHING HERE PRINTS A SECRET. Keep it that way: no `set -x`, no echo of a
# fetched value. cloud-init's log and the journal are not secret stores.
set -euo pipefail

DATA_DEVICE_NAME=sdf          # tofu aws_volume_attachment.device_name, minus /dev/
DATA_MOUNT=/data
DATA_LABEL=passband-data
APP_DIR=/opt/passband-monitor
COMPOSE_VERSION=v5.6.0
COMPOSE_SHA256=733ec76717ceb59052a9609b9dadfb523b2df8eab57a54212872d10a58078ea2  # docker-compose-linux-aarch64

log() { echo "bootstrap: $*" >&2; }

# --- packages ----------------------------------------------------------------
# docker and the ECR credential helper are in the AL2023 repos; the compose v2
# plugin is not, so it comes from the upstream release, checksum-pinned.
# amazon-ec2-utils (ebsnvme-id, the NVMe udev rules) ships in the base AMI.
dnf install -y docker jq amazon-ecr-credential-helper amazon-ec2-utils
systemctl enable --now docker

PLUGIN=/usr/local/lib/docker/cli-plugins/docker-compose
if ! "$PLUGIN" version 2>/dev/null | grep -q "${COMPOSE_VERSION#v}"; then
  tmp=$(mktemp)
  curl -fsSL -o "$tmp" \
    "https://github.com/docker/compose/releases/download/${COMPOSE_VERSION}/docker-compose-linux-aarch64"
  echo "${COMPOSE_SHA256}  ${tmp}" | sha256sum -c -
  install -D -m 0755 "$tmp" "$PLUGIN"
  rm -f "$tmp"
fi
docker compose version

# --- instance identity (IMDSv2) ---------------------------------------------
IMDS=http://169.254.169.254
TOKEN=$(curl -fsS -X PUT "$IMDS/latest/api/token" -H "X-aws-ec2-metadata-token-ttl-seconds: 300")
DOC=$(curl -fsS -H "X-aws-ec2-metadata-token: $TOKEN" "$IMDS/latest/dynamic/instance-identity/document")
ACCOUNT_ID=$(jq -r .accountId <<<"$DOC")
REGION=$(jq -r .region <<<"$DOC")
ECR_REGISTRY="${ACCOUNT_ID}.dkr.ecr.${REGION}.amazonaws.com"

# --- data volume -------------------------------------------------------------
# On Nitro instances (every t4g) EBS volumes are NVMe devices: /dev/sdf in
# the attachment shows up as /dev/nvmeXn1, and X is assigned in DISCOVERY
# order, so it is not stable across boots or attachments. The requested name
# is stored in the NVMe controller's vendor data; `ebsnvme-id -b` reads it
# back. AL2023's udev rules also use it to create a /dev/sdf symlink, but
# matching on ebsnvme-id directly does not depend on those rules having run.
#
# The attachment is a separate tofu resource that lands AFTER the instance is
# running, so user data can get here before the disk exists: wait for it.
#
# Once formatted, the filesystem is mounted by UUID, which is stable for the
# life of the volume regardless of NVMe numbering.
find_data_device() {
  local dev name
  for dev in /dev/nvme*n1; do
    [ -b "$dev" ] || continue
    name=$(ebsnvme-id -b "$dev" 2>/dev/null || true)
    name=${name#/dev/}
    if [ "$name" = "$DATA_DEVICE_NAME" ] || [ "$name" = "xvd${DATA_DEVICE_NAME#sd}" ]; then
      echo "$dev"
      return 0
    fi
  done
  return 1
}

DEV=""
for _ in $(seq 1 120); do
  DEV=$(find_data_device) && break
  sleep 5
done
[ -n "$DEV" ] || { log "data volume /dev/${DATA_DEVICE_NAME} never attached (10 min)"; exit 1; }

# Never the root disk, whatever ebsnvme-id said.
ROOT_DISK=/dev/$(lsblk -no PKNAME "$(findmnt -no SOURCE /)")
[ "$DEV" != "$ROOT_DISK" ] || { log "refusing: $DEV is the root disk"; exit 1; }
log "data volume is $DEV"

# blkid -p probes the raw device: exit 0 = some signature (filesystem,
# partition table, RAID member...), exit 2 = nothing at all. Only a device
# with NOTHING on it is formatted; anything unexpected stops the script
# rather than risking 30 days of metrics.
set +e
blkid -p "$DEV" >/dev/null 2>&1
rc=$?
set -e
case "$rc" in
  0) log "$DEV already has a signature; not formatting" ;;
  2) log "$DEV is blank; formatting xfs"; mkfs.xfs -L "$DATA_LABEL" "$DEV" ;;
  *) log "blkid -p $DEV failed ($rc)"; exit 1 ;;
esac
FSTYPE=$(blkid -o value -s TYPE "$DEV")
[ "$FSTYPE" = xfs ] || { log "$DEV holds $FSTYPE, expected xfs; refusing to mount"; exit 1; }
UUID=$(blkid -o value -s UUID "$DEV")

mkdir -p "$DATA_MOUNT"
if ! grep -qE "^[^#]+[[:space:]]${DATA_MOUNT}[[:space:]]" /etc/fstab; then
  echo "UUID=${UUID} ${DATA_MOUNT} xfs defaults,noatime,nofail,x-systemd.device-timeout=5min 0 2" >> /etc/fstab
fi
systemctl daemon-reload
mountpoint -q "$DATA_MOUNT" || mount "$DATA_MOUNT"
mkdir -p "$DATA_MOUNT/prometheus" "$DATA_MOUNT/grafana"

# --- app dir -----------------------------------------------------------------
install -d -m 0755 "$APP_DIR" "$APP_DIR/bin"

# Compose interpolation only; no secrets in here. IMAGE_TAG starts at latest
# and each deploy pins it to the commit SHA it built.
if [ ! -f "$APP_DIR/.env" ]; then
  cat > "$APP_DIR/.env" <<EOF
AWS_REGION=${REGION}
ECR_REGISTRY=${ECR_REGISTRY}
IMAGE_TAG=latest
EOF
fi

# Pull through the credential helper: no ECR token is ever written to disk,
# and there is no 12-hour login to expire under a long-running box.
install -d -m 0700 /root/.docker
jq -n --arg r "$ECR_REGISTRY" '{credHelpers: {($r): "ecr-login"}}' > /root/.docker/config.json

# First-boot copy of compose.yaml. Every deploy overwrites it from the repo;
# deploy/aws/monitor/check.sh fails if this copy and compose.yaml differ.
if [ ! -f "$APP_DIR/compose.yaml" ]; then
  cat > "$APP_DIR/compose.yaml" <<'COMPOSE_EOF'
# BEGIN compose.yaml
# The AWS `monitor` box (deploy/aws/DESIGN.md, "monitoring"): prometheus,
# grafana and blackbox, from ECR, on one t4g.small. Lives on the box at
# /opt/passband-monitor/compose.yaml; bootstrap.sh installs the first copy and
# every deploy (deploy-aws.yml, monitor job) overwrites it with this file.
#
# Interpolated from /opt/passband-monitor/.env, which bootstrap.sh writes and
# the deploy updates: ECR_REGISTRY (<account>.dkr.ecr.<region>.amazonaws.com)
# and IMAGE_TAG (the commit SHA the deploy built). Secrets are NOT in that
# file: they are fetched from Secrets Manager on every start into per-service
# env files on tmpfs (/run/passband-monitor, mode 0600), so each container
# sees only its own, the way each Railway service had only its own variables.
#
# Ports: grafana :3000 and prometheus :9090 are published because the ALB
# targets them (grafana.passband.app, metrics.passband.app); the security group
# admits only the ALB. blackbox is NOT published, ever.
name: passband-monitor

x-logging: &logging
  # The root volume is small; unrotated json logs are how a box fills up.
  driver: json-file
  options:
    max-size: "10m"
    max-file: "3"

services:
  prometheus:
    image: ${ECR_REGISTRY:?}/passband-prometheus:${IMAGE_TAG:?}
    restart: unless-stopped
    env_file: /run/passband-monitor/prometheus.env
    volumes:
      # 30 days of TSDB (retention is set in the shared entrypoint.sh).
      - /data/prometheus:/prometheus
    ports:
      - "9090:9090"
    logging: *logging

  grafana:
    image: ${ECR_REGISTRY:?}/passband-grafana:${IMAGE_TAG:?}
    restart: unless-stopped
    env_file: /run/passband-monitor/grafana.env
    environment:
      # Non-secret settings Railway carried as service variables.
      GF_SERVER_ROOT_URL: https://grafana.passband.app
      GF_USERS_ALLOW_SIGN_UP: "false"
    volumes:
      - /data/grafana:/var/lib/grafana
    ports:
      - "3000:3000"
    depends_on:
      - prometheus
    logging: *logging

  blackbox:
    image: ${ECR_REGISTRY:?}/passband-blackbox:${IMAGE_TAG:?}
    restart: unless-stopped
    env_file: /run/passband-monitor/blackbox.env
    # Reachable as blackbox:9115 on the compose network only.
    expose:
      - "9115"
    logging: *logging
# END compose.yaml
COMPOSE_EOF
fi

# Secrets Manager -> per-service env files on tmpfs (/run), 0600, every start.
cat > "$APP_DIR/bin/fetch-secrets" <<'SCRIPT_EOF'
#!/bin/bash
# Writes /run/passband-monitor/<service>.env from passband/monitor/<NAME>.
# /run is tmpfs: the values never touch the EBS volumes, and a reboot wipes
# them, which is why passband-monitor.service runs this on every start.
#
# Values are written single-quoted, which compose reads LITERALLY: unquoted,
# compose would interpolate the `$` runs in PROM_WEB_BCRYPT ($2y$10$...) and
# hand Prometheus a mangled hash. A value containing a single quote or a
# newline cannot be expressed that way and is refused (by NAME, never value).
#
# A secret with no value yet (container created by tofu, never set) is
# skipped with a warning, so one missing value does not keep the other
# services down; the container that needs it fails visibly instead, as it
# did on Railway with an unset variable. Any OTHER error (permissions,
# network) fails the unit, and systemd retries it.
set -euo pipefail
umask 077
# shellcheck disable=SC1091
. /opt/passband-monitor/.env
RUN=/run/passband-monitor
install -d -m 0700 "$RUN"
ERR=$(mktemp "$RUN/.err.XXXXXX")
trap 'rm -f "$ERR"' EXIT

fetch_into() {
  local name=$1 out=$2 val
  if val=$(aws secretsmanager get-secret-value --region "$AWS_REGION" \
        --secret-id "passband/monitor/${name}" \
        --query SecretString --output text 2>"$ERR"); then
    case "$val" in
      *"'"*|*$'\n'*)
        echo "fetch-secrets: passband/monitor/${name} contains a quote or newline; refusing" >&2
        exit 1 ;;
    esac
    printf "%s='%s'\n" "$name" "$val" >> "$out"
  elif grep -q ResourceNotFoundException "$ERR"; then
    echo "fetch-secrets: passband/monitor/${name} has no value; ${name} left unset" >&2
  else
    cat "$ERR" >&2
    exit 1
  fi
}

write_env() {
  local service=$1 tmp name
  shift
  tmp=$(mktemp "$RUN/.${service}.XXXXXX")
  for name in "$@"; do fetch_into "$name" "$tmp"; done
  mv -f "$tmp" "$RUN/${service}.env"
}

write_env prometheus PROM_REMOTE_WRITE_PASSWORD PROM_WEB_BCRYPT
write_env grafana GF_SECURITY_ADMIN_PASSWORD PROM_PASSWORD SLACK_ALERTS_WEBHOOK_URL
write_env blackbox BLACKBOX_BIFROST_CANARY_KEY BLACKBOX_CANARY_MODEL
SCRIPT_EOF

# Bring the stack to the images named in .env and prove it is serving. Both
# boot (ExecStart) and every deploy (ExecReload, via SSM) run this, so a deploy
# fails if the new images do not come up healthy.
cat > "$APP_DIR/bin/up" <<'SCRIPT_EOF'
#!/bin/bash
set -euo pipefail
cd /opt/passband-monitor
/opt/passband-monitor/bin/fetch-secrets
docker compose pull --quiet
docker compose up -d --remove-orphans

healthy() {
  curl -fsS -o /dev/null http://127.0.0.1:3000/api/health || return 1
  # Basic auth on every Prometheus route: 401 means it is up and guarding.
  case "$(curl -sS -o /dev/null -w '%{http_code}' http://127.0.0.1:9090/-/healthy)" in
    200|401) ;;
    *) return 1 ;;
  esac
  docker compose exec -T blackbox wget -qO /dev/null http://127.0.0.1:9115/-/healthy || return 1
}
for _ in $(seq 1 36); do
  if healthy; then
    docker compose ps
    echo "passband-monitor: healthy" >&2
    exit 0
  fi
  sleep 5
done
docker compose ps >&2
docker compose logs --tail 50 >&2
echo "passband-monitor: not healthy after 3 minutes" >&2
exit 1
SCRIPT_EOF
chmod 0755 "$APP_DIR/bin/fetch-secrets" "$APP_DIR/bin/up"

cat > /etc/systemd/system/passband-monitor.service <<EOF
[Unit]
Description=Passband monitoring stack (prometheus, grafana, blackbox)
Requires=docker.service
After=docker.service network-online.target
Wants=network-online.target
RequiresMountsFor=${DATA_MOUNT}
# Retry forever: on a fresh box the images may not be in ECR yet.
StartLimitIntervalSec=0

[Service]
Type=oneshot
RemainAfterExit=yes
WorkingDirectory=${APP_DIR}
ExecStart=${APP_DIR}/bin/up
ExecReload=${APP_DIR}/bin/up
TimeoutStartSec=15min
Restart=on-failure
RestartSec=60

[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload
systemctl enable passband-monitor.service
# --no-block: on first boot the images may not exist yet, and user data must
# finish regardless; the unit keeps retrying until the first deploy lands.
systemctl restart --no-block passband-monitor.service
log "done; follow with: journalctl -u passband-monitor -f"
