use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::io::{FromRawFd, RawFd};
use std::path::PathBuf;
#[cfg(unix)]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(unix)]
unsafe extern "C" {
    fn mkfifo(path: *const std::os::raw::c_char, mode: u32) -> std::os::raw::c_int;
    fn unlink(path: *const std::os::raw::c_char) -> std::os::raw::c_int;
}

#[cfg(unix)]
static COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenType {
    Internal,
    Fifo,
    Pipe,
    None,
}

pub struct TokenGuard<'a> {
    server: &'a JobServer,
    pub token_type: TokenType,
}

impl<'a> Drop for TokenGuard<'a> {
    fn drop(&mut self) {
        self.server.release_token(self.token_type);
    }
}

enum JobServerMode {
    Disabled,
    Fifo {
        #[allow(dead_code)]
        path: PathBuf,
        #[allow(dead_code)]
        is_master: bool,
        read_file: File,
        write_file: File,
    },
    #[cfg(unix)]
    Pipe {
        read_file: File,
        write_file: File,
    },
}

pub struct JobServer {
    mode: JobServerMode,
    has_internal_token: AtomicBool,
    pub jobs: usize,
    pub auth_str: Option<String>,
}

impl JobServer {
    /// Detect an inherited jobserver from flags/MAKEFLAGS, or create a new master FIFO jobserver.
    pub fn detect_or_create(requested_jobs: usize, cli_auth: Option<&str>) -> io::Result<Self> {
        let auth_val = cli_auth.map(|s| s.to_string()).or_else(|| {
            std::env::var("MAKEFLAGS").ok().and_then(|mf| {
                for token in mf.split_whitespace() {
                    if let Some(val) = token.strip_prefix("--jobserver-auth=") {
                        return Some(val.to_string());
                    }
                    if let Some(val) = token.strip_prefix("--jobserver-fds=") {
                        return Some(val.to_string());
                    }
                }
                None
            })
        });

        if let Some(auth) = auth_val {
            if let Some(path_str) = auth.strip_prefix("fifo:") {
                let fifo_path = PathBuf::from(path_str);
                if fifo_path.exists() {
                    let file = OpenOptions::new().read(true).write(true).open(&fifo_path)?;
                    let read_file = file.try_clone()?;
                    let write_file = file;
                    return Ok(Self {
                        mode: JobServerMode::Fifo {
                            path: fifo_path,
                            is_master: false,
                            read_file,
                            write_file,
                        },
                        has_internal_token: AtomicBool::new(true),
                        jobs: requested_jobs.max(1),
                        auth_str: Some(auth),
                    });
                }
            }
            #[cfg(unix)]
            if auth.contains(',') {
                let parts: Vec<&str> = auth.split(',').collect();
                if parts.len() == 2 {
                    if let (Ok(r_fd), Ok(w_fd)) =
                        (parts[0].parse::<RawFd>(), parts[1].parse::<RawFd>())
                    {
                        let read_file = unsafe { File::from_raw_fd(r_fd) };
                        let write_file = unsafe { File::from_raw_fd(w_fd) };
                        return Ok(Self {
                            mode: JobServerMode::Pipe {
                                read_file,
                                write_file,
                            },
                            has_internal_token: AtomicBool::new(true),
                            jobs: requested_jobs.max(1),
                            auth_str: Some(auth),
                        });
                    }
                }
            }
        }

        if requested_jobs > 1 {
            #[cfg(unix)]
            {
                let pid = std::process::id();
                let cnt = COUNTER.fetch_add(1, Ordering::SeqCst);
                let tmp_dir = std::env::temp_dir();
                let fifo_path = tmp_dir.join(format!("maked_jobserver_{pid}_{cnt}.fifo"));
                let c_path = std::ffi::CString::new(fifo_path.to_str().unwrap()).unwrap();

                let res = unsafe { mkfifo(c_path.as_ptr(), 0o600) };
                if res != 0 {
                    return Err(io::Error::last_os_error());
                }

                let file = OpenOptions::new().read(true).write(true).open(&fifo_path)?;
                let mut write_file = file;
                let read_file = write_file.try_clone()?;

                // Write jobs - 1 tokens into the FIFO
                let tokens_to_write = requested_jobs.saturating_sub(1);
                if tokens_to_write > 0 {
                    let tokens = vec![b'+'; tokens_to_write];
                    write_file.write_all(&tokens)?;
                    write_file.flush()?;
                }

                let auth = format!("fifo:{}", fifo_path.display());
                return Ok(Self {
                    mode: JobServerMode::Fifo {
                        path: fifo_path,
                        is_master: true,
                        read_file,
                        write_file,
                    },
                    has_internal_token: AtomicBool::new(true),
                    jobs: requested_jobs,
                    auth_str: Some(auth),
                });
            }
        }

        Ok(Self {
            mode: JobServerMode::Disabled,
            has_internal_token: AtomicBool::new(true),
            jobs: requested_jobs.max(1),
            auth_str: None,
        })
    }

