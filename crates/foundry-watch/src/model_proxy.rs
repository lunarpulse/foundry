//! Narrow model proxy + cast-container boundary — M5 T44b.
//!
//! astra #15: the cast container talks to models ONLY through a
//! narrow protocol: fixed provider destinations, no caller-supplied
//! URL/headers/credential fields, no redirects, bounded response
//! size, per-run/budget-scope attribution, server-side egress
//! screening. DNS-rebinding/redirect/host-access/error-leak are the
//! tested attacks.
//!
//! T34's boundary table made cast structurally unable to READ
//! credential files. T44b completes the other half: cast cannot
//! DECIDE where its network requests go. The proxy is the only door,
//! and the door has a fixed shape.
#![allow(clippy::type_complexity)] // W6a clippy debt: preserve the existing internal tuple and storage representation

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// A fixed provider destination. Registered at proxy start; callers
/// reference providers BY NAME and cannot add destinations at request
/// time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderEndpoint {
    pub name: String,
    /// exact https origin, no path wildcards
    pub origin: String,
    pub env_key_name: String, // which env var holds ITS key (proxy-side only)
    pub max_response_bytes: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProxyError {
    UnknownProvider {
        name: String,
    },
    /// caller tried to supply any of the forbidden fields
    CallerSuppliedUrl,
    CallerSuppliedAuth,
    CallerSuppliedHeaders,
    /// the target resolved/redirected somewhere outside the fixed origin
    OffOrigin {
        provider: String,
        attempted: String,
    },
    ResponseTooLarge {
        provider: String,
        got: u64,
        max: u64,
    },
    /// run is cancelled/fenced or budget scope invalid — refuse to spend
    RunNotAdmissible {
        run: String,
        reason: String,
    },
    /// error bodies are screened: no upstream echo of internals
    ScreenedError {
        public: String,
    },
}

/// A proxy request. NOTE WHAT IS ABSENT: no url, no headers, no auth
/// material. The caller physically cannot express an arbitrary
/// egress — the type is the boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProxyRequest {
    pub provider: String,
    pub body: String,
    pub run: String,
    /// budget scope this spend is attributed to (T44a lineage)
    pub budget_scope: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyResponse {
    pub status: u16,
    pub body: String,
    pub spend_points: u64,
    pub charged_to: String, // run attribution
}

/// The proxy's frozen configuration.
pub struct ModelProxy {
    providers: BTreeMap<String, ProviderEndpoint>,
    /// admission hook (T44a ledger): is this run still allowed to spend?
    admissible: Box<dyn Fn(&str) -> Result<(), String> + Send + Sync>,
}

impl ModelProxy {
    pub fn new(
        providers: Vec<ProviderEndpoint>,
        admissible: impl Fn(&str) -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        ModelProxy {
            providers: providers.into_iter().map(|p| (p.name.clone(), p)).collect(),
            admissible: Box::new(admissible),
        }
    }

    /// Validate request shape. This is the static door: forbidden
    /// fields simply do not exist in ProxyRequest, and provider names
    /// resolve ONLY against the registered table.
    pub fn validate(&self, req: &ProxyRequest) -> Result<&ProviderEndpoint, ProxyError> {
        (self.admissible)(&req.run).map_err(|reason| ProxyError::RunNotAdmissible {
            run: req.run.clone(),
            reason,
        })?;
        self.providers
            .get(&req.provider)
            .ok_or_else(|| ProxyError::UnknownProvider {
                name: req.provider.clone(),
            })
    }

    /// Egress screening: given the connection target the transport
    /// layer reports (after DNS resolution and on every redirect
    /// hop), decide whether it stays inside the provider's fixed
    /// origin. Redirects to ANY other origin are refused — the
    /// provider table is the universe.
    pub fn screen_egress(&self, provider: &str, target_origin: &str) -> Result<(), ProxyError> {
        let p = self
            .providers
            .get(provider)
            .ok_or_else(|| ProxyError::UnknownProvider {
                name: provider.to_string(),
            })?;
        if normalize_origin(target_origin) != normalize_origin(&p.origin) {
            return Err(ProxyError::OffOrigin {
                provider: provider.to_string(),
                attempted: target_origin.to_string(),
            });
        }
        Ok(())
    }

    /// Screen an upstream error before it reaches the cast container:
    /// internal identifiers, credential file names, and paths are
    /// replaced. Cast must not learn the proxy's filesystem.
    pub fn screen_error(&self, upstream: &str) -> String {
        let mut out = upstream.to_string();
        for secret_hint in [
            "/home/",
            "/root/",
            "gh_token",
            "die.env",
            "notifier.env",
            "identity.db",
            "Authorization:",
            "Bearer ",
        ] {
            if out.contains(secret_hint) {
                out = "upstream error (details screened)".to_string();
                break;
            }
        }
        out
    }

