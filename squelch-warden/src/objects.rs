//! Every Kubernetes object a tenant is made of, built as typed
//! `k8s-openapi` structs.
//!
//! **The hard rule of this crate lives here: nothing in squelch-warden renders
//! YAML.** There is no template, no YAML serializer, no `format!` that produces a
//! manifest. A tenant label reaches the cluster only as a [`TenantName`] placed
//! in a typed field, which is why [`crate::validate::TenantName`] cannot be
//! constructed without passing the label validator. The static infrastructure
//! that installs the warden itself is YAML, and it lives in `deploy/hosted/`
//! where no tenant value ever appears.
//!
//! A tenant is up to nine objects in the `tenants` namespace:
//!
//! | Object | Name | Lifetime |
//! |---|---|---|
//! | Secret (identity) | `<label>-identity` | forever; the tenant's age key |
//! | Secret (credential) | `<label>-credential` | forever; the sealed blob |
//! | PersistentVolumeClaim | `<label>-data` | forever; the mailbox index |
//! | Secret (LLM key) | `<label>-llm` | while the control plane keeps one minted |
//! | Secret (share token) | `<label>-control` | while the control plane keeps one minted |
//! | NetworkPolicy | `<label>` | while provisioned |
//! | Service | `<label>` | while provisioned |
//! | Deployment | `<label>` | while provisioned |
//! | Ingress | `<label>` | while provisioned |
//!
//! DELETE removes the bottom six and keeps the top three, which is what
//! "cancel my account, do not destroy my mail" means: the LLM Secret and the
//! share token go with the workload because they are live credentials against
//! shared services, not tenant data.
//!
//! The namespace's ResourceQuota (`deploy/hosted/70-tenant-limits.yaml`)
//! counts Secrets, pods and claims, so the number of each a tenant holds is
//! what sizes its `count/*` lines: the Secrets are the one that is not one,
//! and the tests here (`SECRETS_PER_TENANT`) hold the committed file to it.

use std::collections::BTreeMap;

use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec, DeploymentStrategy};
use k8s_openapi::api::core::v1::{
    Capabilities, Container, ContainerPort, EmptyDirVolumeSource, EnvVar, EnvVarSource,
    HTTPGetAction, KeyToPath, LocalObjectReference, PersistentVolumeClaim,
    PersistentVolumeClaimSpec, PersistentVolumeClaimVolumeSource, PodSecurityContext, PodSpec,
    PodTemplateSpec, Probe, ResourceRequirements, SeccompProfile, Secret, SecretKeySelector,
    SecretVolumeSource, SecurityContext, Service, ServicePort, ServiceSpec, TCPSocketAction,
    Volume, VolumeMount, VolumeResourceRequirements,
};
use k8s_openapi::api::networking::v1::{
    HTTPIngressPath, HTTPIngressRuleValue, IPBlock, Ingress, IngressBackend, IngressRule,
    IngressServiceBackend, IngressSpec, IngressTLS, NetworkPolicy, NetworkPolicyEgressRule,
    NetworkPolicyIngressRule, NetworkPolicyPeer, NetworkPolicyPort, NetworkPolicySpec,
    ServiceBackendPort,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;

use sha2::{Digest, Sha256};

use crate::config::{Config, Resources};
use crate::identity::TenantIdentity;
use crate::validate::TenantName;

/// The port the daemon binds inside its pod. Every pod has its own network
/// namespace, so this is invisible outside the Service: there is no allocator
/// and no collision, and every tenant uses the same number.
pub const DAEMON_PORT: i32 = 8848;

/// The port the daemon serves Prometheus text metrics on, when
/// `SQUELCH_METRICS_BIND` is set. A listener with no authentication of any
/// kind, which is why it is a second port and not a route on the first.
///
/// **It is deliberately absent from the Service and from the Ingress**, and
/// that absence is the isolation argument: an Ingress can only send traffic to
/// a port its backing Service declares, so no allowlist mistake in
/// [`HUMAN_DOOR_PREFIXES`] or [`AGENT_DOOR_PREFIXES`] can publish this port
/// through a tenant vhost. The
/// only thing that reaches it is the one [`network_policy`] ingress rule that
/// names the monitoring namespace's prometheus-agent pod.
pub const METRICS_PORT: i32 = 9464;

/// The tenant's own volume: SQLite index, the writable credentials file, and
/// the embedding-weights cache under `HOME`.
pub const DATA_DIR: &str = "/data";

/// Where the identity Secret is projected. Read-only, one file.
pub const IDENTITY_DIR: &str = "/etc/squelch/identity";

/// `SQUELCH_CRED_AGE_IDENTITY`.
pub const IDENTITY_FILE: &str = "/etc/squelch/identity/identity.txt";

/// Where the credential Secret is projected, for the init container only. The
/// daemon container never mounts it: it reads the copy on its own volume.
pub const CREDENTIAL_SEED_DIR: &str = "/etc/squelch/credential";

/// `SQUELCH_CREDENTIALS_PATH`. On the tenant's volume, NOT on the Secret mount,
/// because the daemon rewrites this file every time Google hands back a
/// refreshed token and a Secret volume is read-only. The init container seeds
/// it; see [`SEED_SCRIPT`].
pub const CREDENTIAL_PATH: &str = "/data/credentials.json";

/// What the init container remembers about the blob it installed: the SHA-256
/// of the Secret it copied from, and nothing else. Beside the credential on the
/// tenant's own volume, and never readable by anyone the credential is not.
pub const CREDENTIAL_MARKER_PATH: &str = "/data/.credentials.seed";

/// Where the shared, pre-seeded embedding weights are mounted, when the
/// operator configured a model PVC.
pub const MODELS_DIR: &str = "/models";

/// Data key holding the tenant's private age identity.
pub const IDENTITY_KEY: &str = "identity.txt";

/// Data key holding the tenant's public age recipient. Public, but kept beside
/// the identity so the two can never drift apart.
pub const RECIPIENT_KEY: &str = "recipient";

/// Data key holding the tenant's mailbox address.
///
/// In the Secret rather than in the Deployment because Secret data is what k3s
/// encrypts at rest (`--secrets-encryption`); a Deployment's env value is
/// plaintext in the datastore forever. The daemon gets it through a
/// `secretKeyRef`, so the address is never in a pod spec either.
pub const ACCOUNT_EMAIL_KEY: &str = "account-email";

/// Data key holding the age-armored credentials file.
pub const CREDENTIAL_KEY: &str = "credentials.json";

/// Data key holding the tenant's LLM gateway virtual key, in `<label>-llm`.
///
/// The daemon receives it as a `secretKeyRef` (see [`llm_secret`]), and the
/// warden does handle the plaintext: it writes it into the Secret and reads it
/// back to hash it for [`LLM_KEY_HASH_ANNOTATION`]. The posture is narrower:
/// never logged, never returned on the wire, and only the hash lands on the
/// world-readable pod spec.
pub const LLM_API_KEY_KEY: &str = "api-key";

/// Data key holding the tenant's assistant relay virtual key, beside
/// [`LLM_API_KEY_KEY`] in `<label>-llm`.
///
/// The second credential the same Secret can carry: the key the daemon uses to
/// proxy the Passband assistant through the gateway. Present only once the
/// control plane has minted one — a tenant keyed before the assistant era has
/// a Secret without this entry, and its pod boots anyway because the env
/// reference is optional (see [`daemon_env`]). Same posture as the triage key:
/// never logged, never returned on the wire, only its part of the combined
/// hash ([`llm_keys_hash`]) lands on the pod spec.
pub const ASSISTANT_API_KEY_KEY: &str = "assistant-api-key";

/// Data key holding the tenant's share token, in `<label>-control`.
///
/// The bearer this tenant's daemon presents to the control plane to mint an
/// invite for a friend. Same posture as the LLM keys, and for the same reason
/// (the warden writes it and reads it back to hash it for
/// [`SHARE_TOKEN_HASH_ANNOTATION`]): never logged, never returned on the wire,
/// and only the hash lands on the world-readable pod spec.
pub const SHARE_TOKEN_KEY: &str = "share-token";

/// Data keys in the shared Google OAuth client Secret
/// ([`crate::config::Config::oauth_secret_name`]).
///
/// The warden never reads either one: they go into the pod as `secretKeyRef`,
/// so the client secret is in the API server's encrypted store and in the
/// tenant's process, and never in a manifest or in this process's memory.
pub const OAUTH_CLIENT_ID_KEY: &str = "client_id";
/// See [`OAUTH_CLIENT_ID_KEY`].
pub const OAUTH_CLIENT_SECRET_KEY: &str = "client_secret";

/// Data key in the shared APNs relay Secret
/// ([`crate::config::Config::relay_secret_name`]): the relay's PUSH-ONLY
/// bearer (`SQUELCH_RELAY_PUSH_TOKEN` on the relay), which can push and can
/// NOT drain the relay's opens buffer. Never the full `SQUELCH_RELAY_AUTH_TOKEN`.
///
/// Same posture as the OAuth keys: the warden never reads it, it goes into the
/// pod as a `secretKeyRef`, and only the Secret's NAME is ever in a pod spec,
/// a log line or this process's memory.
pub const RELAY_TOKEN_KEY: &str = "token";

/// Key inside the fleet's Browserbase Secret
/// ([`crate::config::Config::browserbase_secret_name`]): the API key the
/// automatic unsubscribe's browser rung rents its remote browser with.
pub const BROWSERBASE_API_KEY_KEY: &str = "api-key";

/// Annotation on the pod template carrying the SHA-256 of the credential
/// ciphertext this pod was rolled for.
///
/// It is what makes re-consent possible. A new sealed blob changes only the
/// contents of a Secret, and a Secret's contents are not part of a Deployment's
/// pod template, so nothing would restart and the running daemon would keep
/// using the credential it already has. Putting the hash in the template makes
/// a changed blob a changed pod spec, which is a roll, which re-runs the seed.
/// The HASH, never the ciphertext: an annotation is world-readable to anyone
/// with `get deployment`.
pub const CREDENTIAL_HASH_ANNOTATION: &str = "passband.email/credential-hash";

/// Annotation on the pod template carrying the SHA-256 of the LLM virtual key
/// this pod was rolled for, when one exists.
///
/// Same rationale as [`CREDENTIAL_HASH_ANNOTATION`]: a rotated key changes
/// only the contents of a Secret, and a Secret's contents are not part of a
/// Deployment's pod template, so without this nothing would restart and the
/// running daemon would keep the env value it booted with. The HASH, never the
/// key: an annotation is world-readable to anyone with `get deployment`.
pub const LLM_KEY_HASH_ANNOTATION: &str = "passband.email/llm-key-hash";

/// Annotation on the pod template recording which share token the pod was
/// rolled for, as a hash.
///
/// The same mechanism [`LLM_KEY_HASH_ANNOTATION`] is: a tenant that has just
/// been given a share token has a Secret the running pod never read, and
/// without a changed pod spec the daemon would go on believing it cannot
/// share until something else happened to roll it.
pub const SHARE_TOKEN_HASH_ANNOTATION: &str = "passband.email/share-token-hash";

/// Annotation on the identity Secret recording when phase one ran, as Unix
/// seconds. The pending sweep needs an age, and this is the only place a
/// tenant's own record of one lives.
pub const CREATED_AT_ANNOTATION: &str = "passband.email/created-at";

/// Annotation on the identity Secret recording that this account was
/// CANCELLED, as Unix seconds. Present means cancelled; the value is for a
/// person reading `kubectl get secret -o yaml`, and no code compares it.
///
/// It is a record of INTENT, and that is what makes it worth an annotation
/// rather than a shape somebody works out from the objects that are left.
/// [`crate::provision::Warden::delete`] removes four objects one at a time and
/// stops at its first error, so a cancellation can leave any prefix of that
/// teardown standing - a Deployment still running with its Service and Ingress
/// already gone, for instance. Every reader that has to tell "this account is
/// closed" from "this repair did not finish" was reading that wreckage, and
/// wreckage cannot say which one it is: the same missing Deployment is both.
/// So `delete` stamps this FIRST, before it removes anything, and the readers
/// ask the marker. A stopped tenant carrying it is a closed account; one
/// without it is an unfinished job.
///
/// On the identity Secret because that is the one object of a tenant's that
/// `delete` deliberately keeps ([`identity_secret`]) - a marker on anything the
/// teardown removes would be a marker the teardown erases. Cleared by
/// [`crate::provision::Warden::set_credentials`], which is the reopen path: a
/// tenant that has just been re-credentialed is not cancelled any more.
pub const CANCELLED_AT_ANNOTATION: &str = "passband.email/cancelled-at";

/// Selects everything this warden created, for the pending sweep. Matches the
/// managed-by label [`labels`] puts on every tenant object.
pub const MANAGED_SELECTOR: &str = "app.kubernetes.io/managed-by=squelch-warden";

/// The path prefixes an Ingress ALWAYS routes to a tenant daemon: the human
/// door.
///
/// The daemon serves four trees: `/client/*` (the human door's API),
/// `/console/*` (the tenant's own browser console), `/t/*` (the read-tracking
/// pixel), and `/mcp` (the agent door). These three are published on every
/// tenant. The agent door is published only when the operator has configured
/// an OAuth issuer, and its prefixes live in [`AGENT_DOOR_PREFIXES`] so that
/// the two decisions cannot be confused for one another. A request for
/// anything on neither list matches no rule, and the ingress controller
/// answers its own 404 (both Traefik and ingress-nginx do). With no issuer
/// configured, that includes `/mcp`.
///
/// An allowlist, not a deny rule, and deliberately: a deny rule has to name
/// every spelling of the thing it is refusing, and it fails open when it misses
/// one. This fails closed. The cost is that a new unauthenticated route in
/// `squelch-api` needs a line here, which is a compile-time-visible list rather
/// than a controller annotation nobody reads. `/console` is that cost being
/// paid for the first time.
pub const HUMAN_DOOR_PREFIXES: &[&str] = &["/client", "/console", "/t"];

/// The path prefixes the Ingress ADDS when the agent door is published, which
/// is exactly when [`Config::mcp_oauth_issuer`] is set.
///
/// For a long time hosted published the human door only, because `/mcp` had no
/// authentication of its own: its one guard was rmcp's Host-header allow-list,
/// and putting it on a public hostname would have handed a tenant's mail to
/// anyone who could type the URL. What changed is that the DAEMON now enforces
/// OAuth on the agent door itself. In OAuth mode every request to `/mcp` must
/// carry an access token from the configured issuer, for this tenant's own
/// resource URL and this tenant's own mailbox address, or it is refused before
/// it reaches the MCP service. The Ingress is no longer the thing keeping the
/// door shut; it is the thing deciding whether the door is reachable at all.
///
/// Why these two and nothing else:
///
/// - `/mcp` is the agent door itself (Prefix, so `/mcp/` and anything the
///   transport hangs under it are the same door).
/// - `/.well-known/oauth-protected-resource` is the RFC 9728 metadata an agent
///   is pointed at by the daemon's `401`. Prefix, because the spec puts the
///   resource's own path after it (`.../oauth-protected-resource/mcp`) and
///   older clients ask for the bare form; the daemon serves both, and nothing
///   else, under it.
///
/// Deliberately NOT `/.well-known/` as a whole, and not
/// `/.well-known/oauth-authorization-server`: the authorization server is
/// AuthKit, at its own origin, and a tenant vhost that answered for it would be
/// a second, unaudited place to look for one.
///
/// THE ROLLOUT INVARIANT (I5) this list leans on: the warden NEVER renders
/// `SQUELCH_MCP_ALLOWED_HOSTS`. A daemon adds its own public hostname to the
/// rmcp Host allow-list only when it is in OAuth mode. So a tenant still on an
/// image that predates OAuth, sitting behind an Ingress that now publishes
/// `/mcp`, sees a Host header it does not allow and answers 403, instead of
/// serving mail with no authentication. Rendering that variable from here
/// would turn exactly that tenant into an open door.
pub const AGENT_DOOR_PREFIXES: &[&str] = &["/mcp", "/.well-known/oauth-protected-resource"];

/// Namespace CoreDNS runs in on k3s. The one egress peer every tenant needs
/// before it can resolve anything.
pub const DNS_NAMESPACE: &str = "kube-system";

/// Namespace the metrics collectors run in, and the prometheus-agent pod's own
/// label. The single peer allowed to reach [`METRICS_PORT`].
///
/// Hardcoded rather than configured because the pod on the other side is
/// installed from this repo (`deploy/hosted/80-monitoring.yaml`): these two
/// values and that manifest's namespace and pod labels are one contract, and a
/// knob would only let them drift apart in an install nobody tests.
pub const MONITORING_NAMESPACE: &str = "monitoring";
/// See [`MONITORING_NAMESPACE`]. The label on the prometheus-agent pod template,
/// which is also what its own Deployment selects on.
pub const MONITORING_POD_LABEL: (&str, &str) = ("app.kubernetes.io/name", "prometheus-agent");

/// The auto-applied label Kubernetes puts on every namespace, which is how a
/// `namespaceSelector` names one without the warden needing to label it.
pub const NAMESPACE_NAME_LABEL: &str = "kubernetes.io/metadata.name";

/// Networks a tenant may NOT reach, subtracted from its internet egress.
///
/// This list is the "no tenant-to-tenant reachability" claim in one place. On
/// k3s the pod CIDR is 10.42.0.0/16 and the service CIDR is 10.43.0.0/16, so
/// excluding 10.0.0.0/8 removes every other tenant's pod, the warden's pod and
/// Service, and the in-cluster `kubernetes` API Service at 10.43.0.1 in one
/// stroke. The rest are the other RFC 1918 ranges, the CGNAT range, and
/// link-local, which is where every cloud's instance-metadata endpoint lives.
pub const BLOCKED_EGRESS_CIDRS: &[&str] = &[
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "169.254.0.0/16",
    "100.64.0.0/10",
];

/// What the init container runs before the daemon starts.
///
/// A constant with no interpolation of any kind: no tenant label, no path from
/// config, nothing a caller can influence. It is a shell script only because
/// copying two files is not worth an image of its own.
///
/// It exists for one reason that is not optional: **the credentials file has to
/// be writable.** `FileCredentialStore` rewrites it on every token refresh, and
/// a Secret volume is read-only, so a daemon pointed straight at the Secret
/// mount would sync happily for an hour and then fail forever.
///
/// The copy is neither unconditional nor no-clobber, and both would be wrong:
///
/// - unconditional throws away the refreshed credential on every restart, and
///   Google does rotate refresh tokens;
/// - no-clobber makes seeding one-shot forever, so a re-consent stored by
///   `PUT /credentials` reaches the Secret and never reaches the daemon.
///
/// So it keeps a marker ([`CREDENTIAL_MARKER_PATH`]) holding the SHA-256 of the
/// blob it last installed, and copies only when the mounted Secret hashes to
/// something else. A daemon-rewritten file survives every restart; a genuinely
/// new ciphertext installs on the roll that
/// [`CREDENTIAL_HASH_ANNOTATION`] triggers.
///
/// The second half is the embedding weights, and it is a no-op unless the
/// operator configured a model PVC (see `deploy/hosted/SETUP.md`). Copied
/// rather than symlinked because the daemon's root filesystem is read-only and
/// fastembed expects to own its cache directory.
///
/// PER MODEL DIRECTORY, not per cache. The obvious condition is "copy if the
/// cache directory does not exist", and it has a hole the size of the whole
/// feature: a tenant that booted once BEFORE the PVC was configured downloaded
/// its own weights, so the cache directory exists, so it never takes anything
/// from the PVC again, including a model it does not have. Every tenant
/// provisioned before this volume existed is in exactly that state, and so is
/// every tenant on the day the pinned model changes. Copying entry by entry and
/// skipping the ones already present fixes both, keeps the tenant's own
/// downloads untouched, and stays idempotent across restarts. `$m` is
/// `/models/<model dir>`, so `${m#/models/}` is its name without needing
/// `basename` in the image.
///
/// Per DIRECTORY is also what makes the copy safe. A model in fastembed's
/// Hugging Face cache layout is mostly symlinks from `snapshots/` into
/// `blobs/`, and whether `cp -r` recreates those or dereferences them differs
/// between coreutils and BSD. Either is correct here, because the links are
/// relative and point inside the directory being copied: preserved they still
/// resolve, dereferenced the copy is just bigger. A per-file copy is what would
/// break, which is why the loop iterates `/models/*` and not what is under it.
///
/// TEMP PATH THEN RENAME, because "skip what is already there" and "copy in
/// place" combine into a trap. A `cp -r` straight to `$d` that dies partway
/// (the node under memory pressure, the init container OOM-killed, a source
/// file it cannot read) leaves a half-populated model directory, and every
/// later boot sees `-e "$d"` and skips it. What that costs depends on how it
/// died: hf-hub re-fetches what it cannot resolve, so a missing snapshot
/// pointer or a dangling link only makes the tenant pay the download it was
/// meant to skip. The one that does not heal is a TRUNCATED file at the right
/// path, which looks fetched and fails at session load. Copying to
/// `$d.seed-tmp` and renaming means `$d` only ever appears complete: rename is
/// atomic, and both paths are on the tenant's own volume so it is a rename and
/// not a cross-device copy. The `rm -rf "$t"` at the top of each iteration
/// sweeps the leftovers of a run that died, so an interrupted seed costs disk
/// once rather than accumulating.
///
/// A FAILED MODEL COPY IS NOT A FAILED POD. The weights half is best-effort by
/// construction: the daemon can always download what it does not find, so the
/// worst a failure here costs is the slow first boot this feature exists to
/// avoid. Fatal, it would cost the fleet. `set -eu` plus one unreadable file on
/// the shared volume (a PVC whose modes or ownership are wrong, which
/// `deploy/hosted/SETUP.md` step 10 warns about) exits the init container 1 on
/// EVERY tenant's next roll, and [`crate::provision`]'s roll reads a tenant
/// that will not come up as a casualty and halts. So the copy is chained rather
/// than trusted to `set -e`, and a failure sweeps its own temp directory and
/// says so on stderr. The credentials half above stays strict: a daemon with no
/// credentials file is not a daemon, and there is nothing for it to fall back
/// to.
///
/// The limit, stated: this protects a directory THIS script wrote. A cache
/// already half-populated under the model's real name (a download the daemon
/// itself did not finish) is indistinguishable from a good one at `-e`, and the
/// script leaves it alone. Repairing that is the operator's job, and the
/// procedure is the reclaim step in `deploy/hosted/PRODUCTION.md`: delete the
/// bad directory and let the next boot seed or re-download it.
pub const SEED_SCRIPT: &str = "set -eu\n\
    want=$(sha256sum /etc/squelch/credential/credentials.json | cut -d ' ' -f 1)\n\
    [ -n \"$want\" ] || { echo 'squelch-seed: no credential file to seed from' >&2; exit 1; }\n\
    have=''\n\
    if [ -f /data/.credentials.seed ]; then have=$(cat /data/.credentials.seed); fi\n\
    if [ \"$want\" != \"$have\" ]; then\n\
    \x20 cp /etc/squelch/credential/credentials.json /data/credentials.json\n\
    \x20 chmod 600 /data/credentials.json\n\
    \x20 printf '%s\\n' \"$want\" > /data/.credentials.seed\n\
    fi\n\
    if [ -d /models ] && mkdir -p /data/.local/share/squelch/models; then\n\
    \x20\x20for m in /models/*; do\n\
    \x20\x20\x20\x20[ -e \"$m\" ] || continue\n\
    \x20\x20\x20\x20d=/data/.local/share/squelch/models/${m#/models/}\n\
    \x20\x20\x20\x20t=$d.seed-tmp\n\
    \x20\x20\x20\x20rm -rf \"$t\"\n\
    \x20\x20\x20\x20if [ ! -e \"$d\" ]; then\n\
    \x20\x20\x20\x20\x20\x20cp -r \"$m\" \"$t\" && mv \"$t\" \"$d\" || {\n\
    \x20\x20\x20\x20\x20\x20\x20\x20rm -rf \"$t\"\n\
    \x20\x20\x20\x20\x20\x20\x20\x20echo \"squelch-seed: could not copy ${m#/models/}, the daemon will download it\" >&2\n\
    \x20\x20\x20\x20\x20\x20}\n\
    \x20\x20\x20\x20fi\n\
    \x20\x20done\n\
    fi\n";

/// The init container's bounds. Constants rather than config: it copies at most
/// two files and exits, and nothing about a tenant changes what that costs. They
/// exist so the pod has no unbounded container, not because anyone will ever
/// want to tune them. The ceiling is generous enough for a 130 MB `cp -r` of
/// the weights and small enough that a stuck one cannot take the node.
pub fn seed_resources() -> Resources {
    Resources {
        cpu_request: "10m".to_string(),
        cpu_limit: "500m".to_string(),
        memory_request: "16Mi".to_string(),
        memory_limit: "64Mi".to_string(),
        // Both volumes it touches are volumes; this covers its own logs.
        ephemeral_request: "16Mi".to_string(),
        ephemeral_limit: "64Mi".to_string(),
    }
}

/// A container's `resources`, as Kubernetes wants them.
fn requirements(resources: &Resources) -> ResourceRequirements {
    ResourceRequirements {
        requests: Some(BTreeMap::from([
            ("cpu".to_string(), Quantity(resources.cpu_request.clone())),
            (
                "memory".to_string(),
                Quantity(resources.memory_request.clone()),
            ),
            (
                "ephemeral-storage".to_string(),
                Quantity(resources.ephemeral_request.clone()),
            ),
        ])),
        limits: Some(BTreeMap::from([
            ("cpu".to_string(), Quantity(resources.cpu_limit.clone())),
            (
                "memory".to_string(),
                Quantity(resources.memory_limit.clone()),
            ),
            (
                "ephemeral-storage".to_string(),
                Quantity(resources.ephemeral_limit.clone()),
            ),
        ])),
        ..Default::default()
    }
}

/// The SHA-256 of a credential ciphertext, hex, for
/// [`CREDENTIAL_HASH_ANNOTATION`].
///
/// A hash of the ARMORED text exactly as the control plane sent it, which is
/// also exactly what lands in the Secret and what the init container hashes on
/// the other side. Nothing here parses or normalises it: two blobs are the same
/// blob only if they are byte-identical.
pub fn credential_hash(ciphertext: &str) -> String {
    let digest = Sha256::digest(ciphertext.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The pod-roll hash for the LLM Secret, over BOTH of the keys it can carry.
///
/// [`LLM_KEY_HASH_ANNOTATION`] must move when EITHER credential in
/// `<label>-llm` changes, or rotating one would leave the pod running on the
/// stale pair. A hash of hashes joined by a newline rather than a hash of
/// concatenated keys, so no choice of key bytes can make two different pairs
/// read as one (the inner digests are fixed-width hex, which no boundary
/// shift survives). An absent slot — either slot: a half-failed mint can
/// leave the Secret holding only one of the two — contributes the empty
/// string, distinct from every minted key's digest, and unambiguous because
/// [`crate::validate::validate_llm_api_key`] refuses an empty key, so
/// "absent" and "empty" are the same state by construction.
///
/// Both computations of this hash — `set_llm_key` with the keys in hand, and
/// `set_credentials` reading them back out of the stored Secret — go through
/// this one function so they cannot diverge and spuriously roll (or fail to
/// roll) a pod.
pub fn llm_keys_hash(api_key: Option<&str>, assistant_api_key: Option<&str>) -> String {
    let triage = api_key.map(credential_hash).unwrap_or_default();
    let assistant = assistant_api_key.map(credential_hash).unwrap_or_default();
    credential_hash(&format!("{triage}\n{assistant}"))
}

/// Labels every object of a tenant carries.
pub fn labels(name: &TenantName) -> BTreeMap<String, String> {
    let mut labels = selector_labels(name);
    labels.insert(
        "app.kubernetes.io/managed-by".to_string(),
        "squelch-warden".to_string(),
    );
    labels
}

/// The subset that selects a tenant's pods. Kept separate because a
/// Deployment's selector is immutable after creation: adding a label to
/// [`labels`] must never change what an existing Deployment selects.
pub fn selector_labels(name: &TenantName) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("app.kubernetes.io/name".to_string(), "squelchd".to_string()),
        (
            "app.kubernetes.io/instance".to_string(),
            name.as_str().to_string(),
        ),
    ])
}

