use std::fs;
use std::net::TcpListener;
use std::process::Command;
use std::thread;

fn get_free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

#[test]
fn test_distributed_remote_worker_execution() {
    let port = get_free_port();
    let addr = format!("127.0.0.1:{port}");

    // 1. Spawn worker daemon in background thread
    let addr_clone = addr.clone();
    let _worker_thread = thread::spawn(move || {
        let _ = makeyd::distributed::run_worker_daemon(&addr_clone);
    });

    // Wait briefly for daemon listener to bind
    thread::sleep(std::time::Duration::from_millis(100));

    let temp_dir = std::env::temp_dir().join(format!("makeyd_test_dist_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    let makefile_content = r#"
all: final_output.txt

final_output.txt: input_a.txt input_b.txt
	cat input_a.txt input_b.txt > final_output.txt
"#;
    let makefile_path = temp_dir.join("Makefile");
    fs::write(&makefile_path, makefile_content).unwrap();

    fs::write(temp_dir.join("input_a.txt"), "DISTRIBUTED_").unwrap();
    fs::write(temp_dir.join("input_b.txt"), "BUILD_SUCCESS\n").unwrap();

    let makeyd_bin = env!("CARGO_BIN_EXE_makeyd");

    // 2. Run makeyd targeting remote worker
    let out = Command::new(makeyd_bin)
        .arg("-f")
        .arg(&makefile_path)
        .arg(format!("--remote-workers={addr}"))
        .current_dir(&temp_dir)
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "makeyd distributed build failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let output_file = temp_dir.join("final_output.txt");
    assert!(
        output_file.exists(),
        "Target file was not created by distributed execution"
    );
    let content = fs::read_to_string(&output_file).unwrap();
    assert_eq!(content, "DISTRIBUTED_BUILD_SUCCESS\n");

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_distributed_fallback_to_local_when_worker_unreachable() {
    let unused_port = get_free_port();
    let bad_addr = format!("127.0.0.1:{unused_port}");

    let temp_dir =
        std::env::temp_dir().join(format!("makeyd_test_dist_fallback_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    let makefile_content = r#"
all: fallback.txt

fallback.txt:
	echo "LOCAL_FALLBACK_OK" > fallback.txt
"#;
    let makefile_path = temp_dir.join("Makefile");
    fs::write(&makefile_path, makefile_content).unwrap();

    let makeyd_bin = env!("CARGO_BIN_EXE_makeyd");

    // Run with bad worker address: must transparently fall back to local thread
    let out = Command::new(makeyd_bin)
        .arg("-f")
        .arg(&makefile_path)
        .arg(format!("--remote-workers={bad_addr}"))
        .current_dir(&temp_dir)
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "makeyd failed fallback: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let output_file = temp_dir.join("fallback.txt");
    assert!(output_file.exists());
    let content = fs::read_to_string(&output_file).unwrap();
    assert!(content.contains("LOCAL_FALLBACK_OK"));

    let _ = fs::remove_dir_all(&temp_dir);
}
