#![warn(missing_docs)]
//! `orlando-runtime` — the single-silo runtime for Orlando grains.
//!
//! Hosts grain activations in a [`Silo`]: the [`GrainDirectory`] tracks active
//! grains (activate-on-miss, deactivate-on-idle), [`Activation`] owns a grain's
//! mailbox task, [`WorkerPool`] backs stateless-worker grains, and
//! [`MetricsFilter`] records per-call metrics. Build one with [`SiloBuilder`].

mod activation;
mod directory;
mod metrics_filter;
mod silo;
mod worker_pool;

pub use activation::Activation;
pub use directory::GrainDirectory;
pub use metrics_filter::MetricsFilter;
pub use silo::{Silo, SiloBuilder};
pub use worker_pool::WorkerPool;