/// The label selector, as the API wants it on a list request.
pub fn pod_selector(name: &TenantName) -> String {
    format!(
        "app.kubernetes.io/name=squelchd,app.kubernetes.io/instance={}",
        name.as_str()
    )
}

fn meta(config: &Config, name: &TenantName, object_name: String) -> ObjectMeta {
    ObjectMeta {
        name: Some(object_name),
        namespace: Some(config.namespace.clone()),
        labels: Some(labels(name)),
        ..Default::default()
    }
}

/// The tenant's identity Secret: its age key pair and its mailbox address.
///
/// Created in phase one, before the recipient is handed back, so the control
/// plane can never hold a recipient whose identity failed to persist. The only
/// Secret this service ever deletes, and only through the pending sweep
/// ([`crate::provision::Warden::sweep_pending`]): losing it once a tenant is
/// running means every credential sealed to that tenant is unrecoverable.
///
/// `created_at` is Unix seconds, and it is what the sweep ages a pending tenant
/// by; see [`CREATED_AT_ANNOTATION`].
pub fn identity_secret(
    config: &Config,
    name: &TenantName,
    identity: &TenantIdentity,
    account_email: &str,
    created_at: u64,
) -> Secret {
    let mut metadata = meta(config, name, name.identity_secret());
    metadata.annotations = Some(BTreeMap::from([(
        CREATED_AT_ANNOTATION.to_string(),
        created_at.to_string(),
    )]));

    Secret {
        metadata,
        type_: Some("Opaque".to_string()),
        // `string_data` rather than `data`: the API server does the base64, so
        // no code here handles a key as bytes it might log by accident.
        string_data: Some(BTreeMap::from([
            (IDENTITY_KEY.to_string(), identity.expose().to_string()),
            (RECIPIENT_KEY.to_string(), identity.recipient().to_string()),
            (ACCOUNT_EMAIL_KEY.to_string(), account_email.to_string()),
        ])),
        ..Default::default()
    }
}

/// The tenant's credential Secret: the age-armored blob the control plane
/// sealed to this tenant's recipient, verbatim.
///
/// The warden cannot read it. It kept no identity after
/// [`identity_secret`] was applied, and it never had this tenant's plaintext.
pub fn credential_secret(config: &Config, name: &TenantName, ciphertext: &str) -> Secret {
    Secret {
        metadata: meta(config, name, name.credential_secret()),
        type_: Some("Opaque".to_string()),
        string_data: Some(BTreeMap::from([(
            CREDENTIAL_KEY.to_string(),
            ciphertext.to_string(),
        )])),
        ..Default::default()
    }
}

/// The tenant's LLM Secret: the gateway virtual keys the control plane minted
/// for this tenant, verbatim — whichever of the triage and assistant relay
/// slots exist, and at least one (see [`ASSISTANT_API_KEY_KEY`]).
///
/// Unlike the credential blob, these are plaintext to the warden: they are
/// embedded in this Secret object and read back to be hashed for the roll
/// annotation ([`LLM_KEY_HASH_ANNOTATION`]). What holds instead: neither key
/// is ever logged or returned on the wire, and only the combined hash lands on
/// the world-readable pod spec — the daemon gets the keys themselves as
/// `secretKeyRef`s (see [`daemon_env`]).
pub fn llm_secret(
    config: &Config,
    name: &TenantName,
    api_key: Option<&str>,
    assistant_api_key: Option<&str>,
) -> Secret {
    // Only the slots that exist: the apply force-owns this Secret, so a data
    // key rendered here is the whole set that survives. The caller
    // ([`crate::provision::Warden::set_llm_key`]) always passes the union of
    // the request and what was stored, and at least one slot.
    let mut string_data = BTreeMap::new();
    if let Some(api_key) = api_key {
        string_data.insert(LLM_API_KEY_KEY.to_string(), api_key.to_string());
    }
    if let Some(assistant) = assistant_api_key {
        string_data.insert(ASSISTANT_API_KEY_KEY.to_string(), assistant.to_string());
    }
    Secret {
        metadata: meta(config, name, name.llm_secret()),
        type_: Some("Opaque".to_string()),
        string_data: Some(string_data),
        ..Default::default()
    }
}

/// The tenant's control Secret: the share token the control plane minted for
/// this tenant, verbatim.
///
/// One data key and no optional slots, unlike [`llm_secret`]: there is one
/// credential here, so a write is always the whole object and there is nothing
/// for a partial apply to clear.
pub fn control_secret(config: &Config, name: &TenantName, share_token: &str) -> Secret {
    Secret {
        metadata: meta(config, name, name.control_secret()),
        type_: Some("Opaque".to_string()),
        string_data: Some(BTreeMap::from([(
            SHARE_TOKEN_KEY.to_string(),
            share_token.to_string(),
        )])),
        ..Default::default()
    }
}

