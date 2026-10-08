//! Startup configuration, read once from the environment and validated hard
//! enough that a running warden is a warden whose names, images, bounds and
//! timeouts all make sense.
//!
//! Everything here except the bearer token is a name, a hostname, a quantity, a
//! duration or a flag. The token is the one secret this process holds, so
//! [`Config`] has a manual `Debug` that redacts it: a derived one would print it
//! the first time anyone adds a `dbg!` to a handler.
//!
//! The v1 warden had a config full of filesystem paths and binary locations
//! because it *was* the orchestrator. This one has none: kube owns the
//! lifecycle, so what is left is the handful of cluster facts a manifest needs.
//!
//! Reading is parameterised over a lookup rather than hardwired to
//! `std::env::var`, which is what lets the boot-refusal table below run in
//! parallel against maps instead of mutating the process environment.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

/// All interfaces by design. The warden runs as a pod behind a ClusterIP
/// Service and its own Ingress; binding loopback inside a pod would make the
/// Service unreachable. The pod has its own network namespace and a
/// NetworkPolicy in front, which is what "not exposed" means here.
pub const DEFAULT_BIND: &str = "0.0.0.0:8852";

/// The namespace every tenant's objects live in. The warden's RBAC is scoped to
/// exactly this namespace and nothing else.
pub const DEFAULT_TENANT_NAMESPACE: &str = "tenants";

/// The namespace the ingress controller runs in. k3s ships Traefik in
/// `kube-system`.
pub const DEFAULT_INGRESS_NAMESPACE: &str = "kube-system";

/// Pod label the ingress controller carries, as `key=value`. Used by the
/// per-tenant NetworkPolicy to name the one peer allowed to reach a tenant.
pub const DEFAULT_INGRESS_POD_LABEL: &str = "app.kubernetes.io/name=traefik";

/// `ingressClassName` on every tenant Ingress.
pub const DEFAULT_INGRESS_CLASS: &str = "traefik";

/// Secret holding the wildcard certificate for `*.<base domain>`, in the tenant
/// namespace, issued by cert-manager via DNS-01.
pub const DEFAULT_TLS_SECRET: &str = "passband-wildcard-tls";

/// Secret holding the Google OAuth client every tenant daemon refreshes with.
///
/// One Secret, shared by every tenant, in the tenant namespace. A tenant's
/// refresh token was minted by the control plane's CONFIDENTIAL WEB client, and
/// a refresh token only works for the client that minted it, so a tenant daemon
/// that does not hold that client's id and secret cannot refresh and does not
/// boot. See `deploy/hosted/SETUP.md`, "The Google OAuth client".
pub const DEFAULT_OAUTH_SECRET_NAME: &str = "google-oauth-client";

/// Secret holding the APNs relay's PUSH-ONLY bearer, data key `token`.
///
/// One Secret, shared by every tenant, in the tenant namespace, exactly like
/// [`DEFAULT_OAUTH_SECRET_NAME`]. Its value is the relay's
/// `SQUELCH_RELAY_PUSH_TOKEN`, NEVER its `SQUELCH_RELAY_AUTH_TOKEN`: the push
/// token opens `POST /v1/push` and is a 401 at `GET /v1/opens`, so a
/// compromised tenant pod cannot read or delete the read-tracking opens of the
/// self-hosted daemons that hold the full token. The relay knows one push
/// token, so there is still nothing per tenant to mint. Read only when
/// [`Config::relay_url`] is set. See `deploy/hosted/apns-relay.example.yaml`.
pub const DEFAULT_RELAY_SECRET_NAME: &str = "apns-relay";

/// k3s's built-in dynamic provisioner.
/// The deepest archive the warden will render, in days: the daemon's own
/// `ARCHIVE_DAYS_MAX` (squelch-core is deliberately not a dependency here, so
/// the number is repeated and a test in each crate pins it at 7300).
pub const ARCHIVE_DAYS_MAX: u32 = 7300;

pub const DEFAULT_STORAGE_CLASS: &str = "local-path";

/// Per-tenant data volume size. The SQLite index plus the embedding cache.
pub const DEFAULT_STORAGE_SIZE: &str = "10Gi";

/// A tenant daemon's CPU floor. An idle mailbox is a poll loop; this is enough
/// to keep one responsive without reserving a core it will not use.
pub const DEFAULT_CPU_REQUEST: &str = "100m";

/// A tenant daemon's CPU ceiling. One core: an embedding backfill is the only
/// thing that ever wants more, and a tenant that wants more than a core is a
/// tenant taking the box from everyone else.
pub const DEFAULT_CPU_LIMIT: &str = "1000m";

/// A tenant daemon's memory floor: SQLite plus the ONNX embedder's working set.
///
/// A request is a promise about the p50, and the scheduler is the thing that
/// believes it. This said 256Mi until somebody measured the promise: on the
/// carrier box (3 vCPU, 3814 MiB, no swap) the four tenant pods read 91, 317,
/// 376 and 509 MiB on one pass and 123, 293, 349 and 545 MB on another the same
/// day, because a daemon that has embedded anything keeps its ONNX session
/// resident and rests at roughly 300-500 MB. 384Mi is about the p50 of that,
/// which is a truthful number and not a comfortable one: half the fleet is
/// above it on any given read. The scheduler was reserving 256Mi per tenant
/// while the kernel paid three to five hundred, and on 2026-08-19 the node ran
/// out of memory globally with FOUR tenants on it and OOM-killed two squelchd
/// processes.
///
/// Which tenant dies in that is nobody's decision. A tenant pod is burstable
/// (request below limit, `oom_score_adj` 933), so the kernel picks a RUNNING
/// mailbox over anything the node keeps for itself, and the signup that
/// overcommitted the box finished successfully minutes or days earlier. That is
/// exactly the failure a request exists to prevent: with the request telling
/// the truth, the tenant that will not fit fails to SCHEDULE, which surfaces at
/// signup as the already-documented `500 not_ready` (`deploy/hosted/SETUP.md`
/// §7, "A tenant refused by the quota looks like a provision that times out")
/// rather than as a dead neighbour.
///
/// 384Mi is today's truth and a stopgap, not a target. The daemon-side work in
/// flight - unloading the embedder when it goes idle, smaller embed batches -
/// brings the real number down over the coming weeks, so revisit this off a
/// week of `container_memory_working_set_bytes` at p50 and p99 rather than off
/// a feeling. The limit stays at 1Gi: that one is the ceiling, this one is the
/// promise, and they answer different questions.
///
/// Inert in production on its own. `deploy/hosted/15-warden-config.yaml`
/// restates every bound as an env entry and an env entry outranks this
/// constant, so a change here that is not made there ships nothing.
pub const DEFAULT_MEMORY_REQUEST: &str = "384Mi";

/// A tenant daemon's memory ceiling. Past this the container is OOM-killed and
/// restarted, which is one tenant down rather than the node down.
pub const DEFAULT_MEMORY_LIMIT: &str = "1Gi";

/// Ephemeral-storage floor for a tenant daemon: `/tmp` plus its own logs. The
/// mailbox lives on a PersistentVolume and does not count against this.
pub const DEFAULT_EPHEMERAL_REQUEST: &str = "256Mi";

/// Ephemeral-storage ceiling. Without one, a tenant filling `/tmp` fills the
/// NODE's root filesystem and takes every other tenant with it.
pub const DEFAULT_EPHEMERAL_LIMIT: &str = "1Gi";

/// `sizeLimit` on the `/tmp` emptyDir. An emptyDir with no size limit is a
/// tmpfs-or-disk hole with the node's free space at the bottom of it.
pub const DEFAULT_TMP_SIZE: &str = "512Mi";

/// UID/GID every tenant daemon runs as. Fixed and high, so it collides with no
/// system account on the node, and so `runAsNonRoot` has something to enforce.
/// With `hostUsers: false` this is a namespaced id anyway.
pub const DEFAULT_RUN_AS: i64 = 10001;

/// How long provisioning waits for a tenant pod to report Ready before giving
/// up. First boot pulls an image and opens a fresh SQLite file; three minutes
/// is generous for that and short enough that a signup does not hang forever.
pub const DEFAULT_READY_TIMEOUT_SECS: u64 = 180;

/// How long a tenant's pod must stay Ready before the Deployment controller
/// counts the replica Available (`minReadySeconds`).
///
/// Thirty seconds, and the number is picked against the failure it catches: a
/// daemon that comes up and dies on its first real work. A rejected credential
/// surfaces within a sync tick and an OOM on the first embed batch within tens
/// of seconds, so half a minute of staying up is the difference between "the
/// process started" and "the process survived contact with the mailbox". It is
/// also cheap: it adds 30s to one tenant's roll, and every wait it lengthens is
/// bounded by [`DEFAULT_READY_TIMEOUT_SECS`], which it has to stay well under.
pub const DEFAULT_MIN_READY_SECS: i32 = 30;

/// Ceiling on `SQUELCH_WARDEN_MIN_READY_SECS`. A soak longer than this is a
/// fleet roll that spends its whole window watching one healthy tenant; the
/// tighter bound is the ready timeout, which is checked against the configured
/// value rather than against this one.
pub const MAX_MIN_READY_SECS: i32 = 300;

/// How long a tenant may sit `pending` before the sweep collects it. A signup
/// that reached phase one and never came back has parked an identity Secret
/// that nothing will ever use and that holds the subdomain against everyone
/// else. A day is far longer than any real signup and short enough that a
/// squatted label frees itself.
pub const DEFAULT_PENDING_TTL_SECS: u64 = 24 * 60 * 60;

/// Floor on the pending TTL. Anything shorter would race a signup that is still
/// in the operator's hands.
pub const MIN_PENDING_TTL_SECS: u64 = 600;

/// Ceiling on `SQUELCH_WARDEN_TRUSTED_PROXY_HOPS`. No deployment stacks eight
/// proxies; a larger number is a typo, and a typo here degrades silently to one
/// shared bucket for every caller, so it is a refusal to boot instead.
pub const MAX_TRUSTED_PROXY_HOPS: usize = 8;

/// Ceiling on `SQUELCH_WARDEN_EMBED_IDLE_UNLOAD_SECS`. The knob is an idle
/// window in seconds and 0 is its off switch, so a value past a month is a
/// typo, not a policy; the daemon adds its reap interval to it, and a ceiling
/// keeps that sum from ever being a question.
pub const MAX_EMBED_IDLE_UNLOAD_SECS: u64 = 30 * 24 * 60 * 60;

/// Shortest bearer token that may be configured.
///
/// This is the credential for a service that can create workloads and read
/// Secrets in the tenant namespace, so a short one is a boot failure rather
/// than a warning. 32 characters is what `openssl rand -base64 24` prints.
pub const MIN_TOKEN_LEN: usize = 32;

/// Why the warden refused to start.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0}")]
    Invalid(String),
}

impl ConfigError {
    fn invalid(msg: impl Into<String>) -> Self {
        Self::Invalid(msg.into())
    }
}

/// Compute bounds for one container, as Kubernetes quantities.
///
/// Requests are what the scheduler reserves; limits are what the kubelet
/// enforces. Both halves are here because a pod with requests and no limits
/// starves its neighbours under load, and a pod with limits and no requests
/// gets scheduled onto a box that has nothing left to give it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resources {
    pub cpu_request: String,
    pub cpu_limit: String,
    pub memory_request: String,
    pub memory_limit: String,
    pub ephemeral_request: String,
    pub ephemeral_limit: String,
}

