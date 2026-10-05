use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// How close a run came to the best possible schedule with `jobs` slots.
///
/// Every valid schedule takes at least `lower_bound_us` =
/// max(critical path, ⌈work / jobs⌉) (`work_le_slots_mul_makespan`,
/// `chain_dur_le_finish`), and a greedy one at most `graham_bound_us` =
/// (work + (jobs - 1) * critical path) / jobs (`greedy_makespan_bound_tight`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduleBounds {
    pub work_us: u64,
    pub span_us: u64,
    pub critical_path_us: u64,
    pub jobs: usize,
    pub lower_bound_us: u64,
    pub graham_bound_us: u64,
}

impl ScheduleBounds {
    pub fn new(work_us: u64, span_us: u64, critical_path_us: u64, jobs: usize) -> Self {
        let m = jobs.max(1) as u64;
        Self {
            work_us,
            span_us,
            critical_path_us,
            jobs,
            lower_bound_us: critical_path_us.max(work_us.div_ceil(m)),
            graham_bound_us: (work_us + (m - 1) * critical_path_us) / m,
        }
    }

    /// Measured span over the lower bound: 1.00 means no schedule could
    /// have been shorter.
    pub fn gap(&self) -> f64 {
        if self.lower_bound_us == 0 {
            1.0
        } else {
            self.span_us as f64 / self.lower_bound_us as f64
        }
    }
}

#[derive(Debug, Clone)]
pub struct TraceEvent {
    pub name: String,
    pub cat: String,
    pub ph: String, // "X" for complete event, "M" for metadata
    pub ts: u64,    // microseconds relative to build start
    pub dur: u64,   // duration in microseconds
    pub pid: u32,
    pub tid: u32,
    pub args: HashMap<String, String>,
}

#[derive(Clone)]
pub struct TraceCollector {
    start_time: Instant,
    events: Arc<Mutex<Vec<TraceEvent>>>,
}

impl Default for TraceCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl TraceCollector {
    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn start_micros(&self) -> u64 {
        self.start_time.elapsed().as_micros() as u64
    }

    // One argument per Chrome Trace event field.
    #[allow(clippy::too_many_arguments)]
    pub fn record_complete(
        &self,
        name: String,
        cat: &str,
        start_ts: u64,
        dur: u64,
        pid: u32,
        tid: u32,
        args: HashMap<String, String>,
    ) {
        let event = TraceEvent {
            name,
            cat: cat.to_string(),
            ph: "X".to_string(),
            ts: start_ts,
            dur,
            pid,
            tid,
            args,
        };
        self.events.lock().unwrap().push(event);
    }

    pub fn record_metadata(&self, name: &str, pid: u32, tid: u32, meta_name: &str, val: &str) {
        let mut args = HashMap::new();
        args.insert(meta_name.to_string(), val.to_string());
        let event = TraceEvent {
            name: name.to_string(),
            cat: "__metadata".to_string(),
            ph: "M".to_string(),
            ts: 0,
            dur: 0,
            pid,
            tid,
            args,
        };
        self.events.lock().unwrap().push(event);
    }

    /// Calculate the critical path (longest execution chain) through the build DAG
    pub fn compute_critical_path(
        &self,
        adj: &HashMap<String, Vec<String>>,
        root: &str,
    ) -> (u64, Vec<String>) {
        let events = self.events.lock().unwrap();
        let mut durations: HashMap<String, u64> = HashMap::new();
        for ev in events.iter() {
            if ev.ph == "X" && ev.cat == "rule" {
                durations.insert(ev.name.clone(), ev.dur);
            }
        }
        drop(events);
        // Nothing ran (a null build): every path has length 0 and the walk
        // below would only return the root.
        if durations.is_empty() {
            return (0, vec![root.to_string()]);
        }

        // Longest latency path by post-order DP. Iterative (explicit stack) so
        // deep chains cannot overflow the thread stack, and each node stores
        // only its best predecessor, so time and memory stay O(V + E).
        let mut best: HashMap<&str, (u64, Option<&str>)> = HashMap::new();
        let mut visiting: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut stack: Vec<(&str, bool)> = vec![(root, false)];
        while let Some((u, expanded)) = stack.pop() {
            if best.contains_key(u) {
                continue;
            }
            let prereqs = adj.get(u).map(Vec::as_slice).unwrap_or(&[]);
            if !expanded {
                if !visiting.insert(u) {
                    continue;
                }
                stack.push((u, true));
                for dep in prereqs {
                    let dep = dep.as_str();
                    if !best.contains_key(dep) && !visiting.contains(dep) {
                        stack.push((dep, false));
                    }
                }
                continue;
            }
            let mut best_dep: Option<&str> = None;
            let mut best_dep_dur = 0u64;
            for dep in prereqs {
                // A dep still missing here is on a cycle; treat it as zero.
                let d = best.get(dep.as_str()).map_or(0, |&(t, _)| t);
                if d > best_dep_dur {
                    best_dep_dur = d;
                    best_dep = Some(dep.as_str());
                }
            }
            let my_dur = durations.get(u).copied().unwrap_or(0);
            best.insert(u, (my_dur + best_dep_dur, best_dep));
            visiting.remove(u);
        }

        let total = best.get(root).map_or(0, |&(t, _)| t);
        let mut path = Vec::new();
        let mut cur = Some(root);
        while let Some(u) = cur {
            path.push(u.to_string());
            cur = best.get(u).and_then(|&(_, prev)| prev);
        }
        path.reverse();
        (total, path)
    }

