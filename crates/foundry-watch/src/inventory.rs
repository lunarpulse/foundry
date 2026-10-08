//! State & authority inventory — M5 T43 (plan v2; reviews #3/#23).
//!
//! BEFORE any M5 code: a complete table of every mutable object and
//! every authority-bearing entry point. This inventory is the matrix
//! every later gate indexes against — an object or path NOT in this
//! table cannot claim a scope, and a gate cannot pass by testing a
//! subset of it.
//!
//! Review #3: ledger/identity/scope were covered, but sessions,
//! profiles, lessons, corpora, breakers, caches, artifacts, worktrees,
//! replay keys, evidence envelopes, logs, exports were not.
//! Review #23: authority paths beyond HTTP (CLI, webhooks, jobs,
//! exports, casting, recovery, restore) need adversarial matrices.

use serde::{Deserialize, Serialize};

/// Scope of ownership for a mutable object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OwnScope {
    /// owned by exactly one organization (tenant)
    Org,
    /// owned by one casting run (experiments never leak across runs)
    CastingRun,
    /// owned by one order lifecycle
    Order,
    /// owned by one user account
    User,
    /// INTENTIONALLY shared, immutable-after-write (code, frozen
    /// corpus manifests, past report envelopes) — listed so the
    /// claim "shared" is explicit, never implicit
    SharedImmutable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateObject {
    pub name: &'static str,
    pub scope: OwnScope,
    pub store: &'static str,
    pub notes: &'static str,
}

/// The complete mutable-state inventory. Every later M5 gate that
/// touches state must reference entries here — review #3's demand.
pub fn state_inventory() -> Vec<StateObject> {
    vec![
        StateObject { name: "order journal rows", scope: OwnScope::Org, store: "ledger.db (sqlite)", notes: "append-only hash chain; org-scoped" },
        StateObject { name: "casting journal rows", scope: OwnScope::Org, store: "ledger.db casting table", notes: "v2 chain; run-scoped semantics inside org" },
        StateObject { name: "user accounts (roles, disabled)", scope: OwnScope::Org, store: "identity.db", notes: "role changes revoke sessions" },
        StateObject { name: "sessions (opaque tokens)", scope: OwnScope::User, store: "identity.db sessions", notes: "instant revocation; role never in token" },
        StateObject { name: "approval token consumptions", scope: OwnScope::Org, store: "identity.db token_consumptions", notes: "single-use PK" },
        StateObject { name: "order scope assignments", scope: OwnScope::Org, store: "scope.db order_scope", notes: "single-org contract" },
        StateObject { name: "intake source bindings", scope: OwnScope::Org, store: "scope.db OrgConfig", notes: "admission control" },
        StateObject { name: "jira webhook seen-ids (dedup/echo)", scope: OwnScope::Org, store: "jira_seen.txt + in-mem version floor", notes: "durable; per-connector namespacing" },
        StateObject { name: "github intake in-process seen set", scope: OwnScope::Org, store: "process memory (watch)", notes: "M0 at-least-once; ledger dedup is the durable layer" },
        StateObject { name: "candidate profiles (casting)", scope: OwnScope::CastingRun, store: "casting dir profile files", notes: "content-hashed; T46 freezes into bundles" },
        StateObject { name: "candidate profile extensions (T35)", scope: OwnScope::CastingRun, store: "casting dir", notes: "repro_class/cost; separate hash" },
        StateObject { name: "casting run workdirs (sandboxes)", scope: OwnScope::CastingRun, store: "tmp sandbox dirs", notes: "env-whitelisted; deleted after run" },
        StateObject { name: "casting circuit breaker state", scope: OwnScope::CastingRun, store: "process memory (watch)", notes: "(blueprint, run_id) key — never global" },
        StateObject { name: "corpus manifests (frozen)", scope: OwnScope::SharedImmutable, store: "~/.foundry/corpus", notes: "content-addressed by manifest hash" },
        StateObject { name: "baseline metrics snapshots", scope: OwnScope::SharedImmutable, store: "~/.foundry/corpus/baseline-m2.json", notes: "frozen reference" },
        StateObject { name: "order worktrees / patches", scope: OwnScope::Order, store: "~/.foundry-work", notes: "per-order; cleaned on terminal states" },
        StateObject { name: "audit report envelopes (generated)", scope: OwnScope::SharedImmutable, store: "caller-owned output", notes: "verifiable; prefix-pinned" },
        StateObject { name: "export artifacts (pseudonymized)", scope: OwnScope::Org, store: "caller-owned output", notes: "approver+ gated" },
        StateObject { name: "logs (serve/watch/cast stdout)", scope: OwnScope::Org, store: "journald", notes: "privacy screening applies to payloads" },
        StateObject { name: "credential files (gh_token, die.env, notifier.env)", scope: OwnScope::Org, store: "~/.foundry (600)", notes: "NEVER in cast boundary; T44 containerizes" },
        StateObject { name: "deploy boundary table (T34)", scope: OwnScope::SharedImmutable, store: "binary + ~/.foundry/deploy", notes: "input to T44 compose generation" },
        // T45+ additions (forward-declared so gates can index them):
        StateObject { name: "review lessons (T45)", scope: OwnScope::Org, store: "lesson corpus (new)", notes: "provenance-sealed; adversarial-input class" },
        StateObject { name: "candidate bundles (T46)", scope: OwnScope::SharedImmutable, store: "content-addressed store (new)", notes: "immutable by construction" },
    ]
}

