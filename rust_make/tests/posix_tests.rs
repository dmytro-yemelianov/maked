use maked::executor::{ExecutionConfig, Executor};
use maked::graph::DependencyGraph;
use maked::parser::parse_makefile_content;
use std::fs;
use std::process::Command;

fn make_config(jobs: usize) -> ExecutionConfig {
    ExecutionConfig {
        keep_going: false,
        jobs,
        dry_run: false,
        always_make: false,
        silent: false,
        question: false,
        use_hash: false,
        ignore_errors: false,
        touch_only: false,
        profile: false,
        trace_file: None,
        ..Default::default()
    }
}

#[test]
fn test_posix_dry_run_flag() {
    let makefile_content = "
target_dry:
\ttouch target_dry.txt
";
    let mf = parse_makefile_content(makefile_content, &[]).expect("parse");
    let graph = DependencyGraph::from_makefile(&mf);
    let mut config = make_config(1);
    config.dry_run = true;

    let executor = Executor::new(&mf, &graph, config);
    let stats = executor.execute("target_dry").expect("execute");
    assert_eq!(stats.targets_rebuilt, 1);
    assert_eq!(stats.commands_executed, 1);
    // target file should NOT exist on disk because of dry-run
    assert!(!std::path::Path::new("target_dry.txt").exists());
}

