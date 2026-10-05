use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::sync::{Arc, Mutex};
use std::time::Instant;

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

        collector.record_metadata("process_name", pid, 0, "name", "makeyd");
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
}
