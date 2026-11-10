# Hosted monitoring

Two halves, split by one rule: **nothing that judges carrier's health may live
on carrier.**

## The AWS half (this directory, run by `deploy/aws/monitor/`)

One EC2 box, `monitor` (t4g.small, private subnet, no SSH: SSM Session Manager
only), running three containers under docker compose
(`deploy/aws/monitor/compose.yaml`). Each image is built by
`deploy/aws/monitor/Dockerfile.<name>`, which bakes in the configs from this
directory. This directory is the single source of truth for every config; the
Dockerfiles carry no copies.

| Container | Image base | Reachable at |
|---|---|---|
| `prometheus` | `prom/prometheus:v3.15.1` | `https://metrics.passband.app` (ALB to :9090), and only from carrier's egress address: carrier pushes here |
| `blackbox` | `prom/blackbox-exporter:v0.29.1` | **never**: no published port, no ALB route, compose network only |
| `grafana` | `grafana/grafana:13.2.4` | tailnet only: `https://passband-monitor.<tailnet>.ts.net:8443/`. `grafana.passband.app` (ALB to :3000) admits only the NAT EIP and `operator_cidrs`, so from a laptop it is a 404 |

Prometheus stores 30 days on the box's encrypted data volume, receives
`remote_write` from carrier (basic auth, every route), and scrapes blackbox
probes of `signup.passband.app/healthz`, `warden.passband.app/healthz` and
`passband.app`, so uptime is measured from outside the box being watched.

Grafana provisions its datasource and the `passband-health` dashboard from
files baked into the image. Edit the dashboard by editing
`grafana/dashboards/passband-health.json` and **merging it to `main`**, not in
the UI: UI edits die with the next deploy (`allowUiUpdates: false` says so out
loud).

A merge to `main` that touches this directory or `deploy/aws/monitor/` is the
deploy: `.github/workflows/deploy-aws.yml` builds the three images, pushes them
to ECR, and tells the box to pull them through the `passband-monitor-deploy`
SSM document (which pins `IMAGE_TAG` to the commit). To redeploy by hand:
`gh workflow run deploy-aws.yml -f service=monitor`.

Because the dashboard ships inside the image, **merging a panel change is not
the same as seeing it**: the deploy job has to finish. Check the thing itself
rather than the job status, from an SSM session on the box
(`aws ssm start-session --target <AWS_MONITOR_INSTANCE_ID>`):

```sh
cd /opt/passband-monitor && sudo docker compose exec -T grafana \
  grep -c last_over_time /etc/grafana/dashboards/passband-health.json
```

Credentials: `PROM_REMOTE_WRITE_PASSWORD` (plain) and `PROM_WEB_BCRYPT` (its
bcrypt) for prometheus; the same plain value as `PROM_PASSWORD` for grafana,
plus `GF_SECURITY_ADMIN_PASSWORD` and `SLACK_ALERTS_WEBHOOK_URL`; the canary's
`BLACKBOX_BIFROST_CANARY_KEY` for blackbox. All live in Secrets Manager under
`passband/monitor/*` (the box fetches them at boot into a tmpfs env file per
container) and, for the remote-write password, in the
`monitoring/remote-write-auth` Secret on carrier. Nowhere else.

## The carrier half

`deploy/hosted/80-monitoring.yaml`: node-exporter, kube-state-metrics, and a
Prometheus in **agent mode** that scrapes node-exporter, kubelet, cadvisor,
kube-state-metrics, Traefik and cert-manager, then pushes what survives
relabeling. No inbound port; the firewall stays 22/80/443. It is meant to be a
small tenant of the box and has to be kept that way on purpose; see below.

### cadvisor and kubelet are allow-listed by metric name

Those two endpoints are the cardinality hogs, and the agent is what pays for
them: a 213 MiB working set against a 256Mi limit on a 3.8 GB box that has
already global-OOM-killed tenant daemons. (How much of that 213 was series and
how much was page cache over the agent's WAL is unmeasured; the agent now
scrapes itself, so the next answer is a measurement.) So once the config below
is applied they are not scraped whole: cadvisor keeps five families, drops the
`id`, `name` and `image` labels, and drops the container-less rollups; kubelet
keeps `kubelet_volume_stats_*` and nothing else. Everything else the dashboard
reads with a `container_` in the name (`kube_pod_container_info`,
`kube_pod_container_status_*`) is kube-state-metrics, not cadvisor, and that
job is still scraped in full.

**Adding a panel on a cadvisor or kubelet family that is not in the keep regex
gets you NO DATA, not a slow query.** The fix is to widen the regex in the job's
`metric_relabel_configs` in `80-monitoring.yaml`. The comment above each job
says which names are in and why one is deliberately out.