    /// Acquire a job token (blocking until available)
    pub fn acquire(&self) -> io::Result<TokenGuard<'_>> {
        // First check internal token slot
        if self.has_internal_token.swap(false, Ordering::SeqCst) {
            return Ok(TokenGuard {
                server: self,
                token_type: TokenType::Internal,
            });
        }

        match &self.mode {
            JobServerMode::Disabled => Ok(TokenGuard {
                server: self,
                token_type: TokenType::None,
            }),
            JobServerMode::Fifo { read_file, .. } => {
                let mut rf = read_file;
                let mut buf = [0u8; 1];
                rf.read_exact(&mut buf)?;
                Ok(TokenGuard {
                    server: self,
                    token_type: TokenType::Fifo,
                })
            }
            #[cfg(unix)]
            JobServerMode::Pipe { read_file, .. } => {
                let mut rf = read_file;
                let mut buf = [0u8; 1];
                rf.read_exact(&mut buf)?;
                Ok(TokenGuard {
                    server: self,
                    token_type: TokenType::Pipe,
                })
            }
        }
    }

    /// Release a job token back to the pool
    pub fn release_token(&self, token_type: TokenType) {
        match token_type {
            TokenType::Internal => {
                self.has_internal_token.store(true, Ordering::SeqCst);
            }
            TokenType::Fifo => {
                if let JobServerMode::Fifo { write_file, .. } = &self.mode {
                    let mut wf = write_file;
                    let _ = wf.write_all(b"+");
                    let _ = wf.flush();
                }
            }
            TokenType::Pipe =>
            {
                #[cfg(unix)]
                if let JobServerMode::Pipe { write_file, .. } = &self.mode {
                    let mut wf = write_file;
                    let _ = wf.write_all(b"+");
                    let _ = wf.flush();
                }
            }
            TokenType::None => {}
        }
    }

    /// MAKEFLAGS for a recipe's environment, in GNU make's layout:
    /// `<flag letters> -jN --jobserver-auth=... -- VAR=value ...`. Uses the
    /// flags and command-line variables `main` registered with
    /// `set_makeflags_base`; without them, extends `current`.
    pub fn child_makeflags(&self, current: &str) -> String {
        let jobs = self
            .auth_str
            .as_ref()
            .map(|auth| format!("-j{} --jobserver-auth={auth}", self.jobs));
        if let Some((letters, vars)) = MAKEFLAGS_BASE.get() {
            let mut parts: Vec<String> = Vec::new();
            if !letters.is_empty() {
                parts.push(letters.clone());
            }
            if let Some(j) = jobs {
                parts.push(j);
            }
            if !vars.is_empty() {
                parts.push("--".to_string());
                parts.extend(vars.iter().cloned());
            }
            return parts.join(" ");
        }
        match jobs {
            Some(j)
                if !current.contains("--jobserver-auth")
                    && !current.contains("--jobserver-fds") =>
            {
                format!("{current} {j}").trim().to_string()
            }
            _ => current.to_string(),
        }
    }
}

static MAKEFLAGS_BASE: std::sync::OnceLock<(String, Vec<String>)> = std::sync::OnceLock::new();

/// Record the single-letter flags (e.g. "ns") and command-line variable
/// assignments (`VAR=value`, with `\` and spaces escaped) to pass down.
pub fn set_makeflags_base(letters: String, vars: Vec<String>) {
    let _ = MAKEFLAGS_BASE.set((letters, vars));
}

/// Turn an inherited MAKEFLAGS value into argv-style words that `main`
/// parses before the real arguments: "ns -j8 --jobserver-auth=X -- V=1"
/// becomes ["-n", "-s", "-j8", "--jobserver-auth=X", "V=1"].
pub fn makeflags_to_args(makeflags: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut words = split_escaped(makeflags).into_iter();
    let mut first = true;
    while let Some(w) = words.next() {
        if w == "--" {
            out.extend(words.by_ref());
            break;
        }
        if first && !w.starts_with('-') && !w.contains('=') {
            out.extend(w.chars().map(|c| format!("-{c}")));
        } else if w.contains('=') && !w.starts_with('-') {
            out.push(w);
        } else {
            out.push(w);
        }
        first = false;
    }
    out
}

