//! Per-target recipe durations from earlier runs (`.makeyd_log`), and the
//! scheduling priority computed from them.
//!
//! The parallel executor starts ready targets in order of their *bottom
//! level*: the target's duration plus the longest chain of dependents above
//! it. Any order that never leaves a slot idle while work is ready keeps
//! Graham's bound (`greedy_makespan_bound` in
//! `lean_make/LeanMake/Scheduling.lean`). This order also starts the
//! critical path first.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::Path;

pub const LOG_FILENAME: &str = ".makeyd_log";
const HEADER: &str = "# makeyd duration log v1: <microseconds>\t<target>";

#[derive(Debug, Default, Clone)]
pub struct DurationLog {
    pub durations: HashMap<String, u64>,
}

impl DurationLog {
    /// Missing or unreadable logs are empty: history only orders work.
    pub fn load<P: AsRef<Path>>(path: P) -> Self {
        let mut durations = HashMap::new();
        if let Ok(text) = fs::read_to_string(path) {
            for line in text.lines() {
                if line.starts_with('#') {
                    continue;
                }
                if let Some((us, target)) = line.split_once('\t') {
                    if let Ok(us) = us.parse::<u64>() {
                        durations.insert(target.to_string(), us);
                    }
                }
            }
        }
        Self { durations }
    }

    /// Record durations measured in this run, keeping older entries.
    pub fn merge(&mut self, measured: impl IntoIterator<Item = (String, u64)>) {
        for (target, us) in measured {
            self.durations.insert(target, us);
        }
    }

    pub fn save<P: AsRef<Path>>(&self, path: P) -> io::Result<()> {
        let mut entries: Vec<_> = self.durations.iter().collect();
        entries.sort();
        let mut out = String::with_capacity(entries.len() * 24 + HEADER.len() + 1);
        out.push_str(HEADER);
        out.push('\n');
        for (target, us) in entries {
            out.push_str(&format!("{us}\t{target}\n"));
        }
        let tmp = format!("{}.tmp", path.as_ref().display());
        fs::File::create(&tmp)?.write_all(out.as_bytes())?;
        fs::rename(tmp, path)
    }

    /// Estimated duration: recorded, else the mean of recorded ones, else 1
    /// (so that, with no history, priority is the hop count to the top).
    fn estimate(&self, target: &str, fallback: u64) -> u64 {
        self.durations
            .get(target)
            .copied()
            .unwrap_or(fallback)
            .max(1)
    }
}

/// Bottom level of every node in `nodes`: its estimated duration plus the
/// largest bottom level among its dependents. Iterative (Kahn order on the
/// reversed graph), so deep graphs cannot overflow the stack. Nodes left on a
/// cycle keep only their own duration.
pub fn bottom_levels(
    nodes: &HashSet<String>,
    dependents: &HashMap<String, Vec<String>>,
    log: &DurationLog,
) -> HashMap<String, u64> {
    let known: Vec<u64> = nodes
        .iter()
        .filter_map(|n| log.durations.get(n).copied())
        .collect();
    let fallback = if known.is_empty() {
        1
    } else {
        known.iter().sum::<u64>() / known.len() as u64
    };

    // Out-degree towards dependents; nodes with none are processed first.
    let mut pending: HashMap<&str, usize> = HashMap::new();
    for n in nodes {
        let outs = dependents
            .get(n)
            .map_or(0, |ds| ds.iter().filter(|d| nodes.contains(*d)).count());
        pending.insert(n.as_str(), outs);
    }
    let mut prereqs_of: HashMap<&str, Vec<&str>> = HashMap::new();
    for n in nodes {
        if let Some(ds) = dependents.get(n) {
            for d in ds {
                if nodes.contains(d) {
                    prereqs_of.entry(d.as_str()).or_default().push(n.as_str());
                }
            }
        }
    }

    let mut level: HashMap<String, u64> = HashMap::new();
    let mut stack: Vec<&str> = pending
        .iter()
        .filter(|(_, c)| **c == 0)
        .map(|(n, _)| *n)
        .collect();
    while let Some(n) = stack.pop() {
        let above = dependents.get(n).map_or(0, |ds| {
            ds.iter()
                .filter_map(|d| level.get(d).copied())
                .max()
                .unwrap_or(0)
        });
        level.insert(n.to_string(), log.estimate(n, fallback) + above);
        if let Some(ps) = prereqs_of.get(n) {
            for p in ps {
                if let Some(c) = pending.get_mut(p) {
                    *c -= 1;
                    if *c == 0 {
                        stack.push(p);
                    }
                }
            }
        }
    }
    for n in nodes {
        level
            .entry(n.clone())
            .or_insert_with(|| log.estimate(n, fallback));
    }
    level
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(edges: &[(&str, &str)]) -> (HashSet<String>, HashMap<String, Vec<String>>) {
        // (prereq, dependent)
        let mut nodes = HashSet::new();
        let mut deps: HashMap<String, Vec<String>> = HashMap::new();
        for (p, d) in edges {
            nodes.insert(p.to_string());
            nodes.insert(d.to_string());
            deps.entry(p.to_string()).or_default().push(d.to_string());
        }
        (nodes, deps)
    }

    #[test]
    fn test_bottom_levels_hops_without_history() {
        let (nodes, deps) = graph(&[("c1", "c2"), ("c2", "c3"), ("c3", "all"), ("s1", "all")]);
        let bl = bottom_levels(&nodes, &deps, &DurationLog::default());
        assert_eq!(bl["all"], 1);
        assert_eq!(bl["c3"], 2);
        assert_eq!(bl["c1"], 4);
        assert_eq!(bl["s1"], 2);
    }

    #[test]
    fn test_bottom_levels_use_recorded_durations() {
        let (nodes, deps) = graph(&[("slow", "all"), ("fast", "mid"), ("mid", "all")]);
        let mut log = DurationLog::default();
        log.merge([
            ("slow".to_string(), 900),
            ("fast".to_string(), 10),
            ("mid".to_string(), 10),
            ("all".to_string(), 1),
        ]);
        let bl = bottom_levels(&nodes, &deps, &log);
        assert!(bl["slow"] > bl["fast"], "{bl:?}");
        assert_eq!(bl["fast"], 21);
    }

    #[test]
    fn test_log_roundtrip() {
        let path = std::env::temp_dir().join(format!("makeyd_log_{}", std::process::id()));
        let mut log = DurationLog::default();
        log.merge([("a b".to_string(), 42), ("c".to_string(), 7)]);
        log.save(&path).unwrap();
        let back = DurationLog::load(&path);
        assert_eq!(back.durations, log.durations);
        let _ = fs::remove_file(path);
    }
}
