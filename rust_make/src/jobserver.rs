use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::io::{FromRawFd, RawFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(unix)]
unsafe extern "C" {
    fn pipe(fds: *mut std::os::raw::c_int) -> std::os::raw::c_int;
    fn unlink(path: *const std::os::raw::c_char) -> std::os::raw::c_int;
}

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
    /// The descriptors belong to the process (inherited from a parent make,
    /// or created as master and passed to children), not to this value:
    /// several `JobServer`s in one process may wrap the same pair, so dropping
    /// one must not close them (`ManuallyDrop`).
    #[cfg(unix)]
    Pipe {
        read_file: std::mem::ManuallyDrop<File>,
        write_file: std::mem::ManuallyDrop<File>,
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
                        let read_file =
                            std::mem::ManuallyDrop::new(unsafe { File::from_raw_fd(r_fd) });
                        let write_file =
                            std::mem::ManuallyDrop::new(unsafe { File::from_raw_fd(w_fd) });
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
                // Master: an anonymous pipe passed as `--jobserver-auth=R,W`.
                // Every GNU make since 4.2 understands this form; the
                // `fifo:` form needs 4.4, and gcc's LTO wrapper runs whatever
                // `make` is installed (4.3 on Ubuntu 24.04). pipe(2) leaves
                // the descriptors inheritable, so recipes' children get them.
                let mut fds = [0 as std::os::raw::c_int; 2];
                if unsafe { pipe(fds.as_mut_ptr()) } != 0 {
                    return Err(io::Error::last_os_error());
                }
                let read_file = std::mem::ManuallyDrop::new(unsafe { File::from_raw_fd(fds[0]) });
                let mut write_file =
                    std::mem::ManuallyDrop::new(unsafe { File::from_raw_fd(fds[1]) });
                let tokens_to_write = requested_jobs.saturating_sub(1);
                if tokens_to_write > 0 {
                    write_file.write_all(&vec![b'+'; tokens_to_write])?;
                    write_file.flush()?;
                }
                return Ok(Self {
                    mode: JobServerMode::Pipe {
                        read_file,
                        write_file,
                    },
                    has_internal_token: AtomicBool::new(true),
                    jobs: requested_jobs,
                    auth_str: Some(format!("{},{}", fds[0], fds[1])),
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
                let mut rf: &File = read_file;
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
                    let mut wf: &File = write_file;
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
        // Without a jobserver (-n), GNU make still passes -jN down.
        let jobs = match self.auth_str.as_ref() {
            Some(auth) => Some(format!("-j{} --jobserver-auth={auth}", self.jobs)),
            None if self.jobs > 1 => Some(format!("-j{}", self.jobs)),
            None => None,
        };
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
    fn test_jobserver_master_pipe_tokens() {
        let master = JobServer::detect_or_create(3, None).unwrap();
        let auth = master.auth_str.as_ref().unwrap();
        // `R,W` (GNU make >= 4.2), not `fifo:` (>= 4.4 only).
        let (r, w) = auth.split_once(',').expect("R,W form");
        assert!(
            r.parse::<i32>().is_ok() && w.parse::<i32>().is_ok(),
            "{auth}"
        );

        let t1 = master.acquire().unwrap();
        assert_eq!(t1.token_type, TokenType::Internal);
        let t2 = master.acquire().unwrap();
        assert_eq!(t2.token_type, TokenType::Pipe);
        let t3 = master.acquire().unwrap();
        assert_eq!(t3.token_type, TokenType::Pipe);
        // Returning a token makes it available again.
        drop(t3);
        let t4 = master.acquire().unwrap();
        assert_eq!(t4.token_type, TokenType::Pipe);
        drop((t1, t2, t4));
    }
}