Two things bite here. First, `up` and the other synthetic scrape series are
appended after metric relabeling, so a broken keep regex looks like a healthy
target serving nothing, not like a dead target. That is what the third line on
the dashboard's **Unhealthy signals** panel watches for: it counts cadvisor and
kubelet targets that are either down or reporting zero samples past relabeling,
and it should sit flat at zero. Second, **applying the ConfigMap does not
reload the agent**: it runs without `--web.enable-lifecycle` and reads its
config once, at boot. Carrier served a 13-day-stale Prometheus config exactly
that way, and every per-tenant panel was blank while nothing looked wrong.

```sh
kubectl apply -f deploy/hosted/80-monitoring.yaml
kubectl -n monitoring rollout restart deploy/prometheus-agent
```

Traefik's latency histogram is configured separately, through k3s's Helm
controller rather than a ConfigMap, because Traefik is k3s's own chart:

```sh
kubectl apply -f deploy/hosted/05-traefik-metrics.yaml
kubectl -n kube-system rollout status deploy/traefik
```

The chart's default histogram has four buckets (0.1/0.3/1.2/5.0), and a p95
read off it is an interpolation inside one of them: it rendered a 0.35s floor
as 1.1s and clamped every spike to exactly 5s for as long as the dashboard
existed. The manifest sets real buckets; the file says why.

To see what a target actually offers before guessing at a name, dump the
families off the endpoint through the API server's node proxy, which spares you
the service-account token, the TLS flags and a shell inside the agent's
container:

```sh
NODE=$(kubectl get node -o jsonpath='{.items[0].metadata.name}')
kubectl get --raw "/api/v1/nodes/$NODE/proxy/metrics/cadvisor" \
  | grep '^# TYPE' | awk '{print $3}'
```

Swap `/metrics/cadvisor` for `/metrics` to do the same for the kubelet's own
families.

The agent scrapes itself on a job of its own (`prometheus-agent`, under ten
series), because the allow-lists are a memory argument and it should be possible
to read whether they worked. On the monitor box's Prometheus:

```promql
prometheus_agent_active_series{cluster="carrier"}
process_resident_memory_bytes{cluster="carrier", job="prometheus-agent"}
```

The two do not track each other: resident memory includes page cache over the
agent's WAL under `/data`, so a series cut moves the first immediately and the
second only as the cache turns over. `prometheus_target_scrapes_sample_out_of_order_total`
is on the same job, and it is where a relabel rule that collapses two series
into one shows up. From the box, the resident half of that, without Grafana:

```sh
kubectl -n monitoring top pod -l app.kubernetes.io/name=prometheus-agent
```

## What the dashboard answers

- **Is it up?** — external probes of signup/warden/landing, TLS days left,
  the box still reporting, pods not ready.
- **carrier** — CPU (incl. **steal**, the shared-vCPU tax), memory, both
  disks (root = backed up, tenant volume = not), load, network.
- **Kubernetes** — restarts, top pods by CPU/memory, OOM kills.
- **Certificates & edge** — cert-manager expiry (wildcard renewal failing
  shows here first), Traefik request/5xx/latency. The p95 is one line per
  backend service, so a slow tenant cannot hide inside a fleet-wide blend; a
  line pinned flat at the top bucket means "worse than this", not "this".
- **Tenants** — who is still using it (**Tenant activity**: time since each
  tenant's last write on the client door and since its app was last seen at
  all, from the daemon's per-route request counters, so it reaches back only
  as far as Prometheus's 30 days; plus the newest Mac and iOS app version to
  have connected and when, from `squelchd_client_last_seen_timestamp_seconds`,
  which the daemon keeps in its own store; those columns do not appear until
  some daemon exports the metric, and read `-` where no app of that platform
  has named itself: the daemon or app predates the `X-Passband-Client`
  header, or the tenant has no such device; and when each platform's app last
  made a write, from `squelchd_client_last_action_timestamp_seconds`, which
  is the last-action question split by Mac and iOS), pod count vs the 100-user cap,
  per-tenant CPU/memory/PVC/restarts.
- **Daemon rollout** — which image version each tenant pod runs, and the
  switchover as it happens, plus the roller's own two vital signs: a casualty
  (a roll that exited 4, the fleet frozen, the one thing worth a page) and how
  long since the timer last fired. See below.
- **Inside squelchd** — daemon p95 by route (the daemon's own clock, request
  to response head, so a slow route is named rather than inferred from the
  edge; routes that wait on a model carry `class="llm"` and get their own
  panel, so their seconds are not read as a slow store), sync staleness per
  tenant, Gmail API errors by kind,
  24h LLM spend, store size, triage throughput.

## Watching a release roll

