//! foundry-watch — the single-writer daemon loop (plan Task 9).
//!
//! Loop: Intake.poll → dedup → Line.submit → drive → FurnaceHold → notify.
//! Holds the Line lease for its whole life (critique #4: single writer).
//! Reconcile pass re-judges Unknown publishes each tick (critique #2).
pub mod github_publisher;
pub mod intake;
pub mod watch;

pub use github_publisher::GithubPublisher;
pub use intake::{GithubLabelIntake, IntakeConfig};
pub use watch::{run, tick, WatchConfig};