    /// Enforce the response size bound. The transport reads at most
    /// max+1 bytes; anything over the provider's bound is refused
    /// (not truncated — truncated model output would silently corrupt
    /// reproducibility).
    pub fn check_response_size(&self, provider: &str, got: u64) -> Result<(), ProxyError> {
        let p = self
            .providers
            .get(provider)
            .ok_or_else(|| ProxyError::UnknownProvider {
                name: provider.to_string(),
            })?;
        if got > p.max_response_bytes {
            return Err(ProxyError::ResponseTooLarge {
                provider: provider.to_string(),
                got,
                max: p.max_response_bytes,
            });
        }
        Ok(())
    }

    /// Deterministic request id for audit: binds run + provider +
    /// body content. Every proxied byte is attributable.
    pub fn request_id(&self, req: &ProxyRequest) -> String {
        let mut h = Sha256::new();
        h.update(req.run.as_bytes());
        h.update(b"|");
        h.update(req.provider.as_bytes());
        h.update(b"|");
        h.update(req.body.as_bytes());
        format!("px-{:.16x}", h.finalize())
    }
}

fn normalize_origin(o: &str) -> String {
    let o = o.trim().to_ascii_lowercase();
    let o = o.strip_prefix("https://").unwrap_or(&o);
    let o = o.strip_suffix('/').unwrap_or(o);
    o.to_string()
}

// ---------------------------------------------------------------------------
// Container boundary descriptor: the compose/unit spec the cast
// container runs under. Generated, never hand-written — T34's
// boundary table expressed in runtime terms.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CastContainerSpec {
    pub image: String,
    /// network mode: the ONLY network attachment is the proxy
    pub networks: Vec<String>,
    /// volume mounts — credential paths ABSENT by construction
    pub mounts: Vec<String>,
    pub read_only_rootfs: bool,
    pub capabilities_drop_all: bool,
    pub env_passthrough: Vec<String>,
}

/// Generate the cast container spec from the T34 boundary table's
/// cast row. This function CANNOT produce a spec that mounts
/// ~/.foundry credentials — the paths are not inputs.
pub fn cast_container_spec(image: &str, proxy_network: &str) -> CastContainerSpec {
    CastContainerSpec {
        image: image.to_string(),
        networks: vec![proxy_network.to_string()],
        mounts: vec![], // no mounts: work arrives via stdin/artifact, not volumes
        read_only_rootfs: true,
        capabilities_drop_all: true,
        env_passthrough: vec![
            // NO *_API_KEY: the proxy holds provider keys, cast holds none
            "FOUNDRY_ORDER_ID",
            "FOUNDRY_TASK",
            "FOUNDRY_ARTIFACT_DIGEST",
            "FOUNDRY_PROXY_ADDR",
        ]
        .into_iter()
        .map(|s| s.to_string())
        .collect(),
    }
}