/// The tenant's data volume. `ReadWriteOnce` on k3s local-path: one node, one
/// writer, and the Deployment's `Recreate` strategy is what keeps it that way
/// across an image change.
pub fn data_pvc(config: &Config, name: &TenantName) -> PersistentVolumeClaim {
    PersistentVolumeClaim {
        metadata: meta(config, name, name.data_pvc()),
        spec: Some(PersistentVolumeClaimSpec {
            access_modes: Some(vec!["ReadWriteOnce".to_string()]),
            storage_class_name: Some(config.storage_class.clone()),
            resources: Some(VolumeResourceRequirements {
                requests: Some(BTreeMap::from([(
                    "storage".to_string(),
                    Quantity(config.storage_size.clone()),
                )])),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// The tenant's Service. ClusterIP, one port, reachable only by whatever the
/// NetworkPolicy allows.
pub fn service(config: &Config, name: &TenantName) -> Service {
    Service {
        metadata: meta(config, name, name.as_str().to_string()),
        spec: Some(ServiceSpec {
            type_: Some("ClusterIP".to_string()),
            selector: Some(selector_labels(name)),
            ports: Some(vec![ServicePort {
                name: Some("http".to_string()),
                port: DAEMON_PORT,
                target_port: Some(IntOrString::String("http".to_string())),
                protocol: Some("TCP".to_string()),
                ..Default::default()
            }]),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// The tenant's Ingress: `<label>.<base domain>` over the wildcard certificate,
/// routing the human door, and the agent door when an OAuth issuer is
/// configured.
///
/// See [`HUMAN_DOOR_PREFIXES`] for why an unpublished door is absent rather
/// than denied, and [`AGENT_DOOR_PREFIXES`] for what publishing the agent door
/// rests on. With no issuer configured the paths are exactly the human door's,
/// in the same order, so the object is byte-identical to the one a warden that
/// never heard of the agent door built.
pub fn ingress(config: &Config, name: &TenantName) -> Ingress {
    let backend = IngressBackend {
        service: Some(IngressServiceBackend {
            name: name.as_str().to_string(),
            port: Some(ServiceBackendPort {
                name: Some("http".to_string()),
                ..Default::default()
            }),
        }),
        ..Default::default()
    };
    let host = config.hostname(name.as_str());
    // The agent door rides on the SAME setting that puts the daemon into OAuth
    // mode (see [`daemon_env`]), so this Ingress never publishes `/mcp` for a
    // pod this render did not also tell to lock it.
    let agent_door: &[&str] = if config.mcp_oauth_issuer.is_some() {
        AGENT_DOOR_PREFIXES
    } else {
        &[]
    };

    Ingress {
        metadata: meta(config, name, name.as_str().to_string()),
        spec: Some(IngressSpec {
            ingress_class_name: Some(config.ingress_class.clone()),
            tls: Some(vec![IngressTLS {
                hosts: Some(vec![host.clone()]),
                secret_name: Some(config.tls_secret.clone()),
            }]),
            rules: Some(vec![IngressRule {
                host: Some(host),
                http: Some(HTTPIngressRuleValue {
                    paths: HUMAN_DOOR_PREFIXES
                        .iter()
                        .chain(agent_door)
                        .map(|prefix| HTTPIngressPath {
                            path: Some((*prefix).to_string()),
                            path_type: "Prefix".to_string(),
                            backend: backend.clone(),
                        })
                        // The bare root, EXACT on purpose: it serves one
                        // redirect to /console and nothing else. A Prefix "/"
                        // would match EVERY path (/metrics-shaped mistakes, any
                        // future route, and /mcp on a warden with no issuer)
                        // and turn this allowlist into decoration.
                        .chain(std::iter::once(HTTPIngressPath {
                            path: Some("/".to_string()),
                            path_type: "Exact".to_string(),
                            backend: backend.clone(),
                        }))
                        .collect(),
                }),
            }]),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// The tenant's NetworkPolicy: the isolation claim, written down.
///
/// Ingress: nothing may open a connection to a tenant pod except the ingress
/// controller, on the daemon port, and the monitoring namespace's
/// prometheus-agent, on the metrics port. Not other tenants, not the warden,
/// not anything else in the cluster.
///
/// Egress: DNS, and TCP 443 to the public internet minus
/// [`BLOCKED_EGRESS_CIDRS`]. That is Gmail and Anthropic and the model
/// download, and it is not the API server, not the warden, and not another
/// tenant's pod. Note what selecting both `policyTypes` does: an empty rule
/// list would be deny-all, and these two lists are the entire allowance.
///
/// The second ingress rule is the metrics allowance, and it is worth stating
/// precisely because this is the isolation claim: one pod
/// ([`MONITORING_POD_LABEL`]) in one namespace ([`MONITORING_NAMESPACE`]) may
/// open a connection to [`METRICS_PORT`] and to nothing else. It cannot reach
/// 8848, which stays sealed to the ingress controller, so the agent can read a
/// tenant's counters and cannot touch a tenant's mail; and no other tenant gains
/// anything, because a peer is a namespace AND a pod selector, not either. The
/// metrics port is absent from the Service and the Ingress, so this rule is the
/// only path to it that exists.
///
/// One optional third ingress rule: the NODE, when
/// [`crate::config::Config::node_cidr`] is set. A kubelet readiness probe is a
/// connection from the node's own address, which matches no pod and no
/// namespace, so a CNI that does not exempt host-originated traffic drops every
/// probe and every provision times out on a pod that is perfectly healthy. The
/// hole is one CIDR to the two ports a probe can land on — 8848 and
/// [`METRICS_PORT`], both of them whichever probe [`readiness_probe`] currently
/// renders, for the reason at the rule itself — and it lets in node-originated
/// traffic only: another tenant's pod still arrives from a pod IP and is still
/// refused.
pub fn network_policy(config: &Config, name: &TenantName) -> NetworkPolicy {
    let (ingress_label_key, ingress_label_value) = config.ingress_pod_label.clone();

    let daemon_port = vec![NetworkPolicyPort {
        port: Some(IntOrString::Int(DAEMON_PORT)),
        protocol: Some("TCP".to_string()),
        ..Default::default()
    }];
    let mut ingress = vec![NetworkPolicyIngressRule {
        from: Some(vec![NetworkPolicyPeer {
            namespace_selector: Some(LabelSelector {
                match_labels: Some(BTreeMap::from([(
                    NAMESPACE_NAME_LABEL.to_string(),
                    config.ingress_namespace.clone(),
                )])),
                ..Default::default()
            }),
            pod_selector: Some(LabelSelector {
                match_labels: Some(BTreeMap::from([(ingress_label_key, ingress_label_value)])),
                ..Default::default()
            }),
            ..Default::default()
        }]),
        ports: Some(daemon_port.clone()),
    }];
    ingress.push(NetworkPolicyIngressRule {
        from: Some(vec![NetworkPolicyPeer {
            namespace_selector: Some(LabelSelector {
                match_labels: Some(BTreeMap::from([(
                    NAMESPACE_NAME_LABEL.to_string(),
                    MONITORING_NAMESPACE.to_string(),
                )])),
                ..Default::default()
            }),
            pod_selector: Some(LabelSelector {
                match_labels: Some(BTreeMap::from([(
                    MONITORING_POD_LABEL.0.to_string(),
                    MONITORING_POD_LABEL.1.to_string(),
                )])),
                ..Default::default()
            }),
            ..Default::default()
        }]),
        ports: Some(vec![NetworkPolicyPort {
            port: Some(IntOrString::Int(METRICS_PORT)),
            protocol: Some("TCP".to_string()),
            ..Default::default()
        }]),
    });
    if let Some(node_cidr) = &config.node_cidr {
        let mut probe_ports = daemon_port;
        // BOTH probe ports, whichever probe is currently rendered. The hole has
        // to cover whatever port the kubelet was told to probe, and that is one
        // config flip away ([`readiness_probe`]) — while a NetworkPolicy is the
        // object no roll ever converges, because a drift report renders and
        // diffs a tenant's Deployment alone. Narrowing this to today's probe
        // would mean the edit that moves the probe silently needs a per-tenant
        // `reconcile` first, and skipping it takes the whole fleet NotReady on
        // exactly the clusters that needed this rule at all.
        //
        // The peer is the node's own address. It already holds every Secret
        // mounted into the pods it runs; letting it read a tenant's counters is
        // not the boundary that matters here.
        probe_ports.push(NetworkPolicyPort {
            port: Some(IntOrString::Int(METRICS_PORT)),
            protocol: Some("TCP".to_string()),
            ..Default::default()
        });
        ingress.push(NetworkPolicyIngressRule {
            from: Some(vec![NetworkPolicyPeer {
                ip_block: Some(IPBlock {
                    cidr: node_cidr.clone(),
                    except: None,
                }),
                ..Default::default()
            }]),
            ports: Some(probe_ports),
        });
    }

    NetworkPolicy {
        metadata: meta(config, name, name.as_str().to_string()),
        spec: Some(NetworkPolicySpec {
            pod_selector: Some(LabelSelector {
                match_labels: Some(selector_labels(name)),
                ..Default::default()
            }),
            policy_types: Some(vec!["Ingress".to_string(), "Egress".to_string()]),
            ingress: Some(ingress),
            egress: Some(vec![
                // CoreDNS, and only CoreDNS. A tenant that could reach any
                // resolver could exfiltrate over DNS to one it chose.
                NetworkPolicyEgressRule {
                    to: Some(vec![NetworkPolicyPeer {
                        namespace_selector: Some(LabelSelector {
                            match_labels: Some(BTreeMap::from([(
                                NAMESPACE_NAME_LABEL.to_string(),
                                DNS_NAMESPACE.to_string(),
                            )])),
                            ..Default::default()
                        }),
                        pod_selector: Some(LabelSelector {
                            match_labels: Some(BTreeMap::from([(
                                "k8s-app".to_string(),
                                "kube-dns".to_string(),
                            )])),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }]),
                    ports: Some(vec![
                        NetworkPolicyPort {
                            port: Some(IntOrString::Int(53)),
                            protocol: Some("UDP".to_string()),
                            ..Default::default()
                        },
                        NetworkPolicyPort {
                            port: Some(IntOrString::Int(53)),
                            protocol: Some("TCP".to_string()),
                            ..Default::default()
                        },
                    ]),
                },
                // Gmail, Anthropic, the relay, the one-time weights download.
                NetworkPolicyEgressRule {
                    to: Some(vec![NetworkPolicyPeer {
                        ip_block: Some(IPBlock {
                            cidr: "0.0.0.0/0".to_string(),
                            except: Some(
                                BLOCKED_EGRESS_CIDRS
                                    .iter()
                                    .map(|c| (*c).to_string())
                                    .collect(),
                            ),
                        }),
                        ..Default::default()
                    }]),
                    ports: Some(vec![NetworkPolicyPort {
                        port: Some(IntOrString::Int(443)),
                        protocol: Some("TCP".to_string()),
                        ..Default::default()
                    }]),
                },
            ]),
        }),
    }
}

/// The daemon's readiness route, on [`METRICS_PORT`]. Two states, one word,
/// nothing about the tenant behind it.
pub const HEALTHZ_PATH: &str = "/healthz";

/// The tenant daemon's readiness probe, in one of its two shapes.
///
/// **A bare TCP accept on the door, by default.** The doors serve nothing
/// unauthenticated, so an HTTP probe against 8848 would have to hold a bearer
/// token, and a token the kubelet holds is a credential in a pod spec.
///
/// **An HTTP GET of [`HEALTHZ_PATH`] on [`METRICS_PORT`], when
/// [`crate::config::Config::http_readiness`] is on.** That listener is already
/// unauthenticated and already rendered into every tenant pod, so the honest
/// probe costs no new authenticated surface. It is worth having because the
/// accept answers the wrong question: the daemon binds its listener BEFORE it
/// builds its embedder, deliberately, so that a first-run model download cannot
/// leave the doors unreachable — which means a TCP probe calls a tenant Ready
/// seconds into a startup that has not happened yet, and calls one Ready that
/// died on the way up after the bind. `/healthz` answers 503 until the daemon
/// has finished starting, and 503 again if its sync task comes apart afterwards.
///
/// What it does NOT catch is a credential Google has stopped accepting: the
/// daemon's sync loop retries that forever rather than ending, on purpose, and
/// a mailbox pulled out of its own Service is a mailbox its owner cannot reach
/// to re-consent through. That is an alert on the metrics next door, not a
/// readiness state.
///
/// The knob is OFF by default, and it has TWO prerequisites rather than one.
/// Switching it on before the fleet is on a daemon that serves the route takes
/// every tenant that is behind DOWN; and because `/healthz` waits out the
/// first-run model download, a tenant whose weights cache is cold does not
/// answer 200 for as long as that download takes — which is longer than
/// [`crate::config::Config::ready_timeout`], so a signup times out and a roll
/// reads the tenant as a casualty. [`crate::config::Config::model_pvc`] is what
/// removes the download; see [`crate::config::Config::http_readiness`] for the
/// sequence. The route is reachable at all because the warden sets
/// `SQUELCH_METRICS_BIND` on every tenant it renders; see [`daemon_env`].
///
/// Everything except the action is identical between the two, so flipping the
/// knob changes exactly one thing about a tenant. The initial delay stays small
/// because the route answers while it is starting rather than refusing the
/// connection, and a readiness probe that fails never restarts a container:
/// a daemon two minutes into a model download is NotReady and untouched, which
/// is precisely what it should be.
fn readiness_probe(config: &Config) -> Probe {
    let (tcp_socket, http_get) = if config.http_readiness {
        (
            None,
            Some(HTTPGetAction {
                path: Some(HEALTHZ_PATH.to_string()),
                port: IntOrString::Int(METRICS_PORT),
                ..Default::default()
            }),
        )
    } else {
        (
            Some(TCPSocketAction {
                port: IntOrString::Int(DAEMON_PORT),
                ..Default::default()
            }),
            None,
        )
    };
    Probe {
        tcp_socket,
        http_get,
        initial_delay_seconds: Some(2),
        period_seconds: Some(3),
        ..Default::default()
    }
}

/// The tenant's Deployment.
///
/// One replica, `Recreate`, and a pod that gives up everything it can:
/// non-root at a fixed high uid, no privilege escalation, every capability
/// dropped, the runtime's default seccomp profile, a read-only root filesystem
/// with writable mounts for the data volume and `/tmp` and nothing else, no
/// ServiceAccount token, no service-link environment, and (unless the operator
/// turned it off) its own user namespace.
///
/// The image's entrypoint is deliberately bypassed: `docker-entrypoint.sh`
/// starts as root to chown the volume and then drops with `setpriv`, which
/// cannot work under `runAsNonRoot`. `fsGroup` does that chown instead, at the
/// kubelet, before the container starts.
///
/// `credential_hash` is [`credential_hash`] of the ciphertext this tenant is
/// being provisioned with. It rides in an annotation on the pod template so a
/// re-consent actually reaches the daemon; see [`CREDENTIAL_HASH_ANNOTATION`].
///
/// `llm_key_hash` is the same mechanism for the LLM virtual key, when this
/// tenant has one: absent for a tenant never keyed, present so a rotated key
/// is a changed pod spec and a roll. See [`LLM_KEY_HASH_ANNOTATION`].
///
/// `share_token_hash` is the same again, for the share token. THREE SEPARATE
/// ARGUMENTS rather than one bundled hash, because they move independently: a
/// re-consent, a key rotation, and a share mint each have to roll the pod
/// without needing to know what the other two currently are.
pub fn deployment(
    config: &Config,
    name: &TenantName,
    credential_hash: &str,
    llm_key_hash: Option<&str>,
    share_token_hash: Option<&str>,
) -> Deployment {
    let pod_security = PodSecurityContext {
        run_as_non_root: Some(true),
        run_as_user: Some(config.run_as),
        run_as_group: Some(config.run_as),
        // What makes the PersistentVolume writable by an unprivileged uid
        // without anything in the pod ever being root.
        fs_group: Some(config.run_as),
        // `Always` (the default) walks and chowns the whole volume on every
        // start. A mailbox index plus an embedding cache is a lot of inodes to
        // touch before a restart can begin serving, and the ownership only ever
        // changes if `run_as` does. `OnRootMismatch` checks the top directory
        // and skips the walk when it already matches.
        fs_group_change_policy: Some("OnRootMismatch".to_string()),
        seccomp_profile: Some(SeccompProfile {
            type_: "RuntimeDefault".to_string(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let container_security = SecurityContext {
        allow_privilege_escalation: Some(false),
        read_only_root_filesystem: Some(true),
        run_as_non_root: Some(true),
        capabilities: Some(Capabilities {
            drop: Some(vec!["ALL".to_string()]),
            ..Default::default()
        }),
        ..Default::default()
    };

    let data_mount = VolumeMount {
        name: "data".to_string(),
        mount_path: DATA_DIR.to_string(),
        ..Default::default()
    };
    let tmp_mount = VolumeMount {
        name: "tmp".to_string(),
        mount_path: "/tmp".to_string(),
        ..Default::default()
    };

    let mut volumes = vec![
        Volume {
            name: "data".to_string(),
            persistent_volume_claim: Some(PersistentVolumeClaimVolumeSource {
                claim_name: name.data_pvc(),
                ..Default::default()
            }),
            ..Default::default()
        },
        Volume {
            name: "identity".to_string(),
            secret: Some(SecretVolumeSource {
                secret_name: Some(name.identity_secret()),
                // Projected key by key: the Secret also holds the recipient and
                // the mailbox address, and the daemon has no business seeing a
                // file it did not ask for.
                items: Some(vec![KeyToPath {
                    key: IDENTITY_KEY.to_string(),
                    path: IDENTITY_KEY.to_string(),
                    ..Default::default()
                }]),
                default_mode: Some(0o400),
                ..Default::default()
            }),
            ..Default::default()
        },
        Volume {
            name: "credential".to_string(),
            secret: Some(SecretVolumeSource {
                secret_name: Some(name.credential_secret()),
                items: Some(vec![KeyToPath {
                    key: CREDENTIAL_KEY.to_string(),
                    path: CREDENTIAL_KEY.to_string(),
                    ..Default::default()
                }]),
                default_mode: Some(0o400),
                ..Default::default()
            }),
            ..Default::default()
        },
        Volume {
            name: "tmp".to_string(),
            empty_dir: Some(EmptyDirVolumeSource {
                // An emptyDir with no sizeLimit is a hole with the node's free
                // space at the bottom of it, shared with every other tenant.
                size_limit: Some(Quantity(config.tmp_size.clone())),
                ..Default::default()
            }),
            ..Default::default()
        },
    ];

    let mut seed_mounts = vec![
        data_mount.clone(),
        VolumeMount {
            name: "credential".to_string(),
            mount_path: CREDENTIAL_SEED_DIR.to_string(),
            read_only: Some(true),
            ..Default::default()
        },
    ];

    if let Some(model_pvc) = &config.model_pvc {
        volumes.push(Volume {
            name: "models".to_string(),
            persistent_volume_claim: Some(PersistentVolumeClaimVolumeSource {
                claim_name: model_pvc.clone(),
                read_only: Some(true),
            }),
            ..Default::default()
        });
        seed_mounts.push(VolumeMount {
            name: "models".to_string(),
            mount_path: MODELS_DIR.to_string(),
            read_only: Some(true),
            ..Default::default()
        });
    }

    let seed = Container {
        name: "seed".to_string(),
        image: Some(config.image.clone()),
        command: Some(vec!["/bin/sh".to_string(), "-c".to_string()]),
        args: Some(vec![SEED_SCRIPT.to_string()]),
        volume_mounts: Some(seed_mounts),
        security_context: Some(container_security.clone()),
        resources: Some(requirements(&seed_resources())),
        ..Default::default()
    };

    let daemon = Container {
        name: "squelchd".to_string(),
        image: Some(config.image.clone()),
        // The image's ENTRYPOINT is root-then-setpriv; see the doc comment.
        command: Some(vec!["/usr/local/bin/squelchd".to_string()]),
        args: Some(vec!["serve".to_string()]),
        ports: Some(vec![
            ContainerPort {
                name: Some("http".to_string()),
                container_port: DAEMON_PORT,
                protocol: Some("TCP".to_string()),
                ..Default::default()
            },
            // Declared so prometheus-agent's pod discovery can keep on a port
            // NAME rather than a number. Naming a port here publishes nothing:
            // the Service does not carry it, so it is reachable only through
            // the NetworkPolicy rule. See [`METRICS_PORT`].
            ContainerPort {
                name: Some("metrics".to_string()),
                container_port: METRICS_PORT,
                protocol: Some("TCP".to_string()),
                ..Default::default()
            },
        ]),
        env: Some(daemon_env(config, name)),
        volume_mounts: Some(vec![
            data_mount,
            VolumeMount {
                name: "identity".to_string(),
                mount_path: IDENTITY_DIR.to_string(),
                read_only: Some(true),
                ..Default::default()
            },
            tmp_mount,
        ]),
        readiness_probe: Some(readiness_probe(config)),
        security_context: Some(container_security),
        resources: Some(requirements(&config.daemon_resources)),
        ..Default::default()
    };

    Deployment {
        metadata: meta(config, name, name.as_str().to_string()),
        spec: Some(DeploymentSpec {
            replicas: Some(1),
            // How long the pod must STAY Ready before the controller counts the
            // replica Available. Readiness is a snapshot and a daemon that
            // comes up and dies passes through it, so this is the per-tenant
            // soak that a caller waiting on `availableReplicas` gets for free;
            // see [`crate::config::Config::min_ready_secs`].
            //
            // Absent rather than `0` when the operator turned it off: zero is
            // the API server's own default and would be dropped from the
            // object it stores, leaving every drift report quoting a field the
            // live object does not carry.
            min_ready_seconds: (config.min_ready_secs > 0).then_some(config.min_ready_secs),
            // Never two daemons on one SQLite file: the volume is
            // ReadWriteOnce, and a rolling update would try to start the new
            // pod before the old one released it.
            strategy: Some(DeploymentStrategy {
                type_: Some("Recreate".to_string()),
                ..Default::default()
            }),
            selector: LabelSelector {
                match_labels: Some(selector_labels(name)),
                ..Default::default()
            },
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(labels(name)),
                    // What makes a re-consent (and a key rotation, and a share
                    // mint) land: see [`CREDENTIAL_HASH_ANNOTATION`],
                    // [`LLM_KEY_HASH_ANNOTATION`] and
                    // [`SHARE_TOKEN_HASH_ANNOTATION`].
                    annotations: Some({
                        let mut annotations = BTreeMap::from([(
                            CREDENTIAL_HASH_ANNOTATION.to_string(),
                            credential_hash.to_string(),
                        )]);
                        if let Some(hash) = llm_key_hash {
                            annotations
                                .insert(LLM_KEY_HASH_ANNOTATION.to_string(), hash.to_string());
                        }
                        if let Some(hash) = share_token_hash {
                            annotations
                                .insert(SHARE_TOKEN_HASH_ANNOTATION.to_string(), hash.to_string());
                        }
                        annotations
                    }),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    // Per-tenant user namespace: root inside the pod is an
                    // unprivileged id on the node. `None` when the operator
                    // turned it off, because sending `true` would ask for the
                    // host's user namespace explicitly, which is the opposite
                    // of what "unsupported cluster" means.
                    host_users: config.user_namespaces.then_some(false),
                    security_context: Some(pod_security),
                    // A tenant pod has no business talking to the API server,
                    // and no reason to learn the names of other Services.
                    automount_service_account_token: Some(false),
                    enable_service_links: Some(false),
                    init_containers: Some(vec![seed]),
                    containers: vec![daemon],
                    volumes: Some(volumes),
                    image_pull_secrets: config
                        .pull_secret
                        .as_ref()
                        .map(|name| vec![LocalObjectReference { name: name.clone() }]),
                    ..Default::default()
                }),
            },
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// The daemon's environment. These are the REAL variable names `squelch-core`
/// reads, checked against `squelch-core/src/config.rs`; v1 shipped two that the
/// daemon never looked at.
fn daemon_env(config: &Config, name: &TenantName) -> Vec<EnvVar> {
    let plain = |key: &str, value: &str| EnvVar {
        name: key.to_string(),
        value: Some(value.to_string()),
        ..Default::default()
    };
    let from_secret = |key: &str, secret: String, data_key: &str| EnvVar {
        name: key.to_string(),
        value_from: Some(EnvVarSource {
            secret_key_ref: Some(SecretKeySelector {
                name: secret,
                key: data_key.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    // The same, but `optional: true`: the kubelet leaves the variable unset when
    // the Secret or the key is missing instead of holding the pod in
    // `CreateContainerConfigError` forever.
    let from_optional_secret = |key: &str, secret: String, data_key: &str| EnvVar {
        name: key.to_string(),
        value_from: Some(EnvVarSource {
            secret_key_ref: Some(SecretKeySelector {
                name: secret,
                key: data_key.to_string(),
                optional: Some(true),
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut env = vec![
        // Inside the pod's own network namespace. Not reachable from anywhere
        // the NetworkPolicy does not allow.
        plain("SQUELCH_BIND", "0.0.0.0:8848"),
        // Turns on the metrics listener, which is off unless this is set. A
        // second port, unauthenticated, and reachable only from
        // prometheus-agent: see [`METRICS_PORT`] and [`network_policy`].
        plain("SQUELCH_METRICS_BIND", "0.0.0.0:9464"),
        plain("SQUELCH_DB_PATH", "/data/squelch.db"),
        plain("SQUELCH_CRED_BACKEND", "file"),
        plain("SQUELCH_CREDENTIALS_PATH", CREDENTIAL_PATH),
        plain("SQUELCH_CRED_AGE_IDENTITY", IDENTITY_FILE),
        // Where the embedding-weights cache resolves to
        // (`$HOME/.local/share/squelch/models`), and where config.toml
        // discovery starts. On the tenant's own volume, so neither escapes it.
        plain("HOME", DATA_DIR),
        // From the Secret, never from the pod spec: see [`ACCOUNT_EMAIL_KEY`].
        from_secret(
            "SQUELCH_ACCOUNT_EMAIL",
            name.identity_secret(),
            ACCOUNT_EMAIL_KEY,
        ),
        // The OAuth client the tenant's refresh token was minted by, without
        // which the daemon refuses to start (`Config::oauth_client`) and could
        // not refresh an access token if it did.
        //
        // This is the control plane's CONFIDENTIAL WEB client, and its secret
        // being inside a tenant pod is deliberate: a refresh token only works
        // for the client that minted it, hosted daemons are our own
        // infrastructure, and this is the custody hosted admits to. Shared by
        // every tenant, because it is one Google project's one web client. See
        // `deploy/hosted/SETUP.md`, "The Google OAuth client".
        from_secret(
            "SQUELCH_CLIENT_ID",
            config.oauth_secret_name.clone(),
            OAUTH_CLIENT_ID_KEY,
        ),
        from_secret(
            "SQUELCH_CLIENT_SECRET",
            config.oauth_secret_name.clone(),
            OAUTH_CLIENT_SECRET_KEY,
        ),
    ];

    // The automatic unsubscribe's browser rung, for every tenant: there is no
    // switch here. OPTIONAL for the LLM keys' reason: a missing Secret must
    // leave the variable unset rather than hold every tenant in
    // CreateContainerConfigError. Unset, the daemon has no browser to rent and
    // unsubscribes exactly as it did before the rung existed.
    env.push(from_optional_secret(
        "SQUELCH_BROWSERBASE_API_KEY",
        config.browserbase_secret_name.clone(),
        BROWSERBASE_API_KEY_KEY,
    ));

    // Which embedding weights the daemon loads, when the operator pinned one.
    // ABSENT unless set, so a warden with the knob unset builds the same pod a
    // warden that never heard of it did: the daemon ships its own pin, and this
    // exists only so the fleet can be moved off it without cutting an image.
    if let Some(model) = &config.embed_model {
        env.push(plain("SQUELCH_EMBED_MODEL", model));
    }

    // The control plane's origin, behind the console's "Continue with Google"
    // link. ABSENT when the operator did not configure one, rather than empty:
    // the daemon treats a blank value as unset anyway, but an empty env var in a
    // pod spec reads like a setting somebody meant to fill in.
    //
    // ONE CONFIGURED VALUE, TWO VARIABLES. The same origin is also where the
    // daemon posts to mint an invite, and the daemon reads the two through
    // different settings because they are different errands: one is a link it
    // renders for a human, the other an API it authenticates to. An operator
    // configuring the control plane twice is an operator who can get them out
    // of step.
    if let Some(url) = &config.console_sso_url {
        env.push(plain("SQUELCH_CONSOLE_SSO_URL", url));
        env.push(plain("SQUELCH_CONTROL_URL", url));
        // The share token, from the Secret, never inline - and OPTIONAL, for
        // the reason the LLM keys are: a tenant nobody has run `share mint`
        // for has no such Secret at all and must still boot. The daemon's
        // share route answers unavailable until one is installed.
        env.push(from_optional_secret(
            "SQUELCH_CONTROL_TOKEN",
            name.control_secret(),
            SHARE_TOKEN_KEY,
        ));
    }

    // The tenant daemon's idle-embedder window, present only when the operator
    // pinned one. Unset, the daemon keeps its own default: this is a pod-shaped
    // override, not a policy the warden holds an opinion about.
    if let Some(secs) = config.embed_idle_unload_secs {
        env.push(plain("SQUELCH_EMBED_IDLE_UNLOAD_SECS", &secs.to_string()));
    }

    // The LLM gateway block, present only when the operator configured one.
    // With it absent the env above is byte-identical to what a warden without
    // the feature built, which is the env-contract tests' whole claim.
    if let Some(base_url) = &config.llm_base_url {
        env.push(plain("SQUELCH_ANTHROPIC_BASE_URL", base_url));
        // Belt and braces: the base URL alone would already select Anthropic's
        // wire shape, but the daemon also sniffs the provider from the model
        // name, and a gateway-side model alias must not flip a tenant onto a
        // provider path the gateway does not speak.
        env.push(plain("SQUELCH_STAGE2_PROVIDER", "anthropic"));
        // The virtual key, from the Secret, never inline — and OPTIONAL: a
        // tenant whose key was never minted must still boot. An unminted
        // tenant resolves no key at all, and the daemon fail-softs to
        // heuristic-only triage until `squelch-control llm mint` runs.
        env.push(from_optional_secret(
            "SQUELCH_STAGE2_API_KEY",
            name.llm_secret(),
            LLM_API_KEY_KEY,
        ));
        // The assistant relay credential, and OPTIONAL for a second reason on
        // top of the triage key's: a tenant minted before the assistant era
        // has an LLM Secret with no such data key at all, and it must still
        // boot — the daemon's assistant route answers unavailable until the
        // control plane mints the key.
        env.push(from_optional_secret(
            "SQUELCH_ASSISTANT_API_KEY",
            name.llm_secret(),
            ASSISTANT_API_KEY_KEY,
        ));
        if let Some(model) = &config.llm_stage1_model {
            env.push(plain("SQUELCH_STAGE1_MODEL", model));
        }
        if let Some(model) = &config.llm_stage2_model {
            // Stage 2's model variable really is `SQUELCH_MODEL`: it predates
            // the stage split and the daemon never grew a prefixed alias.
            env.push(plain("SQUELCH_MODEL", model));
        }
        if let Some(cap) = config.llm_stage1_daily_cap {
            env.push(plain("SQUELCH_STAGE1_GLOBAL_DAILY_CAP", &cap.to_string()));
        }
        if let Some(cap) = config.llm_stage2_daily_cap {
            env.push(plain("SQUELCH_STAGE2_GLOBAL_DAILY_CAP", &cap.to_string()));
        }
    }

    // The agent door's OAuth pair, present only when the operator configured
    // an issuer, and ABSENT otherwise so the pod is byte-identical to one a
    // warden that never heard of the agent door built. Both or neither: the
    // daemon treats exactly one of them as MISCONFIGURED and answers 503 on
    // `/mcp`, so rendering them from one setting is what keeps a tenant out of
    // that state.
    //
    // The public URL is built from [`Config::hostname`], the same helper the
    // Ingress host comes from, so the resource a token is minted for and the
    // vhost that routes it cannot disagree. It is the `aud` the daemon
    // requires, and the hostname it adds to its own Host allow-list.
    //
    // NEVER `SQUELCH_MCP_ALLOWED_HOSTS`, here or anywhere in this file: see
    // [`AGENT_DOOR_PREFIXES`] for why its absence is what keeps an old daemon
    // image behind a published `/mcp` answering 403.
    if let Some(issuer) = &config.mcp_oauth_issuer {
        env.push(plain("SQUELCH_MCP_OAUTH_ISSUER", issuer));
        env.push(plain(
            "SQUELCH_MCP_PUBLIC_URL",
            &format!("https://{}/mcp", config.hostname(name.as_str())),
        ));
    }

    // The APNs relay block, present only when the operator configured one, and
    // byte-identical-when-unset for the LLM block's reason. Without
    // SQUELCH_RELAY_URL the daemon never spawns its pusher, which is why no
    // hosted tenant had ever received an iOS push before this block existed.
    if let Some(relay_url) = &config.relay_url {
        env.push(plain("SQUELCH_RELAY_URL", relay_url));
        // The relay's push-only bearer, from the shared Secret, never inline.
        // Push-only so a compromised tenant cannot drain the opens buffer the
        // self-hosted daemons share (the relay 401s it at /v1/opens), and
        // OPTIONAL for the LLM keys' reason: a missing Secret must leave the
        // variable unset rather than hold EVERY tenant in
        // CreateContainerConfigError at once. Unset, the daemon pushes with no
        // bearer, the relay answers 401, the pusher logs it and backs off, and
        // mail keeps working.
        env.push(from_optional_secret(
            "SQUELCH_RELAY_TOKEN",
            config.relay_secret_name.clone(),
            RELAY_TOKEN_KEY,
        ));
        if let Some(topic) = &config.relay_topic {
            env.push(plain("SQUELCH_RELAY_TOPIC", topic));
        }
        // SQUELCH_RELAY_APNS_ENV IS NEVER SET HERE, and that is the fix rather
        // than an omission. Sandbox versus production is a property of the
        // build that registered a device token, so the client declares it per
        // device at registration and the daemon forwards it per push; a client
        // that predates that gets the relay's own default, production, which
        // is what a TestFlight or App Store build needs. A fleet-wide switch
        // here would force one answer onto every device of every tenant, which
        // is exactly the bug being fixed.
    }
    env
}

/// The argv `squelchd pair` runs as inside a tenant pod.
///
/// Everything the command needs beyond this is already in the container's
/// environment, which an exec session inherits: the store path, the account,
/// and `HOME`. The URL is passed explicitly because the daemon cannot know its
/// own public hostname.
pub fn pair_argv(config: &Config, name: &TenantName) -> Vec<String> {
    vec![
        "/usr/local/bin/squelchd".to_string(),
        "pair".to_string(),
        "--url".to_string(),
        config.tenant_url(name.as_str()),
    ]
}

/// The argv `squelchd token first-paired` runs as inside a tenant pod: when a
/// client device first paired with this mailbox, for the activation signal.
///
/// TAKES NEITHER THE CONFIG NOR THE TENANT, unlike [`pair_argv`], and that is
/// the whole shape of the command rather than an omission. The store path and
/// the account come from the container's environment, which an exec session
/// inherits; there is no URL to pass because nothing is being handed to a
/// device. Every byte of this argv is a fixed string, so no tenant label can
/// reach a command line from here even by accident.
///
/// A daemon image too old to have the subcommand exits non-zero on it, which the
/// caller reports as a cluster failure and the control plane's poller simply
/// waits out — see [`crate::provision::Warden::first_paired`]. No version
/// negotiation, because the next fleet roll is the fix.
pub fn first_paired_argv() -> Vec<String> {
    vec![
        "/usr/local/bin/squelchd".to_string(),
        "token".to_string(),
        "first-paired".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DEFAULT_MIN_READY_SECS;
    use crate::testing::test_config;
    use k8s_openapi::api::batch::v1::CronJob;
    use k8s_openapi::api::core::v1::{ConfigMap, LimitRange, LimitRangeItem, Pod, ResourceQuota};

    fn name() -> TenantName {
        TenantName::parse("alice").unwrap()
    }

    /// A tenant's Deployment as provisioning builds it, for the tests that do
    /// not care which ciphertext it was rolled for. No LLM key, which is also
    /// the shape every unkeyed tenant gets.
    fn tenant_deployment(config: &Config) -> Deployment {
        deployment(config, &name(), &credential_hash("ct"), None, None)
    }

    /// A config with the LLM gateway turned on and nothing else changed.
    fn llm_config() -> Config {
        let mut c = test_config();
        c.llm_base_url = Some("https://llm.passband.internal".to_string());
        c
    }

    #[test]
    fn every_object_lands_in_the_tenant_namespace_under_the_tenants_name() {
        let c = test_config();
        let n = name();
        let id = TenantIdentity::mint();

        let expect = |meta: &ObjectMeta, object_name: &str| {
            assert_eq!(meta.name.as_deref(), Some(object_name));
            assert_eq!(meta.namespace.as_deref(), Some("tenants"));
            assert_eq!(
                meta.labels.as_ref().unwrap()["app.kubernetes.io/instance"],
                "alice"
            );
            assert_eq!(
                meta.labels.as_ref().unwrap()["app.kubernetes.io/managed-by"],
                "squelch-warden"
            );
        };

        expect(
            &identity_secret(&c, &n, &id, "alice@example.com", 1_700_000_000).metadata,
            "alice-identity",
        );
        expect(
            &credential_secret(&c, &n, "ct").metadata,
            "alice-credential",
        );
        expect(&data_pvc(&c, &n).metadata, "alice-data");
        expect(&service(&c, &n).metadata, "alice");
        expect(&ingress(&c, &n).metadata, "alice");
        expect(&network_policy(&c, &n).metadata, "alice");
        expect(&tenant_deployment(&c).metadata, "alice");
    }

    #[test]
    fn the_identity_secret_carries_the_pair_the_address_and_its_age() {
        let c = test_config();
        let id = TenantIdentity::mint();
        let secret = identity_secret(&c, &name(), &id, "alice@example.com", 1_700_000_000);
        let data = secret.string_data.unwrap();
        assert_eq!(data[IDENTITY_KEY], id.expose());
        assert_eq!(data[RECIPIENT_KEY], id.recipient());
        assert_eq!(data[ACCOUNT_EMAIL_KEY], "alice@example.com");
        assert_eq!(secret.type_.as_deref(), Some("Opaque"));
        assert_eq!(data.len(), 3);
        // What the pending sweep ages a half-finished signup by.
        assert_eq!(
            secret.metadata.annotations.unwrap()[CREATED_AT_ANNOTATION],
            "1700000000"
        );
    }

    #[test]
    fn the_credential_secret_is_the_ciphertext_verbatim() {
        let c = test_config();
        let ct = crate::testing::armored("alice");
        let secret = credential_secret(&c, &name(), &ct);
        assert_eq!(secret.string_data.unwrap()[CREDENTIAL_KEY], ct);
    }

    /// The isolation claim, asserted field by field. Every line here is
    /// something a reviewer of the hosted pitch is entitled to check.
    #[test]
    fn the_pod_gives_up_everything_it_can() {
        let c = test_config();
        let d = tenant_deployment(&c);
        let pod = d.spec.unwrap().template.spec.unwrap();

        // User namespaces on by default.
        assert_eq!(pod.host_users, Some(false));
        assert_eq!(pod.automount_service_account_token, Some(false));
        assert_eq!(pod.enable_service_links, Some(false));

        let ctx = pod.security_context.unwrap();
        assert_eq!(ctx.run_as_non_root, Some(true));
        assert_eq!(ctx.run_as_user, Some(10001));
        assert_eq!(ctx.run_as_group, Some(10001));
        assert_eq!(ctx.fs_group, Some(10001));
        assert_eq!(
            ctx.fs_group_change_policy.as_deref(),
            Some("OnRootMismatch")
        );
        assert_eq!(ctx.seccomp_profile.unwrap().type_, "RuntimeDefault");

        // Both containers, not just the daemon: an init container with
        // capabilities is an init container that can be abused.
        let mut containers = pod.init_containers.clone().unwrap();
        containers.extend(pod.containers.clone());
        assert_eq!(containers.len(), 2);
        for container in &containers {
            let sec = container.security_context.clone().unwrap();
            assert_eq!(sec.allow_privilege_escalation, Some(false));
            assert_eq!(sec.read_only_root_filesystem, Some(true));
            assert_eq!(sec.run_as_non_root, Some(true));
            assert_eq!(sec.capabilities.unwrap().drop.unwrap(), vec!["ALL"]);
        }

        // Read-only root means the writable mounts are the whole list.
        let daemon = &pod.containers[0];
        let mut writable: Vec<&str> = daemon
            .volume_mounts
            .as_ref()
            .unwrap()
            .iter()
            .filter(|m| m.read_only != Some(true))
            .map(|m| m.mount_path.as_str())
            .collect();
        writable.sort_unstable();
        assert_eq!(writable, vec!["/data", "/tmp"]);
    }

    /// The two probe shapes, and the flag that is the only difference between
    /// them.
    ///
    /// Off is the default and has to stay the default: the HTTP shape asks for
    /// a route only a new enough daemon serves, and a tenant on an older image
    /// would fail it on every period and never report Ready.
    #[test]
    fn the_readiness_probe_is_an_accept_until_the_operator_says_otherwise() {
        let mut c = test_config();
        assert!(!c.http_readiness, "the render must not opt a fleet in");

        let probe = tenant_deployment(&c)
            .spec
            .unwrap()
            .template
            .spec
            .unwrap()
            .containers[0]
            .readiness_probe
            .clone()
            .unwrap();
        assert_eq!(
            probe.tcp_socket.unwrap().port,
            IntOrString::Int(DAEMON_PORT)
        );
        assert!(probe.http_get.is_none(), "the door serves no health route");
        let (delay, period) = (probe.initial_delay_seconds, probe.period_seconds);

        c.http_readiness = true;
        let probe = tenant_deployment(&c)
            .spec
            .unwrap()
            .template
            .spec
            .unwrap()
            .containers[0]
            .readiness_probe
            .clone()
            .unwrap();
        let get = probe.http_get.unwrap();
        assert_eq!(get.path.as_deref(), Some(HEALTHZ_PATH));
        // The metrics listener, not the door: it is the one that is already
        // unauthenticated, so the probe holds no credential.
        assert_eq!(get.port, IntOrString::Int(METRICS_PORT));
        assert!(probe.tcp_socket.is_none(), "one action, not two");
        // Nothing else about the probe moves with the flag, so flipping it
        // changes exactly one thing about a tenant.
        assert_eq!(
            (probe.initial_delay_seconds, probe.period_seconds),
            (delay, period)
        );
    }

    /// The per-tenant soak: a replica is not Available until it has been Ready
    /// this long, which is what `rolled_out` waits on.
    #[test]
    fn a_tenant_pod_has_to_stay_ready_before_it_counts() {
        let mut c = test_config();
        assert_eq!(
            tenant_deployment(&c).spec.unwrap().min_ready_seconds,
            Some(DEFAULT_MIN_READY_SECS)
        );

        // Turned off, the field is ABSENT rather than 0: zero is the API
        // server's own default and would be dropped from the stored object,
        // leaving every drift report quoting a field the live object lacks.
        c.min_ready_secs = 0;
        assert_eq!(tenant_deployment(&c).spec.unwrap().min_ready_seconds, None);
    }

    #[test]
    fn user_namespaces_can_be_turned_off_for_a_cluster_that_lacks_them() {
        let mut c = test_config();
        c.user_namespaces = false;
        let d = tenant_deployment(&c);
        let pod = d.spec.unwrap().template.spec.unwrap();
        // Absent, not `true`: asking for the host's user namespace explicitly
        // would be a different and worse statement.
        assert_eq!(pod.host_users, None);
    }

    /// The v1 bug this contract calls out by name: the daemon reads these exact
    /// variable names, and nothing else configures it.
    #[test]
    fn the_daemon_gets_the_real_variable_names() {
        let c = test_config();
        let d = tenant_deployment(&c);
        let pod = d.spec.unwrap().template.spec.unwrap();
        let env = pod.containers[0].env.clone().unwrap();

        let plain: BTreeMap<&str, &str> = env
            .iter()
            .filter_map(|e| Some((e.name.as_str(), e.value.as_deref()?)))
            .collect();
        assert_eq!(plain["SQUELCH_BIND"], "0.0.0.0:8848");
        // Absent, the daemon serves no metrics at all; the collectors would
        // scrape a closed port forever and the dashboard would stay empty.
        assert_eq!(plain["SQUELCH_METRICS_BIND"], "0.0.0.0:9464");
        assert_eq!(plain["SQUELCH_CRED_BACKEND"], "file");
        assert_eq!(plain["SQUELCH_DB_PATH"], "/data/squelch.db");
        assert_eq!(plain["SQUELCH_CREDENTIALS_PATH"], "/data/credentials.json");
        assert_eq!(
            plain["SQUELCH_CRED_AGE_IDENTITY"],
            "/etc/squelch/identity/identity.txt"
        );
        assert_eq!(plain["HOME"], "/data");
        // Unpinned, the daemon picks its own embedding model, and the var is
        // absent rather than empty.
        assert!(!plain.contains_key("SQUELCH_EMBED_MODEL"));
        // Unset by default, so the daemon keeps its own idle-unload window.
        assert!(
            env.iter()
                .all(|e| e.name != "SQUELCH_EMBED_IDLE_UNLOAD_SECS"),
            "an unset embedder knob still reached the pod"
        );

        // The address comes from the Secret, so it is encrypted at rest and is
        // not in the Deployment anyone can `kubectl get -o yaml`.
        let email = env
            .iter()
            .find(|e| e.name == "SQUELCH_ACCOUNT_EMAIL")
            .unwrap();
        assert!(email.value.is_none());
        let from = email.value_from.clone().unwrap().secret_key_ref.unwrap();
        assert_eq!(from.name, "alice-identity");
        assert_eq!(from.key, "account-email");

        // With no gateway configured, the LLM block is entirely absent: not a
        // base URL, not a provider pin, not an optional key reference. This is
        // the byte-identical-when-unset half of the contract.
        // ANTHROPIC_API_KEY is in the list as a tombstone: the shared-key
        // bridge injected the real provider key under that name, and a revert
        // that resurrects it must fail here, not in production.
        for llm_var in [
            "SQUELCH_ANTHROPIC_BASE_URL",
            "SQUELCH_STAGE2_PROVIDER",
            "SQUELCH_STAGE2_API_KEY",
            "SQUELCH_ASSISTANT_API_KEY",
            "SQUELCH_STAGE1_MODEL",
            "SQUELCH_MODEL",
            "SQUELCH_STAGE1_GLOBAL_DAILY_CAP",
            "SQUELCH_STAGE2_GLOBAL_DAILY_CAP",
            "ANTHROPIC_API_KEY",
        ] {
            assert!(
                env.iter().all(|e| e.name != llm_var),
                "{llm_var} appears in an unconfigured pod"
            );
        }

        // Same claim for the relay block: no URL, no token reference, no
        // topic, and never the APNs environment.
        for relay_var in [
            "SQUELCH_RELAY_URL",
            "SQUELCH_RELAY_TOKEN",
            "SQUELCH_RELAY_TOPIC",
            "SQUELCH_RELAY_APNS_ENV",
        ] {
            assert!(
                env.iter().all(|e| e.name != relay_var),
                "{relay_var} appears in an unconfigured pod"
            );
        }
    }

    /// The push half of the env contract. A configured relay reaches the pod
    /// as a plain URL and an OPTIONAL secretKeyRef into the shared Secret:
    /// a missing Secret must leave every tenant booting, not wedge the fleet.
    #[test]
    fn a_configured_relay_reaches_the_daemon_and_the_token_stays_optional() {
        let mut c = test_config();
        c.relay_url = Some("https://relay.squelch.sh".to_string());
        let pod = tenant_deployment(&c).spec.unwrap().template.spec.unwrap();
        let env = pod.containers[0].env.clone().unwrap();

        let plain: BTreeMap<&str, &str> = env
            .iter()
            .filter_map(|e| Some((e.name.as_str(), e.value.as_deref()?)))
            .collect();
        assert_eq!(plain["SQUELCH_RELAY_URL"], "https://relay.squelch.sh");

        let token = env
            .iter()
            .find(|e| e.name == "SQUELCH_RELAY_TOKEN")
            .unwrap();
        // From the Secret, never inline: the pod spec is world-readable.
        assert!(token.value.is_none());
        let from = token.value_from.clone().unwrap().secret_key_ref.unwrap();
        assert_eq!(from.name, "apns-relay");
        assert_eq!(from.key, "token");
        assert_eq!(from.optional, Some(true));

        // No topic configured, no topic rendered: the relay's default stands.
        assert!(
            env.iter().all(|e| e.name != "SQUELCH_RELAY_TOPIC"),
            "an unset topic reached the pod"
        );
        // The APNs environment is per device now, never fleet-wide.
        assert!(
            env.iter().all(|e| e.name != "SQUELCH_RELAY_APNS_ENV"),
            "a fleet-wide APNs environment reached the pod"
        );
        // And the relay turns on nothing of the LLM block's.
        assert!(!plain.contains_key("SQUELCH_ANTHROPIC_BASE_URL"));
    }

    /// Every tenant pod references the Browserbase key with nothing
    /// configured, as an OPTIONAL reference and never a value in the pod spec,
    /// and a renamed Secret (a rotated key) is what the pod then names.
    #[test]
    fn every_pod_references_the_browserbase_key_optionally_and_never_inline() {
        for (configured, expected) in [
            (None, "browserbase"),
            (Some("browserbase-2"), "browserbase-2"),
        ] {
            let mut c = test_config();
            if let Some(name) = configured {
                c.browserbase_secret_name = name.to_string();
            }
            let pod = tenant_deployment(&c).spec.unwrap().template.spec.unwrap();
            let env = pod.containers[0].env.clone().unwrap();
            let var = env
                .iter()
                .find(|e| e.name == "SQUELCH_BROWSERBASE_API_KEY")
                .unwrap();
            assert!(var.value.is_none(), "the key is never inline");
            let from = var.value_from.clone().unwrap().secret_key_ref.unwrap();
            assert_eq!(from.name, expected);
            assert_eq!(from.key, BROWSERBASE_API_KEY_KEY);
            assert_eq!(
                from.optional,
                Some(true),
                "a cluster without the Secret must still boot its tenants"
            );
        }
    }

    /// The full relay set: the topic lands under the daemon's real name, the
    /// Secret can be renamed, and SQUELCH_RELAY_APNS_ENV is still absent.
    #[test]
    fn the_relay_topic_and_secret_name_reach_the_pod_when_configured() {
        let mut c = test_config();
        c.relay_url = Some("https://relay.squelch.sh".to_string());
        c.relay_secret_name = "passband-relay".to_string();
        c.relay_topic = Some("app.passband.client.ios".to_string());
        let pod = tenant_deployment(&c).spec.unwrap().template.spec.unwrap();
        let env = pod.containers[0].env.clone().unwrap();

        let plain: BTreeMap<&str, &str> = env
            .iter()
            .filter_map(|e| Some((e.name.as_str(), e.value.as_deref()?)))
            .collect();
        assert_eq!(plain["SQUELCH_RELAY_URL"], "https://relay.squelch.sh");
        assert_eq!(plain["SQUELCH_RELAY_TOPIC"], "app.passband.client.ios");

        let from = env
            .iter()
            .find(|e| e.name == "SQUELCH_RELAY_TOKEN")
            .unwrap()
            .value_from
            .clone()
            .unwrap()
            .secret_key_ref
            .unwrap();
        assert_eq!(from.name, "passband-relay");
        assert_eq!(from.key, RELAY_TOKEN_KEY);
        assert_eq!(from.optional, Some(true));

        assert!(
            env.iter().all(|e| e.name != "SQUELCH_RELAY_APNS_ENV"),
            "a fleet-wide APNs environment reached the pod"
        );
        // Each relay variable exactly once.
        for relay_var in [
            "SQUELCH_RELAY_URL",
            "SQUELCH_RELAY_TOKEN",
            "SQUELCH_RELAY_TOPIC",
        ] {
            assert_eq!(
                env.iter().filter(|e| e.name == relay_var).count(),
                1,
                "{relay_var}"
            );
        }
    }

    /// The LLM half of the env contract: a configured gateway reaches the pod
    /// as a base URL, a provider pin, and an OPTIONAL secretKeyRef — a tenant
    /// whose virtual key was never minted must still boot, and the daemon
    /// fail-softs triage when the var is absent.
    #[test]
    fn a_configured_gateway_reaches_the_daemon_and_the_key_stays_optional() {
        let c = llm_config();
        let pod = tenant_deployment(&c).spec.unwrap().template.spec.unwrap();
        let env = pod.containers[0].env.clone().unwrap();

        let plain: BTreeMap<&str, &str> = env
            .iter()
            .filter_map(|e| Some((e.name.as_str(), e.value.as_deref()?)))
            .collect();
        assert_eq!(
            plain["SQUELCH_ANTHROPIC_BASE_URL"],
            "https://llm.passband.internal"
        );
        assert_eq!(plain["SQUELCH_STAGE2_PROVIDER"], "anthropic");

        let key = env
            .iter()
            .find(|e| e.name == "SQUELCH_STAGE2_API_KEY")
            .unwrap();
        // From the Secret, never inline in the pod spec.
        assert!(key.value.is_none());
        let from = key.value_from.clone().unwrap().secret_key_ref.unwrap();
        assert_eq!(from.name, "alice-llm");
        assert_eq!(from.key, "api-key");
        assert_eq!(from.optional, Some(true));

        // The assistant relay credential, same Secret, same optional shape: a
        // tenant minted before the assistant era has no such data key and its
        // pod must still boot.
        let assistant = env
            .iter()
            .find(|e| e.name == "SQUELCH_ASSISTANT_API_KEY")
            .unwrap();
        assert!(assistant.value.is_none());
        let from = assistant
            .value_from
            .clone()
            .unwrap()
            .secret_key_ref
            .unwrap();
        assert_eq!(from.name, "alice-llm");
        assert_eq!(from.key, "assistant-api-key");
        assert_eq!(from.optional, Some(true));

        // The base URL alone pins no model and no cap: those are separate
        // knobs, absent unless the operator set them.
        for tuned in [
            "SQUELCH_STAGE1_MODEL",
            "SQUELCH_MODEL",
            "SQUELCH_STAGE1_GLOBAL_DAILY_CAP",
            "SQUELCH_STAGE2_GLOBAL_DAILY_CAP",
        ] {
            assert!(
                env.iter().all(|e| e.name != tuned),
                "{tuned} appeared unset"
            );
        }

        // "No tenant ever holds a real provider key" has to hold in BOTH
        // modes: a configured gateway injects the virtual key only, never the
        // shared-bridge ANTHROPIC_API_KEY this guarantee replaced.
        assert!(
            env.iter().all(|e| e.name != "ANTHROPIC_API_KEY"),
            "the raw provider key reappeared in a gateway-configured pod"
        );
    }

    /// The daemon reads `SQUELCH_EMBED_IDLE_UNLOAD_SECS`, but a hosted tenant's
    /// env is this CLOSED list, so the daemon knob is unreachable unless the
    /// warden renders it. `0` is the off switch, and it has to survive the
    /// render as `"0"` rather than being read as unset.
    #[test]
    fn the_embedder_idle_window_reaches_the_pod_when_the_operator_pins_one() {
        let mut c = test_config();
        c.embed_idle_unload_secs = Some(900);
        let pod = tenant_deployment(&c).spec.unwrap().template.spec.unwrap();
        let env = pod.containers[0].env.clone().unwrap();
        let plain: BTreeMap<&str, &str> = env
            .iter()
            .filter_map(|e| Some((e.name.as_str(), e.value.as_deref()?)))
            .collect();
        assert_eq!(plain["SQUELCH_EMBED_IDLE_UNLOAD_SECS"], "900");

        c.embed_idle_unload_secs = Some(0);
        let pod = tenant_deployment(&c).spec.unwrap().template.spec.unwrap();
        let env = pod.containers[0].env.clone().unwrap();
        let zero = env
            .iter()
            .find(|e| e.name == "SQUELCH_EMBED_IDLE_UNLOAD_SECS")
            .expect("the off switch is a value, not an absence");
        assert_eq!(zero.value.as_deref(), Some("0"));
    }

    /// The tuning knobs, each landing under the daemon's real variable name.
    /// Stage 2's model is `SQUELCH_MODEL`, not `SQUELCH_STAGE2_MODEL`: it
    /// predates the stage split.
    #[test]
    fn the_llm_tuning_knobs_land_under_the_daemons_real_names() {
        let mut c = llm_config();
        c.llm_stage1_model = Some("claude-haiku-4-5".to_string());
        c.llm_stage2_model = Some("claude-sonnet-4-5".to_string());
        c.llm_stage1_daily_cap = Some(200);
        c.llm_stage2_daily_cap = Some(40);
        let pod = tenant_deployment(&c).spec.unwrap().template.spec.unwrap();
        let env = pod.containers[0].env.clone().unwrap();

        let plain: BTreeMap<&str, &str> = env
            .iter()
            .filter_map(|e| Some((e.name.as_str(), e.value.as_deref()?)))
            .collect();
        assert_eq!(plain["SQUELCH_STAGE1_MODEL"], "claude-haiku-4-5");
        assert_eq!(plain["SQUELCH_MODEL"], "claude-sonnet-4-5");
        assert!(!plain.contains_key("SQUELCH_STAGE2_MODEL"));
        assert_eq!(plain["SQUELCH_STAGE1_GLOBAL_DAILY_CAP"], "200");
        assert_eq!(plain["SQUELCH_STAGE2_GLOBAL_DAILY_CAP"], "40");
    }

    /// The embedding pin reaching a tenant. Not part of the LLM block: it needs
    /// no gateway, because it names weights the daemon loads locally.
    ///
    /// The var is a CLOSED list's newest member. The daemon has read
    /// `SQUELCH_EMBED_MODEL` since the model was pinned, but nothing this file
    /// does not render can reach a tenant, so without this line the daemon-side
    /// override would be unreachable on hosted and moving the fleet's embedding
    /// model would mean cutting a new image.
    #[test]
    fn a_pinned_embedding_model_reaches_the_daemon_without_a_gateway() {
        let mut c = test_config();
        c.embed_model = Some("Xenova/bge-small-en-v1.5".to_string());
        let pod = tenant_deployment(&c).spec.unwrap().template.spec.unwrap();
        let env = pod.containers[0].env.clone().unwrap();

        let plain: BTreeMap<&str, &str> = env
            .iter()
            .filter_map(|e| Some((e.name.as_str(), e.value.as_deref()?)))
            .collect();
        assert_eq!(plain["SQUELCH_EMBED_MODEL"], "Xenova/bge-small-en-v1.5");
        // No gateway was configured, and the LLM block is still absent.
        assert!(!plain.contains_key("SQUELCH_ANTHROPIC_BASE_URL"));
    }

    /// The rotation mechanism for the virtual key, same story as the
    /// credential hash: a keyed pod carries the hash, an unkeyed pod carries
    /// nothing, and a different key is a different pod spec.
    #[test]
    fn a_new_llm_key_rolls_the_deployment_and_an_unkeyed_pod_carries_no_hash() {
        let c = llm_config();
        let n = name();
        let annotations = |llm_hash: Option<&str>| {
            deployment(&c, &n, &credential_hash("ct"), llm_hash, None)
                .spec
                .unwrap()
                .template
                .metadata
                .unwrap()
                .annotations
                .unwrap()
        };

        assert!(!annotations(None).contains_key(LLM_KEY_HASH_ANNOTATION));
        let first = annotations(Some(&credential_hash("key-one")));
        let a = first[LLM_KEY_HASH_ANNOTATION].clone();
        assert_eq!(
            a,
            annotations(Some(&credential_hash("key-one")))[LLM_KEY_HASH_ANNOTATION],
            "the same key is the same pod spec"
        );
        assert_ne!(
            a,
            annotations(Some(&credential_hash("key-two")))[LLM_KEY_HASH_ANNOTATION],
            "a new key must roll the pod"
        );
        // A hash, not the key: the annotation is world-readable.
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        // And the credential hash rides alongside untouched.
        assert!(first.contains_key(CREDENTIAL_HASH_ANNOTATION));
    }

    #[test]
    fn the_llm_secret_is_the_key_verbatim_under_the_one_data_key() {
        let c = test_config();
        let secret = llm_secret(&c, &name(), Some("sk-vk-abc123"), None);
        assert_eq!(secret.metadata.name.as_deref(), Some("alice-llm"));
        assert_eq!(secret.metadata.namespace.as_deref(), Some("tenants"));
        assert_eq!(secret.type_.as_deref(), Some("Opaque"));
        let data = secret.string_data.unwrap();
        assert_eq!(data[LLM_API_KEY_KEY], "sk-vk-abc123");
        assert_eq!(data.len(), 1);
    }

    /// The assistant key rides in the SAME Secret under its own data key, and
    /// only when one was minted: the no-assistant shape stays byte-identical
    /// to what the pre-assistant warden wrote.
    #[test]
    fn the_llm_secret_carries_the_assistant_key_only_when_minted() {
        let c = test_config();
        let secret = llm_secret(&c, &name(), Some("sk-vk-abc123"), Some("sk-vk-assistant"));
        let data = secret.string_data.unwrap();
        assert_eq!(data[LLM_API_KEY_KEY], "sk-vk-abc123");
        assert_eq!(data[ASSISTANT_API_KEY_KEY], "sk-vk-assistant");
        assert_eq!(data.len(), 2);

        // The other half-failed-mint shape: assistant alone, no triage entry.
        let secret = llm_secret(&c, &name(), None, Some("sk-vk-assistant"));
        let data = secret.string_data.unwrap();
        assert_eq!(data[ASSISTANT_API_KEY_KEY], "sk-vk-assistant");
        assert_eq!(data.len(), 1);
    }

    /// The combined roll hash: rotating EITHER key moves it, the same pair is
    /// the same hash, and the no-assistant shape is distinct from every keyed
    /// one. This is the single helper both provisioning sites go through.
    #[test]
    fn the_combined_llm_hash_moves_when_either_key_does() {
        let bare = llm_keys_hash(Some("triage-one"), None);
        assert_eq!(bare, llm_keys_hash(Some("triage-one"), None));
        assert_ne!(bare, llm_keys_hash(Some("triage-two"), None));

        let keyed = llm_keys_hash(Some("triage-one"), Some("assist-one"));
        assert_ne!(bare, keyed, "minting the assistant key must move the hash");
        assert_eq!(keyed, llm_keys_hash(Some("triage-one"), Some("assist-one")));
        assert_ne!(keyed, llm_keys_hash(Some("triage-one"), Some("assist-two")));
        assert_ne!(keyed, llm_keys_hash(Some("triage-two"), Some("assist-one")));

        // Either slot alone is its own shape, distinct from the pair.
        let assistant_only = llm_keys_hash(None, Some("assist-one"));
        assert_ne!(assistant_only, keyed);
        assert_ne!(assistant_only, llm_keys_hash(Some("assist-one"), None));

        // Still a bare hex digest: it lands in a world-readable annotation.
        assert_eq!(keyed.len(), 64);
        assert!(keyed.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    /// Without these the daemon exits at boot: `Config::oauth_client` is
    /// required by `squelchd serve`, and a refresh token only works for the
    /// client that minted it, which for a hosted tenant is the control plane's
    /// web client.
    #[test]
    fn the_daemon_gets_the_oauth_client_the_control_plane_consented_with() {
        let c = test_config();
        let pod = tenant_deployment(&c).spec.unwrap().template.spec.unwrap();
        let env = pod.containers[0].env.clone().unwrap();

        for (name, key) in [
            ("SQUELCH_CLIENT_ID", "client_id"),
            ("SQUELCH_CLIENT_SECRET", "client_secret"),
        ] {
            let var = env.iter().find(|e| e.name == name).unwrap();
            // From the Secret, never inline: a client secret in a pod spec is a
            // client secret in `kubectl get deploy -o yaml`.
            assert!(var.value.is_none(), "{name} is inline in the pod spec");
            let from = var.value_from.clone().unwrap().secret_key_ref.unwrap();
            assert_eq!(from.name, "google-oauth-client");
            assert_eq!(from.key, key);
        }
    }

    #[test]
    fn the_oauth_secret_can_be_renamed() {
        let mut c = test_config();
        c.oauth_secret_name = "passband-oauth".to_string();
        let pod = tenant_deployment(&c).spec.unwrap().template.spec.unwrap();
        let refs: Vec<String> = pod.containers[0]
            .env
            .clone()
            .unwrap()
            .iter()
            .filter(|e| e.name.starts_with("SQUELCH_CLIENT_"))
            .map(|e| {
                e.value_from
                    .clone()
                    .unwrap()
                    .secret_key_ref
                    .unwrap()
                    .name
                    .clone()
            })
            .collect();
        assert_eq!(refs, vec!["passband-oauth", "passband-oauth"]);
    }

    /// The console's Google half is dead without this: the daemon renders the
    /// sign-in link only when `SQUELCH_CONSOLE_SSO_URL` is set, and nothing else
    /// in the hosted path sets it.
    #[test]
    fn the_console_sso_url_reaches_the_daemon_when_it_is_configured() {
        let mut c = test_config();
        c.console_sso_url = Some("https://signup.passband.app".to_string());
        let pod = tenant_deployment(&c).spec.unwrap().template.spec.unwrap();
        let var = pod.containers[0]
            .env
            .clone()
            .unwrap()
            .into_iter()
            .find(|e| e.name == "SQUELCH_CONSOLE_SSO_URL")
            .expect("SQUELCH_CONSOLE_SSO_URL is missing from the tenant pod");
        // Plain, not a secretKeyRef: it is an origin that ends up in a public
        // page, so there is nothing here to keep out of a pod spec.
        assert_eq!(var.value.as_deref(), Some("https://signup.passband.app"));
    }

    /// The sharing pair rides on the SAME switch as the console link, because
    /// they are the same fact: an origin to talk to. The token is a
    /// `secretKeyRef` and OPTIONAL, so a tenant nobody has run `share mint`
    /// for still boots.
    #[test]
    fn the_control_plane_pair_reaches_the_daemon_when_the_origin_is_configured() {
        let mut c = test_config();
        c.console_sso_url = Some("https://signup.passband.app".to_string());
        let pod = tenant_deployment(&c).spec.unwrap().template.spec.unwrap();
        let env = pod.containers[0].env.clone().unwrap();

        let url = env
            .iter()
            .find(|e| e.name == "SQUELCH_CONTROL_URL")
            .expect("SQUELCH_CONTROL_URL is missing from the tenant pod");
        assert_eq!(url.value.as_deref(), Some("https://signup.passband.app"));

        let token = env
            .iter()
            .find(|e| e.name == "SQUELCH_CONTROL_TOKEN")
            .expect("SQUELCH_CONTROL_TOKEN is missing from the tenant pod");
        // NEVER a plain value: it is a live bearer, and a pod spec is
        // world-readable to anyone who can list Deployments.
        assert!(token.value.is_none());
        let key_ref = token
            .value_from
            .clone()
            .unwrap()
            .secret_key_ref
            .expect("the share token must arrive by reference");
        assert_eq!(key_ref.name, "alice-control");
        assert_eq!(key_ref.key, SHARE_TOKEN_KEY);
        assert_eq!(
            key_ref.optional,
            Some(true),
            "an unminted tenant must still boot"
        );
    }

    /// No control plane, no sharing: with the origin unconfigured neither
    /// variable exists, so a self-hosted daemon is never told to post an
    /// invite mint at a service that is not there.
    #[test]
    fn no_control_origin_means_no_sharing_variables() {
        let c = test_config();
        assert!(c.console_sso_url.is_none());
        let pod = tenant_deployment(&c).spec.unwrap().template.spec.unwrap();
        let env = pod.containers[0].env.clone().unwrap();
        assert!(
            !env.iter()
                .any(|e| e.name == "SQUELCH_CONTROL_URL" || e.name == "SQUELCH_CONTROL_TOKEN")
        );
    }

    /// The share token's own roll: minting one has to reach a pod that is
    /// already running, and revoking has to take the variable away again.
    #[test]
    fn a_share_token_rolls_the_deployment_and_an_unshared_pod_carries_no_hash() {
        let c = test_config();
        let n = name();
        let annotations = |share_hash: Option<&str>| {
            deployment(&c, &n, &credential_hash("ct"), None, share_hash)
                .spec
                .unwrap()
                .template
                .metadata
                .unwrap()
                .annotations
                .unwrap()
        };

        assert!(!annotations(None).contains_key(SHARE_TOKEN_HASH_ANNOTATION));
        let first = annotations(Some(&credential_hash("token-one")));
        let a = first[SHARE_TOKEN_HASH_ANNOTATION].clone();
        assert_eq!(
            a,
            annotations(Some(&credential_hash("token-one")))[SHARE_TOKEN_HASH_ANNOTATION],
            "the same token is the same pod spec"
        );
        assert_ne!(
            a,
            annotations(Some(&credential_hash("token-two")))[SHARE_TOKEN_HASH_ANNOTATION],
            "a rotated token must roll the pod"
        );
        // A hash, not the token: the annotation is world-readable.
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(first.contains_key(CREDENTIAL_HASH_ANNOTATION));
    }

    /// The two rotations are INDEPENDENT: a pod carrying both annotations has
    /// each move on its own, which is what stops one rotation knocking the
    /// other's off the template.
    #[test]
    fn the_llm_and_share_rolls_do_not_collide() {
        let c = test_config();
        let n = name();
        let both = deployment(
            &c,
            &n,
            &credential_hash("ct"),
            Some(&credential_hash("key")),
            Some(&credential_hash("token")),
        )
        .spec
        .unwrap()
        .template
        .metadata
        .unwrap()
        .annotations
        .unwrap();
        assert!(both.contains_key(LLM_KEY_HASH_ANNOTATION));
        assert!(both.contains_key(SHARE_TOKEN_HASH_ANNOTATION));
        assert_ne!(
            both[LLM_KEY_HASH_ANNOTATION], both[SHARE_TOKEN_HASH_ANNOTATION],
            "two credentials, two hashes"
        );
    }

    #[test]
    fn the_control_secret_is_the_token_verbatim_under_the_one_data_key() {
        let c = test_config();
        let secret = control_secret(&c, &name(), "pbs_abc123");
        assert_eq!(secret.metadata.name.as_deref(), Some("alice-control"));
        assert_eq!(secret.metadata.namespace.as_deref(), Some("tenants"));
        assert_eq!(secret.type_.as_deref(), Some("Opaque"));
        let data = secret.string_data.unwrap();
        assert_eq!(data[SHARE_TOKEN_KEY], "pbs_abc123");
        assert_eq!(data.len(), 1);
    }

    /// Unset means ABSENT, not empty. A self-host daemon's console is the
    /// pasted-code form alone, and a hosted deploy that has not configured the
    /// control plane's origin gets the same page rather than a broken link.
    #[test]
    fn no_console_sso_url_means_no_variable_at_all() {
        let c = test_config();
        assert!(c.console_sso_url.is_none());
        let pod = tenant_deployment(&c).spec.unwrap().template.spec.unwrap();
        assert!(
            !pod.containers[0]
                .env
                .clone()
                .unwrap()
                .iter()
                .any(|e| e.name == "SQUELCH_CONSOLE_SSO_URL")
        );
    }

    /// Every container bounded, and the one writable scratch volume bounded
    /// too. An unbounded tenant is every other tenant's problem.
    #[test]
    fn nothing_in_the_pod_is_unbounded() {
        let c = test_config();
        let pod = tenant_deployment(&c).spec.unwrap().template.spec.unwrap();

        let mut containers = pod.init_containers.clone().unwrap();
        containers.extend(pod.containers.clone());
        assert_eq!(containers.len(), 2);
        for container in &containers {
            let r = container.resources.clone().unwrap();
            let requests = r.requests.unwrap();
            let limits = r.limits.unwrap();
            for key in ["cpu", "memory", "ephemeral-storage"] {
                assert!(
                    requests.contains_key(key),
                    "{} has no {key} request",
                    container.name
                );
                assert!(
                    limits.contains_key(key),
                    "{} has no {key} limit",
                    container.name
                );
            }
        }

        // The daemon's numbers come from config, so an operator can size them
        // to the box.
        let daemon = pod.containers[0].resources.clone().unwrap();
        assert_eq!(
            daemon.requests.clone().unwrap()["cpu"],
            Quantity("100m".into())
        );
        assert_eq!(
            daemon.requests.clone().unwrap()["memory"],
            Quantity("384Mi".into())
        );
        assert_eq!(
            daemon.requests.unwrap()["ephemeral-storage"],
            Quantity("256Mi".into())
        );
        assert_eq!(
            daemon.limits.clone().unwrap()["cpu"],
            Quantity("1000m".into())
        );
        assert_eq!(
            daemon.limits.clone().unwrap()["memory"],
            Quantity("1Gi".into())
        );
        assert_eq!(
            daemon.limits.unwrap()["ephemeral-storage"],
            Quantity("1Gi".into())
        );

        // The one emptyDir. Without a sizeLimit it is the node's free space.
        let tmp = pod
            .volumes
            .as_ref()
            .unwrap()
            .iter()
            .find(|v| v.name == "tmp")
            .unwrap()
            .empty_dir
            .clone()
            .unwrap();
        assert_eq!(tmp.size_limit, Some(Quantity("512Mi".into())));
    }

    #[test]
    fn the_bounds_come_from_config() {
        let mut c = test_config();
        c.daemon_resources.memory_limit = "2Gi".to_string();
        c.tmp_size = "64Mi".to_string();
        let pod = tenant_deployment(&c).spec.unwrap().template.spec.unwrap();
        assert_eq!(
            pod.containers[0].resources.clone().unwrap().limits.unwrap()["memory"],
            Quantity("2Gi".into())
        );
        assert_eq!(
            pod.volumes
                .unwrap()
                .iter()
                .find(|v| v.name == "tmp")
                .unwrap()
                .empty_dir
                .clone()
                .unwrap()
                .size_limit,
            Some(Quantity("64Mi".into()))
        );
    }

    /// The re-consent path, asserted on the object: a different sealed blob is
    /// a different pod template, which is a roll, which re-runs the seeder.
    #[test]
    fn a_new_ciphertext_rolls_the_deployment() {
        let c = test_config();
        let n = name();
        let first = crate::testing::armored("first");
        let second = crate::testing::armored("second");

        let annotation = |ct: &str| {
            deployment(&c, &n, &credential_hash(ct), None, None)
                .spec
                .unwrap()
                .template
                .metadata
                .unwrap()
                .annotations
                .unwrap()[CREDENTIAL_HASH_ANNOTATION]
                .clone()
        };

        let a = annotation(&first);
        assert_eq!(a, annotation(&first), "the same blob is the same pod spec");
        assert_ne!(a, annotation(&second), "a new blob must roll the pod");
        // A hash, not the blob: this annotation is readable by anyone who can
        // read a Deployment.
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(!a.contains("AGE"));
    }

    /// The seeder's half of the same story. It is still a constant with no
    /// tenant value in it, and it now clobbers exactly when the mounted Secret
    /// differs from what it last installed.
    #[test]
    fn the_seed_script_reinstalls_only_a_changed_credential() {
        assert!(SEED_SCRIPT.contains("sha256sum /etc/squelch/credential/credentials.json"));
        assert!(SEED_SCRIPT.contains(CREDENTIAL_MARKER_PATH));
        assert!(SEED_SCRIPT.contains("if [ \"$want\" != \"$have\" ]"));
        // The old no-clobber guard is what made rotation impossible.
        assert!(!SEED_SCRIPT.contains("if [ ! -f /data/credentials.json ]"));
    }

    /// The weights half seeds model by model. The whole-cache guard it replaced
    /// could only ever fire on a tenant's very first boot, which meant a tenant
    /// provisioned before the PVC existed, or one alive on the day the pinned
    /// model changed, took nothing from it ever again and downloaded instead.
    #[test]
    fn the_seed_script_fills_in_a_missing_model_beside_the_ones_a_tenant_has() {
        // The guard that made it first-boot-only.
        assert!(!SEED_SCRIPT.contains("[ ! -d /data/.local/share/squelch/models ]"));
        // Per entry, and skipping what is already there.
        assert!(SEED_SCRIPT.contains("for m in /models/*; do"));
        assert!(SEED_SCRIPT.contains("if [ ! -e \"$d\" ]; then"));
        // `set -eu` still governs the credentials half above: an unset variable
        // or a failed install there must stop the container.
        assert!(SEED_SCRIPT.starts_with("set -eu\n"));
    }

    /// The destination appears complete or not at all. Copying in place would
    /// pair with the "skip what exists" guard to make one interrupted `cp` a
    /// permanent one: the half-copied directory is skipped by every later boot.
    /// hf-hub re-fetches a dangling link, so most of those heal at the price of
    /// the download; a truncated file at the right path does not.
    #[test]
    fn a_model_is_copied_to_a_temp_path_and_renamed_into_place() {
        assert!(SEED_SCRIPT.contains("t=$d.seed-tmp"));
        assert!(SEED_SCRIPT.contains("cp -r \"$m\" \"$t\""));
        assert!(SEED_SCRIPT.contains("mv \"$t\" \"$d\""));
        // Never the destination directly: that is the whole bug.
        assert!(!SEED_SCRIPT.contains("cp -r \"$m\" \"$d\""));
        // An interrupted run costs disk once, not once per restart.
        assert!(SEED_SCRIPT.contains("rm -rf \"$t\""));
        // And the sweep has to come BEFORE the copy, or it collects the temp
        // directory the same iteration just built.
        let sweep = SEED_SCRIPT.find("rm -rf \"$t\"").unwrap();
        let copy = SEED_SCRIPT.find("cp -r \"$m\" \"$t\"").unwrap();
        assert!(sweep < copy, "the sweep must precede the copy");
    }

    /// A model the init container cannot copy costs that tenant the download
    /// this feature exists to avoid. Fatal, it would cost the FLEET: one source
    /// file the container cannot read (a shared volume whose modes or ownership
    /// are wrong) exits every tenant's init container 1 on its next roll, and
    /// [`crate::provision`]'s roll reads a tenant that will not come up as a
    /// casualty and halts. So the copy is chained, not left to `set -e`.
    /// `want=$(sha256sum ... | cut ...)` takes the PIPELINE's status, which is
    /// `cut`'s, so under `set -e` a missing or unreadable credential file did
    /// not stop the script: `want` came out empty, matched the empty `have` of a
    /// fresh volume, the copy was skipped, and the init container exited 0 with
    /// no credentials installed. A daemon with no credentials file is not a
    /// daemon, so an empty hash is the init container's failure, said out loud.
    #[test]
    fn an_absent_credential_file_fails_the_seed_instead_of_seeding_nothing() {
        let guard = "[ -n \"$want\" ] || { echo 'squelch-seed: no credential file to seed from' >&2; exit 1; }";
        assert!(SEED_SCRIPT.contains(guard));
        let hashed = SEED_SCRIPT.find("want=$(sha256sum").unwrap();
        let guarded = SEED_SCRIPT.find(guard).unwrap();
        let compared = SEED_SCRIPT.find("if [ \"$want\" != \"$have\" ]").unwrap();
        assert!(
            hashed < guarded && guarded < compared,
            "guard sits between the hash and the compare"
        );
    }

    #[test]
    fn a_failed_model_copy_does_not_fail_the_pod() {
        // Chained, with the fallback branch that keeps the shell's status 0.
        assert!(SEED_SCRIPT.contains("cp -r \"$m\" \"$t\" && mv \"$t\" \"$d\" || {"));
        assert!(SEED_SCRIPT.contains("the daemon will download it"));
        // The fallback sweeps its own wreckage, so a failure costs no disk.
        let fallback = SEED_SCRIPT.find("|| {").unwrap();
        let swept = SEED_SCRIPT[fallback..].find("rm -rf \"$t\"").unwrap();
        let said = SEED_SCRIPT[fallback..]
            .find("squelch-seed: could not copy")
            .unwrap();
        assert!(swept < said, "sweep the temp path before saying so");
        // `mkdir -p` rides in the `if` condition for the same reason: a failure
        // there is a verdict, not an exit.
        assert!(
            SEED_SCRIPT
                .contains("if [ -d /models ] && mkdir -p /data/.local/share/squelch/models; then")
        );
        // The credentials half stays strict. A daemon with no credentials file
        // is not a daemon, and it has nothing to fall back to.
        assert!(
            SEED_SCRIPT
                .contains(" cp /etc/squelch/credential/credentials.json /data/credentials.json\n")
        );
    }

    #[test]
    fn the_identity_is_projected_one_key_at_a_time_and_read_only() {
        let c = test_config();
        let d = tenant_deployment(&c);
        let pod = d.spec.unwrap().template.spec.unwrap();

        let identity = pod
            .volumes
            .as_ref()
            .unwrap()
            .iter()
            .find(|v| v.name == "identity")
            .unwrap()
            .secret
            .clone()
            .unwrap();
        assert_eq!(identity.secret_name.as_deref(), Some("alice-identity"));
        let items = identity.items.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].key, "identity.txt");
        assert_eq!(identity.default_mode, Some(0o400));

        let mount = pod.containers[0]
            .volume_mounts
            .as_ref()
            .unwrap()
            .iter()
            .find(|m| m.name == "identity")
            .unwrap();
        assert_eq!(mount.mount_path, "/etc/squelch/identity");
        assert_eq!(mount.read_only, Some(true));
    }

    /// The sealed blob is seeded onto the tenant's own volume, because the
    /// daemon rewrites it on every token refresh and a Secret mount is
    /// read-only. The daemon container must NOT mount the Secret.
    #[test]
    fn the_credential_secret_is_mounted_only_by_the_init_container() {
        let c = test_config();
        let d = tenant_deployment(&c);
        let pod = d.spec.unwrap().template.spec.unwrap();

        let daemon_mounts = pod.containers[0].volume_mounts.clone().unwrap();
        assert!(daemon_mounts.iter().all(|m| m.name != "credential"));

        let seed = &pod.init_containers.as_ref().unwrap()[0];
        assert_eq!(seed.name, "seed");
        let seed_mount = seed
            .volume_mounts
            .as_ref()
            .unwrap()
            .iter()
            .find(|m| m.name == "credential")
            .unwrap();
        assert_eq!(seed_mount.mount_path, "/etc/squelch/credential");
        assert_eq!(seed_mount.read_only, Some(true));
        // The script is a constant: no tenant value is interpolated into it.
        assert_eq!(seed.args.clone().unwrap(), vec![SEED_SCRIPT.to_string()]);
        assert!(!SEED_SCRIPT.contains("alice"));
    }

    #[test]
    fn the_model_volume_appears_only_when_one_is_configured() {
        let mut c = test_config();
        assert!(c.model_pvc.is_none());
        let pod = tenant_deployment(&c).spec.unwrap().template.spec.unwrap();
        assert!(pod.volumes.unwrap().iter().all(|v| v.name != "models"));

        c.model_pvc = Some("squelch-models".to_string());
        let pod = tenant_deployment(&c).spec.unwrap().template.spec.unwrap();
        let models = pod
            .volumes
            .as_ref()
            .unwrap()
            .iter()
            .find(|v| v.name == "models")
            .unwrap();
        assert_eq!(
            models.persistent_volume_claim.as_ref().unwrap().claim_name,
            "squelch-models"
        );
        assert_eq!(
            models.persistent_volume_claim.as_ref().unwrap().read_only,
            Some(true)
        );
        // Mounted by the seeder only; the daemon never sees it.
        assert!(
            pod.containers[0]
                .volume_mounts
                .as_ref()
                .unwrap()
                .iter()
                .all(|m| m.name != "models")
        );
    }

    #[test]
    fn a_rolling_update_can_never_double_mount_the_volume() {
        let c = test_config();
        let spec = tenant_deployment(&c).spec.unwrap();
        assert_eq!(spec.replicas, Some(1));
        assert_eq!(spec.strategy.unwrap().type_.as_deref(), Some("Recreate"));
    }

    /// The `/mcp` refusal, asserted on the typed object. Hosted publishes the
    /// human door and nothing else, so the agent door matches no rule and the
    /// ingress controller answers 404.
    #[test]
    fn the_ingress_does_not_route_the_agent_door() {
        let c = test_config();
        let ing = ingress(&c, &name());
        let spec = ing.spec.unwrap();
        assert_eq!(spec.ingress_class_name.as_deref(), Some("traefik"));

        let rules = spec.rules.unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].host.as_deref(), Some("alice.passband.email"));

        let paths = rules[0].http.clone().unwrap().paths;
        let declared: Vec<&str> = paths.iter().map(|p| p.path.as_deref().unwrap()).collect();
        // The human door is the API, the browser console, the pixel, and the
        // bare root's redirect. The agent door is not on this list and that is
        // the whole point.
        assert_eq!(declared, vec!["/client", "/console", "/t", "/"]);
        for path in &paths {
            // "/" is Exact and everything else is Prefix: an Exact root
            // matches one path in the universe, a Prefix root would match all
            // of them.
            let expected = if path.path.as_deref() == Some("/") {
                "Exact"
            } else {
                "Prefix"
            };
            assert_eq!(path.path_type, expected, "{:?}", path.path);
            assert_eq!(
                path.backend.service.as_ref().unwrap().name,
                "alice",
                "every path goes to the tenant's own Service"
            );
        }
        // The load-bearing assertion: nothing routes the agent door, under any
        // spelling. Prefix paths are checked as string prefixes (coarser than
        // Kubernetes' element-wise match, so passing here passes there); the
        // Exact root matches only a literal "/", which none of these are.
        let prefixes: Vec<&str> = paths
            .iter()
            .filter(|p| p.path_type == "Prefix")
            .map(|p| p.path.as_deref().unwrap())
            .collect();
        // This is the render with NO OAuth issuer configured, which is every
        // deploy until an operator turns the agent door on: the door and its
        // metadata are both absent.
        assert!(c.mcp_oauth_issuer.is_none());
        for spelling in [
            "/mcp",
            "/mcp/",
            "/mcp/messages",
            "/mcp/sse",
            "/.well-known/oauth-protected-resource",
            "/.well-known/oauth-protected-resource/mcp",
        ] {
            assert!(
                !prefixes.iter().any(|p| spelling.starts_with(*p)),
                "{spelling} would be routed to the daemon"
            );
            assert_ne!(spelling, "/", "an Exact root cannot match {spelling}");
        }
        // ...and the new prefix reaches the console and stops there.
        assert!(declared.contains(&"/console"));
        for spelling in ["/console", "/console/callback", "/console/pair"] {
            assert!(
                declared.iter().any(|p| spelling.starts_with(*p)),
                "{spelling} would 404 at the ingress"
            );
        }
    }

    /// [`test_config`] with the agent door published: an AuthKit-shaped
    /// issuer and nothing else changed.
    fn agent_door_config() -> Config {
        let mut c = test_config();
        c.mcp_oauth_issuer = Some("https://example.authkit.app".to_string());
        c
    }

    /// Kubernetes' `Prefix` match, as the Ingress spec defines it: ELEMENT-WISE
    /// on `/`-separated segments, with empty segments (and so a trailing `/`)
    /// ignored. `/mcp` matches `/mcp`, `/mcp/` and `/mcp/x`, and does NOT match
    /// `/mcpx`.
    ///
    /// The other tests in this module use a plain string prefix instead, which
    /// is coarser: everything this function matches, a string prefix matches
    /// too, so "no string prefix matches" proves "unrouted" under the spec AND
    /// under any controller that implements `Prefix` as a plain string prefix.
    /// That stronger check is used wherever it can pass. It cannot for a
    /// spelling like `/mcpx`, which is unrouted under the spec and would reach
    /// the daemon under a string-prefix controller; see the test that uses this.
    fn k8s_prefix_matches(prefix: &str, path: &str) -> bool {
        let want: Vec<&str> = prefix.split('/').filter(|e| !e.is_empty()).collect();
        let got: Vec<&str> = path.split('/').filter(|e| !e.is_empty()).collect();
        got.len() >= want.len() && got[..want.len()] == want[..]
    }

    /// The Ingress's declared (path, pathType) pairs, in order.
    fn ingress_paths(config: &Config, label: &str) -> Vec<(String, String)> {
        let n = TenantName::parse(label).unwrap();
        let rules = ingress(config, &n).spec.unwrap().rules.unwrap();
        assert_eq!(rules.len(), 1, "one vhost per tenant");
        rules[0]
            .http
            .clone()
            .unwrap()
            .paths
            .into_iter()
            .map(|p| (p.path.unwrap(), p.path_type))
            .collect()
    }

    /// The daemon container's environment.
    fn daemon_env_of(config: &Config, label: &str) -> Vec<EnvVar> {
        let n = TenantName::parse(label).unwrap();
        deployment(config, &n, &credential_hash("ct"), None, None)
            .spec
            .unwrap()
            .template
            .spec
            .unwrap()
            .containers[0]
            .env
            .clone()
            .unwrap()
    }

    /// Publishing the agent door adds EXACTLY its two prefixes to the Ingress,
    /// as Prefix paths to the same backend, and the bare root stays Exact.
    #[test]
    fn an_issuer_publishes_the_agent_door_and_nothing_else() {
        let c = agent_door_config();
        let ing = ingress(&c, &name());
        let rules = ing.spec.unwrap().rules.unwrap();
        assert_eq!(rules[0].host.as_deref(), Some("alice.passband.email"));
        let paths = rules[0].http.clone().unwrap().paths;

        let declared: Vec<(&str, &str)> = paths
            .iter()
            .map(|p| (p.path.as_deref().unwrap(), p.path_type.as_str()))
            .collect();
        assert_eq!(
            declared,
            vec![
                ("/client", "Prefix"),
                ("/console", "Prefix"),
                ("/t", "Prefix"),
                ("/mcp", "Prefix"),
                ("/.well-known/oauth-protected-resource", "Prefix"),
                // Still Exact: a Prefix root would route every path there is.
                ("/", "Exact"),
            ]
        );
        for path in &paths {
            let backend = path.backend.service.as_ref().unwrap();
            assert_eq!(
                backend.name, "alice",
                "every path goes to the tenant's own Service"
            );
            assert_eq!(
                backend.port,
                Some(ServiceBackendPort {
                    name: Some("http".to_string()),
                    ..Default::default()
                }),
                "the agent door is the daemon port, never the metrics port"
            );
        }

        let prefixes: Vec<&str> = paths
            .iter()
            .filter(|p| p.path_type == "Prefix")
            .map(|p| p.path.as_deref().unwrap())
            .collect();

        // ROUTED, under the spec's element-wise match (which implies a string
        // prefix match too, so these are routed under either reading).
        for spelling in [
            "/mcp",
            "/mcp/",
            "/.well-known/oauth-protected-resource",
            "/.well-known/oauth-protected-resource/mcp",
        ] {
            assert!(
                prefixes.iter().any(|p| k8s_prefix_matches(p, spelling)),
                "{spelling} would 404 at the ingress with the agent door published"
            );
        }

        // UNROUTED, under the STRONGER string-prefix check: no declared prefix
        // is even a string prefix of these, so no controller, however it reads
        // `Prefix`, sends them to the daemon. The Exact root matches only a
        // literal "/", which none of them are. `/.well-known/oauth-authorization-server`
        // is on the list by name: the authorization server is AuthKit, and a
        // tenant vhost must not answer for it.
        for spelling in [
            "/metrics",
            "/metrics/",
            "/healthz",
            "/.well-known/oauth-authorization-server",
            "/.well-known/openid-configuration",
            "/.well-known/anything-else",
            "/.well-known",
            "/.well-known/",
        ] {
            assert!(
                !prefixes.iter().any(|p| spelling.starts_with(*p)),
                "{spelling} would be routed to the daemon"
            );
            assert_ne!(spelling, "/", "an Exact root cannot match {spelling}");
        }

        // UNROUTED under the spec only. `/mcp` is a string prefix of `/mcpx`,
        // so the coarse check above cannot clear it; Kubernetes' element-wise
        // match does not route it. A controller that implements `Prefix` as a
        // plain string prefix WOULD hand it to the daemon, which serves no
        // such route and answers 404 itself, so the worst case is a 404 from a
        // different process. This asserts what the Ingress object says, which
        // is all this function controls.
        for spelling in [
            "/mcpx",
            "/mcp-admin",
            "/.well-known/oauth-protected-resourcex",
        ] {
            assert!(
                !prefixes.iter().any(|p| k8s_prefix_matches(p, spelling)),
                "{spelling} would be routed to the daemon"
            );
        }
    }

    /// Publishing the agent door changes NOTHING else about the Ingress: take
    /// its two paths back out and what is left is the unpublished object, field
    /// for field. And unpublished is what every existing test above pins.
    #[test]
    fn the_agent_door_is_the_only_difference_an_issuer_makes_to_the_ingress() {
        let open = serde_json::to_value(ingress(&agent_door_config(), &name())).unwrap();
        let shut = serde_json::to_value(ingress(&test_config(), &name())).unwrap();

        let mut stripped = open.clone();
        let paths = stripped["spec"]["rules"][0]["http"]["paths"]
            .as_array_mut()
            .unwrap();
        let before = paths.len();
        paths.retain(|p| !AGENT_DOOR_PREFIXES.contains(&p["path"].as_str().unwrap()));
        assert_eq!(before - paths.len(), AGENT_DOOR_PREFIXES.len());
        assert_eq!(stripped, shut);
        assert_ne!(open, shut, "the issuer has to publish something");
    }

    /// With no issuer configured, the daemon is told nothing about the agent
    /// door: no `SQUELCH_MCP_*` variable at all, so the pod is byte-identical
    /// to one a warden that never heard of OAuth rendered, and the daemon stays
    /// in its open, loopback-guarded mode behind an Ingress that does not
    /// route it.
    #[test]
    fn no_issuer_means_no_agent_door_variable_at_all() {
        let c = test_config();
        assert!(c.mcp_oauth_issuer.is_none());
        let pod = tenant_deployment(&c).spec.unwrap().template.spec.unwrap();
        let mut containers = pod.init_containers.clone().unwrap();
        containers.extend(pod.containers.clone());
        for container in &containers {
            let stray: Vec<&str> = container
                .env
                .iter()
                .flatten()
                .map(|e| e.name.as_str())
                .filter(|n| n.starts_with("SQUELCH_MCP_"))
                .collect();
            assert!(
                stray.is_empty(),
                "{} carries {stray:?} with no issuer configured",
                container.name
            );
        }
    }

    /// With an issuer configured, every pod gets EXACTLY two more variables,
    /// appended, and nothing it already had moves. The public URL is the
    /// tenant's OWN, and its host is the Ingress host to the byte: that URL is
    /// the `aud` the daemon requires and the hostname it allows, so a
    /// disagreement with the vhost would refuse every token or 403 every
    /// request.
    #[test]
    fn an_issuer_tells_each_pod_its_issuer_and_its_own_resource_url() {
        let open = agent_door_config();
        let shut = test_config();

        for label in ["alice", "bob"] {
            let with = daemon_env_of(&open, label);
            let without = daemon_env_of(&shut, label);
            assert_eq!(with.len(), without.len() + 2, "exactly two new variables");
            assert_eq!(with[..without.len()], without[..], "nothing else moved");

            let added: Vec<(&str, Option<&str>)> = with[without.len()..]
                .iter()
                .map(|e| (e.name.as_str(), e.value.as_deref()))
                .collect();
            let public_url = format!("https://{label}.passband.email/mcp");
            assert_eq!(
                added,
                vec![
                    (
                        "SQUELCH_MCP_OAUTH_ISSUER",
                        Some("https://example.authkit.app")
                    ),
                    ("SQUELCH_MCP_PUBLIC_URL", Some(public_url.as_str())),
                ]
            );
            // Plain values, never references: an issuer and a public URL are
            // published in this tenant's own metadata document.
            assert!(with[without.len()..].iter().all(|e| e.value_from.is_none()));

            // The host in the resource URL IS the Ingress host.
            let n = TenantName::parse(label).unwrap();
            let ingress_host = ingress(&open, &n).spec.unwrap().rules.unwrap()[0]
                .host
                .clone()
                .unwrap();
            let url_host = public_url
                .strip_prefix("https://")
                .and_then(|rest| rest.strip_suffix("/mcp"))
                .unwrap();
            assert_eq!(url_host, ingress_host);
            // And the path is the one the Ingress publishes.
            assert!(
                ingress_paths(&open, label)
                    .iter()
                    .any(|(p, t)| p == "/mcp" && t == "Prefix")
            );
        }
    }

    /// INVARIANT I5, in both states: the warden never renders
    /// `SQUELCH_MCP_ALLOWED_HOSTS`. A daemon adds its public hostname to the
    /// rmcp Host allow-list itself, and only in OAuth mode, so an image that
    /// predates OAuth behind a published `/mcp` answers 403 rather than serving
    /// mail unauthenticated. Checked on the whole serialized Deployment, not
    /// just the daemon's env list, so no container, annotation or future field
    /// can carry it either.
    #[test]
    fn the_warden_never_renders_the_mcp_host_allow_list() {
        for (state, c) in [("unset", test_config()), ("set", agent_door_config())] {
            let rendered = serde_json::to_string(&tenant_deployment(&c)).unwrap();
            assert!(
                !rendered.contains("SQUELCH_MCP_ALLOWED_HOSTS"),
                "the issuer-{state} render carries SQUELCH_MCP_ALLOWED_HOSTS"
            );
            assert!(
                daemon_env_of(&c, "alice")
                    .iter()
                    .all(|e| e.name != "SQUELCH_MCP_ALLOWED_HOSTS")
            );
        }
    }

    #[test]
    fn the_ingress_uses_the_wildcard_certificate() {
        let c = test_config();
        let tls = ingress(&c, &name()).spec.unwrap().tls.unwrap();
        assert_eq!(tls.len(), 1);
        assert_eq!(tls[0].secret_name.as_deref(), Some("passband-wildcard-tls"));
        assert_eq!(
            tls[0].hosts.clone().unwrap(),
            vec!["alice.passband.email".to_string()]
        );
    }

    /// The reachability claim: only the ingress controller gets in, only DNS
    /// and public HTTPS get out, and every cluster-internal range is excluded.
    #[test]
    fn the_network_policy_isolates_the_tenant() {
        let c = test_config();
        let spec = network_policy(&c, &name()).spec.unwrap();

        assert_eq!(
            spec.policy_types.unwrap(),
            vec!["Ingress".to_string(), "Egress".to_string()]
        );
        assert_eq!(
            spec.pod_selector.unwrap().match_labels.unwrap()["app.kubernetes.io/instance"],
            "alice"
        );

        let ingress_rules = spec.ingress.unwrap();
        assert_eq!(ingress_rules.len(), 2);
        let from = ingress_rules[0].from.clone().unwrap();
        assert_eq!(from.len(), 1);
        assert_eq!(
            from[0]
                .namespace_selector
                .clone()
                .unwrap()
                .match_labels
                .unwrap()["kubernetes.io/metadata.name"],
            "kube-system"
        );
        assert_eq!(
            from[0].pod_selector.clone().unwrap().match_labels.unwrap()["app.kubernetes.io/name"],
            "traefik"
        );
        assert_eq!(
            ingress_rules[0].ports.clone().unwrap()[0].port,
            Some(IntOrString::Int(8848))
        );

        let egress = spec.egress.unwrap();
        assert_eq!(egress.len(), 2);

        // DNS, and only CoreDNS.
        let dns_peer = egress[0].to.clone().unwrap();
        assert_eq!(
            dns_peer[0]
                .pod_selector
                .clone()
                .unwrap()
                .match_labels
                .unwrap()["k8s-app"],
            "kube-dns"
        );
        let dns_ports: Vec<_> = egress[0]
            .ports
            .clone()
            .unwrap()
            .into_iter()
            .map(|p| (p.protocol.unwrap(), p.port.unwrap()))
            .collect();
        assert_eq!(
            dns_ports,
            vec![
                ("UDP".to_string(), IntOrString::Int(53)),
                ("TCP".to_string(), IntOrString::Int(53)),
            ]
        );

        // The internet, minus everything in the cluster.
        let block = egress[1].to.clone().unwrap()[0].ip_block.clone().unwrap();
        assert_eq!(block.cidr, "0.0.0.0/0");
        let except = block.except.unwrap();
        for cidr in [
            "10.0.0.0/8",
            "172.16.0.0/12",
            "192.168.0.0/16",
            "169.254.0.0/16",
            "100.64.0.0/10",
        ] {
            assert!(except.contains(&cidr.to_string()), "{cidr} is reachable");
        }
        assert_eq!(
            egress[1].ports.clone().unwrap()[0].port,
            Some(IntOrString::Int(443))
        );
    }

    /// The metrics allowance, asserted as narrowly as it is written: one
    /// namespace AND one pod label, one port, and that port is not 8848.
    #[test]
    fn only_the_metrics_collector_reaches_the_metrics_port() {
        let c = test_config();
        let rule = network_policy(&c, &name()).spec.unwrap().ingress.unwrap()[1].clone();

        let from = rule.from.clone().unwrap();
        assert_eq!(from.len(), 1, "one peer, not a list of alternatives");
        // Both selectors on the SAME peer: a namespace selector alone would let
        // in anything that ever lands in `monitoring`.
        assert_eq!(
            from[0]
                .namespace_selector
                .clone()
                .unwrap()
                .match_labels
                .unwrap()["kubernetes.io/metadata.name"],
            "monitoring"
        );
        assert_eq!(
            from[0].pod_selector.clone().unwrap().match_labels.unwrap()["app.kubernetes.io/name"],
            "prometheus-agent"
        );
        assert!(from[0].ip_block.is_none());

        let ports = rule.ports.unwrap();
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].port, Some(IntOrString::Int(9464)));
        assert_eq!(ports[0].protocol.as_deref(), Some("TCP"));
        // The load-bearing half: the human door stays sealed to Traefik, so a
        // collector that went rogue reads counters and not mail.
        assert_ne!(ports[0].port, Some(IntOrString::Int(DAEMON_PORT)));
    }

    /// The other half of the metrics isolation claim: the port exists on the
    /// pod and on nothing that publishes it. An Ingress can only reach a port
    /// its Service declares, so a Service without it cannot be routed to by any
    /// mistake in [`HUMAN_DOOR_PREFIXES`].
    #[test]
    fn the_metrics_port_is_on_the_pod_and_on_nothing_public() {
        let c = test_config();
        let n = name();

        let pod = tenant_deployment(&c).spec.unwrap().template.spec.unwrap();
        let ports = pod.containers[0].ports.clone().unwrap();
        let named: Vec<(&str, i32)> = ports
            .iter()
            .map(|p| (p.name.as_deref().unwrap(), p.container_port))
            .collect();
        assert_eq!(named, vec![("http", 8848), ("metrics", 9464)]);

        // Not on the Service...
        let service_ports = service(&c, &n).spec.unwrap().ports.unwrap();
        assert_eq!(service_ports.len(), 1);
        assert_eq!(service_ports[0].port, DAEMON_PORT);
        assert!(
            service_ports
                .iter()
                .all(|p| p.name.as_deref() != Some("metrics")),
            "the metrics port on the Service would make it routable"
        );

        // ...so nothing the Ingress declares can arrive at it, and no path
        // spelling reaches /metrics either.
        let rules = ingress(&c, &n).spec.unwrap().rules.unwrap();
        let paths = rules[0].http.clone().unwrap().paths;
        for path in &paths {
            assert_eq!(
                path.backend.service.as_ref().unwrap().port,
                Some(ServiceBackendPort {
                    name: Some("http".to_string()),
                    ..Default::default()
                })
            );
        }
        // Prefix paths are checked as string prefixes; the Exact root matches
        // only a literal "/", which /metrics is not.
        let prefixes: Vec<&str> = paths
            .iter()
            .filter(|p| p.path_type == "Prefix")
            .map(|p| p.path.as_deref().unwrap())
            .collect();
        for spelling in ["/metrics", "/metrics/"] {
            assert!(
                !prefixes.iter().any(|p| spelling.starts_with(*p)),
                "{spelling} would be routed to the daemon"
            );
            assert_ne!(spelling, "/", "an Exact root cannot match {spelling}");
        }
    }

    /// The kubelet probe hole: absent unless the operator states the node
    /// network, and when present it is one CIDR to one port and nothing else.
    #[test]
    fn the_node_may_probe_only_when_the_operator_says_so() {
        let mut c = test_config();
        assert!(c.node_cidr.is_none());
        // Traefik and the metrics collector; the node is not in the list yet.
        assert_eq!(
            network_policy(&c, &name())
                .spec
                .unwrap()
                .ingress
                .unwrap()
                .len(),
            2
        );

        c.node_cidr = Some("10.0.0.5/32".to_string());
        let ingress = network_policy(&c, &name()).spec.unwrap().ingress.unwrap();
        assert_eq!(ingress.len(), 3);
        let from = ingress[2].from.clone().unwrap();
        assert_eq!(from.len(), 1);
        let block = from[0].ip_block.clone().unwrap();
        assert_eq!(block.cidr, "10.0.0.5/32");
        assert!(block.except.is_none());
        // Still only the daemon port, and still nothing else about the peer:
        // node-originated traffic, not "anything on that address".
        assert!(from[0].pod_selector.is_none());
        assert!(from[0].namespace_selector.is_none());
        // The two ports a kubelet probe can land on, and nothing else. Both
        // whichever probe is rendered: a NetworkPolicy is the object no roll
        // converges, so a hole that tracked the current probe would have to be
        // re-applied per tenant, by hand, in the same edit that moves it.
        let expected = vec![
            Some(IntOrString::Int(DAEMON_PORT)),
            Some(IntOrString::Int(METRICS_PORT)),
        ];
        let ports = ingress[2].ports.clone().unwrap();
        assert_eq!(
            ports.iter().map(|p| p.port.clone()).collect::<Vec<_>>(),
            expected
        );
        assert!(ports.iter().all(|p| p.protocol.as_deref() == Some("TCP")));

        c.http_readiness = true;
        let ingress = network_policy(&c, &name()).spec.unwrap().ingress.unwrap();
        let ports = ingress[2].ports.clone().unwrap();
        assert_eq!(
            ports.iter().map(|p| p.port.clone()).collect::<Vec<_>>(),
            expected,
            "flipping the probe must not need a second NetworkPolicy apply"
        );
        // And the peer is still the node and only the node.
        assert!(ingress[2].from.clone().unwrap()[0].pod_selector.is_none());
    }

    #[test]
    fn the_pvc_asks_the_configured_class_for_the_configured_size() {
        let c = test_config();
        let spec = data_pvc(&c, &name()).spec.unwrap();
        assert_eq!(
            spec.access_modes.unwrap(),
            vec!["ReadWriteOnce".to_string()]
        );
        assert_eq!(spec.storage_class_name.as_deref(), Some("local-path"));
        assert_eq!(
            spec.resources.unwrap().requests.unwrap()["storage"],
            Quantity("10Gi".to_string())
        );
    }

    #[test]
    fn the_service_targets_the_named_container_port() {
        let c = test_config();
        let spec = service(&c, &name()).spec.unwrap();
        assert_eq!(spec.type_.as_deref(), Some("ClusterIP"));
        assert_eq!(
            spec.selector.unwrap()["app.kubernetes.io/instance"],
            "alice"
        );
        let port = &spec.ports.unwrap()[0];
        assert_eq!(port.port, 8848);
        assert_eq!(
            port.target_port,
            Some(IntOrString::String("http".to_string()))
        );
    }

    #[test]
    fn the_pair_exec_carries_the_tenants_public_url() {
        let c = test_config();
        assert_eq!(
            pair_argv(&c, &name()),
            vec![
                "/usr/local/bin/squelchd".to_string(),
                "pair".to_string(),
                "--url".to_string(),
                "https://alice.passband.email".to_string(),
            ]
        );
    }

    #[test]
    fn the_pull_secret_appears_only_when_one_is_configured() {
        let mut c = test_config();
        let pod = tenant_deployment(&c).spec.unwrap().template.spec.unwrap();
        assert!(pod.image_pull_secrets.is_none());

        c.pull_secret = Some("ghcr".to_string());
        let pod = tenant_deployment(&c).spec.unwrap().template.spec.unwrap();
        assert_eq!(pod.image_pull_secrets.unwrap()[0].name, "ghcr");
    }

    /// How many Secrets one tenant holds at most: identity, credential, LLM key
    /// and share token (the four `-> Secret` builders below, under the four names
    /// [`TenantName`] gives them). The tenants ResourceQuota's `count/secrets` has
    /// to be at least this many times its `pods`, plus two for every claim slot
    /// beyond the pod slots (a cancelled tenant keeps its identity and credential
    /// with its claim), plus the namespace's shared Secrets, or it is the line
    /// that refuses a signup first; the test
    /// `the_committed_quota_lets_memory_bind_before_any_count_line` holds the
    /// committed file to that.
    const SECRETS_PER_TENANT: u32 = 4;

    #[test]
    fn a_tenant_holds_four_secrets_under_four_names() {
        let n = name();
        let names = [
            n.identity_secret(),
            n.credential_secret(),
            n.llm_secret(),
            n.control_secret(),
        ];
        let distinct: std::collections::BTreeSet<&str> = names.iter().map(String::as_str).collect();
        assert_eq!(distinct.len(), names.len());
        assert_eq!(names.len() as u32, SECRETS_PER_TENANT);
    }

    /// The committed tenants ResourceQuota, `deploy/hosted/70-tenant-limits.yaml`.
    const TENANT_LIMITS: &str = include_str!("../../deploy/hosted/70-tenant-limits.yaml");
    /// The committed warden ConfigMap, `deploy/hosted/15-warden-config.yaml`:
    /// what a tenant pod asks for on carrier is what this sets, not the
    /// defaults in `config.rs`.
    const WARDEN_CONFIG: &str = include_str!("../../deploy/hosted/15-warden-config.yaml");
    /// The warden's Deployment, `deploy/hosted/20-warden.yaml`, and the fleet
    /// roller's CronJob, `deploy/hosted/90-warden-roller.yaml`, which renders
    /// the same tenant pods: both take the ConfigMap by `envFrom`, and an
    /// `env:` entry of their own would outrank the same name silently.
    const WARDEN_DEPLOYMENT: &str = include_str!("../../deploy/hosted/20-warden.yaml");
    const WARDEN_ROLLER: &str = include_str!("../../deploy/hosted/90-warden-roller.yaml");
    /// The model volume and its seed pod, `deploy/hosted/60-models.yaml`: the
    /// one claim and the one pod in the namespace that are not a tenant's.
    const MODELS: &str = include_str!("../../deploy/hosted/60-models.yaml");
    /// The certificates, `deploy/hosted/40-wildcard-certificate.yaml`: each
    /// one in the tenant namespace is a Secret there, plus a private key
    /// beside it while cert-manager renews.
    const CERTIFICATES: &str = include_str!("../../deploy/hosted/40-wildcard-certificate.yaml");

    /// Every document of a committed manifest, as the API server would read
    /// it. The YAML reader is the one `kube` already links; nothing here
    /// renders YAML. It refuses a run of more than 32 comment lines, and these
    /// files explain themselves at length, so comment lines, which YAML
    /// drops, are dropped first. That is blind to a `#` line that is text,
    /// which only a block scalar or a quoted scalar continued over lines can
    /// hold (a plain scalar's `#` line is a comment), so both are refused.
    fn documents(manifest: &str) -> Vec<serde_json::Value> {
        let uncommented: Vec<&str> = manifest
            .lines()
            .filter(|line| !line.trim_start().starts_with('#'))
            .collect();
        for line in &uncommented {
            let last = line.split_whitespace().last().unwrap_or("");
            assert!(
                !last.starts_with(['|', '>']),
                "a block scalar; comment lines can no longer be dropped blind: {line:?}"
            );
            if let Some((_, value)) = line.split_once(": ") {
                let value = value.trim();
                if let Some(quote) = value.chars().next().filter(|c| matches!(c, '"' | '\'')) {
                    assert!(
                        value[1..].contains(quote),
                        "a quoted scalar continued over lines; comment lines can no longer be \
                         dropped blind: {line:?}"
                    );
                }
            }
        }
        serde_saphyr::from_multiple(&uncommented.join("\n")).expect("the manifest parses")
    }

    /// The documents of one kind, as the typed objects the API server would
    /// make of them.
    fn of_kind<T: serde::de::DeserializeOwned>(
        documents: &[serde_json::Value],
        kind: &str,
    ) -> Vec<T> {
        documents
            .iter()
            .filter(|document| document["kind"].as_str() == Some(kind))
            .map(|document| {
                serde_json::from_value(document.clone())
                    .unwrap_or_else(|e| panic!("not a {kind}: {e}"))
            })
            .collect()
    }

    /// The one document of that kind in a manifest.
    fn committed<T: serde::de::DeserializeOwned>(manifest: &str, kind: &str) -> T {
        one(&documents(manifest), kind)
    }

    /// The one document of that kind among those read.
    fn one<T: serde::de::DeserializeOwned>(documents: &[serde_json::Value], kind: &str) -> T {
        let mut all = of_kind(documents, kind);
        assert_eq!(all.len(), 1, "exactly one {kind} in the manifest");
        all.pop().unwrap()
    }

    /// How many Secrets the namespace can hold that are no running tenant's,
    /// which the quota's `count/secrets` has to leave room for on top of what
    /// the tenants take: the ones the config carries a name for, set in the
    /// ConfigMap or defaulted in `config.rs` (the wildcard TLS, the OAuth
    /// client, the APNs relay, the Browserbase key, and a pull secret if one
    /// is set) and the TLS Secret of every certificate in the namespace; a
    /// successor beside each of the three that rotate under a new name
    /// (`google-oauth-client.example.yaml`, `apns-relay.example.yaml`,
    /// `15-warden-config.yaml`); and the private key cert-manager keeps
    /// beside each certificate while it renews. A pending signup's identity
    /// Secret, held for up to a day with no pod or claim, is what the slack
    /// above this floor is for.
    fn shared_secrets(config: &Config) -> u64 {
        let certificates: Vec<serde_json::Value> = documents(CERTIFICATES)
            .into_iter()
            .filter(|document| {
                document["kind"].as_str() == Some("Certificate")
                    && document["metadata"]["namespace"].as_str() == Some(&config.namespace)
            })
            .collect();
        let rotating = [
            config.oauth_secret_name.as_str(),
            config.relay_secret_name.as_str(),
            config.browserbase_secret_name.as_str(),
        ];
        let named: std::collections::BTreeSet<&str> = rotating
            .into_iter()
            .chain([config.tls_secret.as_str()])
            .chain(config.pull_secret.as_deref())
            .chain(certificates.iter().map(|certificate| {
                certificate["spec"]["secretName"]
                    .as_str()
                    .expect("a Certificate names its Secret")
            }))
            .collect();
        let rotating: std::collections::BTreeSet<&str> = rotating.into_iter().collect();
        (named.len() + rotating.len() + certificates.len()) as u64
    }

    /// The warden's config as the committed ConfigMap sets it, through the
    /// real loader, with the one value the ConfigMap does not carry (the
    /// token, which is a Secret). Each pod spec that runs the warden is held
    /// to take that ConfigMap, by name, as its only `envFrom`, with an `env:`
    /// naming the token and nothing else of the warden's (both files say so),
    /// so what the loader sees here is what those pods see.
    fn committed_config() -> Config {
        let config_map: ConfigMap = committed(WARDEN_CONFIG, "ConfigMap");
        let name = config_map.metadata.name.expect("the ConfigMap is named");
        let deployment: Deployment = committed(WARDEN_DEPLOYMENT, "Deployment");
        let roller: CronJob = committed(WARDEN_ROLLER, "CronJob");
        for (file, pod) in [
            (
                "20-warden.yaml",
                deployment.spec.unwrap().template.spec.unwrap(),
            ),
            (
                "90-warden-roller.yaml",
                roller
                    .spec
                    .unwrap()
                    .job_template
                    .spec
                    .unwrap()
                    .template
                    .spec
                    .unwrap(),
            ),
        ] {
            for container in pod.init_containers.iter().flatten().chain(&pod.containers) {
                let sources = container.env_from.as_deref().unwrap_or_default();
                assert_eq!(sources.len(), 1, "{file}: one envFrom source");
                assert!(
                    sources[0].secret_ref.is_none() && sources[0].prefix.is_none(),
                    "{file}"
                );
                assert_eq!(
                    sources[0]
                        .config_map_ref
                        .as_ref()
                        .map(|source| source.name.as_str()),
                    Some(name.as_str()),
                    "{file} takes a ConfigMap that is not the committed one"
                );
                let restated: Vec<&str> = container
                    .env
                    .iter()
                    .flatten()
                    .map(|entry| entry.name.as_str())
                    .filter(|name| {
                        name.starts_with("SQUELCH_WARDEN_") && *name != "SQUELCH_WARDEN_TOKEN"
                    })
                    .collect();
                assert!(
                    restated.is_empty(),
                    "{file} restates {restated:?} as env: entries, which outrank the ConfigMap"
                );
            }
        }
        let data = config_map.data.unwrap_or_default();
        Config::from_source(&|name| match name {
            "SQUELCH_WARDEN_TOKEN" => Some(crate::testing::TEST_TOKEN.to_string()),
            _ => data.get(name).cloned(),
        })
        .expect("the committed ConfigMap loads")
    }

    /// A memory or storage quantity in MiB: bytes, or any of the binary and
    /// decimal suffixes Kubernetes takes; a decimal is rounded.
    fn mebibytes(quantity: &str) -> u64 {
        const SUFFIXES: [(&str, f64); 12] = [
            ("Ki", 1024.0),
            ("Mi", 1024.0 * 1024.0),
            ("Gi", 1024.0 * 1024.0 * 1024.0),
            ("Ti", 1024.0 * 1024.0 * 1024.0 * 1024.0),
            ("Pi", 1024.0 * 1024.0 * 1024.0 * 1024.0 * 1024.0),
            ("Ei", 1024.0 * 1024.0 * 1024.0 * 1024.0 * 1024.0 * 1024.0),
            ("k", 1e3),
            ("M", 1e6),
            ("G", 1e9),
            ("T", 1e12),
            ("P", 1e15),
            ("E", 1e18),
        ];
        let (number, scale) = SUFFIXES
            .iter()
            .find_map(|(suffix, scale)| quantity.strip_suffix(suffix).map(|n| (n, *scale)))
            .unwrap_or((quantity, 1.0));
        let number: f64 = number
            .parse()
            .unwrap_or_else(|_| panic!("not a byte quantity: {quantity:?}"));
        (number * scale / (1024.0 * 1024.0)).round() as u64
    }

    /// A CPU quantity in millicores: `100m`, or cores, whole or decimal.
    fn millicores(quantity: &str) -> u64 {
        let (number, scale) = match quantity.strip_suffix('m') {
            Some(m) => (m, 1.0),
            None => (quantity, 1000.0),
        };
        let number: f64 = number
            .parse()
            .unwrap_or_else(|_| panic!("not a CPU quantity: {quantity:?}"));
        (number * scale).round() as u64
    }

    /// One resource's quantity out of a requests, limits or bounds map.
    fn quantity(
        map: Option<&BTreeMap<String, Quantity>>,
        name: &str,
        parse: fn(&str) -> u64,
    ) -> Option<u64> {
        map.and_then(|map| map.get(name))
            .map(|quantity| parse(&quantity.0))
    }

    /// A container's request and limit for one resource as the API server
    /// settles them before any bound or quota is applied: a missing limit is
    /// the LimitRange's Container default, itself defaulted to the entry's
    /// max; a missing request is the limit if one was given, else the entry's
    /// default request, itself defaulted to the limit, then to the entry's
    /// min.
    fn settled(
        container: &Container,
        entry: &LimitRangeItem,
        name: &str,
        parse: fn(&str) -> u64,
    ) -> (Option<u64>, Option<u64>) {
        let requirements = container.resources.as_ref();
        let own_limit = quantity(requirements.and_then(|r| r.limits.as_ref()), name, parse);
        let own_request = quantity(requirements.and_then(|r| r.requests.as_ref()), name, parse);
        let limit = own_limit
            .or_else(|| quantity(entry.default.as_ref(), name, parse))
            .or_else(|| quantity(entry.max.as_ref(), name, parse));
        let request = own_request
            .or(own_limit)
            .or_else(|| quantity(entry.default_request.as_ref(), name, parse))
            .or(limit)
            .or_else(|| quantity(entry.min.as_ref(), name, parse));
        (request, limit)
    }

    /// What a pod charges a quota line for one resource, the way the API
    /// server computes it: the larger of its init containers' largest and
    /// its other containers' sum, each settled as above.
    fn charge(
        pod: &PodSpec,
        entry: &LimitRangeItem,
        limits: bool,
        name: &str,
        parse: fn(&str) -> u64,
    ) -> u64 {
        let of = |container: &Container| {
            let (request, limit) = settled(container, entry, name, parse);
            if limits { limit } else { request }.unwrap_or(0)
        };
        let init = pod
            .init_containers
            .iter()
            .flatten()
            .map(of)
            .max()
            .unwrap_or(0);
        let main: u64 = pod.containers.iter().map(of).sum();
        init.max(main)
    }

    /// `requests.memory` is the line that is meant to refuse a signup, because
    /// it is the one sized to the box (the file says why). Every other line
    /// has to admit more tenants than it does (more, not as many: a tie is
    /// lost to the first pod shaped unlike a tenant, and the model seed is
    /// one), or a full quota stops meaning a full box: 2026-09-28, memory was
    /// raised alone and cpu, ephemeral storage, pods and PVCs all bound first;
    /// and the patch that fixed it carried `limits.cpu: "12"`,
    /// `requests.storage: 200Gi` and `count/secrets: "100"` on a `pods` of 40,
    /// which would have refused the thirteenth, the twentieth and the
    /// twenty-fourth tenant while memory still had room for thirty.
    ///
    /// What one tenant charges each line is read off the pod the warden
    /// renders from the committed ConfigMap ([`charge`]), a claim is
    /// `SQUELCH_WARDEN_STORAGE_SIZE`, and the Secrets are
    /// [`SECRETS_PER_TENANT`]. The namespace's own pod and claim, the model
    /// seed's, are charged to every line they touch while the seed runs, and
    /// its own Secrets ([`shared_secrets`]) to the secrets line.
    ///
    /// The same file's LimitRange is checked first: a container the ConfigMap
    /// sizes past its `max`, or under its `min`, or a claim outside its
    /// bounds, is refused at admission before any quota is consulted,
    /// fleet-wide on the next roll.
    #[test]
    fn the_committed_quota_lets_memory_bind_before_any_count_line() {
        let c = committed_config();
        let pod = tenant_deployment(&c).spec.unwrap().template.spec.unwrap();
        let limits = documents(TENANT_LIMITS);
        let limit_range: LimitRange = one(&limits, "LimitRange");
        let quota: ResourceQuota = one(&limits, "ResourceQuota");
        let models = documents(MODELS);
        let other_pods: Vec<Pod> = of_kind(&models, "Pod");
        let other_claims: Vec<PersistentVolumeClaim> = of_kind(&models, "PersistentVolumeClaim");

        // The LimitRange: every container of the pod inside the Container
        // entry's bounds with its request under its limit, and the claim
        // inside the PersistentVolumeClaim entry's.
        let items = limit_range.spec.unwrap().limits;
        let entry = |type_: &str| -> &LimitRangeItem {
            items
                .iter()
                .find(|item| item.type_ == type_)
                .unwrap_or_else(|| panic!("the LimitRange has no {type_} entry"))
        };
        let container_entry = entry("Container");
        let resources = [
            ("cpu", millicores as fn(&str) -> u64),
            ("memory", mebibytes),
            ("ephemeral-storage", mebibytes),
        ];
        for (name, parse) in resources {
            let floor = quantity(container_entry.min.as_ref(), name, parse).unwrap_or(0);
            let ceiling = quantity(container_entry.max.as_ref(), name, parse).unwrap_or(u64::MAX);
            for container in pod.init_containers.iter().flatten().chain(&pod.containers) {
                let (request, limit) = settled(container, container_entry, name, parse);
                let (request, limit) = (request.unwrap_or(0), limit.unwrap_or(u64::MAX));
                assert!(
                    floor <= request && request <= limit && limit <= ceiling,
                    "the {} container's {name} {request}..{limit} is outside the LimitRange's \
                     {floor}..{ceiling} or inverted",
                    container.name
                );
            }
        }
        let claim_entry = entry("PersistentVolumeClaim");
        let claim = mebibytes(&c.storage_size);
        assert!(
            quantity(claim_entry.min.as_ref(), "storage", mebibytes).unwrap_or(0) <= claim
                && claim
                    <= quantity(claim_entry.max.as_ref(), "storage", mebibytes).unwrap_or(u64::MAX),
            "a tenant's {} claim is outside the LimitRange",
            c.storage_size
        );

        // The quota.
        let hard = quota.spec.unwrap().hard.unwrap_or_default();
        let line = |key: &str| -> &str {
            &hard
                .get(key)
                .unwrap_or_else(|| panic!("the quota has no `{key}` line"))
                .0
        };
        let count = |key: &str| line(key).parse::<u64>().unwrap();
        // What the namespace's own pods and claims take off each line.
        let others = |limits: bool, name: &str, parse: fn(&str) -> u64| -> u64 {
            other_pods
                .iter()
                .map(|other| {
                    charge(
                        other.spec.as_ref().unwrap(),
                        container_entry,
                        limits,
                        name,
                        parse,
                    )
                })
                .sum()
        };
        let other_claim_count = other_claims.len() as u64;
        let other_claims_storage: u64 = other_claims
            .into_iter()
            .map(|claim| {
                mebibytes(&claim.spec.unwrap().resources.unwrap().requests.unwrap()["storage"].0)
            })
            .sum();
        let other_pods = other_pods.len() as u64;
        let shared_secrets = shared_secrets(&c);
        // What each line admits, with the seed pod running. Printed so
        // `--nocapture` shows the table the file's comment describes.
        let admits = |key: &str, limits: bool, name: &str, parse: fn(&str) -> u64| -> u64 {
            parse(line(key)).saturating_sub(others(limits, name, parse))
                / charge(&pod, container_entry, limits, name, parse)
        };
        let by_memory = admits("requests.memory", false, "memory", mebibytes);
        let admitted = [
            (
                "requests.cpu",
                admits("requests.cpu", false, "cpu", millicores),
            ),
            (
                "requests.ephemeral-storage",
                admits(
                    "requests.ephemeral-storage",
                    false,
                    "ephemeral-storage",
                    mebibytes,
                ),
            ),
            ("limits.cpu", admits("limits.cpu", true, "cpu", millicores)),
            (
                "limits.memory",
                admits("limits.memory", true, "memory", mebibytes),
            ),
            (
                "limits.ephemeral-storage",
                admits(
                    "limits.ephemeral-storage",
                    true,
                    "ephemeral-storage",
                    mebibytes,
                ),
            ),
            (
                "requests.storage",
                mebibytes(line("requests.storage")).saturating_sub(other_claims_storage) / claim,
            ),
            ("pods", count("pods").saturating_sub(other_pods)),
            (
                "persistentvolumeclaims",
                count("persistentvolumeclaims").saturating_sub(other_claim_count),
            ),
            (
                "count/secrets",
                count("count/secrets").saturating_sub(shared_secrets)
                    / u64::from(SECRETS_PER_TENANT),
            ),
        ];
        // Every line of the file is one of these, or the test is not holding
        // it: a new line is a new row here, with its charge per tenant.
        let known: std::collections::BTreeSet<&str> = ["requests.memory"]
            .into_iter()
            .chain(admitted.iter().map(|(dimension, _)| *dimension))
            .collect();
        let lines: std::collections::BTreeSet<&str> = hard.keys().map(String::as_str).collect();
        assert_eq!(lines, known, "a quota line this test does not divide");
        println!("tenants admitted by requests.memory (the model seed pod running): {by_memory}");
        assert!(by_memory >= 1, "the memory line admits no tenant at all");
        for (dimension, admits) in admitted {
            println!("tenants admitted by {dimension}: {admits}");
            assert!(
                admits > by_memory,
                "`{dimension}` admits {admits} tenants, not more than the {by_memory} memory does: \
                 it would refuse a signup while the box still has room"
            );
        }
        // And the two rules the file states, whatever memory says: the secrets
        // ceiling never binds before the pods ceiling, and the storage line
        // never before the claims line. A cancelled tenant keeps its claim and
        // two of its Secrets (`crate::provision::Warden::delete`), so the
        // headroom those count lines have over the memory line is what
        // cancellations spend, and the lines sized from them must keep pace:
        // four Secrets for every pod slot, two more for every claim slot
        // beyond the pod slots (a cancelled tenant, with no pod), and the
        // namespace's own.
        let claim_slots_beyond_pods = count("persistentvolumeclaims").saturating_sub(count("pods"));
        assert!(
            count("count/secrets")
                >= u64::from(SECRETS_PER_TENANT) * count("pods")
                    + 2 * claim_slots_beyond_pods
                    + shared_secrets,
            "count/secrets must be at least {SECRETS_PER_TENANT} x pods, plus 2 for each claim \
             slot beyond pods, plus the namespace's own"
        );
        assert!(
            mebibytes(line("requests.storage"))
                >= count("persistentvolumeclaims").saturating_sub(other_claim_count) * claim
                    + other_claims_storage,
            "requests.storage must hold a {} claim for every claim slot but the model seed's, plus its",
            c.storage_size
        );
    }
}