#[test]
fn test_posix_always_make_flag() {
    let temp_dir = std::env::temp_dir().join("maked_posix_always_make");
    let _ = fs::create_dir_all(&temp_dir);
    let tgt_file = temp_dir.join("existing_target.o");
    fs::write(&tgt_file, "binary").expect("write target");

    let makefile_content = format!(
        "
{}:
\t@echo Rebuilding unconditionally
",
        tgt_file.display()
    );

    let mf = parse_makefile_content(&makefile_content, &[]).expect("parse");
    let graph = DependencyGraph::from_makefile(&mf);

    // Normal execution without -B should find it UpToDate
    let mut config_normal = make_config(1);
    config_normal.silent = true;
    let exec_normal = Executor::new(&mf, &graph, config_normal);
    let stats_normal = exec_normal
        .execute(tgt_file.to_str().unwrap())
        .expect("exec");
    assert_eq!(stats_normal.targets_up_to_date, 1);
    assert_eq!(stats_normal.targets_rebuilt, 0);

    // Execution with -B / always_make should force rebuild
    let mut config_b = make_config(1);
    config_b.always_make = true;
    config_b.silent = true;
    let exec_b = Executor::new(&mf, &graph, config_b);
    let stats_b = exec_b.execute(tgt_file.to_str().unwrap()).expect("exec");
    assert_eq!(stats_b.targets_rebuilt, 1);

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_posix_question_mode() {
    let temp_dir = std::env::temp_dir().join("maked_posix_question");
    let _ = fs::create_dir_all(&temp_dir);
    let tgt_file = temp_dir.join("q_target.o");

    let makefile_content = format!(
        "
{}:
\ttouch {}
",
        tgt_file.display(),
        tgt_file.display()
    );

    let mf = parse_makefile_content(&makefile_content, &[]).expect("parse");
    let graph = DependencyGraph::from_makefile(&mf);

    // Target missing on disk: question mode should detect it needs rebuilding
    let mut config_q = make_config(1);
    config_q.question = true;
    let exec_q = Executor::new(&mf, &graph, config_q);
    let stats_q = exec_q.execute(tgt_file.to_str().unwrap()).expect("exec");
    assert_eq!(stats_q.targets_rebuilt, 1);

    // Now create the file
    fs::write(&tgt_file, "data").expect("write");
    let mut config_fresh = make_config(1);
    config_fresh.question = true;
    let exec_fresh = Executor::new(&mf, &graph, config_fresh);
    let stats_fresh = exec_fresh
        .execute(tgt_file.to_str().unwrap())
        .expect("exec");
    assert_eq!(stats_fresh.targets_rebuilt, 0);
    assert_eq!(stats_fresh.targets_up_to_date, 1);

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_posix_touch_mode() {
    let temp_dir = std::env::temp_dir().join("maked_posix_touch");
    let _ = fs::create_dir_all(&temp_dir);
    let tgt_file = temp_dir.join("touched_target.o");

    let makefile_content = format!(
        "
{}:
\t@echo 'This command must NOT run under -t' && false
",
        tgt_file.display()
    );

    let mf = parse_makefile_content(&makefile_content, &[]).expect("parse");
    let graph = DependencyGraph::from_makefile(&mf);

    let mut config = make_config(1);
    config.touch_only = true;
    let executor = Executor::new(&mf, &graph, config);
    let stats = executor.execute(tgt_file.to_str().unwrap()).expect("exec");

    assert_eq!(stats.targets_rebuilt, 1);
    assert!(tgt_file.exists());

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_posix_ignore_errors() {
    let makefile_content = "
failing_target:
\t@false
\t@echo 'Continued after error'
";

    let mf = parse_makefile_content(makefile_content, &[]).expect("parse");
    let graph = DependencyGraph::from_makefile(&mf);

    // Normal execution should fail
    let mut config_fail = make_config(1);
    config_fail.silent = true;
    let exec_fail = Executor::new(&mf, &graph, config_fail);
    let res_fail = exec_fail.execute("failing_target");
    assert!(res_fail.is_err());

    // With -i / ignore_errors, it should succeed
    let mut config_i = make_config(1);
    config_i.silent = true;
    config_i.ignore_errors = true;
    let exec_i = Executor::new(&mf, &graph, config_i);
    let res_i = exec_i.execute("failing_target");
    assert!(res_i.is_ok());
}

#[test]
fn test_posix_prefix_hyphen_ignore_error() {
    let makefile_content = "
hyphen_target:
\t-false
\t@echo 'Success'
";

    let mf = parse_makefile_content(makefile_content, &[]).expect("parse");
    let graph = DependencyGraph::from_makefile(&mf);

    let mut config = make_config(1);
    config.silent = true;
    let exec = Executor::new(&mf, &graph, config);
    let res = exec.execute("hyphen_target");
    assert!(res.is_ok());
}

#[test]
fn test_posix_default_goal_selection() {
    let makefile_content = "
.SUFFIXES: .c .o .h

.DEFAULT:
\t@echo default

real_goal:
\t@echo 'This should be the default goal'

second_goal:
\t@echo second
";

    let mf = parse_makefile_content(makefile_content, &[]).expect("parse");
    // First non-dot target must be selected as default target
    assert_eq!(mf.default_target, Some("real_goal".to_string()));
}

#[test]
fn test_posix_cli_variable_precedence() {
    let makefile_content = "
CFLAGS = -O0
app:
\t@echo $(CFLAGS)
";
    // CLI variable override CFLAGS=-O3 must take strict precedence over Makefile assignment
    let cli = vec![("CFLAGS".to_string(), "-O3".to_string())];
    let mf = parse_makefile_content(makefile_content, &cli).expect("parse");
    assert_eq!(mf.get_var("CFLAGS"), Some("-O3".to_string()));
}

/// GNU make handles dependency chains thousands of targets deep; maked used
/// to overflow its stack around 12k and spend O(depth^2) memory before that.
#[test]
fn test_deep_dependency_chain() {
    const N: usize = 20_000;
    let temp_dir = std::env::temp_dir().join(format!("maked_deep_chain_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();
    let mut mf = String::from(".PHONY: all\n");
    mf.push_str(&format!("all: node_{}\n", N - 1));
    mf.push_str("node_0:\n\t@touch $@\n");
    for i in 1..N {
        mf.push_str(&format!("node_{i}: node_{}\n\t@touch $@\n", i - 1));
    }
    fs::write(temp_dir.join("Makefile"), mf).unwrap();

    let bin = env!("CARGO_BIN_EXE_maked");
    for jobs in ["-j1", "-j8"] {
        let out = Command::new(bin)
            .arg("-C")
            .arg(&temp_dir)
            .arg(jobs)
            .arg("-n")
            .arg("all")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{jobs}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let lines = String::from_utf8_lossy(&out.stdout).lines().count();
        assert_eq!(lines, N, "{jobs}: expected one touch per node");
    }
    let _ = fs::remove_dir_all(&temp_dir);
}