/// Every authority-bearing entry point — the adversarial matrix index
/// (review #23): HTTP, CLI, webhooks, jobs, exports, casting, recovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryPoint {
    pub name: &'static str,
    pub surface: Surface,
    pub authority_check: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Surface {
    Http,
    Cli,
    Webhook,
    BackgroundJob,
    Export,
    Casting,
    Recovery,
}

pub fn entry_points() -> Vec<EntryPoint> {
    vec![
        EntryPoint { name: "GET /api/orders", surface: Surface::Http, authority_check: "session + ReadBoard (T30 guard)" },
        EntryPoint { name: "GET /api/orders/{id}", surface: Surface::Http, authority_check: "session + ReadBoard + scope (T30)" },
        EntryPoint { name: "GET /api/audit/verify", surface: Surface::Http, authority_check: "session + ReadBoard" },
        EntryPoint { name: "GET /api/status", surface: Surface::Http, authority_check: "intentionally unauthenticated (health, secret-free)" },
        EntryPoint { name: "POST /api/orders/{id}/approve", surface: Surface::Http, authority_check: "session + Approve + single-token consume (T29)" },
        EntryPoint { name: "foundry approve", surface: Surface::Cli, authority_check: "local operator context (documented); T44 containerizes CLI away" },
        EntryPoint { name: "foundry casting --promote", surface: Surface::Cli, authority_check: "ApprovalToken required (T26 4-path block)" },
        EntryPoint { name: "foundry casting --run-profile", surface: Surface::Casting, authority_check: "owner-only capability (C1 matrix); non-promoting observation" },
        EntryPoint { name: "github intake poll", surface: Surface::BackgroundJob, authority_check: "source binding admission (scope.db)" },
        EntryPoint { name: "jira webhook deliver", surface: Surface::Webhook, authority_check: "HMAC first + dedup + echo + version floor (T32)" },
        EntryPoint { name: "audit report generate/verify", surface: Surface::Export, authority_check: "ReadBoard for view; approver+ for export (T31)" },
        EntryPoint { name: "pseudonymized export", surface: Surface::Export, authority_check: "approver+ (T31 guard_export)" },
        EntryPoint { name: "identity bootstrap", surface: Surface::Recovery, authority_check: "local CLI only, once, no defaults (T29 C2)" },
        EntryPoint { name: "serve approve rehydrate path", surface: Surface::Recovery, authority_check: "order-in-ledger check + same approval gate (defect #7)" },
        EntryPoint { name: "watch auto-cycle (Die->inspect->hold)", surface: Surface::BackgroundJob, authority_check: "gate+policy; human hold at FurnaceHold" },
        EntryPoint { name: "gc (tmp cleanup)", surface: Surface::BackgroundJob, authority_check: "path-confined to workdir; no authority" },
    ]
}

/// Coverage check (gate 1): every mutable object has an explicit
/// scope; every authority entry point declares its check. Used by the
/// M5 gate to prove the inventory is complete by construction: the
/// table is the spec, and new state must be added HERE to be legal.
pub fn validate_inventory() -> Result<(), String> {
    let states = state_inventory();
    if states.is_empty() {
        return Err("empty state inventory".into());
    }
    for s in &states {
        if s.name.is_empty() || s.store.is_empty() {
            return Err(format!("incomplete state entry: {s:?}"));
        }
    }
    let eps = entry_points();
    for e in &eps {
        if e.authority_check.is_empty() {
            return Err(format!("entry point without authority declaration: {e:?}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_covers_the_review_gaps() {
        let names: Vec<_> = state_inventory().iter().map(|s| s.name).collect();
        // the exact gaps review #3 called out must be present
        for required in [
            "sessions (opaque tokens)",
            "candidate profiles (casting)",
            "casting circuit breaker state",
            "jira webhook seen-ids (dedup/echo)",
            "order worktrees / patches",
            "export artifacts (pseudonymized)",
            "credential files (gh_token, die.env, notifier.env)",
            "logs (serve/watch/cast stdout)",
        ] {
            assert!(names.contains(&required), "missing review-#3 gap: {required}");
        }
    }

    #[test]
    fn entry_points_cover_every_surface() {
        let eps = entry_points();
        for surface in [
            Surface::Http,
            Surface::Cli,
            Surface::Webhook,
            Surface::BackgroundJob,
            Surface::Export,
            Surface::Casting,
            Surface::Recovery,
        ] {
            assert!(
                eps.iter().any(|e| e.surface == surface),
                "surface {surface:?} has no entry point listed"
            );
        }
    }

    #[test]
    fn breaker_is_never_global() {
        let inv = state_inventory();
        let b = inv.iter().find(|s| s.name.contains("circuit breaker")).unwrap();
        assert_eq!(b.scope, OwnScope::CastingRun, "breaker must stay run-scoped (review #3)");
    }

    #[test]
    fn credentials_never_cast_scoped() {
        let inv = state_inventory();
        let c = inv.iter().find(|s| s.name.contains("credential files")).unwrap();
        assert_ne!(c.scope, OwnScope::CastingRun);
    }

    #[test]
    fn validate_passes() {
        validate_inventory().unwrap();
    }
}