/// Render the spec as docker run(1) arguments — the actual command
/// executed by the runtime test. Returns argv so tests assert on
/// EXACT flags, not on prose.
pub fn docker_argv(spec: &CastContainerSpec, name: &str) -> Vec<String> {
    let mut argv = vec![
        "run".to_string(),
        "--rm".to_string(),
        "--name".to_string(),
        name.to_string(),
    ];
    if spec.read_only_rootfs {
        argv.push("--read-only".into());
    }
    // T56w: non-root by construction — nobody (65534). The runtime
    // test asserts this is LIVE, not just declared.
    argv.push("--user".into());
    argv.push("65534:65534".into());
    if spec.capabilities_drop_all {
        argv.push("--cap-drop=ALL".into());
        argv.push("--security-opt".into());
        argv.push("no-new-privileges".into());
    }
    for n in &spec.networks {
        argv.push("--network".into());
        argv.push(n.clone());
    }
    // note: NO -v flags can appear — mounts is empty by construction
    for e in &spec.env_passthrough {
        argv.push("--env".into());
        argv.push(e.clone());
    }
    argv.push(spec.image.clone());
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proxy() -> ModelProxy {
        ModelProxy::new(
            vec![
                ProviderEndpoint {
                    name: "deepseek".into(),
                    origin: "https://api.deepseek.com".into(),
                    env_key_name: "DEEPSEEK_API_KEY".into(),
                    max_response_bytes: 1_048_576,
                },
                ProviderEndpoint {
                    name: "zai".into(),
                    origin: "https://api.z.ai".into(),
                    env_key_name: "GLM_API_KEY".into(),
                    max_response_bytes: 2_097_152,
                },
            ],
            |run| {
                if run.starts_with("dead") {
                    Err("run is fenced (cancelled)".into())
                } else {
                    Ok(())
                }
            },
        )
    }

    fn req(provider: &str, run: &str) -> ProxyRequest {
        ProxyRequest {
            provider: provider.into(),
            body: "{}".into(),
            run: run.into(),
            budget_scope: vec!["factory".into(), "casting".into()],
        }
    }

    #[test]
    fn request_type_cannot_express_arbitrary_egress() {
        // the compile-time boundary: ProxyRequest has no url/headers/auth
        let r = req("deepseek", "run-1");
        let serialized = serde_json::to_string(&r).unwrap();
        for absent in [
            "\"url\"",
            "\"headers\"",
            "\"authorization\"",
            "\"api_key\"",
            "\"token\"",
        ] {
            assert!(
                !serialized.contains(absent),
                "ProxyRequest must not carry {absent}"
            );
        }
        assert!(proxy().validate(&r).is_ok());
    }

    #[test]
    fn unknown_provider_refused() {
        assert!(matches!(
            proxy().validate(&req("evilcorp", "run-1")),
            Err(ProxyError::UnknownProvider { .. })
        ));
        // including lookalikes
        assert!(matches!(
            proxy().validate(&req("deepseek ", "run-1")),
            Err(ProxyError::UnknownProvider { .. })
        ));
    }

    #[test]
    fn fenced_run_cannot_spend() {
        assert!(matches!(
            proxy().validate(&req("deepseek", "dead-run-9")),
            Err(ProxyError::RunNotAdmissible { .. })
        ));
    }

    #[test]
    fn redirect_off_origin_refused() {
        let p = proxy();
        // provider origin itself passes
        assert!(
            p.screen_egress("deepseek", "https://api.deepseek.com/")
                .is_ok()
        );
        // redirect to attacker host: refused
        assert!(matches!(
            p.screen_egress("deepseek", "https://evil.example.com"),
            Err(ProxyError::OffOrigin { .. })
        ));
        // scheme downgrade / rebinding lookalike: refused
        assert!(matches!(
            p.screen_egress("deepseek", "http://api.deepseek.com"),
            Err(ProxyError::OffOrigin { .. })
        ));
        assert!(matches!(
            p.screen_egress("deepseek", "https://api.deepseek.com.evil.io"),
            Err(ProxyError::OffOrigin { .. })
        ));
        // and one provider cannot be used to reach another's origin
        assert!(matches!(
            p.screen_egress("zai", "https://api.deepseek.com"),
            Err(ProxyError::OffOrigin { .. })
        ));
    }

    #[test]
    fn response_bound_enforced_not_truncated() {
        let p = proxy();
        assert!(p.check_response_size("deepseek", 1_048_576).is_ok());
        assert!(matches!(
            p.check_response_size("deepseek", 1_048_577),
            Err(ProxyError::ResponseTooLarge { got: 1_048_577, .. })
        ));
    }

    #[test]
    fn upstream_errors_are_screened() {
        let p = proxy();
        assert_eq!(
            p.screen_error("connection refused to /home/cosmo/.foundry/gh_token"),
            "upstream error (details screened)"
        );
        assert_eq!(
            p.screen_error("Bearer sk-live-xxxx rejected"),
            "upstream error (details screened)"
        );
        // ordinary errors pass with content intact
        assert_eq!(
            p.screen_error("429 rate limited, retry later"),
            "429 rate limited, retry later"
        );
    }

    #[test]
    fn every_request_is_attributable() {
        let p = proxy();
        let a = p.request_id(&req("deepseek", "run-1"));
        let b = p.request_id(&req("deepseek", "run-2"));
        let c = p.request_id(&req("zai", "run-1"));
        assert_ne!(a, b, "run attribution");
        assert_ne!(a, c, "provider attribution");
        assert_eq!(a, p.request_id(&req("deepseek", "run-1")), "deterministic");
    }

    #[test]
    fn cast_spec_cannot_mount_credentials() {
        let spec = cast_container_spec("foundry-cast:1", "proxynet");
        let argv = docker_argv(&spec, "cast-1");
        // no volume mounts at all
        assert!(spec.mounts.is_empty());
        assert!(
            !argv.iter().any(|a| a == "-v" || a.starts_with("--volume")),
            "no volume flags in cast argv: {argv:?}"
        );
        // no API keys in the environment
        assert!(
            !spec
                .env_passthrough
                .iter()
                .any(|e| e.contains("KEY") || e.contains("TOKEN"))
        );
        // hardening flags present
        assert!(argv.contains(&"--cap-drop=ALL".to_string()));
        assert!(argv.contains(&"--read-only".to_string()));
        assert!(argv.contains(&"--security-opt".to_string()));
        // single network = the proxy network
        assert_eq!(spec.networks, vec!["proxynet".to_string()]);
    }

    #[test]
    fn proxy_holds_keys_cast_does_not() {
        // the provider table maps env KEY NAMES; the proxy process is
        // the only reader of those env vars. The cast spec's env
        // passthrough provably contains no provider key names.
        let spec = cast_container_spec("foundry-cast:1", "proxynet");
        let provider_key_names = ["DEEPSEEK_API_KEY", "GLM_API_KEY"];
        for k in provider_key_names {
            assert!(
                !spec.env_passthrough.contains(&k.to_string()),
                "cast env must never contain {k}"
            );
        }
    }
}