/// Validated startup configuration.
#[derive(Clone)]
pub struct Config {
    pub bind: SocketAddr,
    /// The shared secret the control plane presents. Never logged, never in a
    /// response body, compared only through [`squelch_httpauth::ct_eq`].
    pub token: String,
    /// `passband.email`. Tenant hostnames are `<label>.<base_domain>`; nothing
    /// in this crate ever hardcodes the domain.
    pub base_domain: String,
    /// The squelchd image every tenant Deployment runs, e.g.
    /// `ghcr.io/passband-app/squelchd:daemon-0.4.0`. Pinned by the operator: a tenant
    /// that silently moved to a new daemon on a restart would be an upgrade
    /// nobody scheduled.
    pub image: String,
    /// Namespace every tenant object is created in.
    pub namespace: String,
    /// Namespace the ingress controller runs in.
    pub ingress_namespace: String,
    /// The ingress controller's pod label, split.
    pub ingress_pod_label: (String, String),
    pub ingress_class: String,
    /// Wildcard TLS Secret name, in [`Config::namespace`].
    pub tls_secret: String,
    /// Secret holding the Google OAuth client id and secret every tenant daemon
    /// refreshes with, in [`Config::namespace`]. See
    /// [`DEFAULT_OAUTH_SECRET_NAME`].
    pub oauth_secret_name: String,
    /// The control plane's origin, injected into every tenant daemon as
    /// `SQUELCH_CONSOLE_SSO_URL`. Optional, and the whole hosted/self-host split
    /// of the console login page: set it and each tenant's console renders a
    /// "Continue with Google" link back to the control plane, leave it unset and
    /// the console offers the pasted-code form only.
    ///
    /// A LINK TARGET and nothing more. No trust flows from it: what comes back
    /// from that hop is a pairing code the tenant's own store adjudicates.
    pub console_sso_url: Option<String>,
    pub storage_class: String,
    pub storage_size: String,
    /// Bounds on the tenant daemon container.
    pub daemon_resources: Resources,
    /// `sizeLimit` on the tenant's `/tmp` emptyDir.
    pub tmp_size: String,
    /// `imagePullSecrets` entry for GHCR, when the image is not public.
    pub pull_secret: Option<String>,
    /// Whether to ask for per-pod user namespaces (`hostUsers: false`).
    ///
    /// ON by default, because it is the strongest per-tenant boundary this
    /// design has: root inside a tenant's pod maps to an unprivileged id on the
    /// node. It is a flag because the field needs a kernel with idmapped mounts
    /// and a cluster with the `UserNamespacesSupport` gate on; on a cluster
    /// without it the field is ignored (older API servers prune it), and on a
    /// node without it pods fail to start. See `deploy/hosted/SETUP.md`.
    pub user_namespaces: bool,
    /// Optional PVC holding a pre-seeded embedding-weights cache. When set,
    /// every tenant's init container copies from it instead of each tenant
    /// downloading ~126 MB from Hugging Face on first boot.
    ///
    /// The copy is per model directory and skips what the tenant already has,
    /// so it also fills in a model a tenant is missing rather than only ever
    /// seeding an empty cache; see [`crate::objects::SEED_SCRIPT`]. Nothing a
    /// tenant downloaded itself is ever overwritten, and a copy that fails
    /// leaves the pod up: the daemon downloads what it does not find.
    ///
    /// NOTHING HERE CHECKS THAT THE PVC EXISTS. This name is rendered straight
    /// into every tenant's pod spec, and a name that resolves to no claim
    /// leaves each new tenant pod Pending on a volume that will never bind.
    /// Seed the volume first; `deploy/hosted/SETUP.md` step 10 is the order,
    /// and `deploy/hosted/PRODUCTION.md` has what the failure looks like.
    pub model_pvc: Option<String>,
    /// Which embedding weights tenant daemons load, injected as
    /// `SQUELCH_EMBED_MODEL`. Optional, and unset means the daemon's own pinned
    /// default, which is the normal state.
    ///
    /// It exists because the daemon's env is a CLOSED list rendered here: a
    /// var this file does not name cannot reach a tenant at all, so without
    /// this knob moving the fleet's embedding model would mean cutting a new
    /// image. A fastembed `model_code` or variant name, validated by the daemon
    /// rather than here, since fastembed's table is the authority on what
    /// resolves.
    pub embed_model: Option<String>,
    /// The node network, when the CNI drops kubelet probe traffic without one.
    ///
    /// A readiness probe originates from the NODE, not from a pod, so it
    /// matches no peer in a tenant's NetworkPolicy. Some CNIs exempt
    /// host-originated traffic and some do not; on one that does not, every
    /// provision times out waiting for a pod that is healthy. Setting this adds
    /// one ingress rule allowing that CIDR to the two ports a probe can land
    /// on, the daemon port and the metrics port. See `deploy/hosted/SETUP.md`,
    /// "Verify the policy".
    pub node_cidr: Option<String>,
    /// Whether a tenant's readiness probe is an HTTP GET of the daemon's
    /// `/healthz` on the metrics port instead of a bare TCP accept on the door.
    ///
    /// OFF by default, and the default is the load-bearing half. `/healthz`
    /// exists only on a daemon image new enough to serve it; a tenant still on
    /// an older one would fail an HTTP probe on every period, never report
    /// Ready, never be counted Available, and be pulled out of its own Service
    /// for it. Flipping this while any tenant is behind therefore takes those
    /// tenants DOWN, and the roller would halt on the first of them.
    ///
    /// The order is: ship the daemon that serves the route, roll the fleet onto
    /// it, THEN turn this on and roll again. See `deploy/hosted/PRODUCTION.md`,
    /// "Turning on the HTTP readiness probe".
    ///
    /// It also puts the first-run MODEL DOWNLOAD inside every wait this warden
    /// makes. `/healthz` answers 503 until the daemon's background embedder init
    /// has settled, which on a cold weights cache means ~130 MB from Hugging
    /// Face — longer than [`Config::ready_timeout`]. With no [`Config::model_pvc`]
    /// every new tenant's first pod pays it, so a signup gets `500 not_ready`
    /// for a tenant that is perfectly healthy, and the next roll reads that
    /// tenant as a casualty and stops the whole fleet. Set `model_pvc`, or raise
    /// `ready_timeout` past a cold download, before turning this on; the warden
    /// warns at startup if neither is true.
    pub http_readiness: bool,
    /// UID/GID tenant daemons run as.
    pub run_as: i64,
    /// `minReadySeconds` on every tenant Deployment: how long a pod has to stay
    /// Ready before the controller reports the replica Available.
    ///
    /// A per-tenant soak, and defence in depth behind the roller's own pacing.
    /// Readiness is a snapshot and a crash loop passes through it — a daemon
    /// that comes up, reports Ready and dies is Ready for as long as the probe
    /// takes to notice, which is long enough for a sweep to step to the next
    /// tenant on the strength of it. `availableReplicas` is the count that
    /// waits this long before agreeing, and it is what
    /// [`crate::cluster::rolled_out`] stands on.
    pub min_ready_secs: i32,
    pub ready_timeout: Duration,
    /// How long a `pending` tenant survives the sweep.
    pub pending_ttl: Duration,
    /// How many proxies the operator runs in front of this listener. Zero means
    /// `X-Forwarded-For` is never read, which is the safe default: the header is
    /// caller-supplied and would otherwise mint unlimited rate-limit
    /// identities.
    pub trusted_proxy_hops: usize,
    /// The LLM gateway URL handed to every tenant daemon as
    /// `SQUELCH_ANTHROPIC_BASE_URL`. Absent, the whole LLM env feature is off:
    /// no gateway vars, no key reference, byte-identical pods to a warden that
    /// never heard of it. Always https, because the hosted gateway is not on
    /// this box and a virtual key crossing the wire in the clear would be a
    /// credential leak by configuration.
    pub llm_base_url: Option<String>,
    /// Stage-1 (triage) model override, when the operator pins one.
    pub llm_stage1_model: Option<String>,
    /// Stage-2 (drafting) model override.
    pub llm_stage2_model: Option<String>,
    /// Per-tenant daily ceiling on stage-1 calls.
    pub llm_stage1_daily_cap: Option<u32>,
    /// Per-tenant daily ceiling on stage-2 calls.
    pub llm_stage2_daily_cap: Option<u32>,
    /// Seconds a tenant daemon holds its idle embedding session before dropping
    /// it, passed on as `SQUELCH_EMBED_IDLE_UNLOAD_SECS`. Unset leaves the
    /// daemon's own default, which is the only reason this exists as an option
    /// rather than a number: the env a tenant pod gets is a CLOSED list built
    /// here, so a knob the daemon reads is a knob a hosted tenant cannot reach
    /// unless the warden renders it. `0` pins the session in memory, which is
    /// the off switch if the reaper ever misbehaves in production.
    pub embed_idle_unload_secs: Option<u64>,
    /// How many days back tenant daemons keep searchable archive mail, passed
    /// on as `SQUELCH_ARCHIVE_DAYS`. UNSET RENDERS `0`, NOT NOTHING: the
    /// archive is OFF for hosted tenants until an operator turns it on. The
    /// daemon's own default is a year, which suits a self-hoster's disk and
    /// not a tenant volume nobody has sized for it; left to that default the
    /// whole fleet would start walking at the next daemon roll. Enable it per
    /// fleet after sizing one tenant. Clamped to [`ARCHIVE_DAYS_MAX`].
    pub archive_days: Option<u32>,
    /// The archive walk's free-space floor in MiB, passed on as
    /// `SQUELCH_ARCHIVE_MIN_FREE_MB`. Unset leaves the daemon's own default
    /// (2048); `0` switches the check off.
    pub archive_min_free_mb: Option<u64>,
    /// The OAuth authorization server tenant daemons trust for the AGENT door,
    /// from `SQUELCH_WARDEN_MCP_OAUTH_ISSUER`. Absent, the agent door stays
    /// unpublished and the render is byte-identical to a warden that never
    /// heard of it: no `SQUELCH_MCP_*` variable in any pod, no `/mcp` path on
    /// any Ingress.
    ///
    /// Present, it does two things that only make sense together. Every tenant
    /// pod is told its issuer and its own public resource URL
    /// (`SQUELCH_MCP_OAUTH_ISSUER`, `SQUELCH_MCP_PUBLIC_URL`), which is what
    /// puts the daemon's `/mcp` into OAuth mode; and every tenant Ingress
    /// publishes the agent door ([`crate::objects::AGENT_DOOR_PREFIXES`]). One
    /// knob for both, so an operator cannot publish a door without also telling
    /// the daemon behind it to lock it.
    ///
    /// An ORIGIN, always https: it is the `iss` every agent token is checked
    /// against, and the place agents fetch the authorization server's metadata
    /// from. See `deploy/hosted/SETUP.md`, "Publishing the agent door", for the
    /// order this may be turned on in.
    pub mcp_oauth_issuer: Option<String>,
    /// The APNs relay every tenant daemon pushes through, handed on as
    /// `SQUELCH_RELAY_URL`. Absent, the whole push feature is off: no relay
    /// vars, no token reference, byte-identical pods to a warden that never
    /// heard of it. That absence is why no hosted tenant had EVER received an
    /// iOS push before this field existed: the daemon spawns its pusher only
    /// when `SQUELCH_RELAY_URL` is set, and the env a tenant gets is a CLOSED
    /// list rendered here. Always https, for the gateway's reason: the relay is
    /// remote and its bearer must never cross the wire in the clear.
    pub relay_url: Option<String>,
    /// Secret holding the relay's push-only bearer (key `token`), in
    /// [`Config::namespace`].
    /// See [`DEFAULT_RELAY_SECRET_NAME`].
    pub relay_secret_name: String,
    /// The APNs topic tenant daemons push under, handed on as
    /// `SQUELCH_RELAY_TOPIC`. Refused at boot without [`Config::relay_url`].
    ///
    /// Optional here and NOT optional in practice: the relay allow-lists the
    /// Mac and iOS bundle ids and defaults to the FIRST, the Mac one, which
    /// APNs rejects for an iPhone's device token. Hosted tenants pair iPhones,
    /// so production sets the iOS bundle id explicitly.
    pub relay_topic: Option<String>,
}

