use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub const MAGIC_PROTOCOL: &str = "MAKEYD_DIST_V1";

#[derive(Debug, Clone)]
pub struct RemoteBuildResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub output_files: Vec<(String, Vec<u8>)>,
}

/// Runs a persistent remote build worker daemon listening on `listen_addr`
pub fn run_worker_daemon(listen_addr: &str) -> std::io::Result<()> {
    let listener = TcpListener::bind(listen_addr)?;
    println!("makeyd worker daemon listening on {listen_addr}...");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                std::thread::spawn(move || {
                    if let Err(e) = handle_worker_connection(stream) {
                        eprintln!("makeyd worker error handling client: {e}");
                    }
                });
            }
            Err(e) => {
                eprintln!("makeyd worker connection accept error: {e}");
            }
        }
    }

    Ok(())
}

fn handle_worker_connection(mut stream: TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();

    reader.read_line(&mut line)?;
    if line.trim() != MAGIC_PROTOCOL {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Invalid protocol magic header",
        ));
    }

    // Read target
    line.clear();
    reader.read_line(&mut line)?;
    let target = line.trim().to_string();

    // Read command lines count
    line.clear();
    reader.read_line(&mut line)?;
    let cmd_count: usize = line
        .trim()
        .parse()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    let mut commands = Vec::new();
    for _ in 0..cmd_count {
        line.clear();
        reader.read_line(&mut line)?;
        commands.push(line.trim_end_matches(&['\r', '\n'][..]).to_string());
    }

    // Read input files count
    line.clear();
    reader.read_line(&mut line)?;
    let input_count: usize = line
        .trim()
        .parse()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    let sandbox_dir =
        std::env::temp_dir().join(format!("makeyd_worker_sandbox_{}", std::process::id()));
    let _ = fs::remove_dir_all(&sandbox_dir);
    fs::create_dir_all(&sandbox_dir)?;

    // Receive input files
    for _ in 0..input_count {
        line.clear();
        reader.read_line(&mut line)?;
        let file_path = line.trim().to_string();

        line.clear();
        reader.read_line(&mut line)?;
        let file_len: usize = line
            .trim()
            .parse()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        let mut buf = vec![0u8; file_len];
        reader.read_exact(&mut buf)?;

        let dest = sandbox_dir.join(&file_path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(dest, buf)?;
    }

    // Execute commands in sandbox
    let mut exit_code = 0i32;
    let mut combined_stdout = String::new();
    let mut combined_stderr = String::new();

    for cmd in commands {
        let child_out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&cmd)
            .current_dir(&sandbox_dir)
            .output();

        match child_out {
            Ok(output) => {
                combined_stdout.push_str(&String::from_utf8_lossy(&output.stdout));
                combined_stderr.push_str(&String::from_utf8_lossy(&output.stderr));
                if !output.status.success() {
                    exit_code = output.status.code().unwrap_or(1);
                    break;
                }
            }
            Err(e) => {
                combined_stderr.push_str(&format!("Spawn failed: {e}\n"));
                exit_code = 127;
                break;
            }
        }
    }

    // Collect output artifact if generated
    let mut output_files = Vec::new();
    let target_path = sandbox_dir.join(&target);
    if exit_code == 0 && target_path.exists() {
        if let Ok(bytes) = fs::read(&target_path) {
            output_files.push((target.clone(), bytes));
        }
    }

    let _ = fs::remove_dir_all(&sandbox_dir);

    // Send response back
    writeln!(stream, "{exit_code}")?;
    writeln!(stream, "{}", output_files.len())?;
    for (name, bytes) in &output_files {
        writeln!(stream, "{name}")?;
        writeln!(stream, "{}", bytes.len())?;
        stream.write_all(bytes)?;
    }

    writeln!(stream, "{}", combined_stdout.len())?;
    stream.write_all(combined_stdout.as_bytes())?;
    writeln!(stream, "{}", combined_stderr.len())?;
    stream.write_all(combined_stderr.as_bytes())?;
    stream.flush()?;

    Ok(())
}

