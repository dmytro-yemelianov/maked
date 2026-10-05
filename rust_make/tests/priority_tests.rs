use std::collections::HashMap;
use std::fs;
use std::process::Command;
use std::time::{Duration, Instant};

/// With two slots, a 3 x 0.3 s chain and six 0.15 s independent jobs fit in
/// 0.9 s only if the chain head starts first. Running the short jobs first
/// costs about 1.35 s. The ready queue must prefer the longest remaining
/// path, even before any duration history exists (hop count).
#[test]
fn test_ready_queue_prefers_longest_remaining_path() {
    let dir = std::env::temp_dir().join(format!("maked_priority_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let mut mf = String::from(".PHONY: all\nall: c3 s1 s2 s3 s4 s5 s6\n");
    mf.push_str("c1:\n\t@sleep 0.3 && touch $@\n");
    mf.push_str("c2: c1\n\t@sleep 0.3 && touch $@\n");
    mf.push_str("c3: c2\n\t@sleep 0.3 && touch $@\n");
    for i in 1..=6 {
        mf.push_str(&format!("s{i}:\n\t@sleep 0.15 && touch $@\n"));
    }
    fs::write(dir.join("Makefile"), mf).unwrap();

    let bin = env!("CARGO_BIN_EXE_maked");
    let trace = dir.join("trace.json");
    for round in 0..3 {
        for t in ["c1", "c2", "c3", "s1", "s2", "s3", "s4", "s5", "s6"] {
            let _ = fs::remove_file(dir.join(t));
        }
        let start = Instant::now();
        let out = Command::new(bin)
            .arg("-C")
            .arg(&dir)
            .arg("-j2")
            .arg(format!("--trace={}", trace.display()))
            .arg("all")
            .output()
            .unwrap();
        let elapsed = start.elapsed();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

        // Order, from the trace: c1 is in the first batch, and each chain
        // link starts as soon as its prerequisite finishes.
        let events = rule_events(&fs::read_to_string(&trace).unwrap());
        let t0 = events.values().map(|e| e.0).min().unwrap();
        let (c1, c2, c3) = (events["c1"], events["c2"], events["c3"]);
        assert!(
            c1.0 - t0 < 20_000,
            "round {round}: c1 started {}us late",
            c1.0 - t0
        );
        assert!(
            c2.0 - (c1.0 + c1.1) < 30_000,
            "round {round}: c2 waited behind short jobs"
        );
        assert!(
            c3.0 - (c2.0 + c2.1) < 30_000,
            "round {round}: c3 waited behind short jobs"
        );
        // Loose wall-clock guard: shorts-first costs about 1.45 s here.
        assert!(
            elapsed < Duration::from_millis(1350),
            "round {round}: {elapsed:?}"
        );
    }
    assert!(dir.join(".maked_log").exists(), "duration log not written");
    let _ = fs::remove_dir_all(&dir);
}

/// name -> (start us, duration us) for every "rule" event in a trace file.
fn rule_events(json: &str) -> HashMap<String, (u64, u64)> {
    let mut out = HashMap::new();
    for obj in json.split('{').skip(1) {
        if !obj.contains("\"cat\": \"rule\"") {
            continue;
        }
        let field = |key: &str| -> Option<String> {
            let at = obj.find(&format!("\"{key}\":"))? + key.len() + 3;
            let rest = obj[at..].trim_start();
            let end = rest.find([',', '}', '\n']).unwrap_or(rest.len());
            Some(rest[..end].trim().trim_matches('"').to_string())
        };
        if let (Some(name), Some(ts), Some(dur)) = (field("name"), field("ts"), field("dur")) {
            out.insert(name, (ts.parse().unwrap_or(0), dur.parse().unwrap_or(0)));
        }
    }
    out
}
