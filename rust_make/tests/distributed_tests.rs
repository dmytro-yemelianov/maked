use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::thread;

const TOKEN: &str = "test-token-0123456789abcdef";

fn auth() -> makeyd::distributed::WorkerAuth {
    makeyd::distributed::WorkerAuth::new(TOKEN.as_bytes()).unwrap()
}

fn spawn_daemon() -> String {
    let port = get_free_port();
    let addr = format!("127.0.0.1:{port}");
    let a = addr.clone();
    thread::spawn(move || {
        let _ = makeyd::distributed::run_worker_daemon(&a, auth(), false);
    });
    thread::sleep(std::time::Duration::from_millis(100));
    addr
}

/// Send one hand-built, signed request; returns the worker's reply line.
fn raw_request(addr: &str, token: &str, body: &[u8]) -> String {
    let mut s = TcpStream::connect(addr).unwrap();
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut greeting = String::new();
    r.read_line(&mut greeting).unwrap();
    let nonce_hex = greeting.trim().strip_prefix("MAKEYD_DIST_V2 ").unwrap();
    let nonce: Vec<u8> = (0..nonce_hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&nonce_hex[i..i + 2], 16).unwrap())
        .collect();
    let mut msg = b"req".to_vec();
    msg.extend_from_slice(&nonce);
    msg.extend_from_slice(&makeyd::hash::sha256_bytes(body));
    let mac = makeyd::distributed::hmac_sha256(token.as_bytes(), &msg);
    writeln!(s, "AUTH {} {}", makeyd::hash::to_hex(&mac), body.len()).unwrap();
    s.write_all(body).unwrap();
    s.flush().unwrap();
    let mut reply = String::new();
    let _ = r.read_to_string(&mut reply);
    reply
}

fn get_free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

#[test]
fn test_distributed_remote_worker_execution() {
    // 1. Spawn worker daemon in background thread
    let addr = spawn_daemon();

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
        .env("MAKEYD_WORKER_TOKEN", TOKEN)
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
        .env("MAKEYD_WORKER_TOKEN", TOKEN)
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

#[test]
fn test_worker_runs_nothing_without_the_token() {
    let addr = spawn_daemon();
    let marker = std::env::temp_dir().join(format!("makeyd_marker_{}", std::process::id()));
    let _ = fs::remove_file(&marker);
    let body = format!("out\n1\ntouch {}\n0\n", marker.display());

    let reply = raw_request(&addr, "wrong-token-0123456789abcdef", body.as_bytes());
    thread::sleep(std::time::Duration::from_millis(200));
    assert!(
        !marker.exists(),
        "worker ran a command for a client with the wrong token"
    );
    assert!(
        reply.is_empty(),
        "worker answered an unauthenticated client: {reply:?}"
    );

    // Same request, right token: it runs (so the token is what stopped it).
    let reply = raw_request(&addr, TOKEN, body.as_bytes());
    assert!(reply.starts_with("RESP "), "{reply:?}");
    assert!(marker.exists(), "authenticated request did not run");
    let _ = fs::remove_file(&marker);
}

#[test]
fn test_worker_rejects_paths_outside_its_sandbox() {
    let addr = spawn_daemon();
    let escape = std::env::temp_dir().join(format!("escape_{}.txt", std::process::id()));
    let _ = fs::remove_file(&escape);
    let name = format!("../{}", escape.file_name().unwrap().to_string_lossy());
    let body = format!("out\n0\n1\n{name}\n4\npwn!");
    let reply = raw_request(&addr, TOKEN, body.as_bytes());
    assert!(reply.is_empty(), "traversing input accepted: {reply:?}");
    assert!(!escape.exists(), "worker wrote outside its sandbox");

    let reply = raw_request(&addr, TOKEN, b"/etc/passwd\n0\n0\n");
    assert!(reply.is_empty(), "absolute target accepted: {reply:?}");
}

#[test]
fn test_worker_and_coordinator_require_a_token_and_loopback() {
    let bin = env!("CARGO_BIN_EXE_makeyd");
    let out = Command::new(bin)
        .arg("--worker-listen=127.0.0.1:0")
        .env_remove("MAKEYD_WORKER_TOKEN")
        .env_remove("MAKEYD_WORKER_TOKEN_FILE")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "daemon started without a token");

    let out = Command::new(bin)
        .arg("--worker-listen=0.0.0.0:0")
        .env("MAKEYD_WORKER_TOKEN", TOKEN)
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "daemon listened on 0.0.0.0 without --worker-allow-remote"
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("non-loopback"));

    let dir = std::env::temp_dir().join(format!("makeyd_rw_notoken_{}", std::process::id()));
    let _ = fs::create_dir_all(&dir);
    fs::write(dir.join("Makefile"), "all:\n\t@true\n").unwrap();
    let out = Command::new(bin)
        .arg("-C")
        .arg(&dir)
        .arg("--remote-workers=127.0.0.1:9")
        .env_remove("MAKEYD_WORKER_TOKEN")
        .env_remove("MAKEYD_WORKER_TOKEN_FILE")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "coordinator ran remote workers without a token"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// The recipe must really run on the worker (in its sandbox), not in the
/// silent local fallback, which would also produce the file.
#[test]
fn test_recipe_runs_in_worker_sandbox() {
    let addr = spawn_daemon();
    let dir = std::env::temp_dir().join(format!("makeyd_where_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("Makefile"), "where.txt:\n\tpwd > where.txt\n").unwrap();
    let bin = env!("CARGO_BIN_EXE_makeyd");
    let out = Command::new(bin)
        .arg("-C")
        .arg(&dir)
        .arg(format!("--remote-workers={addr}"))
        .arg("where.txt")
        .env("MAKEYD_WORKER_TOKEN", TOKEN)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let where_ = fs::read_to_string(dir.join("where.txt")).unwrap();
    assert!(
        where_.contains("makeyd_worker_"),
        "ran locally, not on the worker: {where_}"
    );

    // With a token the worker does not share, the coordinator falls back to
    // a local build instead of trusting the worker.
    fs::remove_file(dir.join("where.txt")).unwrap();
    let out = Command::new(bin)
        .arg("-C")
        .arg(&dir)
        .arg(format!("--remote-workers={addr}"))
        .arg("where.txt")
        .env("MAKEYD_WORKER_TOKEN", "some-other-token-0123456789")
        .output()
        .unwrap();
    assert!(out.status.success());
    let where_ = fs::read_to_string(dir.join("where.txt")).unwrap();
    assert!(
        !where_.contains("makeyd_worker_"),
        "worker accepted a foreign token"
    );
    let _ = fs::remove_dir_all(&dir);
}
