use std::fs::{self, File};
use std::io::Write;
use std::process::Command;

fn get_maked_bin() -> String {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.push("maked");
    path.to_str().unwrap().to_string()
}

#[test]
fn test_chrome_trace_perfetto_export_and_critical_path() {
    let maked = get_maked_bin();
    let temp_dir = std::env::temp_dir().join(format!("test_trace_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    let trace_path = temp_dir.join("build.trace.json");

    let mf_path = temp_dir.join("Makefile");
    let mut mf = File::create(&mf_path).unwrap();
    // Diamond DAG: all depends on b and c; b depends on a; c depends on a.
    // b sleeps longer so critical path is a -> b -> all.
    writeln!(mf, "all: b c").unwrap();
    writeln!(mf, "\t@echo \"FINISH all\"").unwrap();
    writeln!(mf, "b: a").unwrap();
    writeln!(mf, "\t@sleep 0.05").unwrap();
    writeln!(mf, "\t@echo \"BUILT b\"").unwrap();
    writeln!(mf, "c: a").unwrap();
    writeln!(mf, "\t@echo \"BUILT c\"").unwrap();
    writeln!(mf, "a:").unwrap();
    writeln!(mf, "\t@echo \"BUILT a\"").unwrap();
    drop(mf);

    let output = Command::new(&maked)
        .arg("-C")
        .arg(&temp_dir)
        .arg("-j4")
        .arg("--profile")
        .arg(format!("--trace={}", trace_path.display()))
        .output()
        .expect("failed to run maked");

    assert!(
        output.status.success(),
        "maked failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Critical path:"),
        "stdout missing critical path: {stdout}"
    );
    assert!(
        stdout.contains("Perfetto trace log:"),
        "stdout missing trace log: {stdout}"
    );

    assert!(trace_path.exists(), "trace file was not generated");
    let trace_content = fs::read_to_string(&trace_path).unwrap();

    // Verify Chrome Trace Event schema
    assert!(trace_content.contains("\"displayTimeUnit\": \"ms\""));
    assert!(trace_content.contains("\"traceEvents\": ["));
    assert!(trace_content.contains("\"name\": \"a\""));
    assert!(trace_content.contains("\"name\": \"b\""));
    assert!(trace_content.contains("\"name\": \"c\""));
    assert!(trace_content.contains("\"name\": \"all\""));
    assert!(trace_content.contains("\"ph\": \"X\""));
    assert!(trace_content.contains("\"ph\": \"M\""));
    assert!(trace_content.contains("Worker"));

    let _ = fs::remove_dir_all(&temp_dir);
}
