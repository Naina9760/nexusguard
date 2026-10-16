//! The scrape loop, shared by `watch` and `serve`.

use std::time::{Duration, Instant};

use crate::monitor::Event;
use crate::prom::{parse_sample, MetricNames};
use crate::sample::Sample;
use crate::sink::fetch;

pub struct WatchConfig {
    pub url: String,
    pub interval: Duration,
    /// Stop after this many scrapes. `None` runs until the process is stopped.
    pub max_scrapes: Option<u64>,
    /// Consecutive failed scrapes before a `scrape_failing` event.
    pub max_scrape_failures: u32,
    pub names: &'static MetricNames,
}

/// Scrapes on an interval. Every good sample goes to `ingest`, which returns the
/// rule events it caused; every event, including scrape failures, goes to `on_event`.
pub fn run(
    cfg: &WatchConfig,
    mut ingest: impl FnMut(Sample) -> Vec<Event>,
    mut on_event: impl FnMut(&Event),
    mut on_scrape_error: impl FnMut(Option<&str>),
) {
    let timeout = cfg
        .interval
        .clamp(Duration::from_secs(1), Duration::from_secs(10));
    let start = Instant::now();
    let (mut failures, mut failing_reported, mut scrapes) = (0u32, false, 0u64);

    loop {
        let tick = Instant::now();
        let t = start.elapsed().as_secs_f64();

        let result = fetch(&cfg.url, timeout)
            .and_then(|body| parse_sample(&body, t, cfg.names).map_err(|e| e.to_string()));

        match result {
            Ok(sample) => {
                if failing_reported {
                    on_event(&Event::ScrapeRecovered { t });
                    failing_reported = false;
                }
                failures = 0;
                on_scrape_error(None);
                for e in ingest(sample) {
                    on_event(&e);
                }
            }
            Err(error) => {
                failures += 1;
                eprintln!("nexusguard: scrape failed ({failures} in a row): {error}");
                on_scrape_error(Some(&error));
                if failures >= cfg.max_scrape_failures && !failing_reported {
                    on_event(&Event::ScrapeFailing {
                        t,
                        consecutive: failures,
                        error,
                    });
                    failing_reported = true;
                }
            }
        }

        scrapes += 1;
        if cfg.max_scrapes.is_some_and(|max| scrapes >= max) {
            return;
        }
        let elapsed = tick.elapsed();
        if elapsed < cfg.interval {
            std::thread::sleep(cfg.interval - elapsed);
        }
    }
}