Releases are rolling: the warden repoints one tenant Deployment at a time, so
for a while the fleet runs two versions at once. The **Daemon rollout** row is
that window, told twice over.

The version a panel means is the image tag with its `daemon-` prefix stripped,
so `ghcr.io/passband-app/squelchd:daemon-0.4.0` reads as `0.4.0` — the same
spelling `squelchd_build_info` uses, which is what lets the two sides be
compared. A digest-pinned image has no tag to strip and reads as its digest.

Two sources, and the gap between them is the point:

- **kube-state-metrics** (`kube_pod_container_info`) is what Kubernetes was
  *told* to run. It moves the instant a Deployment is patched, whether or not
  the pod ever starts.
- **the daemon itself** (`squelchd_build_info`, off port 9464) is what is
  *actually running*. It lags by however long a pull and a start take.

So "Pods by image version" leads and "Pods by daemon version" follows, and a
gap that does not close is a pod holding the new image it cannot run. **Ahead
of the daemon** counts exactly those, and deliberately counts only tenants
whose daemon exports metrics at all, so it reads 0 rather than crying wolf on
tenants not yet re-applied with `SQUELCH_METRICS_BIND`.

The tenant label on the kube-state-metrics side is derived from the pod name,
because kube-state-metrics does not carry pod labels unless allowlisted. A pod
is named `<deployment>-<replicaset>-<pod>` and the warden names a Deployment
after the tenant, so the tenant is the pod name minus its last two hyphen
segments. Tenant labels may contain hyphens themselves, which is why the regex
takes from the end.

Reading the row during a release:

| Panel | Converged | Rolling | Wrong |
|---|---|---|---|
| Image versions live | 1 | 2 | 3+, or stuck at 2 |
| Off the majority image | 0 | counting down | stops falling |
| Image pull failures | 0 | 0 | any — bad tag or pull secret |
| Ahead of the daemon | 0 | brief, per pod | sticky — crash loop on the new build |

**Version by tenant** is the per-tenant answer: one band per tenant and
version, so you can see which tenants have gone over and which have not. The
gap between a tenant's two bands is its downtime for the release: a tenant
Deployment is `Recreate`, not `RollingUpdate`, because its SQLite store takes
exactly one writer, so the old pod is gone before the new one starts. That is
also why the stacked totals dip by one at each handoff rather than bulging.
A gap that does not close is a pod that never came back.

**Tenant image inventory** is the same thing as a list, plus pod age (how long
ago that tenant rolled) and the resolved digest, which is the only column that
can tell two pulls of the same mutable tag apart.

A dashboard-wide annotation marks the moment a version nothing ran ten minutes
ago first appears on a pod, so the roll is a vertical line across every other
panel too — which is how you tell "the restarts at 14:05" from "the release at
14:04".

## The daemon's own metrics

Every tenant daemon runs with `SQUELCH_METRICS_BIND=0.0.0.0:9464` (set by the
warden) and serves Prometheus text on container port `metrics`, named
`squelchd_*`. `prometheus-agent` discovers those pods in the `tenants`
namespace by `app.kubernetes.io/name=squelchd` plus the port name, relabels
`app.kubernetes.io/instance` to `tenant`, and caps each scrape at 500 samples.

One series on that listener is worth naming, because it is the reader for
**Tenant memory** rather than a health signal of its own:
`squelchd_embedder_loaded` is 1 while the daemon's ONNX embedding session is
resident and 0 while it is unloaded. That session is 85-90% of a tenant pod's
RSS, so the **Embedder loaded** timeline is what turns a memory sawtooth from a
mystery into cause and effect. A tenant pinned at 1 is a session the idle reaper
never gets to put down.

That listener has no authentication, so reaching it is a network question and
the answer is one rule: the per-tenant NetworkPolicy admits the `monitoring`
namespace's `app.kubernetes.io/name=prometheus-agent` pod to 9464 and nothing
else. Port 9464 is on the pod only — not on the tenant Service, not in its
Ingress — so it cannot be published through a tenant vhost by any routing
mistake, and the agent cannot reach 8848 (mail) from that allowance.

The alert worth having, which is the thing external probes structurally cannot
see (a pod can be Running, ready and stuck since Tuesday):

```promql
time() - squelchd_sync_last_success_timestamp_seconds > 900
```

One tenant firing is usually a dead refresh token (its
`squelchd_gmail_api_errors_total{kind="auth"}` will be climbing) and that tenant
has to re-consent; every tenant firing at once is the box.

A first backfill that ran without the embedder (the init never settled inside
its three-minute ceiling) is `increase(squelchd_embedder_gate_timeouts_total[1h])
> 0`. The mailbox is fine; it is the memory profile that is not, because the
vector backfill pass then drains that mailbox in batches instead of ingest
embedding it one message at a time. One is worth a look; a train of them is a
wedged init retrying.

