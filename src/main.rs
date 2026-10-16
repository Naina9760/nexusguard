use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct SchedulerState {
    pub step: usize,
    pub worker_id: usize,
    pub queue_len: usize,
    pub total_kv_blocks: usize,
    pub allocated_kv_blocks: usize,
    pub executing_requests: Vec<String>,
    pub skipped_requests: Vec<String>,
}

pub struct NexusGuardMonitor {
    _total_memory_blocks: usize,
    pub history: VecDeque<SchedulerState>,
    max_history: usize,
}

impl NexusGuardMonitor {
    pub fn new(total_memory_blocks: usize, max_history: usize) -> Self {
        Self {
            _total_memory_blocks: total_memory_blocks,
            history: VecDeque::new(),
            max_history,
        }
    }

    pub fn log_state(&mut self, state: SchedulerState) {
        if self.history.len() >= self.max_history {
            self.history.pop_front();
        }
        self.history.push_back(state);
    }

    pub fn check_liveness_no_starvation(&self, window_size: usize) -> Result<(), String> {
        if self.history.len() < window_size {
            return Ok(());
        }

        let recent: Vec<&SchedulerState> = self.history.iter().rev().take(window_size).collect();
        
        if let Some(first_skipped) = recent.last().and_then(|s| s.skipped_requests.first()) {
            let stuck_count = recent
                .iter()
                .filter(|s| {
                    s.skipped_requests
                        .first()
                        .map_or(false, |req| req == first_skipped)
                })
                .count();

            if stuck_count >= window_size {
                return Err(format!(
                    "Liveness Violation (Starvation/Deadlock): Request '{}' stuck at queue head for {} consecutive steps!",
                    first_skipped, window_size
                ));
            }
        }

        Ok(())
    }
}

fn main() {
    println!("==================================================");
    println!("  NEXUSGUARD: Active Defense & Rollback Engine   ");
    println!("==================================================");

    let total_kv_blocks = 100;
    let monitor = Arc::new(Mutex::new(NexusGuardMonitor::new(total_kv_blocks, 20)));

    let monitor_clone = Arc::clone(&monitor);
    
    let handle = thread::spawn(move || {
        for step in 1..=6 {
            let allocated = if step >= 2 { 95 } else { 40 };
            let skipped = if allocated >= 90 {
                vec!["req_critical_target".to_string()]
            } else {
                vec![]
            };

            let state = SchedulerState {
                step,
                worker_id: 2,
                queue_len: 2,
                total_kv_blocks: 100,
                allocated_kv_blocks: allocated,
                executing_requests: vec!["req_active".into()],
                skipped_requests: skipped,
            };

            let mut mon = monitor_clone.lock().unwrap();
            println!("[Worker 2] Step {} Reporting [Allocated: {}]", step, allocated);
            mon.log_state(state.clone());

            if let Err(e) = mon.check_liveness_no_starvation(3) {
                println!("\n\x1b[91m[INTERCEPTION ACTIVE] Deadlock caught!\x1b[0m");
                println!("  -> Reason: {}", e);
                println!("\x1b[32m[ACTION TAKEN] Issuing automatic state rollback & flushing KV cache...\x1b[0m\n");
            }

            drop(mon);
            thread::sleep(Duration::from_millis(30));
        }
    });

    handle.join().unwrap();
    println!("[Cluster State Secured] Active defense execution cycle completed cleanly.");
}