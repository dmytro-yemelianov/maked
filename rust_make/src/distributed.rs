//! Remote build workers (`--worker-listen`, `--remote-workers`).
//!
//! A worker runs recipe commands that a coordinator sends it, so the
//! protocol is built around not letting anyone else do that:
//!
//! - **Authentication.** Both sides hold a shared token, read from the file
//!   named by `MAKEYD_WORKER_TOKEN_FILE` or from `MAKEYD_WORKER_TOKEN`, never
//!   from argv, where `ps` would show it. On connect the worker sends a fresh
//!   random nonce. The request carries HMAC-SHA256(token, "req" ‖ nonce ‖
//!   body), and the response carries HMAC-SHA256(token, "resp" ‖ nonce ‖
//!   body). A request without the token is rejected before anything runs,
//!   and a recorded request cannot be replayed against another nonce.
//! - **Loopback by default.** `--worker-listen` refuses non-loopback
//!   addresses unless `--worker-allow-remote` is given.
//! - **Paths.** Input files and the target must be relative paths without
//!   `..`. Each connection gets its own sandbox directory. The coordinator
//!   accepts only the target file back.
//! - **Limits.** Requests and responses are capped at `MAX_MESSAGE_BYTES`,
//!   and a worker drops a client that does not send its request within
//!   `REQUEST_TIMEOUT`.
//!
//! Traffic is authenticated but **not encrypted**. Sources and outputs cross
//! the network in clear text, so use an SSH tunnel or a VPN between hosts.

use crate::hash::{Sha256, to_hex};
use std::fs;
use std::io::{BufRead, BufReader, Cursor, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const MAGIC_PROTOCOL: &str = "MAKEYD_DIST_V2";
pub const MAX_MESSAGE_BYTES: usize = 256 * 1024 * 1024;
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
pub const MIN_TOKEN_BYTES: usize = 16;
pub const TOKEN_ENV: &str = "MAKEYD_WORKER_TOKEN";
pub const TOKEN_FILE_ENV: &str = "MAKEYD_WORKER_TOKEN_FILE";

#[derive(Debug, Clone)]
pub struct RemoteBuildResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub output_files: Vec<(String, Vec<u8>)>,
}

/// The shared secret both sides authenticate with.
#[derive(Clone)]
pub struct WorkerAuth {
    token: Vec<u8>,
}

impl std::fmt::Debug for WorkerAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WorkerAuth(<redacted>)")
    }
}

impl WorkerAuth {
    pub fn new(token: &[u8]) -> Result<Self, String> {
        if token.len() < MIN_TOKEN_BYTES {
            return Err(format!(
                "worker token must be at least {MIN_TOKEN_BYTES} bytes"
            ));
        }
        Ok(Self {
            token: token.to_vec(),
        })
    }

    /// Token from `MAKEYD_WORKER_TOKEN_FILE` (preferred) or `MAKEYD_WORKER_TOKEN`.
    pub fn from_env() -> Result<Self, String> {
        if let Ok(path) = std::env::var(TOKEN_FILE_ENV) {
            let text = fs::read_to_string(&path)
                .map_err(|e| format!("cannot read {TOKEN_FILE_ENV}={path}: {e}"))?;
            return Self::new(text.trim().as_bytes());
        }
        if let Ok(tok) = std::env::var(TOKEN_ENV) {
            return Self::new(tok.trim().as_bytes());
        }
        Err(format!(
            "remote workers need a shared token: set {TOKEN_FILE_ENV} to a file holding it, \
             or {TOKEN_ENV} (at least {MIN_TOKEN_BYTES} bytes)"
        ))
    }

    fn mac(&self, label: &[u8], nonce: &[u8], body: &[u8]) -> [u8; 32] {
        let mut msg = Vec::with_capacity(label.len() + nonce.len() + 32);
        msg.extend_from_slice(label);
        msg.extend_from_slice(nonce);
        msg.extend_from_slice(&crate::hash::sha256_bytes(body));
        hmac_sha256(&self.token, &msg)
    }
}