/// Executes a single target compilation on a remote worker node
pub fn dispatch_remote_build(
    worker_addr: &str,
    target: &str,
    commands: &[String],
    input_files: &[PathBuf],
) -> Result<RemoteBuildResult, String> {
    let mut stream = TcpStream::connect(worker_addr)
        .map_err(|e| format!("Failed to connect to remote worker {worker_addr}: {e}"))?;

    // Send magic protocol
    writeln!(stream, "{MAGIC_PROTOCOL}").map_err(|e| e.to_string())?;

    // Send target name
    writeln!(stream, "{target}").map_err(|e| e.to_string())?;

    // Send command count and commands
    writeln!(stream, "{}", commands.len()).map_err(|e| e.to_string())?;
    for cmd in commands {
        writeln!(stream, "{cmd}").map_err(|e| e.to_string())?;
    }

    // Read and send existing input files
    let mut valid_inputs = Vec::new();
    for p in input_files {
        if p.exists() && p.is_file() {
            if let Ok(bytes) = fs::read(p) {
                let rel_name = p
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                valid_inputs.push((rel_name, bytes));
            }
        }
    }

    writeln!(stream, "{}", valid_inputs.len()).map_err(|e| e.to_string())?;
    for (name, bytes) in &valid_inputs {
        writeln!(stream, "{name}").map_err(|e| e.to_string())?;
        writeln!(stream, "{}", bytes.len()).map_err(|e| e.to_string())?;
        stream.write_all(bytes).map_err(|e| e.to_string())?;
    }
    stream.flush().map_err(|e| e.to_string())?;

    // Read response
    let mut reader = BufReader::new(stream);
    let mut line = String::new();

    // Exit code
    reader.read_line(&mut line).map_err(|e| e.to_string())?;
    let exit_code: i32 = line
        .trim()
        .parse()
        .map_err(|e| format!("Bad exit code: {e}"))?;

    // Output files count
    line.clear();
    reader.read_line(&mut line).map_err(|e| e.to_string())?;
    let out_count: usize = line
        .trim()
        .parse()
        .map_err(|e| format!("Bad out count: {e}"))?;

    let mut output_files = Vec::new();
    for _ in 0..out_count {
        line.clear();
        reader.read_line(&mut line).map_err(|e| e.to_string())?;
        let name = line.trim().to_string();

        line.clear();
        reader.read_line(&mut line).map_err(|e| e.to_string())?;
        let len: usize = line
            .trim()
            .parse()
            .map_err(|e| format!("Bad out len: {e}"))?;

        let mut buf = vec![0u8; len];
        reader.read_exact(&mut buf).map_err(|e| e.to_string())?;
        output_files.push((name, buf));
    }

    // Stdout
    line.clear();
    reader.read_line(&mut line).map_err(|e| e.to_string())?;
    let stdout_len: usize = line.trim().parse().unwrap_or(0);
    let mut stdout_buf = vec![0u8; stdout_len];
    reader
        .read_exact(&mut stdout_buf)
        .map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&stdout_buf).to_string();

    // Stderr
    line.clear();
    reader.read_line(&mut line).map_err(|e| e.to_string())?;
    let stderr_len: usize = line.trim().parse().unwrap_or(0);
    let mut stderr_buf = vec![0u8; stderr_len];
    reader
        .read_exact(&mut stderr_buf)
        .map_err(|e| e.to_string())?;
    let stderr = String::from_utf8_lossy(&stderr_buf).to_string();

    Ok(RemoteBuildResult {
        exit_code,
        stdout,
        stderr,
        output_files,
    })
}

/// Thread-safe pool of remote workers with round-robin dispatch
#[derive(Debug, Clone)]
pub struct RemoteWorkerPool {
    pub workers: Arc<Vec<String>>,
    pub index: Arc<Mutex<usize>>,
}

impl RemoteWorkerPool {
    pub fn new(workers: Vec<String>) -> Self {
        Self {
            workers: Arc::new(workers),
            index: Arc::new(Mutex::new(0)),
        }
    }

    pub fn acquire_worker(&self) -> Option<String> {
        if self.workers.is_empty() {
            return None;
        }
        let mut idx = self.index.lock().unwrap();
        let selected = self.workers[*idx % self.workers.len()].clone();
        *idx += 1;
        Some(selected)
    }

    pub fn is_empty(&self) -> bool {
        self.workers.is_empty()
    }
}