impl std::fmt::Debug for Config {
    /// Manual, and the reason is the one field: `token`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("bind", &self.bind)
            .field("token", &"<redacted>")
            .field("base_domain", &self.base_domain)
            .field("image", &self.image)
            .field("namespace", &self.namespace)
            .field("ingress_namespace", &self.ingress_namespace)
            .field("ingress_pod_label", &self.ingress_pod_label)
            .field("ingress_class", &self.ingress_class)
            .field("tls_secret", &self.tls_secret)
            .field("oauth_secret_name", &self.oauth_secret_name)
            .field("console_sso_url", &self.console_sso_url)
            .field("storage_class", &self.storage_class)
            .field("storage_size", &self.storage_size)
            .field("daemon_resources", &self.daemon_resources)
            .field("tmp_size", &self.tmp_size)
            .field("pull_secret", &self.pull_secret)
            .field("user_namespaces", &self.user_namespaces)
            .field("model_pvc", &self.model_pvc)
            .field("embed_model", &self.embed_model)
            .field("node_cidr", &self.node_cidr)
            .field("http_readiness", &self.http_readiness)
            .field("run_as", &self.run_as)
            .field("min_ready_secs", &self.min_ready_secs)
            .field("ready_timeout", &self.ready_timeout)
            .field("pending_ttl", &self.pending_ttl)
            .field("trusted_proxy_hops", &self.trusted_proxy_hops)
            .field("llm_base_url", &self.llm_base_url)
            .field("llm_stage1_model", &self.llm_stage1_model)
            .field("llm_stage2_model", &self.llm_stage2_model)
            .field("llm_stage1_daily_cap", &self.llm_stage1_daily_cap)
            .field("llm_stage2_daily_cap", &self.llm_stage2_daily_cap)
            .field("embed_idle_unload_secs", &self.embed_idle_unload_secs)
            .field("archive_days", &self.archive_days)
            .field("archive_min_free_mb", &self.archive_min_free_mb)
            .field("mcp_oauth_issuer", &self.mcp_oauth_issuer)
            // The relay bearer is not a field at all: the warden holds only the
            // Secret's NAME, and the kubelet resolves the value into the pod.
            .field("relay_url", &self.relay_url)
            .field("relay_secret_name", &self.relay_secret_name)
            .field("relay_topic", &self.relay_topic)
            .finish()
    }
}

/// Where one variable comes from. The process environment in production, a map
/// in the tests.
type Lookup<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Read a var, treating whitespace-only as unset.
fn var(get: Lookup, name: &str) -> Option<String> {
    get(name)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// A var that must be a DNS-1123 label (a k8s object or namespace name).
///
/// Every one of these ends up as `metadata.name` or as a `namespaceSelector`
/// value. The API server would refuse a bad one, but it would refuse it on the
/// first signup rather than at boot, and the operator would be reading a kube
/// error instead of a sentence.
fn name_var(get: Lookup, name: &str, default: &str) -> Result<String, ConfigError> {
    let value = var(get, name).unwrap_or_else(|| default.to_string());
    if value.len() > 63 || !is_dns_label(&value) {
        return Err(ConfigError::invalid(format!(
            "{name} must be a Kubernetes name: 1 to 63 characters of a-z, 0-9 and hyphen, not starting or ending with a hyphen"
        )));
    }
    Ok(value)
}

/// A var that must be a Kubernetes quantity: digits, an optional decimal point,
/// and a suffix like `Gi`, `Mi` or `m`. Refused here rather than at the API
/// server so the operator hears it at boot instead of on the first signup.
fn quantity_var(get: Lookup, name: &str, default: &str) -> Result<String, ConfigError> {
    let value = var(get, name).unwrap_or_else(|| default.to_string());
    let shaped = value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'.')
        && value.starts_with(|c: char| c.is_ascii_digit());
    if !shaped {
        return Err(ConfigError::invalid(format!(
            "{name} must be a Kubernetes quantity (e.g. 10Gi, 256Mi, 100m)"
        )));
    }
    Ok(value)
}

/// A var that is a switch, defaulting to `default` when unset.
///
/// Anything that is neither spelling is REFUSED rather than read as off. A
/// switch here decides an isolation boundary or the shape of a probe, and a
/// typo that quietly meant "off" would leave an operator certain they had
/// turned something on.
fn switch_var(get: Lookup, name: &str, default: bool) -> Result<bool, ConfigError> {
    match var(get, name) {
        None => Ok(default),
        Some(v) => match v.to_ascii_lowercase().as_str() {
            "1" | "true" | "on" | "yes" => Ok(true),
            "0" | "false" | "off" | "no" => Ok(false),
            _ => Err(ConfigError::invalid(format!("{name} must be on or off"))),
        },
    }
}

/// A var that must be an optional Kubernetes object name.
fn optional_name_var(get: Lookup, name: &str) -> Result<Option<String>, ConfigError> {
    match var(get, name) {
        None => Ok(None),
        Some(raw) => {
            if !is_dns_label(&raw) || raw.len() > 63 {
                return Err(ConfigError::invalid(format!(
                    "{name} must be a Kubernetes name"
                )));
            }
            Ok(Some(raw))
        }
    }
}

/// `^[a-z0-9]([a-z0-9-]*[a-z0-9])?$`, hand-rolled.
pub(crate) fn is_dns_label(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-')
}

