//! foundry-watch — the single-writer daemon loop (plan Task 9).
//!
//! Loop: Intake.poll → dedup → Line.submit → drive → FurnaceHold → notify.
//! Holds the Line lease for its whole life (critique #4: single writer).
//! Reconcile pass re-judges Unknown publishes each tick (critique #2).
pub mod benchmark;
pub mod breaker;
pub mod casting_run;
pub mod corpus;
pub mod corpus_emit;
pub mod discord_notifier;
pub mod dry_die;
pub mod gc;
pub mod github_publisher;
pub mod intake;
pub mod budget_accounting;
pub mod bundles;
pub mod model_proxy;
pub mod evaluation_protocol;
pub mod governance_events;
pub mod integration_eval;
pub mod inventory;
pub mod lessons;
pub mod jira_intake;
pub mod profile_ext;
pub mod synthesis;
pub mod judging;
pub mod readjudicate;
pub mod repro;
pub mod watch;

pub use github_publisher::GithubPublisher;
pub use intake::{GithubLabelIntake, IntakeConfig};
pub use watch::{WatchConfig, run, tick};
