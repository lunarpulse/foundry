//! foundry-watch — the single-writer daemon loop (plan Task 9).
//!
//! Loop: Intake.poll → dedup → Line.submit → drive → FurnaceHold → notify.
//! Holds the Line lease for its whole life (critique #4: single writer).
//! Reconcile pass re-judges Unknown publishes each tick (critique #2).
pub mod audit_precision;
pub mod benchmark;
pub mod breaker;
pub mod budget_accounting;
pub mod bundles;
pub mod casting_run;
pub mod casting_wiring;
pub mod command_binding;
pub mod coordinator;
pub mod corpus;
pub mod corpus_emit;
pub mod discord_notifier;
pub mod dry_die;
pub mod evaluation_protocol;
pub mod evidence_recompute;
pub mod gc;
pub mod github_product_publisher;
pub mod github_publisher;
pub mod github_rest;
pub mod governance_events;
pub mod intake;
pub mod intake_worker;
pub mod integration_eval;
pub mod inventory;
pub mod jira_intake;
pub mod judging;
pub mod lessons;
pub mod model_proxy;
pub mod policy_apply;
pub mod profile_ext;
pub mod readjudicate;
pub mod repro;
pub mod retro;
pub mod synthesis;
pub mod watch;

pub use github_product_publisher::{GithubProductPublisher, GithubProductPublisherConfig};
pub use github_publisher::GithubPublisher;
pub use github_rest::{
    GithubApiError, GithubApiErrorKind, GithubHttpResponse, GithubHttpTransport, GithubIssue,
    GithubPullRequest, GithubRestClient, GithubRestConfig,
};
pub use intake::{GithubLabelIntake, IntakeConfig};
pub use watch::{WatchConfig, run, tick};
