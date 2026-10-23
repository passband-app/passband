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
# How a deploy reaches the box (deploy-aws.yml, monitor job): CI pushes the
# three images tagged with the commit SHA, then runs the custom SSM document
# passband-monitor-deploy (tofu), whose ONLY input is ImageTag (40 hex). On the
# box it sets IMAGE_TAG in $APP_DIR/.env and reloads the unit. The deploy role
# cannot run arbitrary shell here and cannot write any other file.
#
# compose.yaml therefore ships INSIDE the prometheus image
# (/etc/passband-monitor/compose.yaml, Dockerfile.prometheus). bin/up, run by
# both ExecStart and ExecReload, reads IMAGE_TAG from .env afresh, copies
# compose.yaml out of passband-prometheus:IMAGE_TAG, then pulls and starts the
# stack. A compose.yaml change is an ordinary monitor deploy; no instance
# refresh. A change to THIS file (bin/up, fetch-secrets, the unit) is not: user
# data runs once, so it needs the box replaced or this re-run by hand.
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

# No compose.yaml is written here: bin/up copies it out of the prometheus
# image at IMAGE_TAG on every start and reload (see the header).

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
# REQUIRED secrets (an upper-case R: prefix below) fail closed: missing or
# empty stops this script, so the unit fails before compose starts anything.
# This matters most for GF_SECURITY_ADMIN_PASSWORD: Grafana seeds its admin
# password from it ONCE, when grafana.db is first created on /data, and with
# it unset that is admin/admin, persisted, behind a public ALB.
# OPTIONAL secrets are skipped with a warning when they have no value:
#   SLACK_ALERTS_WEBHOOK_URL     alerts are not delivered to Slack
#   BLACKBOX_BIFROST_CANARY_KEY  the Bifrost canary probe fails (and alerts)
#   BLACKBOX_CANARY_MODEL        the canary uses its built-in default model
# Any OTHER error (permissions, network) fails the unit, and systemd retries.
# Nothing is moved into place until every secret has been fetched.
set -euo pipefail
umask 077
AWS_REGION=$(sed -n 's/^AWS_REGION=//p' /opt/passband-monitor/.env)
RUN=/run/passband-monitor
install -d -m 0700 "$RUN"
ERR=$(mktemp "$RUN/.err.XXXXXX")
TMPS=("$ERR")
trap 'rm -f "${TMPS[@]}"' EXIT

fetch_into() { # fetch_into [R:]<NAME> <file>
  local name=$1 out=$2 required=false val
  case "$name" in R:*) required=true; name=${name#R:} ;; esac
  if val=$(aws secretsmanager get-secret-value --region "$AWS_REGION" \
        --secret-id "passband/monitor/${name}" \
        --query SecretString --output text 2>"$ERR"); then
    case "$val" in
      *"'"*|*$'\n'*)
        echo "fetch-secrets: passband/monitor/${name} contains a quote or newline; refusing" >&2
        exit 1 ;;
    esac
  elif grep -q ResourceNotFoundException "$ERR"; then
    val=""
  else
    cat "$ERR" >&2
    exit 1
  fi
  # `--output text` prints None for a binary-only secret.
  if [ -z "$val" ] || [ "$val" = None ]; then
    if $required; then
      echo "fetch-secrets: REQUIRED passband/monitor/${name} has no value; not starting" >&2
      exit 1
    fi
    echo "fetch-secrets: optional passband/monitor/${name} has no value; ${name} left unset" >&2
    return 0
  fi
  printf "%s='%s'\n" "$name" "$val" >> "$out"
}

SERVICES=()
PENDING=()
write_env() { # write_env <service> [R:]<NAME>...
  local service=$1 tmp name
  shift
  tmp=$(mktemp "$RUN/.${service}.XXXXXX")
  TMPS+=("$tmp")
  for name in "$@"; do fetch_into "$name" "$tmp"; done
  SERVICES+=("$service")
  PENDING+=("$tmp")
}

write_env prometheus R:PROM_REMOTE_WRITE_PASSWORD R:PROM_WEB_BCRYPT
write_env grafana R:GF_SECURITY_ADMIN_PASSWORD R:PROM_PASSWORD SLACK_ALERTS_WEBHOOK_URL
write_env blackbox BLACKBOX_BIFROST_CANARY_KEY BLACKBOX_CANARY_MODEL
for i in "${!SERVICES[@]}"; do mv -f "${PENDING[$i]}" "$RUN/${SERVICES[$i]}.env"; done
SCRIPT_EOF

# Bring the stack to the images named in .env and prove it is serving. Both
# boot (ExecStart) and every deploy (ExecReload, via the SSM document) run
# this, so a deploy fails if the new images do not come up healthy.
cat > "$APP_DIR/bin/up" <<'SCRIPT_EOF'
#!/bin/bash
set -euo pipefail
cd /opt/passband-monitor
# Read afresh on every run (a deploy has just rewritten IMAGE_TAG), and never
# trusted as shell: parsed, not sourced, and the tag must look like one.
ECR_REGISTRY=$(sed -n 's/^ECR_REGISTRY=//p' .env)
IMAGE_TAG=$(sed -n 's/^IMAGE_TAG=//p' .env)
[[ "$IMAGE_TAG" =~ ^([0-9a-f]{40}|latest)$ ]] || { echo "passband-monitor: bad IMAGE_TAG in .env" >&2; exit 1; }
[[ "$ECR_REGISTRY" =~ ^[0-9]{12}\.dkr\.ecr\.[a-z0-9-]+\.amazonaws\.com$ ]] || { echo "passband-monitor: bad ECR_REGISTRY in .env" >&2; exit 1; }

/opt/passband-monitor/bin/fetch-secrets

# compose.yaml travels in the prometheus image at the same tag.
img="$ECR_REGISTRY/passband-prometheus:$IMAGE_TAG"
docker pull --quiet "$img" >/dev/null
cid=$(docker create "$img")
trap 'docker rm -f "$cid" >/dev/null 2>&1 || true' EXIT
docker cp "$cid:/etc/passband-monitor/compose.yaml" compose.yaml.new
mv -f compose.yaml.new compose.yaml

# Compose reads .env itself; the exports only make sure nothing stale in
# this process's environment overrides it.
export ECR_REGISTRY IMAGE_TAG
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
