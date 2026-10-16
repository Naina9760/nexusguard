//! Runtime monitor for LLM inference servers. Reads a /metrics page, keeps a
//! short history and checks temporal rules over it.
//!
//! prom -> sample -> trace -> formula / rules -> monitor -> events.
//! render and serve draw what the monitor recorded.

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