/// HMAC-SHA256 (RFC 2104).
pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&crate::hash::sha256_bytes(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(&k.map(|b| b ^ 0x36));
    inner.update(msg);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(&k.map(|b| b ^ 0x5c));
    outer.update(&inner);
    outer.finalize()
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

fn random_nonce() -> std::io::Result<[u8; 32]> {
    let mut buf = [0u8; 32];
    fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
    Ok(buf)
}

/// A relative path with only normal components (no `..`, root or prefix).
pub fn safe_relative_path(name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.contains('\0') {
        return None;
    }
    let p = Path::new(name);
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

fn invalid(msg: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, msg.into())
}

/// Refuse non-loopback listen addresses unless explicitly allowed.
pub fn check_listen_addr(
    listen_addr: &str,
    allow_remote: bool,
) -> std::io::Result<Vec<SocketAddr>> {
    let addrs: Vec<SocketAddr> = listen_addr.to_socket_addrs()?.collect();
    if addrs.is_empty() {
        return Err(invalid(format!("cannot resolve {listen_addr}")));
    }
    if !allow_remote && addrs.iter().any(|a| !a.ip().is_loopback()) {
        return Err(invalid(format!(
            "refusing to listen on non-loopback address {listen_addr}: a worker runs commands \
             it is sent. Pass --worker-allow-remote to accept connections from other hosts \
             (traffic is authenticated but not encrypted)"
        )));
    }
    Ok(addrs)
}

/// Runs a persistent remote build worker daemon listening on `listen_addr`.
pub fn run_worker_daemon(
    listen_addr: &str,
    auth: WorkerAuth,
    allow_remote: bool,
) -> std::io::Result<()> {
    let addrs = check_listen_addr(listen_addr, allow_remote)?;
    let listener = TcpListener::bind(&addrs[..])?;
    println!(
        "makeyd worker daemon listening on {} (token required)",
        listener.local_addr()?
    );
    let auth = Arc::new(auth);
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let auth = Arc::clone(&auth);
                std::thread::spawn(move || {
                    if let Err(e) = handle_worker_connection(stream, &auth) {
                        eprintln!("makeyd worker: rejected connection: {e}");
                    }
                });
            }
            Err(e) => eprintln!("makeyd worker connection accept error: {e}"),
        }
    }
    Ok(())
}

/// Read `<TAG> <hex mac> <len>\n` then `len` bytes, verifying the MAC.
fn read_signed<R: BufRead>(
    reader: &mut R,
    tag: &str,
    auth: &WorkerAuth,
    label: &[u8],
    nonce: &[u8],
) -> std::io::Result<Vec<u8>> {
    let mut line = String::new();
    reader.by_ref().take(256).read_line(&mut line)?;
    let parts: Vec<&str> = line.split_whitespace().collect();
    let [t, mac_hex, len] = parts[..] else {
        return Err(invalid(format!("malformed {tag} header")));
    };
    if t != tag {
        return Err(invalid(format!("expected {tag}")));
    }
    let len: usize = len.parse().map_err(|_| invalid("bad length"))?;
    if len > MAX_MESSAGE_BYTES {
        return Err(invalid(format!("message of {len} bytes exceeds the limit")));
    }
    let mac = from_hex(mac_hex).ok_or_else(|| invalid("bad MAC encoding"))?;
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    if !ct_eq(&mac, &auth.mac(label, nonce, &body)) {
        return Err(invalid("authentication failed"));
    }
    Ok(body)
}

fn write_signed<W: Write>(
    w: &mut W,
    tag: &str,
    auth: &WorkerAuth,
    label: &[u8],
    nonce: &[u8],
    body: &[u8],
) -> std::io::Result<()> {
    let mac = auth.mac(label, nonce, body);
    writeln!(w, "{tag} {} {}", to_hex(&mac), body.len())?;
    w.write_all(body)?;
    w.flush()
}

