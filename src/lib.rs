//! NexusGuard watches an LLM inference server's metrics and reports when a
//! bounded temporal rule says something is wrong: output has stalled with
//! requests queued, the KV cache is thrashing, or the queue is not draining.
//!
//! Pipeline: [`prom`] turns a scrape into a [`sample::Sample`]; [`trace`] keeps
//! a bounded history and derives rates; [`formula`] evaluates past-time
//! temporal formulas over it; [`rules`] are the built-in formulas; [`monitor`]
//! turns verdict changes into [`monitor::Event`]s.

pub mod formula;
pub mod monitor;
pub mod prom;
pub mod render;
pub mod rules;
pub mod sample;
pub mod serve;
pub mod sim;
pub mod sink;
pub mod trace;
pub mod watch;