impl Config {
    /// Load and validate from the process environment.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_source(&|name| std::env::var(name).ok())
    }

    /// Load and validate from an arbitrary lookup.
    pub fn from_source(get: Lookup) -> Result<Self, ConfigError> {
        let bind_raw = var(get, "SQUELCH_WARDEN_BIND").unwrap_or_else(|| DEFAULT_BIND.to_string());
        let bind: SocketAddr = bind_raw.parse().map_err(|e| {
            ConfigError::invalid(format!("invalid SQUELCH_WARDEN_BIND `{bind_raw}`: {e}"))
        })?;

        // No default and no "generate one for you": a warden that invented its
        // own token would be a warden the control plane cannot reach, and a
        // warden with a guessable one can create workloads in the namespace
        // that holds every tenant's mail.
        let token = var(get, "SQUELCH_WARDEN_TOKEN").ok_or_else(|| {
            ConfigError::invalid(
                "SQUELCH_WARDEN_TOKEN is required (32+ characters of OS entropy: `openssl rand -base64 32`)",
            )
        })?;
        if token.len() < MIN_TOKEN_LEN {
            // The LENGTH is named, never the value.
            return Err(ConfigError::invalid(format!(
                "SQUELCH_WARDEN_TOKEN is {} characters; at least {MIN_TOKEN_LEN} are required",
                token.len()
            )));
        }

        let base_domain = var(get, "SQUELCH_WARDEN_BASE_DOMAIN").ok_or_else(|| {
            ConfigError::invalid(
                "SQUELCH_WARDEN_BASE_DOMAIN is required (the domain tenants are subdomains of, e.g. passband.email)",
            )
        })?;
        let base_domain = canonical_base_domain(&base_domain)?;

        let image = var(get, "SQUELCH_WARDEN_IMAGE").ok_or_else(|| {
            ConfigError::invalid(
                "SQUELCH_WARDEN_IMAGE is required (the squelchd image tenants run, e.g. ghcr.io/passband-app/squelchd:daemon-0.4.0)",
            )
        })?;
        let image = canonical_image(&image)?;

        let ingress_pod_label = {
            let raw = var(get, "SQUELCH_WARDEN_INGRESS_POD_LABEL")
                .unwrap_or_else(|| DEFAULT_INGRESS_POD_LABEL.to_string());
            let (key, value) = raw.split_once('=').ok_or_else(|| {
                ConfigError::invalid(
                    "SQUELCH_WARDEN_INGRESS_POD_LABEL must be `key=value` (e.g. app.kubernetes.io/name=traefik)",
                )
            })?;
            if key.is_empty() || value.is_empty() || raw.contains(char::is_whitespace) {
                return Err(ConfigError::invalid(
                    "SQUELCH_WARDEN_INGRESS_POD_LABEL must be `key=value` with no spaces",
                ));
            }
            (key.to_string(), value.to_string())
        };

        let user_namespaces = switch_var(get, "SQUELCH_WARDEN_USER_NAMESPACES", true)?;
        let http_readiness = switch_var(get, "SQUELCH_WARDEN_HTTP_READINESS", false)?;

        let run_as = match var(get, "SQUELCH_WARDEN_RUN_AS_UID") {
            None => DEFAULT_RUN_AS,
            Some(v) => {
                let uid: i64 = v.parse().map_err(|e| {
                    ConfigError::invalid(format!("invalid SQUELCH_WARDEN_RUN_AS_UID `{v}`: {e}"))
                })?;
                // Zero is the whole point of runAsNonRoot; the upper bound is
                // the conventional ceiling for a container id.
                if !(1000..=65533).contains(&uid) {
                    return Err(ConfigError::invalid(
                        "SQUELCH_WARDEN_RUN_AS_UID must be between 1000 and 65533 (never 0)",
                    ));
                }
                uid
            }
        };

        let ready_timeout = match var(get, "SQUELCH_WARDEN_READY_TIMEOUT_SECS") {
            None => DEFAULT_READY_TIMEOUT_SECS,
            Some(v) => {
                let secs: u64 = v.parse().map_err(|e| {
                    ConfigError::invalid(format!(
                        "invalid SQUELCH_WARDEN_READY_TIMEOUT_SECS `{v}`: {e}"
                    ))
                })?;
                if !(10..=900).contains(&secs) {
                    return Err(ConfigError::invalid(
                        "SQUELCH_WARDEN_READY_TIMEOUT_SECS must be between 10 and 900",
                    ));
                }
                secs
            }
        };

        let min_ready_secs = match var(get, "SQUELCH_WARDEN_MIN_READY_SECS") {
            None => DEFAULT_MIN_READY_SECS,
            Some(v) => {
                let secs: i32 = v.parse().map_err(|e| {
                    ConfigError::invalid(format!(
                        "invalid SQUELCH_WARDEN_MIN_READY_SECS `{v}`: {e}"
                    ))
                })?;
                if !(0..=MAX_MIN_READY_SECS).contains(&secs) {
                    return Err(ConfigError::invalid(format!(
                        "SQUELCH_WARDEN_MIN_READY_SECS must be between 0 and {MAX_MIN_READY_SECS} (0 turns the soak off)"
                    )));
                }
                secs
            }
        };
        // The soak is spent INSIDE every rollout wait: a reconcile blocks until
        // the replica is Available, and Available cannot arrive before the pod
        // has been Ready this long. A soak at or past the timeout would make
        // that wait unwinnable — every tenant would roll correctly and then be
        // reported as one that did not come back.
        if u64::from(min_ready_secs.unsigned_abs()) >= ready_timeout {
            return Err(ConfigError::invalid(format!(
                "SQUELCH_WARDEN_MIN_READY_SECS ({min_ready_secs}) must be below SQUELCH_WARDEN_READY_TIMEOUT_SECS ({ready_timeout}); a soak the rollout wait cannot outlast times out on every healthy tenant"
            )));
        }

        let pending_ttl = match var(get, "SQUELCH_WARDEN_PENDING_TTL_SECS") {
            None => DEFAULT_PENDING_TTL_SECS,
            Some(v) => {
                let secs: u64 = v.parse().map_err(|e| {
                    ConfigError::invalid(format!(
                        "invalid SQUELCH_WARDEN_PENDING_TTL_SECS `{v}`: {e}"
                    ))
                })?;
                if secs < MIN_PENDING_TTL_SECS {
                    return Err(ConfigError::invalid(format!(
                        "SQUELCH_WARDEN_PENDING_TTL_SECS must be at least {MIN_PENDING_TTL_SECS}; a shorter one would collect a signup that is still in progress"
                    )));
                }
                secs
            }
        };

        // Trusting a forwarded header is a claim about the deployment's own
        // topology that only the operator can make, so it is opt-in and the
        // default trusts nothing.
        let trusted_proxy_hops = match var(get, "SQUELCH_WARDEN_TRUSTED_PROXY_HOPS") {
            None => 0,
            Some(v) => {
                let hops: usize = v.parse().map_err(|e| {
                    ConfigError::invalid(format!(
                        "invalid SQUELCH_WARDEN_TRUSTED_PROXY_HOPS `{v}`: {e}"
                    ))
                })?;
                if hops > MAX_TRUSTED_PROXY_HOPS {
                    return Err(ConfigError::invalid(format!(
                        "SQUELCH_WARDEN_TRUSTED_PROXY_HOPS `{hops}` exceeds {MAX_TRUSTED_PROXY_HOPS}; set it to the number of proxies you run in front of this listener (1 for a single ingress controller)"
                    )));
                }
                hops
            }
        };

        let node_cidr = match var(get, "SQUELCH_WARDEN_NODE_CIDR") {
            None => None,
            Some(raw) => Some(canonical_cidr(&raw)?),
        };

        let console_sso_url = match var(get, "SQUELCH_WARDEN_CONSOLE_SSO_URL") {
            None => None,
            Some(raw) => Some(canonical_origin(&raw)?),
        };

        let llm_base_url = match var(get, "SQUELCH_WARDEN_LLM_BASE_URL") {
            None => None,
            Some(raw) => Some(canonical_https_url(
                &raw,
                "SQUELCH_WARDEN_LLM_BASE_URL",
                "the gateway is remote and a virtual key must never cross the wire in the clear",
            )?),
        };
        let llm_stage1_model = env_value_var(get, "SQUELCH_WARDEN_LLM_STAGE1_MODEL")?;
        let llm_stage2_model = env_value_var(get, "SQUELCH_WARDEN_LLM_STAGE2_MODEL")?;
        let llm_stage1_daily_cap = llm_cap_var(get, "SQUELCH_WARDEN_LLM_STAGE1_DAILY_CAP")?;
        let llm_stage2_daily_cap = llm_cap_var(get, "SQUELCH_WARDEN_LLM_STAGE2_DAILY_CAP")?;
        // The four tuning vars mean nothing without a gateway: a model pin the
        // pods never receive is an operator believing something false about
        // what their tenants run, so it is a refusal to boot instead.
        if llm_base_url.is_none() {
            for (name, set) in [
                (
                    "SQUELCH_WARDEN_LLM_STAGE1_MODEL",
                    llm_stage1_model.is_some(),
                ),
                (
                    "SQUELCH_WARDEN_LLM_STAGE2_MODEL",
                    llm_stage2_model.is_some(),
                ),
                (
                    "SQUELCH_WARDEN_LLM_STAGE1_DAILY_CAP",
                    llm_stage1_daily_cap.is_some(),
                ),
                (
                    "SQUELCH_WARDEN_LLM_STAGE2_DAILY_CAP",
                    llm_stage2_daily_cap.is_some(),
                ),
            ] {
                if set {
                    return Err(ConfigError::invalid(format!(
                        "{name} is set but SQUELCH_WARDEN_LLM_BASE_URL is not; without a gateway URL the value would never reach a tenant"
                    )));
                }
            }
        }
        let relay_url = match var(get, "SQUELCH_WARDEN_RELAY_URL") {
            None => None,
            Some(raw) => Some(canonical_https_url(
                &raw,
                "SQUELCH_WARDEN_RELAY_URL",
                "the relay is remote and its bearer must never cross the wire in the clear",
            )?),
        };
        let relay_topic = env_value_var(get, "SQUELCH_WARDEN_RELAY_TOPIC")?;
        // Same posture as the LLM tuning vars: a topic the pods never receive
        // is an operator believing their tenants push under it.
        if relay_url.is_none() && relay_topic.is_some() {
            return Err(ConfigError::invalid(
                "SQUELCH_WARDEN_RELAY_TOPIC is set but SQUELCH_WARDEN_RELAY_URL is not; without a relay URL the value would never reach a tenant",
            ));
        }
        let embed_idle_unload_secs = match var(get, "SQUELCH_WARDEN_EMBED_IDLE_UNLOAD_SECS") {
            None => None,
            Some(value) => {
                let secs = value.parse::<u64>().map_err(|e| {
                    ConfigError::invalid(format!(
                        "invalid SQUELCH_WARDEN_EMBED_IDLE_UNLOAD_SECS `{value}`: {e}"
                    ))
                })?;
                if secs > MAX_EMBED_IDLE_UNLOAD_SECS {
                    return Err(ConfigError::invalid(format!(
                        "SQUELCH_WARDEN_EMBED_IDLE_UNLOAD_SECS `{secs}` exceeds {MAX_EMBED_IDLE_UNLOAD_SECS} (thirty days); 0 is the off switch, anything longer than a month is a typo"
                    )));
                }
                Some(secs)
            }
        };

        let archive_days = match var(get, "SQUELCH_WARDEN_ARCHIVE_DAYS") {
            None => None,
            Some(value) => Some(
                value
                    .parse::<u32>()
                    .map_err(|e| {
                        ConfigError::invalid(format!(
                            "invalid SQUELCH_WARDEN_ARCHIVE_DAYS `{value}`: {e}"
                        ))
                    })?
                    // The daemon clamps too; clamping here keeps the rendered
                    // pod spec saying what the daemon will actually do.
                    .min(ARCHIVE_DAYS_MAX),
            ),
        };
        let archive_min_free_mb = match var(get, "SQUELCH_WARDEN_ARCHIVE_MIN_FREE_MB") {
            None => None,
            Some(value) => Some(value.parse::<u64>().map_err(|e| {
                ConfigError::invalid(format!(
                    "invalid SQUELCH_WARDEN_ARCHIVE_MIN_FREE_MB `{value}`: {e}"
                ))
            })?),
        };

        let mcp_oauth_issuer = match var(get, "SQUELCH_WARDEN_MCP_OAUTH_ISSUER") {
            None => None,
            Some(raw) => Some(canonical_mcp_oauth_issuer(&raw)?),
        };

        // Tombstone for the shared-key bridge: the var no longer exists, and a
        // manifest that still sets it belongs to an operator who believes the
        // shared Secret still feeds new tenants. Same posture as the inert
        // tuning vars above — refuse to boot rather than silently ignore.
        if var(get, "SQUELCH_WARDEN_ANTHROPIC_SECRET_NAME").is_some() {
            return Err(ConfigError::invalid(
                "SQUELCH_WARDEN_ANTHROPIC_SECRET_NAME was removed with the shared-key bridge; \
                 the gateway (SQUELCH_WARDEN_LLM_BASE_URL) is the only LLM path now. Remove the \
                 var, then delete the anthropic-api-key Secret once no legacy pod still mounts it",
            ));
        }

        Ok(Self {
            bind,
            token,
            base_domain,
            image,
            namespace: name_var(
                get,
                "SQUELCH_WARDEN_TENANT_NAMESPACE",
                DEFAULT_TENANT_NAMESPACE,
            )?,
            ingress_namespace: name_var(
                get,
                "SQUELCH_WARDEN_INGRESS_NAMESPACE",
                DEFAULT_INGRESS_NAMESPACE,
            )?,
            ingress_pod_label,
            ingress_class: name_var(get, "SQUELCH_WARDEN_INGRESS_CLASS", DEFAULT_INGRESS_CLASS)?,
            tls_secret: name_var(get, "SQUELCH_WARDEN_TLS_SECRET", DEFAULT_TLS_SECRET)?,
            oauth_secret_name: name_var(
                get,
                "SQUELCH_WARDEN_OAUTH_SECRET_NAME",
                DEFAULT_OAUTH_SECRET_NAME,
            )?,
            console_sso_url,
            storage_class: name_var(get, "SQUELCH_WARDEN_STORAGE_CLASS", DEFAULT_STORAGE_CLASS)?,
            storage_size: quantity_var(get, "SQUELCH_WARDEN_STORAGE_SIZE", DEFAULT_STORAGE_SIZE)?,
            daemon_resources: Resources {
                cpu_request: quantity_var(get, "SQUELCH_WARDEN_CPU_REQUEST", DEFAULT_CPU_REQUEST)?,
                cpu_limit: quantity_var(get, "SQUELCH_WARDEN_CPU_LIMIT", DEFAULT_CPU_LIMIT)?,
                memory_request: quantity_var(
                    get,
                    "SQUELCH_WARDEN_MEMORY_REQUEST",
                    DEFAULT_MEMORY_REQUEST,
                )?,
                memory_limit: quantity_var(
                    get,
                    "SQUELCH_WARDEN_MEMORY_LIMIT",
                    DEFAULT_MEMORY_LIMIT,
                )?,
                ephemeral_request: quantity_var(
                    get,
                    "SQUELCH_WARDEN_EPHEMERAL_REQUEST",
                    DEFAULT_EPHEMERAL_REQUEST,
                )?,
                ephemeral_limit: quantity_var(
                    get,
                    "SQUELCH_WARDEN_EPHEMERAL_LIMIT",
                    DEFAULT_EPHEMERAL_LIMIT,
                )?,
            },
            tmp_size: quantity_var(get, "SQUELCH_WARDEN_TMP_SIZE", DEFAULT_TMP_SIZE)?,
            pull_secret: optional_name_var(get, "SQUELCH_WARDEN_IMAGE_PULL_SECRET")?,
            user_namespaces,
            model_pvc: optional_name_var(get, "SQUELCH_WARDEN_MODEL_PVC")?,
            embed_model: env_value_var(get, "SQUELCH_WARDEN_EMBED_MODEL")?,
            node_cidr,
            http_readiness,
            run_as,
            min_ready_secs,
            ready_timeout: Duration::from_secs(ready_timeout),
            pending_ttl: Duration::from_secs(pending_ttl),
            trusted_proxy_hops,
            llm_base_url,
            llm_stage1_model,
            llm_stage2_model,
            llm_stage1_daily_cap,
            llm_stage2_daily_cap,
            embed_idle_unload_secs,
            archive_days,
            archive_min_free_mb,
            mcp_oauth_issuer,
            relay_url,
            relay_secret_name: name_var(
                get,
                "SQUELCH_WARDEN_RELAY_SECRET_NAME",
                DEFAULT_RELAY_SECRET_NAME,
            )?,
            relay_topic,
        })
    }

    /// `<label>.<base_domain>`.
    pub fn hostname(&self, label: &str) -> String {
        format!("{label}.{}", self.base_domain)
    }

    /// The URL a paired device talks to. Always https: the pairing code and the
    /// device token it buys cross the public internet.
    pub fn tenant_url(&self, label: &str) -> String {
        format!("https://{}", self.hostname(label))
    }
}