fn read_line_limited<R: BufRead>(r: &mut R) -> std::io::Result<String> {
    let mut line = String::new();
    r.by_ref().take(64 * 1024).read_line(&mut line)?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

fn read_count<R: BufRead>(r: &mut R) -> std::io::Result<usize> {
    read_line_limited(r)?
        .trim()
        .parse()
        .map_err(|_| invalid("bad count"))
}

fn read_blob<R: BufRead>(r: &mut R) -> std::io::Result<Vec<u8>> {
    let len = read_count(r)?;
    if len > MAX_MESSAGE_BYTES {
        return Err(invalid("blob too large"));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

static SANDBOX_SEQ: AtomicU64 = AtomicU64::new(0);

fn handle_worker_connection(mut stream: TcpStream, auth: &WorkerAuth) -> std::io::Result<()> {
    stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    let nonce = random_nonce()?;
    writeln!(stream, "{MAGIC_PROTOCOL} {}", to_hex(&nonce))?;
    stream.flush()?;

    let mut reader = BufReader::new(stream.try_clone()?);
    let body = read_signed(&mut reader, "AUTH", auth, b"req", &nonce)?;
    stream.set_read_timeout(None)?;

    // Parse the authenticated request.
    let mut r = Cursor::new(body);
    let target = read_line_limited(&mut r)?;
    let target_rel = safe_relative_path(&target).ok_or_else(|| invalid("unsafe target path"))?;
    let cmd_count = read_count(&mut r)?;
    let mut commands = Vec::with_capacity(cmd_count.min(1024));
    for _ in 0..cmd_count {
        commands.push(read_line_limited(&mut r)?);
    }
    let input_count = read_count(&mut r)?;
    let mut inputs = Vec::with_capacity(input_count.min(1024));
    for _ in 0..input_count {
        let name = read_line_limited(&mut r)?;
        let rel = safe_relative_path(&name).ok_or_else(|| invalid("unsafe input path"))?;
        inputs.push((rel, read_blob(&mut r)?));
    }

    // A fresh sandbox per connection; create_dir fails rather than reuse one.
    let seq = SANDBOX_SEQ.fetch_add(1, Ordering::Relaxed);
    let sandbox_dir = std::env::temp_dir().join(format!(
        "makeyd_worker_{}_{}_{}",
        std::process::id(),
        seq,
        &to_hex(&nonce)[..12]
    ));
    fs::create_dir(&sandbox_dir)?;
    let result = (|| -> std::io::Result<Vec<u8>> {
        for (rel, bytes) in &inputs {
            let dest = sandbox_dir.join(rel);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(dest, bytes)?;
        }
        if let Some(parent) = sandbox_dir.join(&target_rel).parent() {
            fs::create_dir_all(parent)?;
        }

        let mut exit_code = 0i32;
        let mut out = String::new();
        let mut err = String::new();
        for cmd in &commands {
            match std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(cmd)
                .current_dir(&sandbox_dir)
                .output()
            {
                Ok(o) => {
                    out.push_str(&String::from_utf8_lossy(&o.stdout));
                    err.push_str(&String::from_utf8_lossy(&o.stderr));
                    if !o.status.success() {
                        exit_code = o.status.code().unwrap_or(1);
                        break;
                    }
                }
                Err(e) => {
                    err.push_str(&format!("Spawn failed: {e}\n"));
                    exit_code = 127;
                    break;
                }
            }
        }

        let mut outputs = Vec::new();
        let target_path = sandbox_dir.join(&target_rel);
        if exit_code == 0 && target_path.is_file() {
            outputs.push((target.clone(), fs::read(&target_path)?));
        }

        let mut resp = Vec::new();
        writeln!(resp, "{exit_code}")?;
        writeln!(resp, "{}", outputs.len())?;
        for (name, bytes) in &outputs {
            writeln!(resp, "{name}")?;
            writeln!(resp, "{}", bytes.len())?;
            resp.extend_from_slice(bytes);
        }
        writeln!(resp, "{}", out.len())?;
        resp.extend_from_slice(out.as_bytes());
        writeln!(resp, "{}", err.len())?;
        resp.extend_from_slice(err.as_bytes());
        Ok(resp)
    })();
    let _ = fs::remove_dir_all(&sandbox_dir);
    let resp = result?;
    if resp.len() > MAX_MESSAGE_BYTES {
        return Err(invalid("response too large"));
    }
    write_signed(&mut stream, "RESP", auth, b"resp", &nonce, &resp)
}

/// Decode an authenticated response. Only the target itself may come back.
fn parse_response(body: Vec<u8>, target: &str) -> Result<RemoteBuildResult, String> {
    let mut r = Cursor::new(body);
    let e = |x: std::io::Error| x.to_string();
    let exit_code: i32 = read_line_limited(&mut r)
        .map_err(e)?
        .trim()
        .parse()
        .map_err(|_| "bad exit code".to_string())?;
    let out_count = read_count(&mut r).map_err(e)?;
    let mut output_files = Vec::new();
    for _ in 0..out_count {
        let name = read_line_limited(&mut r).map_err(e)?;
        let bytes = read_blob(&mut r).map_err(e)?;
        if name != target || safe_relative_path(&name).is_none() {
            return Err(format!("worker returned unexpected file {name:?}"));
        }
        output_files.push((name, bytes));
    }
    let stdout = String::from_utf8_lossy(&read_blob(&mut r).map_err(e)?).to_string();
    let stderr = String::from_utf8_lossy(&read_blob(&mut r).map_err(e)?).to_string();
    Ok(RemoteBuildResult {
        exit_code,
        stdout,
        stderr,
        output_files,
    })
}

/// Executes a single target's recipe on a remote worker.
pub fn dispatch_remote_build(
    worker_addr: &str,
    auth: &WorkerAuth,
    target: &str,
    commands: &[String],
    input_files: &[PathBuf],
) -> Result<RemoteBuildResult, String> {
    // Targets or inputs outside the build directory cannot be mirrored into
    // a sandbox; build those locally.
    if safe_relative_path(target).is_none() {
        return Err(format!("target {target:?} is not a relative path"));
    }
    let mut req = Vec::new();
    let w = |e: std::io::Error| e.to_string();
    writeln!(req, "{target}").map_err(w)?;
    writeln!(req, "{}", commands.len()).map_err(w)?;
    for cmd in commands {
        if cmd.contains('\n') {
            return Err("multi-line command cannot be sent".to_string());
        }
        writeln!(req, "{cmd}").map_err(w)?;
    }
    let mut inputs = Vec::new();
    for p in input_files {
        if p.is_file() {
            let name = p.to_string_lossy().to_string();
            if safe_relative_path(&name).is_none() {
                return Err(format!("input {name:?} is not a relative path"));
            }
            inputs.push((name, fs::read(p).map_err(w)?));
        }
    }
    writeln!(req, "{}", inputs.len()).map_err(w)?;
    for (name, bytes) in &inputs {
        writeln!(req, "{name}").map_err(w)?;
        writeln!(req, "{}", bytes.len()).map_err(w)?;
        req.extend_from_slice(bytes);
    }
    if req.len() > MAX_MESSAGE_BYTES {
        return Err("request too large for a remote worker".to_string());
    }

    let stream = TcpStream::connect(worker_addr)
        .map_err(|e| format!("Failed to connect to remote worker {worker_addr}: {e}"))?;
    let mut reader = BufReader::new(stream.try_clone().map_err(w)?);
    let mut writer = stream;

    let greeting = read_line_limited(&mut reader).map_err(w)?;
    let nonce = greeting
        .strip_prefix(&format!("{MAGIC_PROTOCOL} "))
        .and_then(from_hex)
        .filter(|n| n.len() == 32)
        .ok_or_else(|| format!("worker {worker_addr} does not speak {MAGIC_PROTOCOL}"))?;

    write_signed(&mut writer, "AUTH", auth, b"req", &nonce, &req).map_err(w)?;
    let body = read_signed(&mut reader, "RESP", auth, b"resp", &nonce)
        .map_err(|e| format!("worker {worker_addr}: {e}"))?;
    parse_response(body, target)
}

/// Thread-safe pool of remote workers with round-robin dispatch
#[derive(Debug, Clone)]
pub struct RemoteWorkerPool {
    pub workers: Arc<Vec<String>>,
    pub index: Arc<Mutex<usize>>,
    pub auth: Option<Arc<WorkerAuth>>,
}

impl RemoteWorkerPool {
    pub fn new(workers: Vec<String>, auth: Option<WorkerAuth>) -> Self {
        Self {
            workers: Arc::new(workers),
            index: Arc::new(Mutex::new(0)),
            auth: auth.map(Arc::new),
        }
    }

    /// Next worker and the token to talk to it; `None` without either.
    pub fn acquire_worker(&self) -> Option<(String, Arc<WorkerAuth>)> {
        let auth = self.auth.clone()?;
        if self.workers.is_empty() {
            return None;
        }
        let mut idx = self.index.lock().unwrap();
        let selected = self.workers[*idx % self.workers.len()].clone();
        *idx += 1;
        Some((selected, auth))
    }

    pub fn is_empty(&self) -> bool {
        self.workers.is_empty() || self.auth.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hmac_sha256_rfc4231_case_2() {
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            to_hex(&mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn test_hmac_sha256_long_key_rfc4231_case_6() {
        let key = [0xaau8; 131];
        let mac = hmac_sha256(
            &key,
            b"Test Using Larger Than Block-Size Key - Hash Key First",
        );
        assert_eq!(
            to_hex(&mac),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn test_safe_relative_path() {
        assert!(safe_relative_path("src/a.c").is_some());
        assert!(safe_relative_path("./a.o").is_some());
        for bad in ["", "/etc/passwd", "../x", "a/../../x", "a/..", "."] {
            assert!(safe_relative_path(bad).is_none(), "{bad:?} accepted");
        }
    }

    #[test]
    fn test_token_minimum_length() {
        assert!(WorkerAuth::new(b"short").is_err());
        assert!(WorkerAuth::new(b"0123456789abcdef").is_ok());
    }

    #[test]
    fn test_non_loopback_listen_refused_by_default() {
        assert!(check_listen_addr("0.0.0.0:0", false).is_err());
        assert!(check_listen_addr("0.0.0.0:0", true).is_ok());
        assert!(check_listen_addr("127.0.0.1:0", false).is_ok());
    }

    #[test]
    fn test_response_may_only_return_the_target() {
        let mut body = Vec::new();
        writeln!(body, "0\n1\n../../.bashrc\n3").unwrap();
        body.extend_from_slice(b"pwn");
        writeln!(body, "0\n0").unwrap();
        assert!(parse_response(body, "out.o").is_err());

        let mut ok = Vec::new();
        writeln!(ok, "0\n1\nout.o\n2").unwrap();
        ok.extend_from_slice(b"ok");
        writeln!(ok, "0\n0").unwrap();
        let r = parse_response(ok, "out.o").unwrap();
        assert_eq!(r.output_files, vec![("out.o".to_string(), b"ok".to_vec())]);
    }
}
