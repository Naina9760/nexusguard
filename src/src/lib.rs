use std::collections::VecDeque;

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

    /// Evaluates bounded liveness / starvation invariants over recent history window.
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