/// Validate the base domain: a bare hostname, nothing else.
///
/// A scheme, a path, or a port would each survive into an Ingress `host` and
/// into the URL baked into a pairing deep link, where the failure shows up as a
/// device that cannot connect and no explanation.
fn canonical_base_domain(raw: &str) -> Result<String, ConfigError> {
    let domain = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    if domain.is_empty() {
        return Err(ConfigError::invalid("SQUELCH_WARDEN_BASE_DOMAIN is empty"));
    }
    if domain.contains("://") || domain.contains('/') || domain.contains(':') {
        return Err(ConfigError::invalid(
            "SQUELCH_WARDEN_BASE_DOMAIN must be a bare domain with no scheme, port or path (e.g. passband.email)",
        ));
    }
    if !domain.contains('.') {
        return Err(ConfigError::invalid(
            "SQUELCH_WARDEN_BASE_DOMAIN must be a domain with at least one dot (e.g. passband.email)",
        ));
    }
    if domain.starts_with('.')
        || domain.contains("..")
        || !domain
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'.'))
    {
        return Err(ConfigError::invalid(
            "SQUELCH_WARDEN_BASE_DOMAIN is not a valid domain name",
        ));
    }
    Ok(domain)
}

/// Validate the tenant image reference.
///
/// It lands in a typed `image` field, so nothing here is stopping an injection.
/// What it stops is a value with a space or a newline in it, which the API
/// server accepts and the kubelet then fails to pull with an error the operator
/// reads three layers down.
fn canonical_image(raw: &str) -> Result<String, ConfigError> {
    let image = raw.trim();
    if image.len() > 512 {
        return Err(ConfigError::invalid(
            "SQUELCH_WARDEN_IMAGE is longer than 512 characters",
        ));
    }
    // Everything an OCI reference may contain and nothing else.
    if !image
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'/' | b':' | b'@'))
    {
        return Err(ConfigError::invalid(
            "SQUELCH_WARDEN_IMAGE is not an image reference (letters, digits and . - _ / : @ only)",
        ));
    }
    // An untagged image is `:latest`, which means a tenant's daemon version is
    // whatever the registry happened to hold the last time the pod restarted.
    if !(image.contains(':') || image.contains('@')) {
        return Err(ConfigError::invalid(
            "SQUELCH_WARDEN_IMAGE must carry a tag or a digest; an untagged image means a tenant silently upgrades on restart",
        ));
    }
    Ok(image.to_string())
}

/// Validate the console SSO origin: a scheme, a host, and nothing after them.
///
/// It rides into every tenant pod and out again as the `href` on the console's
/// sign-in link, so the failure modes are a browser's rather than an API
/// server's. A value with no scheme would resolve RELATIVE to the tenant's own
/// origin, which is a link that sends the user back to the page they are already
/// on; a path or a query on the end would be silently overwritten, because the
/// daemon builds the whole URL itself from this origin plus the tenant label.
///
/// The daemon applies the same rule in `squelch-api`'s `with_console_sso_url`,
/// where an unusable value is simply dropped and the button does not render.
/// Here it is a refusal to boot instead, because an operator who set it meant to
/// have console SSO and a silently missing button is not a thing anyone notices.
fn canonical_origin(raw: &str) -> Result<String, ConfigError> {
    let url = raw.trim().trim_end_matches('/');
    if url.len() > 512 {
        return Err(ConfigError::invalid(
            "SQUELCH_WARDEN_CONSOLE_SSO_URL is longer than 512 characters",
        ));
    }
    let lower = url.to_ascii_lowercase();
    let host = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .ok_or_else(|| {
            ConfigError::invalid(
                "SQUELCH_WARDEN_CONSOLE_SSO_URL must start with https:// or http:// (e.g. https://signup.passband.app)",
            )
        })?;
    if host.is_empty() {
        return Err(ConfigError::invalid(
            "SQUELCH_WARDEN_CONSOLE_SSO_URL has a scheme and no host",
        ));
    }
    // Origin only: the daemon appends the route and the tenant label itself.
    if host.contains('/') || host.contains('?') || host.contains('#') {
        return Err(ConfigError::invalid(
            "SQUELCH_WARDEN_CONSOLE_SSO_URL must be an origin with no path, query or fragment (e.g. https://signup.passband.app)",
        ));
    }
    if url.contains(char::is_whitespace) {
        return Err(ConfigError::invalid(
            "SQUELCH_WARDEN_CONSOLE_SSO_URL contains whitespace",
        ));
    }
    Ok(url.to_string())
}

/// Validate the node CIDR: an address and a prefix length, both parsed.
///
/// It becomes an `ipBlock` that OPENS a hole in every tenant's ingress policy,
/// so a typo here is a policy that allows something nobody meant to allow.
/// `0.0.0.0/0` is refused by name for the same reason.
fn canonical_cidr(raw: &str) -> Result<String, ConfigError> {
    let value = raw.trim();
    let (addr, prefix) = value.split_once('/').ok_or_else(|| {
        ConfigError::invalid(
            "SQUELCH_WARDEN_NODE_CIDR must be a CIDR block with a prefix length (e.g. 10.0.0.5/32)",
        )
    })?;
    let addr: IpAddr = addr.parse().map_err(|_| {
        ConfigError::invalid("SQUELCH_WARDEN_NODE_CIDR does not start with an IP address")
    })?;
    let bits: u8 = prefix.parse().map_err(|_| {
        ConfigError::invalid("SQUELCH_WARDEN_NODE_CIDR has a prefix length that is not a number")
    })?;
    let max = if addr.is_ipv4() { 32 } else { 128 };
    if bits > max {
        return Err(ConfigError::invalid(format!(
            "SQUELCH_WARDEN_NODE_CIDR prefix length must be between 0 and {max}"
        )));
    }
    if bits == 0 {
        return Err(ConfigError::invalid(
            "SQUELCH_WARDEN_NODE_CIDR must name the node network, not every address; a /0 would let anything reach every tenant",
        ));
    }
    Ok(format!("{addr}/{bits}"))
}

/// Validate a remote service URL that a bearer rides to: https, a host, and
/// nothing that would break an environment value. `name` is the variable the
/// operator set and `why` is the sentence that explains the https rule.
///
/// Shared by the LLM gateway URL (`SQUELCH_ANTHROPIC_BASE_URL` in every pod)
/// and the APNs relay URL (`SQUELCH_RELAY_URL`). Both land verbatim in every
/// tenant pod, so the checks are the boot-time versions of the two ways each
/// fails silently later: a non-https scheme sends every tenant's credential
/// over the wire in the clear, and a space or control byte becomes an env value
/// every daemon rejects on its first call, hours after boot. The trailing slash
/// is trimmed so two operator spellings of one service are one string.
fn canonical_https_url(raw: &str, name: &str, why: &str) -> Result<String, ConfigError> {
    let url = raw.trim().trim_end_matches('/');
    let Some(rest) = url.strip_prefix("https://") else {
        return Err(ConfigError::invalid(format!(
            "{name} must be an https:// URL; {why}"
        )));
    };
    if rest.is_empty() || rest.starts_with('/') {
        return Err(ConfigError::invalid(format!("{name} has no host")));
    }
    if !url
        .bytes()
        .all(|b| (b'!'..=b'~').contains(&b) && !matches!(b, b'"' | b'\'' | b'\\' | b'$' | b'`'))
    {
        return Err(ConfigError::invalid(format!(
            "{name} contains a character that is not allowed in a URL"
        )));
    }
    // Three characters a base URL never needs and each of which turns it into
    // something else. `@` makes everything before it USERINFO, so
    // `https://relay@evil.example` is a request to evil.example carrying the
    // bearer. `?` and `#` swallow whatever path the daemon appends after them
    // (`/v1/push`, `/v1/messages`) into a query or a fragment, so every call
    // lands on the bare host with a credential attached.
    if url.contains(['@', '?', '#']) {
        return Err(ConfigError::invalid(format!(
            "{name} must be a plain base URL with no `@` (userinfo would send the bearer to the host after it), `?` or `#` (either would swallow the path the daemon appends)"
        )));
    }
    Ok(url.to_string())
}

/// Validate the agent door's OAuth issuer: `https://`, a host, and NOTHING
/// else, not even a port.
///
/// It lands verbatim in every tenant pod as `SQUELCH_MCP_OAUTH_ISSUER`, where
/// it is two things at once: the value every agent token's `iss` must equal,
/// and the authorization server the daemon's protected-resource metadata
/// sends agents to. Both fail silently when the value is wrong, which is why
/// this is strict and a refusal to boot:
///
/// - **https only.** Agents fetch the authorization server's metadata and the
///   daemon fetches its signing keys from this origin. Over plain http either
///   fetch can be answered by anyone on the path, and a forged JWKS is a forged
///   token for every mailbox in the fleet.
/// - **an origin, no path.** AuthKit issuers are bare origins
///   (`https://<name>.authkit.app`). A value with a path on it is almost always
///   a pasted endpoint (`.../oauth2/jwks`, `.../.well-known/...`), and since the
///   daemon compares `iss` as an exact string, it would refuse every token
///   while the door looked perfectly configured.
/// - **no port, not even `:443`.** A tenant pod's NetworkPolicy allows egress
///   on TCP 443 and nothing else, so a daemon told `:8443` can never fetch the
///   signing keys; and AuthKit's `iss` carries no port, so an explicit `:443`
///   is the same server and a string no token will ever equal. Either way every
///   token is refused with no hint why, so it is a refusal to boot instead.
/// - **no query, fragment, userinfo or whitespace**, and a lowercase host, for
///   the same exact-compare reason: a host typed in capitals is the same server
///   and a different string.
///
/// One trailing `/` is trimmed, matching the daemon's own comparison rule, so
/// two operator spellings of one issuer are one string.
fn canonical_mcp_oauth_issuer(raw: &str) -> Result<String, ConfigError> {
    const NAME: &str = "SQUELCH_WARDEN_MCP_OAUTH_ISSUER";
    let url = raw.trim();
    if url.len() > 512 {
        return Err(ConfigError::invalid(format!(
            "{NAME} is longer than 512 characters"
        )));
    }
    let Some(authority) = url.strip_prefix("https://") else {
        return Err(ConfigError::invalid(format!(
            "{NAME} must be an https:// URL (e.g. https://example.authkit.app); it is the issuer every agent token is checked against, and its keys must never be fetched in the clear"
        )));
    };
    // The one trailing slash, trimmed AFTER the scheme is known, so `https://`
    // on its own is a missing host rather than a missing scheme.
    let authority = authority.strip_suffix('/').unwrap_or(authority);
    if authority.is_empty() || authority.starts_with([':', '/', '?', '#']) {
        return Err(ConfigError::invalid(format!("{NAME} has no host")));
    }
    if authority.contains(['/', '?', '#']) {
        return Err(ConfigError::invalid(format!(
            "{NAME} must be an issuer origin with no path, query or fragment (e.g. https://example.authkit.app)"
        )));
    }
    if authority.contains(':') {
        return Err(ConfigError::invalid(format!(
            "{NAME} must not carry a port, not even :443: tenant pods may only reach the issuer on 443, and AuthKit's token issuer has no port in it, so any explicit port makes every agent token fail (e.g. https://example.authkit.app)"
        )));
    }
    let host = authority;
    let host_ok = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'.'))
        && !host.starts_with(['.', '-'])
        && !host.ends_with(['.', '-'])
        && !host.contains("..");
    if !host_ok {
        return Err(ConfigError::invalid(format!(
            "{NAME} is not a valid issuer: a lowercase host name, nothing else (e.g. https://example.authkit.app)"
        )));
    }
    Ok(format!("https://{authority}"))
}

