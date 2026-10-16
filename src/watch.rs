//! The scrape loop, shared by `watch` and `serve`.

use std::time::{Duration, Instant};

use crate::monitor::Event;
use crate::prom::{parse_page, ServerChoice};
use crate::sample::Sample;
use crate::sink::fetch;

pub struct WatchConfig {
    pub url: String,
    pub interval: Duration,
    /// Stop after this many scrapes. `None` runs until the process is stopped.
    pub max_scrapes: Option<u64>,
    /// Consecutive failed scrapes before a `scrape_failing` event.
    pub max_scrape_failures: u32,
    pub server: ServerChoice,
}

/// What the loop reports back to its caller.
pub trait Hooks {
    /// A good sample arrived. Returns the rule events it caused.
    fn sample(&mut self, sample: Sample) -> Vec<Event>;
    /// An event worth recording (rule transitions and scrape health).
    fn event(&mut self, event: &Event);
    /// Which server profile read the last good page.
    fn server(&mut self, _name: &'static str) {}
    /// `Some(error)` after a failed scrape, `None` after a good one.
    fn scrape_error(&mut self, _error: Option<&str>) {}
}

/// Scrapes on an interval until `max_scrapes` is reached, or forever.
pub fn run(cfg: &WatchConfig, hooks: &mut impl Hooks) {
    let timeout = cfg
        .interval
        .clamp(Duration::from_secs(1), Duration::from_secs(10));
    let start = Instant::now();
    let (mut failures, mut failing_reported, mut scrapes) = (0u32, false, 0u64);

    loop {
        let tick = Instant::now();
        let t = start.elapsed().as_secs_f64();

        let result = fetch(&cfg.url, timeout)
            .and_then(|body| parse_page(&body, t, cfg.server).map_err(|e| e.to_string()));

        match result {
            Ok((sample, profile)) => {
                if failing_reported {
                    hooks.event(&Event::ScrapeRecovered { t });
                    failing_reported = false;
                }
                failures = 0;
                hooks.scrape_error(None);
                hooks.server(profile.name);
                for e in hooks.sample(sample) {
                    hooks.event(&e);
                }
            }
            Err(error) => {
                failures += 1;
                eprintln!("nexusguard: scrape failed ({failures} in a row): {error}");
                hooks.scrape_error(Some(&error));
                if failures >= cfg.max_scrape_failures && !failing_reported {
                    hooks.event(&Event::ScrapeFailing {
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