## Alerts

Grafana alerting, provisioned from `grafana/provisioning/alerting/` and baked
into the image like the dashboard, so an alert is changed by merging to `main`
and not in the UI. Everything goes to one Slack channel (`#passband-alerts`)
through an incoming webhook.

| Rule | Fires when | For |
|---|---|---|
| Tenants quota near full | any `tenants` ResourceQuota dimension is at 80% or more | 15m |
| Tenants quota changed on the box | a hard line of the `tenants` ResourceQuota changed in the last hour (expected once after an apply of `70-tenant-limits.yaml`; otherwise the box was patched past the file) | 0s |
| Tenant has no running pod | a tenant Deployment wants a pod and has none available (`squelch-embed` excluded: it has its own rule) | 10m |
| Tenant mail sync stale | a tenant's freshest `squelchd_sync_last_success_timestamp_seconds` is over 900s old | 15m |
| carrier memory high | the box is 85% or more used (`1 - MemAvailable / MemTotal`) | 2m |
| carrier disk high | the tenant volume (`/mnt/tenant-data`) or the root disk is 85% or more full | 5m |
| carrier metrics missing | no kube-state-metrics series from carrier for 10 minutes | 5m |
| Pod in tenants OOM-killed | a container in `tenants` restarted in the last 15 minutes with `OOMKilled` as its last termination reason | 0s |
| Embedding service down | the `squelch-embed` Deployment exists and its session is not ready, or no series from it says either way | 5m |
| Embedding service refusing requests | `squelch_embed_requests_total` moving for `unauthorized`, `not_ready` or `error`, or for `queue_full` on the interactive lane or before a request has a lane (`none`) | 10m |
| Embedding service near its memory limit | `squelch-embed`'s working set is 85% or more of its own limit (768Mi today) | 5m |

The first and the third exist because a full quota makes signups time out
with no other signal. The second catches drift between the quota on the box
and the file in git: a hard line changing, or the set of lines changing, is
announced, expected once after an apply of the file and otherwise a
`kubectl patch` the file does not record; the quota being deleted outright is
the seventh rule's, which reads these series. The fourth is the sync alert
this README recommended above. The fifth exists because memory is the box's
bottleneck, and with nothing reserved
for k3s and no swap the RAM is the only wall, so this fires before a global OOM
rather than after one. The sixth is the disk, which the tenants quota does not
guard: a claim is nominal on local-path (`deploy/hosted/70-tenant-limits.yaml`),
so the volume is watched here and grown when this says so. The seventh is what
makes the others trustworthy: every rule
reads series the carrier agent remote-writes, so they go quiet together when it
stops, and the others treat NoData as OK so that only this one says so.

The OOM rule and the three embedding rules came with the shared embedding
service (`deploy/hosted/SETUP.md` §10b). On it, a tenant daemon runs under a
512Mi limit sized from August measurements, so an OOM kill is the first sign
that number was wrong, and it is announced per kill rather than discovered as a
crash loop. The embedding rules are silent until `65-embed.yaml` is applied:
"down" asks kube-state-metrics whether the Deployment exists rather than
trusting the service's own series to be there, because a target that stopped
being scraped reads exactly like an idle one. The hole in that, said once: the
Deployment deleted while tenants still point at it reads as "not installed".
Every series these rules read is already scraped: kube-state-metrics' job keeps
everything it exports, the cadvisor allow-list carries
`container_memory_working_set_bytes` and `container_spec_memory_limit_bytes`,
and the `squelch-embed` job keeps the service's whole exposition (its series
carry `job` and `instance` and no namespace or pod, so the rules select it by
`job`).

Slack hears about a state change and nothing else: one message when an alert
starts firing, one when it resolves, no reminders while it stays firing
(`policies.yml` has the two settings that takes). The sync alert is one series
per tenant rather than per pod, so a pod restart does not resolve and re-fire a
tenant who was stale throughout. What is still firing right now is in Grafana
under Alerting, not in the channel's scrollback.

**`passband/monitor/SLACK_ALERTS_WEBHOOK_URL` must have a value before any
change to these files deploys.** Grafana exits at startup when the Slack
receiver has no URL (checked against 12.2.0), and the monitor deploy job fails
("not healthy after 3 minutes"). Nothing alerts while it is down. Set the
secret, then `systemctl reload passband-monitor` on the box.

Two escapes that were tested rather than assumed, both against 12.2.0:
env-var interpolation DOES run over `contact-points.yml` (that is how the URL
arrives) and does NOT run over `rules.yml`, so `$labels` in an annotation is
written bare. A `$$labels` there reaches Slack as a literal `$$labels`.

To check the pipe end to end: Alerting, Contact points, `slack`, Test.