/// An optional free-form value (a model id, an embedding `model_code`):
/// printable, no whitespace, no shell metacharacters, because it becomes an
/// environment value in every tenant pod.
fn env_value_var(get: Lookup, name: &str) -> Result<Option<String>, ConfigError> {
    match var(get, name) {
        None => Ok(None),
        Some(value) => {
            if !value.bytes().all(|b| {
                (b'!'..=b'~').contains(&b) && !matches!(b, b'"' | b'\'' | b'\\' | b'$' | b'`')
            }) {
                return Err(ConfigError::invalid(format!(
                    "{name} contains a character that is not allowed in an environment value"
                )));
            }
            Ok(Some(value))
        }
    }
}

/// An optional per-day call ceiling. A number, refused here rather than parsed
/// leniently by every daemon it reaches.
fn llm_cap_var(get: Lookup, name: &str) -> Result<Option<u32>, ConfigError> {
    match var(get, name) {
        None => Ok(None),
        Some(value) => {
            let cap: u32 = value
                .parse()
                .map_err(|e| ConfigError::invalid(format!("invalid {name} `{value}`: {e}")))?;
            Ok(Some(cap))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::testing::test_config;

    /// The minimum environment a warden boots from: everything else has a
    /// default, and these three have none on purpose.
    fn required() -> BTreeMap<String, String> {
        BTreeMap::from([
            (
                "SQUELCH_WARDEN_TOKEN".to_string(),
                "0123456789abcdef0123456789abcdef".to_string(),
            ),
            (
                "SQUELCH_WARDEN_BASE_DOMAIN".to_string(),
                "passband.email".to_string(),
            ),
            (
                "SQUELCH_WARDEN_IMAGE".to_string(),
                "ghcr.io/passband-app/squelchd:daemon-0.4.0".to_string(),
            ),
        ])
    }

    /// Load from a map instead of the process environment, so the table below
    /// runs in parallel and mutates nothing global.
    fn load(vars: &BTreeMap<String, String>) -> Result<Config, ConfigError> {
        Config::from_source(&|name| vars.get(name).cloned())
    }

    fn with(key: &str, value: &str) -> Result<Config, ConfigError> {
        let mut vars = required();
        vars.insert(key.to_string(), value.to_string());
        load(&vars)
    }

    fn without(key: &str) -> Result<Config, ConfigError> {
        let mut vars = required();
        vars.remove(key);
        load(&vars)
    }

    #[test]
    fn the_defaults_are_what_the_manifests_expect() {
        let c = load(&required()).unwrap();
        assert_eq!(c.bind.to_string(), DEFAULT_BIND);
        assert_eq!(c.namespace, "tenants");
        assert_eq!(c.ingress_namespace, "kube-system");
        assert_eq!(c.ingress_class, "traefik");
        assert_eq!(c.tls_secret, "passband-wildcard-tls");
        assert_eq!(c.oauth_secret_name, "google-oauth-client");
        assert_eq!(c.storage_class, "local-path");
        assert_eq!(c.storage_size, "10Gi");
        assert_eq!(c.daemon_resources.cpu_request, "100m");
        assert_eq!(c.daemon_resources.cpu_limit, "1000m");
        assert_eq!(c.daemon_resources.memory_request, "384Mi");
        assert_eq!(c.daemon_resources.memory_limit, "1Gi");
        assert_eq!(c.daemon_resources.ephemeral_request, "256Mi");
        assert_eq!(c.daemon_resources.ephemeral_limit, "1Gi");
        assert_eq!(c.tmp_size, "512Mi");
        assert_eq!(c.run_as, DEFAULT_RUN_AS);
        assert_eq!(c.ready_timeout, Duration::from_secs(180));
        assert_eq!(c.pending_ttl, Duration::from_secs(86_400));
        assert_eq!(c.trusted_proxy_hops, 0);
        assert_eq!(c.min_ready_secs, DEFAULT_MIN_READY_SECS);
        assert!(c.user_namespaces);
        // OFF, and it has to stay off: an HTTP probe against a daemon image
        // that predates `/healthz` never passes, so a fleet that is not yet on
        // a capable daemon would go NotReady tenant by tenant. See
        // `Config::http_readiness`.
        assert!(!c.http_readiness);
        assert!(c.node_cidr.is_none());
        assert!(c.model_pvc.is_none());
        // Unset means the daemon's own pinned embedding model, which is what a
        // fleet should be running unless somebody deliberately moved it.
        assert!(c.embed_model.is_none());
        assert!(c.pull_secret.is_none());
        assert!(c.console_sso_url.is_none());
        // Unset means the tenant daemon keeps its own idle-unload default.
        assert!(c.embed_idle_unload_secs.is_none());
        // Unset means the tenant daemon keeps its own archive depth.
        assert!(c.archive_days.is_none());
        // The LLM env feature is off unless the operator turns it on.
        assert!(c.llm_base_url.is_none());
        assert!(c.llm_stage1_model.is_none());
        assert!(c.llm_stage2_model.is_none());
        assert!(c.llm_stage1_daily_cap.is_none());
        assert!(c.llm_stage2_daily_cap.is_none());
        // The agent door stays unpublished unless the operator turns it on.
        assert!(c.mcp_oauth_issuer.is_none());
        // Push through the relay is off unless the operator turns it on, and
        // the Secret name has a default so turning it on is one line.
        assert!(c.relay_url.is_none());
        assert!(c.relay_topic.is_none());
        assert_eq!(c.relay_secret_name, "apns-relay");
    }

    /// The boot-refusal table. Every one of these is a value that would
    /// otherwise fail on the first signup, in a kube error, hours later.
    #[test]
    fn refuses_to_boot_on_a_bad_value() {
        // The three with no default.
        assert!(without("SQUELCH_WARDEN_TOKEN").is_err());
        assert!(without("SQUELCH_WARDEN_BASE_DOMAIN").is_err());
        assert!(without("SQUELCH_WARDEN_IMAGE").is_err());
        // Whitespace-only is unset, not a value.
        assert!(with("SQUELCH_WARDEN_TOKEN", "   ").is_err());

        let table: &[(&str, &str)] = &[
            // A token the length of a password somebody typed.
            ("SQUELCH_WARDEN_TOKEN", "hunter2"),
            ("SQUELCH_WARDEN_TOKEN", &"a".repeat(MIN_TOKEN_LEN - 1)),
            ("SQUELCH_WARDEN_BASE_DOMAIN", "https://passband.email"),
            ("SQUELCH_WARDEN_BASE_DOMAIN", "passband"),
            ("SQUELCH_WARDEN_IMAGE", "ghcr.io/passband-app/squelchd"),
            ("SQUELCH_WARDEN_BIND", "not-an-address"),
            // Namespaces and object names, which become metadata.name.
            ("SQUELCH_WARDEN_TENANT_NAMESPACE", "Tenants"),
            ("SQUELCH_WARDEN_TENANT_NAMESPACE", "-tenants"),
            ("SQUELCH_WARDEN_TENANT_NAMESPACE", "tenants/x"),
            ("SQUELCH_WARDEN_TENANT_NAMESPACE", &"a".repeat(64)),
            ("SQUELCH_WARDEN_INGRESS_NAMESPACE", "kube system"),
            ("SQUELCH_WARDEN_TLS_SECRET", "Wildcard"),
            ("SQUELCH_WARDEN_OAUTH_SECRET_NAME", "oauth_client"),
            ("SQUELCH_WARDEN_MODEL_PVC", "Models"),
            ("SQUELCH_WARDEN_IMAGE_PULL_SECRET", "GHCR"),
            // The ingress peer, which is a label rather than a name.
            ("SQUELCH_WARDEN_INGRESS_POD_LABEL", "traefik"),
            (
                "SQUELCH_WARDEN_INGRESS_POD_LABEL",
                "app.kubernetes.io/name=",
            ),
            ("SQUELCH_WARDEN_INGRESS_POD_LABEL", "a = b"),
            // Quantities.
            ("SQUELCH_WARDEN_STORAGE_SIZE", "ten gigs"),
            ("SQUELCH_WARDEN_STORAGE_SIZE", "-10Gi"),
            ("SQUELCH_WARDEN_CPU_REQUEST", "half a core"),
            ("SQUELCH_WARDEN_MEMORY_LIMIT", "1 Gi"),
            ("SQUELCH_WARDEN_TMP_SIZE", "unlimited"),
            ("SQUELCH_WARDEN_EPHEMERAL_LIMIT", "Mi512"),
            // Durations, at both ends.
            ("SQUELCH_WARDEN_READY_TIMEOUT_SECS", "9"),
            ("SQUELCH_WARDEN_READY_TIMEOUT_SECS", "901"),
            ("SQUELCH_WARDEN_READY_TIMEOUT_SECS", "forever"),
            ("SQUELCH_WARDEN_PENDING_TTL_SECS", "60"),
            ("SQUELCH_WARDEN_PENDING_TTL_SECS", "a day"),
            // Identity.
            ("SQUELCH_WARDEN_RUN_AS_UID", "0"),
            ("SQUELCH_WARDEN_RUN_AS_UID", "999"),
            ("SQUELCH_WARDEN_RUN_AS_UID", "65534"),
            ("SQUELCH_WARDEN_RUN_AS_UID", "root"),
            ("SQUELCH_WARDEN_MIN_READY_SECS", "-1"),
            ("SQUELCH_WARDEN_MIN_READY_SECS", "301"),
            ("SQUELCH_WARDEN_MIN_READY_SECS", "half a minute"),
            // A soak the rollout wait cannot outlast: every healthy tenant
            // would time out, one per roll, forever.
            ("SQUELCH_WARDEN_MIN_READY_SECS", "180"),
            // Flags and counts.
            ("SQUELCH_WARDEN_USER_NAMESPACES", "maybe"),
            ("SQUELCH_WARDEN_HTTP_READINESS", "maybe"),
            ("SQUELCH_WARDEN_TRUSTED_PROXY_HOPS", "9"),
            ("SQUELCH_WARDEN_TRUSTED_PROXY_HOPS", "-1"),
            // The one that opens a hole in every tenant's ingress policy.
            ("SQUELCH_WARDEN_NODE_CIDR", "10.0.0.5"),
            ("SQUELCH_WARDEN_NODE_CIDR", "10.0.0.5/33"),
            ("SQUELCH_WARDEN_NODE_CIDR", "0.0.0.0/0"),
            ("SQUELCH_WARDEN_NODE_CIDR", "the node"),
            // The console SSO origin, which becomes an href in a browser.
            // Schemeless would resolve against the tenant's own origin.
            ("SQUELCH_WARDEN_CONSOLE_SSO_URL", "signup.passband.app"),
            (
                "SQUELCH_WARDEN_CONSOLE_SSO_URL",
                "https://signup.passband.app/console/auth",
            ),
            (
                "SQUELCH_WARDEN_CONSOLE_SSO_URL",
                "https://signup.passband.app?tenant=alice",
            ),
            ("SQUELCH_WARDEN_CONSOLE_SSO_URL", "https://"),
            (
                "SQUELCH_WARDEN_CONSOLE_SSO_URL",
                "https://signup passband.app",
            ),
            ("SQUELCH_WARDEN_CONSOLE_SSO_URL", "javascript:alert(1)"),
            // The gateway URL: https or nothing, and always with a host.
            ("SQUELCH_WARDEN_LLM_BASE_URL", "http://llm.internal"),
            ("SQUELCH_WARDEN_LLM_BASE_URL", "llm.internal"),
            ("SQUELCH_WARDEN_LLM_BASE_URL", "https://"),
            ("SQUELCH_WARDEN_LLM_BASE_URL", "https:///v1"),
            // Userinfo sends the virtual key to the host after the `@`; a
            // query or fragment swallows the `/v1/messages` the daemon appends.
            (
                "SQUELCH_WARDEN_LLM_BASE_URL",
                "https://bifrost.passband.app@evil.example/anthropic",
            ),
            (
                "SQUELCH_WARDEN_LLM_BASE_URL",
                "https://bifrost.passband.app/anthropic?x=1",
            ),
            (
                "SQUELCH_WARDEN_LLM_BASE_URL",
                "https://bifrost.passband.app/anthropic#frag",
            ),
            // Each of the four tuning vars, orphaned: without a gateway URL
            // they would never reach a pod, so the operator hears it at boot.
            ("SQUELCH_WARDEN_LLM_STAGE1_MODEL", "claude-haiku-4-5"),
            ("SQUELCH_WARDEN_LLM_STAGE2_MODEL", "claude-sonnet-4-5"),
            ("SQUELCH_WARDEN_LLM_STAGE1_DAILY_CAP", "200"),
            ("SQUELCH_WARDEN_LLM_STAGE2_DAILY_CAP", "40"),
            // The shared-key bridge's var, removed with the bridge: a manifest
            // still setting it believes the shared Secret feeds new tenants.
            ("SQUELCH_WARDEN_ANTHROPIC_SECRET_NAME", "anthropic-api-key"),
            // The relay URL, for the gateway's reason: a bearer rides to it.
            ("SQUELCH_WARDEN_RELAY_URL", "http://relay.squelch.sh"),
            ("SQUELCH_WARDEN_RELAY_URL", "relay.squelch.sh"),
            ("SQUELCH_WARDEN_RELAY_URL", "https://"),
            ("SQUELCH_WARDEN_RELAY_URL", "https:///push"),
            ("SQUELCH_WARDEN_RELAY_URL", "https://relay squelch.sh"),
            // `https://relay@evil.example` is a request to evil.example with
            // the bearer attached, and `?` / `#` eat the `/v1/push` suffix.
            ("SQUELCH_WARDEN_RELAY_URL", "https://relay@evil.example"),
            (
                "SQUELCH_WARDEN_RELAY_URL",
                "https://user:pw@relay.squelch.sh",
            ),
            ("SQUELCH_WARDEN_RELAY_URL", "https://relay.squelch.sh?"),
            ("SQUELCH_WARDEN_RELAY_URL", "https://relay.squelch.sh/?a=b"),
            ("SQUELCH_WARDEN_RELAY_URL", "https://relay.squelch.sh#"),
            ("SQUELCH_WARDEN_RELAY_URL", "https://relay.squelch.sh/#x"),
            ("SQUELCH_WARDEN_RELAY_SECRET_NAME", "APNs_Relay"),
            // A topic orphaned from its URL would never reach a pod.
            ("SQUELCH_WARDEN_RELAY_TOPIC", "app.passband.client.ios"),
        ];
        for (key, value) in table {
            assert!(with(key, value).is_err(), "{key}=`{value}` was accepted");
        }
    }

    #[test]
    fn accepts_the_values_an_operator_actually_sets() {
        let c = with("SQUELCH_WARDEN_NODE_CIDR", " 10.0.0.5/32 ").unwrap();
        assert_eq!(c.node_cidr.as_deref(), Some("10.0.0.5/32"));
        let c = with("SQUELCH_WARDEN_NODE_CIDR", "fd00::1/128").unwrap();
        assert_eq!(c.node_cidr.as_deref(), Some("fd00::1/128"));

        assert!(
            !with("SQUELCH_WARDEN_USER_NAMESPACES", "off")
                .unwrap()
                .user_namespaces
        );
        // Every spelling of a switch, because a ConfigMap is edited by hand and
        // an operator who wrote `true` meant on.
        for on in ["on", "1", "true", "yes", "YES"] {
            assert!(
                with("SQUELCH_WARDEN_HTTP_READINESS", on)
                    .unwrap()
                    .http_readiness,
                "`{on}` must turn the HTTP probe on"
            );
        }
        for off in ["off", "0", "false", "no"] {
            assert!(
                !with("SQUELCH_WARDEN_HTTP_READINESS", off)
                    .unwrap()
                    .http_readiness,
                "`{off}` must leave the accept probe alone"
            );
        }
        // Zero is the soak turned off, and it is a value rather than a refusal:
        // an operator whose cluster cannot afford the wait is not making a
        // mistake, they are making a trade.
        assert_eq!(
            with("SQUELCH_WARDEN_MIN_READY_SECS", "0")
                .unwrap()
                .min_ready_secs,
            0
        );
        assert_eq!(
            with("SQUELCH_WARDEN_MIN_READY_SECS", " 45 ")
                .unwrap()
                .min_ready_secs,
            45
        );
        assert_eq!(
            with("SQUELCH_WARDEN_TRUSTED_PROXY_HOPS", "1")
                .unwrap()
                .trusted_proxy_hops,
            1
        );
        assert_eq!(
            with("SQUELCH_WARDEN_PENDING_TTL_SECS", "3600")
                .unwrap()
                .pending_ttl,
            Duration::from_secs(3600)
        );
        assert_eq!(
            with("SQUELCH_WARDEN_MEMORY_LIMIT", "2Gi")
                .unwrap()
                .daemon_resources
                .memory_limit,
            "2Gi"
        );
        assert_eq!(
            with("SQUELCH_WARDEN_OAUTH_SECRET_NAME", "passband-oauth")
                .unwrap()
                .oauth_secret_name,
            "passband-oauth"
        );
        // Trimmed, and the trailing slash removed: the daemon appends the route.
        assert_eq!(
            with(
                "SQUELCH_WARDEN_CONSOLE_SSO_URL",
                " https://signup.passband.app/ "
            )
            .unwrap()
            .console_sso_url
            .as_deref(),
            Some("https://signup.passband.app")
        );
        // http for a local control plane, and a port is part of an origin.
        assert_eq!(
            with("SQUELCH_WARDEN_CONSOLE_SSO_URL", "http://localhost:8853")
                .unwrap()
                .console_sso_url
                .as_deref(),
            Some("http://localhost:8853")
        );
    }

    /// The agent door's issuer: an https origin, or the door stays shut. Every
    /// refusal here is a value that would otherwise boot, render into every
    /// pod, and make every agent token fail an exact `iss` compare (or, for
    /// http, make the signing keys forgeable on the wire).
    #[test]
    fn the_mcp_oauth_issuer_is_an_https_origin_or_nothing() {
        const KEY: &str = "SQUELCH_WARDEN_MCP_OAUTH_ISSUER";
        let issuer = |value: &str| with(KEY, value).map(|c| c.mcp_oauth_issuer);

        // Accepted, trimmed, and one trailing slash removed: the daemon
        // compares `iss` with one trailing slash trimmed, so the two spellings
        // are one issuer and should be one string in every pod.
        for (value, want) in [
            ("https://example.authkit.app", "https://example.authkit.app"),
            (
                " https://example.authkit.app/ ",
                "https://example.authkit.app",
            ),
        ] {
            assert_eq!(issuer(value).unwrap().as_deref(), Some(want), "`{value}`");
        }

        // Blank is unset, not a refusal: the door stays shut.
        assert_eq!(issuer("").unwrap(), None);
        assert_eq!(issuer("   ").unwrap(), None);
        assert_eq!(without(KEY).unwrap().mcp_oauth_issuer, None);

        // Each refusal is checked for the SENTENCE it produces, not just for
        // being an error: several of these would also trip the host-shape
        // check, and an operator told "not a valid issuer" for a pasted JWKS
        // URL is an operator left guessing which part was wrong.
        const NOT_HTTPS: &str = "must be an https:// URL";
        const NO_HOST: &str = "has no host";
        const NOT_ORIGIN: &str = "no path, query or fragment";
        const BAD_HOST: &str = "is not a valid issuer";
        // ANY explicit port, the harmless-looking `:443` included: tenant
        // pods egress on 443 only, so another port never reaches the keys,
        // and AuthKit's `iss` has no port, so `:443` never equals it.
        const PORT: &str = "must not carry a port, not even :443";
        for (value, why) in [
            ("http://example.authkit.app", NOT_HTTPS),
            ("example.authkit.app", NOT_HTTPS),
            ("HTTPS://example.authkit.app", NOT_HTTPS),
            ("ftp://example.authkit.app", NOT_HTTPS),
            ("https://", NO_HOST),
            ("https:///", NO_HOST),
            ("https:///oauth2", NO_HOST),
            ("https://:443", NO_HOST),
            ("https://?x=1", NO_HOST),
            // A path: a pasted endpoint, never AuthKit's issuer.
            ("https://example.authkit.app/oauth2/jwks", NOT_ORIGIN),
            (
                "https://example.authkit.app/.well-known/oauth-authorization-server",
                NOT_ORIGIN,
            ),
            ("https://example.authkit.app//", NOT_ORIGIN),
            ("https://example.authkit.app?tenant=alice", NOT_ORIGIN),
            ("https://example.authkit.app/?x=1", NOT_ORIGIN),
            ("https://example.authkit.app#frag", NOT_ORIGIN),
            // Trailing garbage and things that are not a host.
            ("https://example.authkit.app junk", BAD_HOST),
            ("https://example\tauthkit.app", BAD_HOST),
            ("https://user@example.authkit.app", BAD_HOST),
            ("https://Example.AuthKit.app", BAD_HOST),
            ("https://example..authkit.app", BAD_HOST),
            ("https://.example.authkit.app", BAD_HOST),
            ("https://example.authkit.app:443", PORT),
            ("https://example.authkit.app:443/", PORT),
            ("https://auth.passband.app:8443", PORT),
            ("https://example.authkit.app:", PORT),
            ("https://example.authkit.app:http", PORT),
            ("https://example.authkit.app:+443", PORT),
            ("https://example.authkit.app:99999", PORT),
            ("https://example.authkit.app\"", BAD_HOST),
        ] {
            let err = match issuer(value) {
                Ok(c) => panic!("{KEY}=`{value}` was accepted as {c:?}"),
                Err(e) => e.to_string(),
            };
            assert!(
                err.contains(why),
                "{KEY}=`{value}`: `{err}` does not say `{why}`"
            );
            assert!(err.contains(KEY), "the refusal names the variable: `{err}`");
        }
        // Too long to be anything but a mistake.
        let long = format!("https://{}.authkit.app", "a".repeat(512));
        assert!(
            issuer(&long)
                .unwrap_err()
                .to_string()
                .contains("longer than 512")
        );
    }

    /// The whole LLM knob set, accepted together and validated together. The
    /// URL is normalized so two spellings of one gateway are one string.
    /// The embedder window parses as a number, and `0` survives as `Some(0)`:
    /// it is the off switch (never unload), not an absent setting.
    #[test]
    fn the_embedder_idle_window_parses_and_zero_is_a_value() {
        let mut vars = required();
        vars.insert(
            "SQUELCH_WARDEN_EMBED_IDLE_UNLOAD_SECS".to_string(),
            "900".to_string(),
        );
        assert_eq!(load(&vars).unwrap().embed_idle_unload_secs, Some(900));

        vars.insert(
            "SQUELCH_WARDEN_EMBED_IDLE_UNLOAD_SECS".to_string(),
            "0".to_string(),
        );
        assert_eq!(load(&vars).unwrap().embed_idle_unload_secs, Some(0));

        vars.insert(
            "SQUELCH_WARDEN_EMBED_IDLE_UNLOAD_SECS".to_string(),
            "ten minutes".to_string(),
        );
        assert!(load(&vars).is_err(), "a bad value is a refusal to boot");
    }

    /// The archive depth parses as a number of days, and `0` survives as
    /// `Some(0)`: it switches the walk off, which is not the same as unset.
    #[test]
    fn the_archive_depth_parses_and_zero_is_a_value() {
        let mut vars = required();
        vars.insert("SQUELCH_WARDEN_ARCHIVE_DAYS".to_string(), "730".to_string());
        assert_eq!(load(&vars).unwrap().archive_days, Some(730));

        vars.insert("SQUELCH_WARDEN_ARCHIVE_DAYS".to_string(), "0".to_string());
        assert_eq!(load(&vars).unwrap().archive_days, Some(0));

        // Past twenty years is clamped, so the pod spec names the depth the
        // daemon will really use.
        vars.insert(
            "SQUELCH_WARDEN_ARCHIVE_DAYS".to_string(),
            "4000000000".to_string(),
        );
        assert_eq!(load(&vars).unwrap().archive_days, Some(ARCHIVE_DAYS_MAX));
        assert_eq!(ARCHIVE_DAYS_MAX, 7300);

        assert_eq!(load(&vars).unwrap().archive_min_free_mb, None);
        vars.insert(
            "SQUELCH_WARDEN_ARCHIVE_MIN_FREE_MB".to_string(),
            "512".to_string(),
        );
        assert_eq!(load(&vars).unwrap().archive_min_free_mb, Some(512));
        vars.insert(
            "SQUELCH_WARDEN_ARCHIVE_MIN_FREE_MB".to_string(),
            "lots".to_string(),
        );
        assert!(load(&vars).is_err());
        vars.remove("SQUELCH_WARDEN_ARCHIVE_MIN_FREE_MB");

        vars.insert(
            "SQUELCH_WARDEN_ARCHIVE_DAYS".to_string(),
            "a year".to_string(),
        );
        assert!(load(&vars).is_err(), "a bad value is a refusal to boot");
    }

    #[test]
    fn accepts_a_full_llm_configuration() {
        let mut vars = required();
        vars.insert(
            "SQUELCH_WARDEN_LLM_BASE_URL".to_string(),
            " https://llm.passband.internal/ ".to_string(),
        );
        vars.insert(
            "SQUELCH_WARDEN_LLM_STAGE1_MODEL".to_string(),
            "claude-haiku-4-5".to_string(),
        );
        vars.insert(
            "SQUELCH_WARDEN_LLM_STAGE2_MODEL".to_string(),
            "claude-sonnet-4-5".to_string(),
        );
        vars.insert(
            "SQUELCH_WARDEN_LLM_STAGE1_DAILY_CAP".to_string(),
            "200".to_string(),
        );
        vars.insert(
            "SQUELCH_WARDEN_LLM_STAGE2_DAILY_CAP".to_string(),
            "40".to_string(),
        );
        let c = load(&vars).unwrap();
        assert_eq!(
            c.llm_base_url.as_deref(),
            Some("https://llm.passband.internal")
        );
        assert_eq!(c.llm_stage1_model.as_deref(), Some("claude-haiku-4-5"));
        assert_eq!(c.llm_stage2_model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(c.llm_stage1_daily_cap, Some(200));
        assert_eq!(c.llm_stage2_daily_cap, Some(40));

        // The URL alone is a complete configuration: the tuning vars are
        // overrides, not requirements.
        let mut vars = required();
        vars.insert(
            "SQUELCH_WARDEN_LLM_BASE_URL".to_string(),
            "https://llm.passband.internal".to_string(),
        );
        assert!(load(&vars).is_ok());

        // A cap that is not a number is refused even with the URL present.
        vars.insert(
            "SQUELCH_WARDEN_LLM_STAGE1_DAILY_CAP".to_string(),
            "many".to_string(),
        );
        assert!(load(&vars).is_err());
        vars.insert(
            "SQUELCH_WARDEN_LLM_STAGE1_DAILY_CAP".to_string(),
            "-1".to_string(),
        );
        assert!(load(&vars).is_err());
        // And a model with a space would break the env value it becomes.
        vars.remove("SQUELCH_WARDEN_LLM_STAGE1_DAILY_CAP");
        vars.insert(
            "SQUELCH_WARDEN_LLM_STAGE1_MODEL".to_string(),
            "claude haiku".to_string(),
        );
        assert!(load(&vars).is_err());
    }

    /// The relay block, accepted together. The URL is normalized the way the
    /// gateway's is, and each refusal names the variable the operator set
    /// rather than the LLM one whose validator it shares.
    #[test]
    fn accepts_a_full_relay_configuration() {
        let mut vars = required();
        vars.insert(
            "SQUELCH_WARDEN_RELAY_URL".to_string(),
            " https://relay.squelch.sh/ ".to_string(),
        );
        vars.insert(
            "SQUELCH_WARDEN_RELAY_SECRET_NAME".to_string(),
            "passband-relay".to_string(),
        );
        vars.insert(
            "SQUELCH_WARDEN_RELAY_TOPIC".to_string(),
            "app.passband.client.ios".to_string(),
        );
        let c = load(&vars).unwrap();
        assert_eq!(c.relay_url.as_deref(), Some("https://relay.squelch.sh"));
        assert_eq!(c.relay_secret_name, "passband-relay");
        assert_eq!(c.relay_topic.as_deref(), Some("app.passband.client.ios"));
        // Nothing about the relay turns the LLM block on, or the reverse.
        assert!(c.llm_base_url.is_none());

        // The URL alone is a complete configuration; the topic is an override.
        let c = with("SQUELCH_WARDEN_RELAY_URL", "https://relay.squelch.sh").unwrap();
        assert!(c.relay_topic.is_none());
        assert_eq!(c.relay_secret_name, "apns-relay");

        for (value, needle) in [
            ("http://relay.squelch.sh", "must be an https:// URL"),
            // A bare `https://` is refused too (the refusal table has it), but
            // as a scheme error: the trailing-slash trim eats the `//` first.
            ("https:///push", "has no host"),
        ] {
            let err = with("SQUELCH_WARDEN_RELAY_URL", value)
                .unwrap_err()
                .to_string();
            assert!(err.contains("SQUELCH_WARDEN_RELAY_URL"), "{err}");
            assert!(!err.contains("LLM"), "{err}");
            assert!(err.contains(needle), "{err}");
        }
        // Userinfo, query and fragment are refused, naming the variable and
        // never repeating the URL (it could be carrying a credential).
        for value in [
            "https://relay@evil.example",
            "https://relay.squelch.sh?x",
            "https://relay.squelch.sh#x",
        ] {
            let err = with("SQUELCH_WARDEN_RELAY_URL", value)
                .unwrap_err()
                .to_string();
            assert!(
                err.starts_with("SQUELCH_WARDEN_RELAY_URL must be a plain base URL"),
                "{err}"
            );
            assert!(
                !err.contains("evil.example") && !err.contains("relay.squelch.sh"),
                "{err}"
            );
        }
        let err = with("SQUELCH_WARDEN_LLM_BASE_URL", "https://a@b.example")
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("SQUELCH_WARDEN_LLM_BASE_URL must be a plain base URL"),
            "{err}"
        );

        // The values production runs today still pass, unchanged.
        let c = with(
            "SQUELCH_WARDEN_LLM_BASE_URL",
            "https://bifrost.passband.app/anthropic",
        )
        .unwrap();
        assert_eq!(
            c.llm_base_url.as_deref(),
            Some("https://bifrost.passband.app/anthropic")
        );
        let c = with("SQUELCH_WARDEN_RELAY_URL", "https://relay.squelch.sh").unwrap();
        assert_eq!(c.relay_url.as_deref(), Some("https://relay.squelch.sh"));

        let err = with("SQUELCH_WARDEN_RELAY_TOPIC", "app.passband.client.ios")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("SQUELCH_WARDEN_RELAY_TOPIC is set but SQUELCH_WARDEN_RELAY_URL is not"),
            "{err}"
        );

        // A topic with a space would break the env value it becomes.
        vars.insert(
            "SQUELCH_WARDEN_RELAY_TOPIC".to_string(),
            "app passband".to_string(),
        );
        assert!(load(&vars).is_err());
    }

    /// The embedding pin, which is NOT part of the LLM block and needs no
    /// gateway: it is a fastembed model name, not a call to anybody.
    #[test]
    fn the_embedding_model_is_optional_and_stands_alone() {
        let mut vars = required();
        vars.insert(
            "SQUELCH_WARDEN_EMBED_MODEL".to_string(),
            "Xenova/bge-small-en-v1.5".to_string(),
        );
        let c = load(&vars).unwrap();
        assert_eq!(c.embed_model.as_deref(), Some("Xenova/bge-small-en-v1.5"));
        // No gateway configured, and it is still accepted.
        assert!(c.llm_base_url.is_none());

        // A value with a space would break the env value it becomes.
        vars.insert(
            "SQUELCH_WARDEN_EMBED_MODEL".to_string(),
            "bge small".to_string(),
        );
        assert!(load(&vars).is_err());
    }

    /// The token is the one value that must never appear in a diagnostic, and
    /// `from_source` is where an operator's bad one is described back to them.
    #[test]
    fn a_refusal_never_quotes_the_token() {
        let rendered = with("SQUELCH_WARDEN_TOKEN", "hunter2")
            .unwrap_err()
            .to_string();
        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert!(rendered.contains("7 characters"), "{rendered}");
    }

    #[test]
    fn derives_tenant_hostnames_from_the_label() {
        let c = test_config();
        assert_eq!(c.hostname("alice"), "alice.passband.email");
        assert_eq!(c.tenant_url("alice"), "https://alice.passband.email");
    }

    #[test]
    fn debug_redacts_the_token() {
        let c = test_config();
        let rendered = format!("{c:?}");
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains(&c.token));
    }

    #[test]
    fn canonicalizes_the_base_domain() {
        assert_eq!(
            canonical_base_domain(" Passband.Email. ").unwrap(),
            "passband.email"
        );
        for bad in [
            "https://passband.email",
            "passband.email/",
            "passband.email:443",
            "passband",
            ".passband.email",
            "pass..band.email",
            "pass band.email",
            "",
        ] {
            assert!(canonical_base_domain(bad).is_err(), "{bad} was accepted");
        }
    }

    #[test]
    fn refuses_an_image_without_a_tag_or_a_digest() {
        assert!(canonical_image("ghcr.io/passband-app/squelchd:v1").is_ok());
        assert!(
            canonical_image(
                "ghcr.io/passband-app/squelchd@sha256:0000000000000000000000000000000000000000000000000000000000000000"
            )
            .is_ok()
        );
        // The one that would silently upgrade every tenant on a restart.
        assert!(canonical_image("ghcr.io/passband-app/squelchd").is_err());
        assert!(canonical_image("ghcr.io/x/y:v1 && rm -rf /").is_err());
        assert!(canonical_image("ghcr.io/x/y:v1\nSOMETHING=else").is_err());
    }

    #[test]
    fn accepts_only_kubernetes_names() {
        assert!(is_dns_label("tenants"));
        assert!(is_dns_label("kube-system"));
        assert!(!is_dns_label("-tenants"));
        assert!(!is_dns_label("tenants-"));
        assert!(!is_dns_label("Tenants"));
        assert!(!is_dns_label("tenants/x"));
        assert!(!is_dns_label(""));
    }
}
