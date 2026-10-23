#!/bin/bash
# Local/CI checks for the monitor box config. Run from anywhere:
#   bash deploy/aws/monitor/check.sh
# 1. bootstrap.sh's first-boot copy of compose.yaml is byte-identical to it.
# 2. bootstrap.sh fits in EC2 user data (16 KiB, before base64).
# 3. The AWS prometheus.yml and datasource differ from the Railway ones ONLY
#    in the Railway hostnames (so a scrape-job change made on one side and
#    not the other fails here).
# 4. compose.yaml renders (docker compose config) and publishes only 3000/9090.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
fail() { echo "check: $*" >&2; exit 1; }

diff -u "$HERE/compose.yaml" \
  <(sed -n '/^# BEGIN compose.yaml$/,/^# END compose.yaml$/p' "$HERE/bootstrap.sh" | sed '1d;$d') \
  || fail "bootstrap.sh's embedded compose.yaml differs from compose.yaml"

size=$(wc -c < "$HERE/bootstrap.sh")
[ "$size" -le 16384 ] || fail "bootstrap.sh is $size bytes; EC2 user data caps at 16384"

strip() { grep -v '^[[:space:]]*#' "$1" | sed -e 's/blackbox\.railway\.internal:9115/blackbox:9115/g' -e 's#http://prometheus\.railway\.internal:9090#http://prometheus:9090#g'; }
diff -u <(strip "$ROOT/deploy/monitoring/prometheus/prometheus.yml") <(strip "$HERE/prometheus/prometheus.yml") \
  || fail "AWS prometheus.yml drifted from deploy/monitoring/prometheus/prometheus.yml"
diff -u <(strip "$ROOT/deploy/monitoring/grafana/provisioning/datasources/prometheus.yml") <(strip "$HERE/grafana/datasources/prometheus.yml") \
  || fail "AWS grafana datasource drifted from the Railway one"
! grep -v '^[[:space:]]*#' "$HERE/prometheus/prometheus.yml" "$HERE/grafana/datasources/prometheus.yml" | grep -q 'railway\.internal' \
  || fail "a railway.internal host survives in the AWS configs"

if command -v docker >/dev/null; then
  rendered=$(cd "$HERE" && ECR_REGISTRY=123456789012.dkr.ecr.us-west-2.amazonaws.com IMAGE_TAG=test \
    docker compose -f compose.yaml config --no-env-resolution --format json 2>&1) \
    || fail "docker compose config failed: $rendered"
  published=$(jq -r '[.services[] | (.ports // [])[] | .published] | sort | join(",")' <<<"$rendered")
  [ "$published" = "3000,9090" ] || fail "published ports are '$published', want 3000,9090"
  [ "$(jq -r '.services.blackbox.ports // [] | length' <<<"$rendered")" = 0 ] || fail "blackbox is published"
fi
echo "check: ok"
