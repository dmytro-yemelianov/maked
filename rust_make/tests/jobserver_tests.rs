use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::process::Command;

fn get_makeyd_bin() -> String {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // drop test binary name
    if path.ends_with("deps") {
        path.pop();
    }
    path.push("makeyd");
    path.to_str().unwrap().to_string()
}

#[test]
fn test_recursive_submake_jobserver_coordination() {
    let makeyd = get_makeyd_bin();
    let temp_dir = std::env::temp_dir().join(format!("test_jobserver_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    let sub1_dir = temp_dir.join("sub1");
    let sub2_dir = temp_dir.join("sub2");
    fs::create_dir_all(&sub1_dir).unwrap();
    fs::create_dir_all(&sub2_dir).unwrap();

    // sub1 Makefile
    let sub1_mf = sub1_dir.join("Makefile");
    let mut f = File::create(&sub1_mf).unwrap();
    writeln!(f, "all: target1 target2").unwrap();
    writeln!(f, "target1:\n\t@echo sub1_t1").unwrap();
    writeln!(f, "target2:\n\t@echo sub1_t2").unwrap();

    // sub2 Makefile
    let sub2_mf = sub2_dir.join("Makefile");
    let mut f = File::create(&sub2_mf).unwrap();
    writeln!(f, "all: target3 target4").unwrap();
    writeln!(f, "target3:\n\t@echo sub2_t3").unwrap();
    writeln!(f, "target4:\n\t@echo sub2_t4").unwrap();

    // Top-level Makefile invoking $(MAKE) -C
    let top_mf = temp_dir.join("Makefile");
    let mut f = File::create(&top_mf).unwrap();
    writeln!(f, "all: job1 job2").unwrap();
    writeln!(f, "job1:\n\t@\"{}\" -C sub1", makeyd).unwrap();
    writeln!(f, "job2:\n\t@\"{}\" -C sub2", makeyd).unwrap();

    let output = Command::new(&makeyd)
        .arg("-C")
        .arg(&temp_dir)
        .arg("-j4")
        .output()
        .expect("failed to run makeyd");

    assert!(
        output.status.success(),
        "makeyd failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("sub1_t1"),
        "missing sub1_t1 in output: {stdout}"
    );
    assert!(
        stdout.contains("sub1_t2"),
        "missing sub1_t2 in output: {stdout}"
    );
    assert!(
        stdout.contains("sub2_t3"),
        "missing sub2_t3 in output: {stdout}"
    );
    assert!(
        stdout.contains("sub2_t4"),
        "missing sub2_t4 in output: {stdout}"
    );

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_makeyd_under_gnu_make_jobserver() {
    let gmake = "/opt/homebrew/bin/gmake";
    if !Path::new(gmake).exists() {
        eprintln!("GNU Make not found at {gmake}, skipping test");
        return;
    }

    let makeyd = get_makeyd_bin();
    let temp_dir =
        std::env::temp_dir().join(format!("test_gmake_jobserver_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    let sub_dir = temp_dir.join("sub");
    fs::create_dir_all(&sub_dir).unwrap();

    // sub Makefile executed by makeyd
    let sub_mf = sub_dir.join("Makefile");
    let mut f = File::create(&sub_mf).unwrap();
    writeln!(f, "all: a b").unwrap();
    writeln!(f, "a:\n\t@echo makeyd_job_a").unwrap();
    writeln!(f, "b:\n\t@echo makeyd_job_b").unwrap();

    // Top-level Makefile executed by GNU Make with -j4
    let top_mf = temp_dir.join("Makefile");
    let mut f = File::create(&top_mf).unwrap();
    writeln!(f, "all:\n\t@\"{}\" -C sub", makeyd).unwrap();

    let output = Command::new(gmake)
        .arg("-C")
        .arg(&temp_dir)
        .arg("-j4")
        .output()
        .expect("failed to run gmake");

    assert!(
        output.status.success(),
        "gmake failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("makeyd_job_a"),
        "missing makeyd_job_a: {stdout}"
    );
    assert!(
        stdout.contains("makeyd_job_b"),
        "missing makeyd_job_b: {stdout}"
    );

    let _ = fs::remove_dir_all(&temp_dir);
}

/// A sub-make reached through `$(MAKE)` must run in parallel using the job
/// slots inherited via MAKEFLAGS, not fall back to -j1 (Lua's
/// `cd src && $(MAKE) macosx` regression).
#[test]
fn test_recursive_submake_inherits_parallelism() {
    let makeyd = get_makeyd_bin();
    let temp_dir = std::env::temp_dir().join(format!("makeyd_recursive_j_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    let sub = temp_dir.join("sub");
    fs::create_dir_all(&sub).unwrap();

    let mut f = File::create(sub.join("Makefile")).unwrap();
    writeln!(f, "all: a b c d").unwrap();
    for t in ["a", "b", "c", "d"] {
        writeln!(f, "{t}:\n\tsleep 0.4").unwrap();
    }
    let mut f = File::create(temp_dir.join("Makefile")).unwrap();
    writeln!(f, "all:\n\t$(MAKE) -C sub").unwrap();

    let start = std::time::Instant::now();
    let out = Command::new(&makeyd)
        .arg("-C")
        .arg(&temp_dir)
        .arg("-j4")
        .env_remove("MAKEFLAGS")
        .output()
        .expect("failed to run makeyd");
    let elapsed = start.elapsed();
    let _ = fs::remove_dir_all(&temp_dir);

    assert!(
        out.status.success(),
        "makeyd failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Serial would be 4 x 0.4s = 1.6s; four slots finish in ~0.4s.
    assert!(
        elapsed < std::time::Duration::from_millis(1100),
        "sub-make ran serially: {elapsed:?}"
    );
}