/// Split on unescaped spaces; `\ ` is a space and `\\` a backslash.
fn split_escaped(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            ' ' | '\t' => {
                if !cur.is_empty() {
                    words.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
}

/// Escape a command-line assignment for MAKEFLAGS.
pub fn escape_makeflags_word(w: &str) -> String {
    let mut out = String::with_capacity(w.len());
    for c in w.chars() {
        if c == ' ' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Job count a sub-make should schedule with when it inherits a jobserver
/// through MAKEFLAGS: the parent's `-jN`, or the host's parallelism when the
/// parent passed only `--jobserver-auth` (the token pool still caps it).
/// `None` when MAKEFLAGS carries no jobserver.
pub fn inherited_jobs(makeflags: &str) -> Option<usize> {
    let tokens: Vec<&str> = makeflags.split_whitespace().collect();
    let has_jobserver = tokens
        .iter()
        .any(|t| t.starts_with("--jobserver-auth=") || t.starts_with("--jobserver-fds="));
    if !has_jobserver {
        return None;
    }
    let from_flag = tokens
        .iter()
        .filter_map(|t| t.strip_prefix("-j"))
        .filter_map(|n| n.parse::<usize>().ok())
        .next_back();
    Some(from_flag.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    }))
    .map(|n| n.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_makeflags_to_args() {
        assert_eq!(
            makeflags_to_args("ns -j8 --jobserver-auth=fifo:/x -- V=1 W=a\\ b"),
            vec![
                "-n",
                "-s",
                "-j8",
                "--jobserver-auth=fifo:/x",
                "V=1",
                "W=a b"
            ]
        );
        assert_eq!(makeflags_to_args(" -j4"), vec!["-j4"]);
        assert!(makeflags_to_args("").is_empty());
    }

    #[test]
    fn test_inherited_jobs_from_makeflags() {
        assert_eq!(inherited_jobs(""), None);
        assert_eq!(inherited_jobs("-j8"), None);
        assert_eq!(inherited_jobs(" -j8 --jobserver-auth=fifo:/tmp/x"), Some(8));
        assert_eq!(inherited_jobs("s -j3 --jobserver-auth=3,4"), Some(3));
        assert!(inherited_jobs("--jobserver-fds=3,4").unwrap() >= 1);
    }

    #[test]
    fn test_jobserver_single_threaded() {
        let js = JobServer::detect_or_create(1, None).unwrap();
        assert!(js.auth_str.is_none());
        let token = js.acquire().unwrap();
        assert_eq!(token.token_type, TokenType::Internal);
        // Second acquire on single-threaded disabled mode gives None
        let token2 = js.acquire().unwrap();
        assert_eq!(token2.token_type, TokenType::None);
    }

    #[test]
    fn test_jobserver_master_fifo_and_client_exchange() {
        let master = JobServer::detect_or_create(3, None).unwrap();
        assert!(master.auth_str.is_some());
        let auth = master.auth_str.as_ref().unwrap();
        assert!(auth.starts_with("fifo:"));

        // Master acquires internal token
        let t1 = master.acquire().unwrap();
        assert_eq!(t1.token_type, TokenType::Internal);

        // Master acquires 2 tokens from FIFO
        let t2 = master.acquire().unwrap();
        assert_eq!(t2.token_type, TokenType::Fifo);

        let t3 = master.acquire().unwrap();
        assert_eq!(t3.token_type, TokenType::Fifo);

        // Connect client to the same FIFO
        let client = JobServer::detect_or_create(2, Some(auth)).unwrap();
        let c1 = client.acquire().unwrap();
        assert_eq!(c1.token_type, TokenType::Internal);

        // Drop one master token to return it to the FIFO
        drop(t3);

        // Now client should be able to acquire the freed token from FIFO!
        let c2 = client.acquire().unwrap();
        assert_eq!(c2.token_type, TokenType::Fifo);

        // Release client token
        drop(c2);
        drop(c1);
        drop(t2);
        drop(t1);
    }
}

impl Drop for JobServer {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let JobServerMode::Fifo {
            path, is_master, ..
        } = &self.mode
        {
            if *is_master {
                if let Ok(c_path) = std::ffi::CString::new(path.to_str().unwrap_or_default()) {
                    unsafe { unlink(c_path.as_ptr()) };
                }
            }
        }
    }
}
