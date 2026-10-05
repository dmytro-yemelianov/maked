use std::io::{IsTerminal, Write, stdout};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct WorkerSlot {
    pub target: Option<String>,
    pub start_time: Option<Instant>,
}

#[derive(Debug)]
pub struct TuiState {
    pub total_targets: usize,
    pub completed_targets: usize,
    pub cached_targets: usize,
    pub workers: Vec<WorkerSlot>,
    pub recent_completed: Vec<(String, String)>,
    pub start_time: Instant,
    pub is_tty: bool,
    pub enabled: bool,
}

#[derive(Clone)]
pub struct TuiReporter {
    state: Arc<Mutex<TuiState>>,
}

impl TuiReporter {
    pub fn new(num_workers: usize, total_targets: usize, enabled: bool) -> Self {
        let is_tty = stdout().is_terminal() && std::env::var("TERM").unwrap_or_default() != "dumb";
        let workers = vec![
            WorkerSlot {
                target: None,
                start_time: None
            };
            num_workers.max(1)
        ];
        let state = TuiState {
            total_targets,
            completed_targets: 0,
            cached_targets: 0,
            workers,
            recent_completed: Vec::new(),
            start_time: Instant::now(),
            is_tty,
            enabled,
        };
        let reporter = Self {
            state: Arc::new(Mutex::new(state)),
        };
        if enabled && is_tty {
            let mut out = stdout();
            let _ = write!(out, "\x1b[?25l");
            let _ = out.flush();
        }
        reporter
    }

    pub fn target_started(&self, worker_id: usize, target: &str) {
        let mut st = self.state.lock().unwrap();
        if !st.enabled {
            return;
        }
        if worker_id < st.workers.len() {
            st.workers[worker_id] = WorkerSlot {
                target: Some(target.to_string()),
                start_time: Some(Instant::now()),
            };
        }
        if st.is_tty {
            Self::render_locked(&st);
        }
    }

    pub fn target_finished(&self, worker_id: usize, target: &str, status: &str, cached: bool) {
        let mut st = self.state.lock().unwrap();
        if !st.enabled {
            return;
        }
        st.completed_targets += 1;
        if cached {
            st.cached_targets += 1;
        }
        if worker_id < st.workers.len() {
            st.workers[worker_id] = WorkerSlot {
                target: None,
                start_time: None,
            };
        }
        st.recent_completed
            .push((target.to_string(), status.to_string()));
        if st.recent_completed.len() > 5 {
            st.recent_completed.remove(0);
        }
        if st.is_tty {
            Self::render_locked(&st);
        } else {
            let pct = if st.total_targets > 0 {
                (st.completed_targets as f64 / st.total_targets as f64) * 100.0
            } else {
                100.0
            };
            println!(
                "[{:>5.1}%] [{}/{}] Finished {} ({})",
                pct, st.completed_targets, st.total_targets, target, status
            );
        }
    }

    fn render_locked(st: &TuiState) {
        let mut out = stdout();
        let elapsed = st.start_time.elapsed().as_secs_f64();
        let total = st.total_targets.max(1);
        let completed = st.completed_targets;
        let pct = ((completed as f64 / total as f64) * 100.0).min(100.0);

        let bar_width: usize = 30;
        let filled = ((completed as f64 / total as f64) * bar_width as f64).round() as usize;
        let empty = bar_width.saturating_sub(filled);
        let bar = format!("[{}{}]", "=".repeat(filled), " ".repeat(empty));

        let mut buf = String::new();
        buf.push_str(&format!(
            "\r\x1b[K\x1b[1;36m┌─ makeyd v{:<6}─ Live Execution Dashboard ──────────────────────┐\x1b[0m\n",
            env!("CARGO_PKG_VERSION")
        ));
        buf.push_str(&format!(
            "\x1b[K│ \x1b[1mProgress:\x1b[0m \x1b[32m{}\x1b[0m {:>5.1}% ({}/{} targets) │\n",
            bar, pct, completed, total
        ));
        buf.push_str(&format!(
            "\x1b[K│ \x1b[1mElapsed:\x1b[0m  {:.2}s | \x1b[1mCached:\x1b[0m {} | \x1b[1mActive Workers:\x1b[0m {}/{}   │\n",
            elapsed,
            st.cached_targets,
            st.workers.iter().filter(|w| w.target.is_some()).count(),
            st.workers.len()
        ));
        buf.push_str("\x1b[K├─ Worker Swimlanes ─────────────────────────────────────────────┤\n");

        for (idx, w) in st.workers.iter().enumerate() {
            let status = match (&w.target, &w.start_time) {
                (Some(tgt), Some(start)) => {
                    let dur_ms = start.elapsed().as_millis();
                    format!("\x1b[33mbuilding\x1b[0m {} ({}ms)", tgt, dur_ms)
                }
                _ => "\x1b[2midle\x1b[0m".to_string(),
            };
            buf.push_str(&format!("\x1b[K│ [Worker {:>2}] {}\n", idx + 1, status));
        }

        buf.push_str("\x1b[K└────────────────────────────────────────────────────────────────┘\n");

        let num_lines = 5 + st.workers.len();
        buf.push_str(&format!("\x1b[{}A", num_lines));

        let _ = write!(out, "{}", buf);
        let _ = out.flush();
    }

    pub fn finish(&self) {
        let st = self.state.lock().unwrap();
        if st.enabled && st.is_tty {
            let mut out = stdout();
            let num_lines = 5 + st.workers.len();
            let _ = write!(out, "\x1b[{}B\x1b[?25h\n", num_lines);
            let _ = out.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tui_reporter_lifecycle() {
        let reporter = TuiReporter::new(2, 4, true);
        reporter.target_started(0, "task1.o");
        reporter.target_finished(0, "task1.o", "rebuilt", false);
        reporter.target_started(1, "task2.o");
        reporter.target_finished(1, "task2.o", "cached", true);
        reporter.finish();

        let st = reporter.state.lock().unwrap();
        assert_eq!(st.completed_targets, 2);
        assert_eq!(st.cached_targets, 1);
    }
}