    /// Measured schedule against the bounds proved in
    /// `lean_make/LeanMake/Scheduling.lean`, from the recorded rule events.
    /// `critical_path_us` is the longest dependency chain (see
    /// `compute_critical_path`). `None` when no rule did any work.
    pub fn schedule_bounds(&self, jobs: usize, critical_path_us: u64) -> Option<ScheduleBounds> {
        let events = self.events.lock().unwrap();
        let rules: Vec<&TraceEvent> = events
            .iter()
            .filter(|e| e.ph == "X" && e.cat == "rule" && e.dur > 0)
            .collect();
        let first = rules.iter().map(|e| e.ts).min()?;
        let last = rules.iter().map(|e| e.ts + e.dur).max()?;
        let work_us: u64 = rules.iter().map(|e| e.dur).sum();
        Some(ScheduleBounds::new(
            work_us,
            last - first,
            critical_path_us,
            jobs,
        ))
    }

    /// Durations of the rules that actually ran a recipe in this run.
    pub fn rebuilt_durations(&self) -> Vec<(String, u64)> {
        let events = self.events.lock().unwrap();
        events
            .iter()
            .filter(|e| {
                e.ph == "X"
                    && e.cat == "rule"
                    && e.dur > 0
                    && e.args.get("status").map(String::as_str) == Some("rebuilt")
            })
            .map(|e| (e.name.clone(), e.dur))
            .collect()
    }

    /// Save Chrome Trace / Perfetto compatible JSON
    pub fn save_to_file(&self, path: &str) -> io::Result<()> {
        let events = self.events.lock().unwrap();
        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);

        writeln!(writer, "{{")?;
        writeln!(writer, "  \"displayTimeUnit\": \"ms\",")?;
        writeln!(writer, "  \"traceEvents\": [")?;

        let len = events.len();
        for (i, ev) in events.iter().enumerate() {
            write!(
                writer,
                "    {{\"name\": \"{}\", \"cat\": \"{}\", \"ph\": \"{}\", \"ts\": {}, \"pid\": {}, \"tid\": {}",
                escape_json(&ev.name),
                escape_json(&ev.cat),
                ev.ph,
                ev.ts,
                ev.pid,
                ev.tid
            )?;

            if ev.dur > 0 {
                write!(writer, ", \"dur\": {}", ev.dur)?;
            }

            if !ev.args.is_empty() {
                write!(writer, ", \"args\": {{")?;
                let mut first = true;
                for (k, v) in &ev.args {
                    if !first {
                        write!(writer, ", ")?;
                    }
                    first = false;
                    write!(writer, "\"{}\": \"{}\"", escape_json(k), escape_json(v))?;
                }
                write!(writer, "}}")?;
            }

            if i + 1 < len {
                writeln!(writer, "}},")?;
            } else {
                writeln!(writer, "}}")?;
            }
        }

        writeln!(writer, "  ]")?;
        writeln!(writer, "}}")?;
        writer.flush()?;

        Ok(())
    }
}

fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trace_collector_and_critical_path() {
        let collector = TraceCollector::new();
        let pid = 100;

        collector.record_metadata("process_name", pid, 0, "name", "maked");
        collector.record_metadata("thread_name", pid, 1, "name", "Worker 1");

        // Record target 'a' (leaf): dur 10ms (10000us)
        let mut args_a = HashMap::new();
        args_a.insert("status".to_string(), "rebuilt".to_string());
        collector.record_complete("a".to_string(), "rule", 0, 10000, pid, 1, args_a);

        // Record target 'b' (leaf): dur 30ms (30000us)
        let mut args_b = HashMap::new();
        args_b.insert("status".to_string(), "rebuilt".to_string());
        collector.record_complete("b".to_string(), "rule", 0, 30000, pid, 2, args_b);

        // Record target 'all' depending on a and b: dur 5ms (5000us)
        let mut args_all = HashMap::new();
        args_all.insert("status".to_string(), "rebuilt".to_string());
        collector.record_complete("all".to_string(), "rule", 30000, 5000, pid, 1, args_all);

        let mut adj = HashMap::new();
        adj.insert("all".to_string(), vec!["a".to_string(), "b".to_string()]);
        adj.insert("a".to_string(), vec![]);
        adj.insert("b".to_string(), vec![]);

        let (crit_dur, crit_path) = collector.compute_critical_path(&adj, "all");
        assert_eq!(crit_dur, 35000); // 30000 (b) + 5000 (all)
        assert_eq!(crit_path, vec!["b".to_string(), "all".to_string()]);

        // Test saving to JSON
        let tmp_path = std::env::temp_dir().join(format!("test_trace_{}.json", std::process::id()));
        collector.save_to_file(tmp_path.to_str().unwrap()).unwrap();
        assert!(tmp_path.exists());

        let content = std::fs::read_to_string(&tmp_path).unwrap();
        assert!(content.contains("\"traceEvents\":"));
        assert!(content.contains("\"name\": \"all\""));
        assert!(content.contains("\"name\": \"b\""));

        let _ = std::fs::remove_file(tmp_path);
    }

    #[test]
    fn test_critical_path_terminates_on_cycle() {
        let collector = TraceCollector::new();
        let mut adj = HashMap::new();
        adj.insert("a".to_string(), vec!["b".to_string()]);
        adj.insert("b".to_string(), vec!["a".to_string()]);
        let (_, path) = collector.compute_critical_path(&adj, "a");
        assert_eq!(path.last().map(String::as_str), Some("a"));
    }

    #[test]
    fn test_critical_path_deep_chain_is_linear_and_stack_safe() {
        // n0 <- n1 <- ... <- n{N-1}; every node 1us. A recursive or
        // path-copying implementation overflows or goes quadratic here.
        const N: usize = 50_000;
        let collector = TraceCollector::new();
        let mut adj = HashMap::new();
        for i in 0..N {
            let name = format!("n{i}");
            collector.record_complete(name.clone(), "rule", 0, 1, 1, 1, HashMap::new());
            let deps = if i == 0 {
                vec![]
            } else {
                vec![format!("n{}", i - 1)]
            };
            adj.insert(name, deps);
        }
        let (dur, path) = collector.compute_critical_path(&adj, &format!("n{}", N - 1));
        assert_eq!(dur, N as u64);
        assert_eq!(path.len(), N);
        assert_eq!(path.first().map(String::as_str), Some("n0"));
        assert_eq!(
            path.last().map(String::as_str),
            Some(&*format!("n{}", N - 1))
        );
    }

    #[test]
    fn test_schedule_bounds_two_slots() {
        // Three independent 10us jobs on 2 slots: a, b at 0; c at 10.
        let collector = TraceCollector::new();
        collector.record_complete("a".into(), "rule", 0, 10, 1, 1, HashMap::new());
        collector.record_complete("b".into(), "rule", 0, 10, 1, 2, HashMap::new());
        collector.record_complete("c".into(), "rule", 10, 10, 1, 1, HashMap::new());
        let b = collector.schedule_bounds(2, 10).unwrap();
        assert_eq!(b.work_us, 30);
        assert_eq!(b.span_us, 20);
        assert_eq!(b.lower_bound_us, 15); // max(CP 10, ceil(30 / 2))
        assert_eq!(b.graham_bound_us, 20); // (30 + 1 * 10) / 2
        assert!((b.gap() - 20.0 / 15.0).abs() < 1e-9);
    }

    #[test]
    fn test_schedule_bounds_none_without_work() {
        let collector = TraceCollector::new();
        collector.record_complete("a".into(), "rule", 5, 0, 1, 1, HashMap::new());
        assert_eq!(collector.schedule_bounds(4, 0), None);
    }
}
