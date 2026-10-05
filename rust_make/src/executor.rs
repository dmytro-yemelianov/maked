use crate::ast::Makefile;
use crate::freshness::{
    FreshnessDecision, evaluate_freshness_hash, evaluate_freshness_with, get_file_mtime,
};
use crate::graph::DependencyGraph;
use crate::parser::expand_variables;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Instant, SystemTime};

#[derive(Debug, Clone, Default)]
pub struct ExecutionConfig {
    pub jobs: usize,
    /// `-k`: after a failure, keep building what does not depend on it.
    pub keep_going: bool,
    pub dry_run: bool,
    pub always_make: bool,
    pub silent: bool,
    pub question: bool,
    pub use_hash: bool,
    pub ignore_errors: bool,
    pub touch_only: bool,
    pub profile: bool,
    pub trace_file: Option<String>,
    pub cache: bool,
    pub cache_dir: Option<String>,
    pub remote_workers: Vec<String>,
    pub tui: bool,
}

#[derive(Debug, Clone)]
pub struct ExecutionStats {
    pub total_evaluated: usize,
    pub targets_rebuilt: usize,
    pub targets_cached: usize,
    pub targets_up_to_date: usize,
    pub commands_executed: usize,
    pub elapsed_wall_time: std::time::Duration,
    pub critical_path_duration: std::time::Duration,
    pub critical_path: Vec<String>,
    /// Measured schedule against the Lean-proved lower and Graham bounds.
    pub schedule: Option<crate::trace::ScheduleBounds>,
}

#[derive(Debug, Clone)]
pub enum ExecutionError {
    BuildFailed(String, i32),
    /// -k: the goals' "not remade" lines are already printed.
    NotRemade,
    NoRuleToMake(String),
    CommandSpawnFailed(String, String),
}

impl std::fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BuildFailed(target, code) => {
                write!(f, "make: *** [{target}] Error {code}")
            }
            Self::NotRemade => Ok(()),
            Self::NoRuleToMake(target) => {
                write!(f, "make: *** No rule to make target '{target}'. Stop.")
            }
            Self::CommandSpawnFailed(cmd, err) => {
                write!(f, "make: failed to spawn command '{cmd}': {err}")
            }
        }
    }
}

impl std::error::Error for ExecutionError {}

/// The phony root maked builds when several goals are given.
pub const GOALS_ROOT: &str = ".MAKED_GOALS";

/// -t for one target with a recipe, as GNU make does it (remake.c,
/// notice_finished_file): lines with `+` or `$(MAKE)` run and the others
/// are skipped, then the target is touched only if
/// some line is not recursive. Under -n the touch is printed, not done.
/// Returns whether anything ran or was touched, or the first failure.
fn touch_target(
    rule: &crate::ast::Rule,
    target: &str,
    makefile: &Makefile,
    config: &ExecutionConfig,
    env: &crate::ast::RecipeEnv,
    makeflags: &str,
    out: &mut Vec<String>,
) -> Result<bool, ExecutionError> {
    let recursive = |raw: &str| recipe_prefixes(raw).3 || mentions_make(raw);
    let mut did = false;
    if rule.commands.iter().any(|c| recursive(c)) {
        for raw in &rule.commands {
            let (cmd_str, s1, i1, f1) = recipe_prefixes(raw);
            let expanded = expand_variables(cmd_str, makefile, Some(target), &rule.prereqs);
            let (cmd, s2, i2, f2) = recipe_prefixes(&expanded);
            if cmd.trim().is_empty() {
                continue;
            }
            // Other lines are skipped, not even printed, -n or not.
            if !(f1 || f2 || mentions_make(raw)) {
                continue;
            }
            if config.dry_run || !(config.silent || s1 || s2) {
                println!("{cmd}");
            }
            did = true;
            let status = run_command_status_fast(cmd, Some(makeflags), env)
                .map_err(|e| ExecutionError::CommandSpawnFailed(cmd.to_string(), e.to_string()))?;
            if !status.success() {
                let code = status.code().unwrap_or(1);
                if config.ignore_errors || i1 || i2 {
                    if !config.silent {
                        eprintln!("make: [{target}] Error {code} (ignored)");
                    }
                } else {
                    return Err(ExecutionError::BuildFailed(target.to_string(), code));
                }
            }
        }
    }
    // Phony targets run their `+` lines but are never touched.
    if !rule.is_phony && rule.commands.iter().any(|c| !recursive(c)) {
        if !config.silent {
            out.push(format!("touch {target}"));
        }
        if !config.dry_run {
            touch_file(target);
        }
        did = true;
    }
    Ok(did)
}

#[derive(Debug, Clone)]
enum TargetStatus {
    UpToDate(Option<SystemTime>),
    Rebuilt(SystemTime),
    Failed,
}

pub struct Executor<'a> {
    pub makefile: &'a Makefile,
    pub graph: &'a DependencyGraph,
    pub config: ExecutionConfig,
    pub db: Arc<Mutex<crate::hash::BuildDatabase>>,
    pub jobserver: Arc<crate::jobserver::JobServer>,
    pub tracer: crate::trace::TraceCollector,
    pub cache: Arc<crate::cache::ContentAddressableCache>,
    pub remote_pool: Arc<crate::distributed::RemoteWorkerPool>,
    pub tui: crate::tui::TuiReporter,
    pub recipe_env: Arc<crate::ast::RecipeEnv>,
    /// Targets finished by an earlier run of this process (remaking the
    /// makefiles), which later runs take as done, as GNU make does.
    settled: Mutex<crate::fxhash::FxHashMap<String, TargetStatus>>,
    /// Targets whose recipe ran (or, under -n, was printed): GNU make says
    /// "Nothing to be done" for a goal only when none ran for it.
    ran: Mutex<Vec<String>>,
    /// -k: goals reported as "not remade because of errors".
    not_remade: Mutex<Vec<String>>,
}

/// Targets one executor finished, handed to the next with
/// [`Executor::with_settled`]: a target remade while remaking the makefiles
/// is not remade again for the goals.
pub struct SettledTargets(crate::fxhash::FxHashMap<String, TargetStatus>);

/// Cross-platform shell command builder (for `$(shell ...)`: /bin/sh).
pub fn create_shell_command(cmd: &str) -> Command {
    create_shell_command_with("/bin/sh", cmd)
}

/// Run `cmd` under `shell -c`. On Unix, `shell` is the makefile's `SHELL`
/// (or /bin/sh); `$SHELL` from the environment is ignored, as in GNU make.
pub fn create_shell_command_with(shell: &str, cmd: &str) -> Command {
    #[cfg(unix)]
    {
        let mut c = Command::new(shell);
        if !cmd.is_empty() {
            c.arg("-c").arg(cmd);
        }
        c
    }
    #[cfg(windows)]
    {
        let _ = shell;
        if let Ok(shell) = std::env::var("SHELL") {
            let mut c = Command::new(shell);
            if !cmd.is_empty() {
                c.arg("-c").arg(cmd);
            }
            c
        } else {
            let comspec = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
            let mut c = Command::new(comspec);
            if !cmd.is_empty() {
                c.arg("/C").arg(cmd);
            }
            c
        }
    }
}

/// On macOS a spawned thread starts below the main thread's QoS class, and
/// recipes it spawns inherit that, landing on efficiency cores (seen as 2x
/// slower, noisy builds). Raise the calling thread to the main thread's
/// class. No-op elsewhere.
pub fn match_main_thread_qos() {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn qos_class_main() -> u32;
            fn pthread_set_qos_class_self_np(qos: u32, relpri: i32) -> i32;
        }
        // SAFETY: plain libc calls on the current thread.
        unsafe {
            pthread_set_qos_class_self_np(qos_class_main(), 0);
        }
    }
}

/// mtimes of prerequisites during a build. A header listed by 500 objects
/// was stat'ed 500 times; GNU make stats each file once. An entry is
/// dropped when a recipe for that target finishes, and dependents only look
/// at a prerequisite after it has finished, so a cached value is never
/// stale when it is read.
static MTIMES: std::sync::OnceLock<Mutex<crate::fxhash::FxHashMap<String, Option<SystemTime>>>> =
    std::sync::OnceLock::new();

/// `-t`: create the file if needed and set its mtime to now (opening it, as
/// before, left an existing file's mtime unchanged).
fn touch_file(path: &str) {
    if let Ok(f) = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
    {
        let _ = f.set_modified(SystemTime::now());
    }
    forget_mtime(path);
}

fn cached_mtime(path: &str) -> Option<SystemTime> {
    let cache = MTIMES.get_or_init(|| Mutex::new(crate::fxhash::FxHashMap::default()));
    if let Some(v) = cache.lock().unwrap().get(path) {
        return *v;
    }
    let v = get_file_mtime(path);
    cache.lock().unwrap().insert(path.to_string(), v);
    v
}

fn forget_mtime(path: &str) {
    if let Some(cache) = MTIMES.get() {
        cache.lock().unwrap().remove(path);
    }
}

static PROCESS_ENV: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static PROGRAM_PATHS: std::sync::OnceLock<Mutex<HashMap<(String, String), String>>> =
    std::sync::OnceLock::new();

/// Put the recipe environment (exported variables, MAKEFLAGS) into this
/// process's own environment once, so recipes inherit it without a
/// per-spawn environment copy. Call from `main` only, before any thread
/// other than the caller exists; tests run executors concurrently in one
/// process and must not use it.
pub fn install_process_env(env: &crate::ast::RecipeEnv, makeflags: &str) {
    for (k, v) in &env.set {
        // SAFETY: called once at startup while no other thread reads or
        // writes the environment.
        unsafe { std::env::set_var(k, v) };
    }
    unsafe { std::env::set_var("MAKEFLAGS", makeflags) };
    let _ = PROCESS_ENV.set(makeflags.to_string());
}

/// `program` as an absolute path, searched once per (PATH, program): Rust's
/// spawn otherwise walks PATH on every call, which costs about a third of a
/// millisecond per recipe line.
fn resolve_program(program: &str, env: &crate::ast::RecipeEnv) -> String {
    if program.contains('/') || program.is_empty() {
        return program.to_string();
    }
    let path = env
        .set
        .iter()
        .find(|(k, _)| k == "PATH")
        .map(|(_, v)| v.clone())
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    let key = (path.clone(), program.to_string());
    let cache = PROGRAM_PATHS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = cache.lock().unwrap().get(&key) {
        return hit.clone();
    }
    #[cfg(unix)]
    let found = {
        use std::os::unix::fs::PermissionsExt;
        path.split(':').find_map(|dir| {
            let dir = if dir.is_empty() { "." } else { dir };
            let cand = std::path::Path::new(dir).join(program);
            let meta = std::fs::metadata(&cand).ok()?;
            (meta.is_file() && meta.permissions().mode() & 0o111 != 0)
                .then(|| cand.to_string_lossy().to_string())
        })
    };
    #[cfg(not(unix))]
    let found: Option<String> = None;
    // Not found: leave the name; spawn reports the error as before.
    let resolved = found.unwrap_or_else(|| program.to_string());
    cache.lock().unwrap().insert(key, resolved.clone());
    resolved
}

/// Strip GNU recipe prefixes (`@` silent, `-` ignore errors, `+` run even
/// under -n). Returns (rest, silent, ignore, force).
pub fn recipe_prefixes(mut s: &str) -> (&str, bool, bool, bool) {
    let (mut silent, mut ignore, mut force) = (false, false, false);
    loop {
        s = s.trim_start();
        match s.chars().next() {
            Some('@') => silent = true,
            Some('-') => ignore = true,
            Some('+') => force = true,
            _ => return (s, silent, ignore, force),
        }
        s = &s[1..];
    }
}

/// A recipe line that runs a sub-make; GNU make runs these even under -n.
fn mentions_make(raw: &str) -> bool {
    raw.contains("$(MAKE)") || raw.contains("${MAKE}")
}

/// Build the process for one recipe line: direct exec when the line has no
/// shell syntax and the shell is the default, else `SHELL -c line`. Applies
/// exported and unexported variables and MAKEFLAGS.
/// Shell builtins and keywords have no binary to exec (GNU make's list).
const SH_BUILTINS: &[&str] = &[
    ".", ":", "alias", "bg", "break", "case", "cd", "command", "continue", "do", "done", "elif",
    "else", "esac", "eval", "exec", "exit", "export", "fc", "fg", "fi", "for", "getopts", "hash",
    "if", "jobs", "login", "logout", "read", "readonly", "return", "set", "shift", "source",
    "test", "then", "times", "trap", "type", "ulimit", "umask", "unalias", "unset", "until",
    "wait", "while", "{", "}", "!", "local", "[",
];

/// Split `cmd` into argv when no shell is needed, as GNU make's
/// construct_command_argv does: words separated by blanks, `'...'` taken
/// literally, `"..."` literally unless it holds `$`, a backtick or `\`.
/// Anything else a shell would interpret (outside quotes), an unbalanced
/// quote, a shell builtin as the first word, or Windows (where `echo` and
/// `copy` are cmd.exe builtins) returns `None`: run it through the shell.
pub(crate) fn split_simple_command(cmd: &str) -> Option<Vec<String>> {
    if cfg!(windows) {
        return None;
    }
    let mut argv: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = cmd.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {
                if in_word {
                    argv.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        q => cur.push(q),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '$' | '`' | '\\' => return None,
                        q => cur.push(q),
                    }
                }
            }
            '*' | '?' | '[' | ']' | '~' | '=' | '|' | '&' | ';' | '<' | '>' | '(' | ')' | '$'
            | '`' | '\\' | '\n' | '#' | '{' | '}' | '!' | '%' | '^' => return None,
            _ => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        argv.push(cur);
    }
    if argv.is_empty() || SH_BUILTINS.contains(&argv[0].as_str()) {
        return None;
    }
    Some(argv)
}

fn recipe_command(cmd: &str, makeflags: Option<&str>, env: &crate::ast::RecipeEnv) -> Command {
    let direct = if env.shell == "/bin/sh" {
        split_simple_command(cmd)
    } else {
        None
    };
    let mut command = match direct {
        Some(argv) => {
            let mut c = Command::new(resolve_program(&argv[0], env));
            c.args(&argv[1..]);
            c
        }
        None => create_shell_command_with(&resolve_program(&env.shell, env), cmd),
    };
    for name in &env.unset {
        command.env_remove(name);
    }
    // When `install_process_env` put the exported variables and MAKEFLAGS
    // into this process's environment, children inherit them as-is; setting
    // them per command would make every spawn copy the whole environment.
    let installed = PROCESS_ENV.get();
    if installed.is_none() {
        for (k, v) in &env.set {
            command.env(k, v);
        }
    }
    if let Some(mf) = makeflags {
        if installed.is_none_or(|m| m != mf) {
            command.env("MAKEFLAGS", mf);
        }
    }
    command
}

/// `:` with plain arguments (git: `: no custom templates yet`) does
/// nothing and succeeds; GNU make does not start a shell for it either.
/// `: > file` and the like still go to the shell.
fn is_noop_line(cmd: &str) -> bool {
    let t = cmd.trim();
    (t == ":" || t.starts_with(": ") || t.starts_with(":\t"))
        && split_simple_command(&format!("true{}", &t[1..])).is_some()
}

#[cfg(unix)]
fn success_status() -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(0)
}

fn run_command_status_fast(
    cmd: &str,
    makeflags: Option<&str>,
    env: &crate::ast::RecipeEnv,
) -> std::io::Result<std::process::ExitStatus> {
    #[cfg(unix)]
    if is_noop_line(cmd) {
        return Ok(success_status());
    }
    recipe_command(cmd, makeflags, env).status()
}

fn run_command_output_fast(
    cmd: &str,
    makeflags: Option<&str>,
    env: &crate::ast::RecipeEnv,
) -> std::io::Result<std::process::Output> {
    #[cfg(unix)]
    if is_noop_line(cmd) {
        return Ok(std::process::Output {
            status: success_status(),
            stdout: Vec::new(),
            stderr: Vec::new(),
        });
    }
    recipe_command(cmd, makeflags, env).output()
}

impl<'a> Executor<'a> {
    pub fn new(
        makefile: &'a Makefile,
        graph: &'a DependencyGraph,
        config: ExecutionConfig,
    ) -> Self {
        let jobserver =
            Arc::new(crate::jobserver::JobServer::detect_or_create(config.jobs, None).unwrap());
        Self::with_jobserver(makefile, graph, config, jobserver)
    }

    pub fn with_jobserver(
        makefile: &'a Makefile,
        graph: &'a DependencyGraph,
        config: ExecutionConfig,
        jobserver: Arc<crate::jobserver::JobServer>,
    ) -> Self {
        let db = if config.use_hash {
            crate::hash::BuildDatabase::load(crate::hash::BuildDatabase::DB_FILENAME)
        } else {
            crate::hash::BuildDatabase::default()
        };
        let cache_config = crate::cache::CacheConfig {
            enabled: config.cache,
            cache_dir: config
                .cache_dir
                .as_ref()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from(".maked_cache")),
        };
        let cache = Arc::new(crate::cache::ContentAddressableCache::new(cache_config));
        let remote_auth = if config.remote_workers.is_empty() {
            None
        } else {
            crate::distributed::WorkerAuth::from_env().ok()
        };
        let remote_pool = Arc::new(crate::distributed::RemoteWorkerPool::new(
            config.remote_workers.clone(),
            remote_auth,
        ));
        let total_targets = graph.all_nodes.len();
        let tui = crate::tui::TuiReporter::new(config.jobs, total_targets, config.tui);
        Self {
            makefile,
            graph,
            config,
            db: Arc::new(Mutex::new(db)),
            jobserver,
            tracer: crate::trace::TraceCollector::new(),
            cache,
            remote_pool,
            recipe_env: Arc::new(makefile.recipe_env()),
            tui,
            settled: Mutex::new(Default::default()),
            ran: Mutex::new(Vec::new()),
            not_remade: Mutex::new(Vec::new()),
        }
    }

    /// Start from the targets an earlier executor finished.
    pub fn with_settled(self, settled: SettledTargets) -> Self {
        *self.settled.lock().unwrap() = settled.0;
        self
    }

    /// Targets whose recipe ran (or was printed under -n), in order.
    pub fn ran_targets(&self) -> Vec<String> {
        self.ran.lock().unwrap().clone()
    }

    /// -k: print "Target 'X' not remade because of errors." for each failed
    /// goal (never for maked's own GOALS_ROOT).
    fn report_not_remade(&self, root: &str, failed: impl Fn(&str) -> bool) {
        let goals: Vec<String> = if root == GOALS_ROOT {
            self.makefile
                .rules
                .get(root)
                .map(|r| r.prereqs.clone())
                .unwrap_or_default()
        } else {
            vec![root.to_string()]
        };
        for g in goals.iter().filter(|g| failed(g)) {
            eprintln!("make: Target '{g}' not remade because of errors.");
            self.not_remade.lock().unwrap().push(g.clone());
        }
    }

    /// -k: the goals that were not remade because of errors.
    pub fn not_remade_goals(&self) -> Vec<String> {
        self.not_remade.lock().unwrap().clone()
    }

    /// The targets this executor has finished so far.
    pub fn into_settled(self) -> SettledTargets {
        SettledTargets(self.settled.into_inner().unwrap())
    }

    /// Single-threaded post-order recursive execution
    pub fn execute_sequential(&self, root: &str) -> Result<ExecutionStats, ExecutionError> {
        let start_time = Instant::now();
        let mut target_statuses: HashMap<String, TargetStatus> = self
            .settled
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let mut stats = ExecutionStats {
            total_evaluated: 0,
            targets_rebuilt: 0,
            targets_cached: 0,
            targets_up_to_date: 0,
            commands_executed: 0,
            elapsed_wall_time: std::time::Duration::ZERO,
            critical_path_duration: std::time::Duration::ZERO,
            critical_path: Vec::new(),
            schedule: None,
        };

        let result = self.doname(root, &mut target_statuses, &mut stats);
        if self.config.keep_going && matches!(result, Ok(TargetStatus::Failed)) {
            self.report_not_remade(root, |g| {
                !matches!(
                    target_statuses.get(g),
                    Some(TargetStatus::UpToDate(_) | TargetStatus::Rebuilt(_))
                )
            });
        }
        // Failures are remembered too: with -k and several goals, a target
        // that failed for one goal is not run again for the next (GNU make).
        self.settled.lock().unwrap().extend(target_statuses);
        let status = result?;
        if matches!(status, TargetStatus::Failed) {
            self.save_duration_log();
            return Err(if self.config.keep_going {
                ExecutionError::NotRemade
            } else {
                ExecutionError::BuildFailed(root.to_string(), 1)
            });
        }
        if self.config.use_hash && !self.config.dry_run {
            let _ = self
                .db
                .lock()
                .unwrap()
                .save(crate::hash::BuildDatabase::DB_FILENAME);
        }
        self.save_duration_log();
        if let Some(ref path) = self.config.trace_file {
            let _ = self.tracer.save_to_file(path);
        }
        let (crit_us, crit_path) = self.tracer.compute_critical_path(&self.graph.adj, root);
        stats.critical_path_duration = std::time::Duration::from_micros(crit_us);
        stats.critical_path = crit_path;
        stats.schedule = self.tracer.schedule_bounds(1, crit_us);
        stats.elapsed_wall_time = start_time.elapsed();
        self.tui.finish();
        Ok(stats)
    }

    /// Make `target` once: every outcome is recorded, so a target several
    /// rules depend on is settled the first time and never redone (-t, -q
    /// and the recipe-less paths return early inside `doname_once`).
    fn doname(
        &self,
        target: &str,
        statuses: &mut HashMap<String, TargetStatus>,
        stats: &mut ExecutionStats,
    ) -> Result<TargetStatus, ExecutionError> {
        if let Some(status) = statuses.get(target) {
            return Ok(status.clone());
        }
        crate::decisions::note_before(target);
        let status = self.doname_once(target, statuses, stats)?;
        statuses.insert(target.to_string(), status.clone());
        self.record_decision(target, &status);
        Ok(status)
    }

    /// For `MAKED_DECISIONS` (see `crate::decisions`).
    fn record_decision(&self, target: &str, status: &TargetStatus) {
        if !crate::decisions::enabled() {
            return;
        }
        let (c, t) = match status {
            TargetStatus::Rebuilt(t) => ('R', Some(*t)),
            TargetStatus::UpToDate(t) => ('U', *t),
            TargetStatus::Failed => ('F', None),
        };
        let rule = self.makefile.get_rule(target);
        crate::decisions::record(target, rule.as_deref(), c, t);
    }

    fn doname_once(
        &self,
        target: &str,
        statuses: &mut HashMap<String, TargetStatus>,
        stats: &mut ExecutionStats,
    ) -> Result<TargetStatus, ExecutionError> {
        stats.total_evaluated += 1;
        let start_ts = self.tracer.start_micros();
        let rule_start = Instant::now();
        self.tui.target_started(0, target);

        let rule = match self.makefile.get_rule(target) {
            Some(r) => r,
            None => {
                let resolved = self.makefile.resolve_path(target);
                let check_path = resolved.as_deref().unwrap_or(target);
                if let Some(mtime) = get_file_mtime(check_path) {
                    let status = TargetStatus::UpToDate(Some(mtime));
                    statuses.insert(target.to_string(), status.clone());
                    return Ok(status);
                } else {
                    return Err(ExecutionError::NoRuleToMake(target.to_string()));
                }
            }
        };

        let mut any_dep_rebuilt = false;
        let mut newest_dep_mtime: Option<SystemTime> = None;

        let mut any_dep_failed = false;
        for dep in &rule.prereqs {
            let dep_status = match self.doname(dep, statuses, stats) {
                Ok(st) => st,
                Err(e) if self.config.keep_going => {
                    eprintln!("{e}");
                    statuses.insert(dep.to_string(), TargetStatus::Failed);
                    TargetStatus::Failed
                }
                Err(e) => return Err(e),
            };
            if cached_mtime(dep).is_none() && self.makefile.resolve_path(dep).is_none() {
                any_dep_rebuilt = true;
            }
            match dep_status {
                TargetStatus::Failed => {
                    if self.config.keep_going {
                        // -k: still bring the other prerequisites up to date.
                        any_dep_failed = true;
                        continue;
                    }
                    statuses.insert(target.to_string(), TargetStatus::Failed);
                    return Ok(TargetStatus::Failed);
                }
                TargetStatus::Rebuilt(new_time) => {
                    any_dep_rebuilt = true;
                    if newest_dep_mtime.is_none_or(|t| new_time > t) {
                        newest_dep_mtime = Some(new_time);
                    }
                }
                TargetStatus::UpToDate(Some(dep_mtime)) => {
                    if newest_dep_mtime.is_none_or(|t| dep_mtime > t) {
                        newest_dep_mtime = Some(dep_mtime);
                    }
                }
                TargetStatus::UpToDate(None) => {}
            }
        }

        if any_dep_failed {
            statuses.insert(target.to_string(), TargetStatus::Failed);
            return Ok(TargetStatus::Failed);
        }

        let all_cmds = rule.commands.join("\n");
        let freshness = if self.config.use_hash {
            let db = self.db.lock().unwrap();
            evaluate_freshness_hash(
                &rule,
                self.config.always_make,
                any_dep_rebuilt,
                &all_cmds,
                &db,
            )
        } else {
            evaluate_freshness_with(
                &rule,
                self.config.always_make,
                any_dep_rebuilt,
                newest_dep_mtime,
                cached_mtime(target),
            )
        };

        let final_status = match freshness {
            FreshnessDecision::UpToDate(mtime) => {
                stats.targets_up_to_date += 1;
                TargetStatus::UpToDate(Some(mtime))
            }
            FreshnessDecision::NeedsRebuild(_reason) => {
                let mtime_before = get_file_mtime(target);
                // GNU make: -q asks whether a recipe would run, and -t
                // touches only targets that have one. A target without a
                // recipe (an alias, `FORCE:`) is remade by doing nothing.
                if self.config.question {
                    if !rule.commands.is_empty() {
                        stats.targets_rebuilt += 1;
                    }
                    return Ok(TargetStatus::Rebuilt(SystemTime::now()));
                }

                if self.config.touch_only
                    && (rule.commands.is_empty()
                        || rule.is_phony
                            && !rule
                                .commands
                                .iter()
                                .any(|c| recipe_prefixes(c).3 || mentions_make(c)))
                {
                    return Ok(TargetStatus::Rebuilt(SystemTime::now()));
                }
                if self.config.touch_only {
                    let mut out = Vec::new();
                    let mf = self
                        .jobserver
                        .child_makeflags(&std::env::var("MAKEFLAGS").unwrap_or_default());
                    let did = touch_target(
                        &rule,
                        target,
                        self.makefile,
                        &self.config,
                        &self.recipe_env,
                        &mf,
                        &mut out,
                    )?;
                    out.iter().for_each(|l| println!("{l}"));
                    if did {
                        self.ran.lock().unwrap().push(target.to_string());
                        crate::decisions::mark_ran(target);
                        stats.commands_executed += 1;
                    }
                    stats.targets_rebuilt += 1;
                    return Ok(TargetStatus::Rebuilt(SystemTime::now()));
                }

                let mut restored_from_cache = false;
                let cache_key = if self.config.cache && !self.config.dry_run {
                    let k = crate::cache::ContentAddressableCache::compute_cache_key(
                        target,
                        &rule.commands,
                        &rule.prereqs,
                    );
                    if self.cache.restore_artifact(&k, target) {
                        restored_from_cache = true;
                        stats.targets_cached += 1;
                        if !self.config.silent {
                            println!("[maked] Restored {target} from cache ({k})");
                        }
                    }
                    Some(k)
                } else {
                    None
                };

                if !restored_from_cache {
                    let mut ran_remotely = false;
                    if !self.remote_pool.is_empty() && !self.config.dry_run {
                        if let Some((worker, auth)) = self.remote_pool.acquire_worker() {
                            let mut expanded_cmds = Vec::new();
                            for raw_cmd in &rule.commands {
                                let mut cmd_str = raw_cmd.trim_start();
                                while cmd_str.starts_with('@')
                                    || cmd_str.starts_with('-')
                                    || cmd_str.starts_with('+')
                                {
                                    cmd_str = cmd_str[1..].trim_start();
                                }
                                expanded_cmds.push(expand_variables(
                                    cmd_str,
                                    self.makefile,
                                    Some(target),
                                    &rule.prereqs,
                                ));
                            }
                            let input_files: Vec<std::path::PathBuf> =
                                rule.prereqs.iter().map(std::path::PathBuf::from).collect();
                            if let Ok(res) = crate::distributed::dispatch_remote_build(
                                &worker,
                                &auth,
                                target,
                                &expanded_cmds,
                                &input_files,
                            ) {
                                if res.exit_code == 0 {
                                    for (fname, bytes) in res.output_files {
                                        let _ = std::fs::write(&fname, bytes);
                                    }
                                    if !res.stdout.is_empty() && !self.config.silent {
                                        print!("{}", res.stdout);
                                    }
                                    stats.commands_executed += expanded_cmds.len();
                                    ran_remotely = true;
                                }
                            }
                        }
                    }

                    if !ran_remotely {
                        // Execute recipe commands locally
                        for raw_cmd in &rule.commands {
                            // Prefixes count before and after expansion
                            // (`QUIET_CC = @printf ...`), as in GNU make.
                            let (cmd_str, s1, i1, f1) = recipe_prefixes(raw_cmd);
                            let expanded = expand_variables(
                                cmd_str,
                                self.makefile,
                                Some(target),
                                &rule.prereqs,
                            );
                            let (cmd, s2, i2, f2) = recipe_prefixes(&expanded);
                            // A line that expands to nothing is skipped, not
                            // echoed or run (GNU make).
                            if cmd.trim().is_empty() {
                                continue;
                            }
                            {
                                let mut ran = self.ran.lock().unwrap();
                                if ran.last().map(String::as_str) != Some(target) {
                                    ran.push(target.to_string());
                                    crate::decisions::mark_ran(target);
                                }
                            }
                            let cmd = cmd.to_string();
                            let force = f1 || f2 || mentions_make(raw_cmd);
                            let run = !self.config.dry_run || force;
                            // Under -n everything is printed, even with -s or `@`.
                            let is_silent =
                                !self.config.dry_run && (self.config.silent || s1 || s2);
                            let ignore_err = self.config.ignore_errors || i1 || i2;
                            if !is_silent {
                                println!("{cmd}");
                            }
                            if run {
                                let cur_mf = std::env::var("MAKEFLAGS").unwrap_or_default();
                                let child_mf = self.jobserver.child_makeflags(&cur_mf);
                                let status = run_command_status_fast(
                                    &cmd,
                                    Some(&child_mf),
                                    &self.recipe_env,
                                )
                                .map_err(|e| {
                                    ExecutionError::CommandSpawnFailed(cmd.clone(), e.to_string())
                                })?;

                                if !status.success() && ignore_err && !self.config.silent {
                                    let code = status.code().unwrap_or(1);
                                    eprintln!("make: [{target}] Error {code} (ignored)");
                                }
                                if !status.success() && !ignore_err {
                                    let code = status.code().unwrap_or(1);
                                    statuses.insert(target.to_string(), TargetStatus::Failed);
                                    let err = ExecutionError::BuildFailed(target.to_string(), code);
                                    if self.config.keep_going {
                                        eprintln!("{err}");
                                        return Ok(TargetStatus::Failed);
                                    }
                                    return Err(err);
                                }
                            }
                            stats.commands_executed += 1;
                        }
                    }

                    if let Some(ref k) = cache_key {
                        self.cache.store_artifact(k, target);
                    }
                }

                if self.config.use_hash && !self.config.dry_run {
                    let mut prereq_hashes = HashMap::new();
                    for dep in &rule.prereqs {
                        if let Ok(h) = crate::hash::sha256_file(dep) {
                            prereq_hashes.insert(dep.clone(), crate::hash::to_hex(&h));
                        }
                    }
                    let target_hash = crate::hash::sha256_file(target)
                        .map(|h| crate::hash::to_hex(&h))
                        .unwrap_or_default();
                    let recipe_hash =
                        crate::hash::to_hex(&crate::hash::sha256_bytes(all_cmds.as_bytes()));
                    self.db.lock().unwrap().update_record(
                        target.to_string(),
                        crate::hash::TargetRecord {
                            target_hash,
                            recipe_hash,
                            prereq_hashes,
                        },
                    );
                }

                stats.targets_rebuilt += 1;
                forget_mtime(target);
                let mtime_after = get_file_mtime(target);
                // A recipe that left an existing file untouched (automake's
                // `config.h: stamp-h1`) does not make dependents stale; GNU
                // make re-stats the target in the same way.
                match (mtime_before, mtime_after) {
                    (Some(b), Some(a)) if a == b && !rule.is_phony && !self.config.dry_run => {
                        TargetStatus::UpToDate(Some(a))
                    }
                    _ => TargetStatus::Rebuilt(mtime_after.unwrap_or_else(SystemTime::now)),
                }
            }
        };

        statuses.insert(target.to_string(), final_status.clone());
        let dur_us = rule_start.elapsed().as_micros() as u64;
        let mut trace_args = HashMap::new();
        trace_args.insert(
            "status".to_string(),
            match &final_status {
                TargetStatus::Rebuilt(_) => "rebuilt".to_string(),
                TargetStatus::UpToDate(_) => "up_to_date".to_string(),
                TargetStatus::Failed => "failed".to_string(),
            },
        );
        self.tracer.record_complete(
            target.to_string(),
            "rule",
            start_ts,
            dur_us,
            std::process::id(),
            0,
            trace_args,
        );
        let (status_str, is_cached) = match &final_status {
            TargetStatus::Rebuilt(_) => ("rebuilt", false),
            TargetStatus::UpToDate(_) => ("up_to_date", false),
            TargetStatus::Failed => ("failed", false),
        };
        self.tui.target_finished(0, target, status_str, is_cached);
        Ok(final_status)
    }

    /// High-performance multi-threaded parallel execution (-j N)
    pub fn execute_parallel(&self, root: &str) -> Result<ExecutionStats, ExecutionError> {
        let start_time = Instant::now();
        let reachable = self.graph.reachable_subgraph(self.makefile, root);

        let mut in_degrees: crate::fxhash::FxHashMap<String, usize> = Default::default();
        let mut dependents: crate::fxhash::FxHashMap<String, Vec<String>> = Default::default();

        for node in &reachable {
            let rule = self.makefile.get_rule(node);
            let prereqs: &[String] = match &rule {
                Some(r) => &r.prereqs,
                None => self.graph.adj.get(node).map(Vec::as_slice).unwrap_or(&[]),
            };

            let mut unique_prereqs: crate::fxhash::FxHashSet<&String> = Default::default();
            for dep in prereqs {
                if reachable.contains(dep) {
                    unique_prereqs.insert(dep);
                }
            }
            in_degrees.insert(node.clone(), unique_prereqs.len());
            for dep in unique_prereqs {
                dependents
                    .entry(dep.clone())
                    .or_default()
                    .push(node.clone());
            }
        }

        let makefile_ref: &Makefile = self.makefile;
        let target_statuses = Arc::new(Mutex::new(self.settled.lock().unwrap().clone()));
        let failed_error = Arc::new(Mutex::new(Option::<ExecutionError>::None));
        let abort_flag = Arc::new(AtomicBool::new(false));
        let num_rebuilt = Arc::new(AtomicUsize::new(0));
        let num_cached = Arc::new(AtomicUsize::new(0));
        let num_commands = Arc::new(AtomicUsize::new(0));

        let (task_tx, task_rx) = mpsc::channel::<String>();
        let (done_tx, done_rx) = mpsc::channel::<(String, TargetStatus, Vec<String>)>();

        let task_rx = Arc::new(Mutex::new(task_rx));
        let num_workers = self.config.jobs.max(1);

        let any_failed = std::thread::scope(|scope| {
            let mut worker_handles = Vec::new();
            let db_arc = Arc::clone(&self.db);
            let jobserver_arc = Arc::clone(&self.jobserver);
            let cache_arc = Arc::clone(&self.cache);
            for worker_id in 0..num_workers {
                let task_rx_clone = Arc::clone(&task_rx);
                let done_tx_clone = done_tx.clone();
                let failed_error_clone = Arc::clone(&failed_error);
                let abort_flag_clone = Arc::clone(&abort_flag);
                let makefile = makefile_ref;
                let config = self.config.clone();
                let target_statuses_clone = Arc::clone(&target_statuses);
                let num_commands_clone = Arc::clone(&num_commands);
                let num_cached_clone = Arc::clone(&num_cached);
                let cache_clone = Arc::clone(&cache_arc);
                let db_clone = Arc::clone(&db_arc);
                let jobserver_clone = Arc::clone(&jobserver_arc);
                let tracer_clone = self.tracer.clone();
                let remote_pool_clone = Arc::clone(&self.remote_pool);
                let recipe_env_clone = Arc::clone(&self.recipe_env);
                let ran_clone = &self.ran;
                let tui_clone = self.tui.clone();
                let worker_num = (worker_id + 1) as u32;

                let handle = scope.spawn(move || {
                    match_main_thread_qos();
                    let pid = std::process::id();
                    tracer_clone.record_metadata(
                        "thread_name",
                        pid,
                        worker_num,
                        "name",
                        &format!("Worker {worker_num}"),
                    );

                    loop {
                        if abort_flag_clone.load(Ordering::Relaxed) {
                            break;
                        }

                        let task = {
                            let rx = match task_rx_clone.lock() {
                                Ok(g) => g,
                                Err(_) => break,
                            };
                            match rx.recv() {
                                Ok(t) => t,
                                Err(_) => break,
                            }
                        };

                        if abort_flag_clone.load(Ordering::Relaxed) {
                            break;
                        }

                        tui_clone.target_started(worker_id, &task);
                        let mut start_ts = tracer_clone.start_micros();
                        let mut rule_start = Instant::now();

                        let rule = match makefile.get_rule(&task) {
                            Some(r) => r,
                            None => {
                                let resolved = makefile.resolve_path(&task);
                                let check_path = resolved.as_deref().unwrap_or(&task);
                                if let Some(mtime) = get_file_mtime(check_path) {
                                    let _ = done_tx_clone.send((
                                        task,
                                        TargetStatus::UpToDate(Some(mtime)),
                                        Vec::new(),
                                    ));
                                } else {
                                    let err = ExecutionError::NoRuleToMake(task.clone());
                                    *failed_error_clone.lock().unwrap() = Some(err);
                                    abort_flag_clone.store(true, Ordering::Relaxed);
                                    let _ = done_tx_clone.send((
                                        task,
                                        TargetStatus::Failed,
                                        Vec::new(),
                                    ));
                                }
                                continue;
                            }
                        };

                        let mut any_dep_rebuilt = false;
                        let mut any_dep_failed = false;
                        let mut newest_dep_mtime: Option<SystemTime> = None;

                        {
                            let statuses = target_statuses_clone.lock().unwrap();
                            for dep in &rule.prereqs {
                                if cached_mtime(dep).is_none()
                                    && makefile.resolve_path(dep).is_none()
                                {
                                    any_dep_rebuilt = true;
                                }
                                if let Some(status) = statuses.get(dep) {
                                    match status {
                                        TargetStatus::Failed => {
                                            any_dep_failed = true;
                                            break;
                                        }
                                        TargetStatus::Rebuilt(t) => {
                                            any_dep_rebuilt = true;
                                            if newest_dep_mtime.is_none_or(|cur| *t > cur) {
                                                newest_dep_mtime = Some(*t);
                                            }
                                        }
                                        TargetStatus::UpToDate(Some(t)) => {
                                            if newest_dep_mtime.is_none_or(|cur| *t > cur) {
                                                newest_dep_mtime = Some(*t);
                                            }
                                        }
                                        TargetStatus::UpToDate(None) => {}
                                    }
                                }
                            }
                        }

                        if any_dep_failed {
                            let _ = done_tx_clone.send((task, TargetStatus::Failed, Vec::new()));
                            continue;
                        }

                        let all_cmds = rule.commands.join("\n");
                        let decision = if config.use_hash {
                            let db_guard = db_clone.lock().unwrap();
                            evaluate_freshness_hash(
                                &rule,
                                config.always_make,
                                any_dep_rebuilt,
                                &all_cmds,
                                &db_guard,
                            )
                        } else {
                            evaluate_freshness_with(
                                &rule,
                                config.always_make,
                                any_dep_rebuilt,
                                newest_dep_mtime,
                                cached_mtime(&task),
                            )
                        };

                        let mut output_lines = Vec::new();
                        let mut build_failed = false;

                        let final_status = match decision {
                            FreshnessDecision::UpToDate(mtime) => {
                                TargetStatus::UpToDate(Some(mtime))
                            }
                            FreshnessDecision::NeedsRebuild(_) => {
                                let mtime_before = get_file_mtime(&task);
                                let _job_token = match jobserver_clone.acquire() {
                                    Ok(t) => t,
                                    Err(e) => {
                                        // Never stop silently: a lost jobserver
                                        // used to end the build "successfully"
                                        // with nothing built.
                                        *failed_error_clone.lock().unwrap() =
                                            Some(ExecutionError::CommandSpawnFailed(
                                                format!("jobserver token for '{task}'"),
                                                e.to_string(),
                                            ));
                                        abort_flag_clone.store(true, Ordering::Relaxed);
                                        let _ = done_tx_clone.send((
                                            task.clone(),
                                            TargetStatus::Failed,
                                            Vec::new(),
                                        ));
                                        break;
                                    }
                                };
                                // The job's duration starts once it holds a slot;
                                // waiting for a token is queueing, not work.
                                start_ts = tracer_clone.start_micros();
                                rule_start = Instant::now();

                                if config.touch_only {
                                    // GNU make's -t (see `touch_target`).
                                    if !rule.commands.is_empty() {
                                        let mf = jobserver_clone.child_makeflags(
                                            &std::env::var("MAKEFLAGS").unwrap_or_default(),
                                        );
                                        match touch_target(
                                            &rule,
                                            &task,
                                            makefile,
                                            &config,
                                            &recipe_env_clone,
                                            &mf,
                                            &mut output_lines,
                                        ) {
                                            Ok(true) => {
                                                ran_clone.lock().unwrap().push(task.clone());
                                                crate::decisions::mark_ran(&task);
                                                num_commands_clone.fetch_add(1, Ordering::Relaxed);
                                            }
                                            Ok(false) => {}
                                            Err(e) => {
                                                // As a failed recipe: without -k
                                                // the coordinator stops on it.
                                                if config.keep_going {
                                                    eprintln!("{e}");
                                                } else {
                                                    *failed_error_clone.lock().unwrap() = Some(e);
                                                    abort_flag_clone.store(true, Ordering::Relaxed);
                                                }
                                                build_failed = true;
                                            }
                                        }
                                    }
                                } else {
                                    let mut restored_from_cache = false;
                                    let cache_key = if config.cache && !config.dry_run {
                                        let k =
                                        crate::cache::ContentAddressableCache::compute_cache_key(
                                            &task,
                                            &rule.commands,
                                            &rule.prereqs,
                                        );
                                        if cache_clone.restore_artifact(&k, &task) {
                                            restored_from_cache = true;
                                            num_cached_clone.fetch_add(1, Ordering::Relaxed);
                                            if !config.silent {
                                                output_lines.push(format!(
                                                    "[maked] Restored {task} from cache ({k})"
                                                ));
                                            }
                                        }
                                        Some(k)
                                    } else {
                                        None
                                    };

                                    if !restored_from_cache && !config.question {
                                        let mut ran_remotely = false;
                                        if !remote_pool_clone.is_empty() && !config.dry_run {
                                            if let Some((worker, auth)) =
                                                remote_pool_clone.acquire_worker()
                                            {
                                                let mut expanded_cmds = Vec::new();
                                                for raw_cmd in &rule.commands {
                                                    let mut cmd_str = raw_cmd.trim_start();
                                                    while cmd_str.starts_with('@')
                                                        || cmd_str.starts_with('-')
                                                        || cmd_str.starts_with('+')
                                                    {
                                                        cmd_str = cmd_str[1..].trim_start();
                                                    }
                                                    expanded_cmds.push(expand_variables(
                                                        cmd_str,
                                                        makefile,
                                                        Some(&task),
                                                        &rule.prereqs,
                                                    ));
                                                }
                                                let input_files: Vec<std::path::PathBuf> = rule
                                                    .prereqs
                                                    .iter()
                                                    .map(std::path::PathBuf::from)
                                                    .collect();
                                                if let Ok(res) =
                                                    crate::distributed::dispatch_remote_build(
                                                        &worker,
                                                        &auth,
                                                        &task,
                                                        &expanded_cmds,
                                                        &input_files,
                                                    )
                                                {
                                                    if res.exit_code == 0 {
                                                        for (fname, bytes) in res.output_files {
                                                            let _ = std::fs::write(&fname, bytes);
                                                        }
                                                        if !res.stdout.is_empty() {
                                                            output_lines.push(
                                                                res.stdout.trim_end().to_string(),
                                                            );
                                                        }
                                                        num_commands_clone.fetch_add(
                                                            expanded_cmds.len(),
                                                            Ordering::Relaxed,
                                                        );
                                                        ran_remotely = true;
                                                    }
                                                }
                                            }
                                        }

                                        if !ran_remotely {
                                            for raw_cmd in &rule.commands {
                                                if abort_flag_clone.load(Ordering::Relaxed) {
                                                    build_failed = true;
                                                    break;
                                                }

                                                let (cmd_str, s1, i1, f1) =
                                                    recipe_prefixes(raw_cmd);
                                                let expanded = expand_variables(
                                                    cmd_str,
                                                    makefile,
                                                    Some(&task),
                                                    &rule.prereqs,
                                                );
                                                let (cmd, s2, i2, f2) = recipe_prefixes(&expanded);
                                                if cmd.trim().is_empty() {
                                                    continue;
                                                }
                                                {
                                                    let mut ran = ran_clone.lock().unwrap();
                                                    if !ran.iter().rev().take(8).any(|t| t == &task) {
                                                        ran.push(task.clone());
                                                        crate::decisions::mark_ran(&task);
                                                    }
                                                }
                                                let cmd = cmd.to_string();
                                                let force = f1 || f2 || mentions_make(raw_cmd);
                                                let run = !config.dry_run || force;
                                                let is_silent =
                                                    !config.dry_run && (config.silent || s1 || s2);
                                                let ignore_err = config.ignore_errors || i1 || i2;
                                                if !is_silent {
                                                    output_lines.push(cmd.clone());
                                                }
                                                if run {
                                                    let cur_mf = std::env::var("MAKEFLAGS")
                                                        .unwrap_or_default();
                                                    let child_mf =
                                                        jobserver_clone.child_makeflags(&cur_mf);
                                                    let res = run_command_output_fast(
                                                        &cmd,
                                                        Some(&child_mf),
                                                        &recipe_env_clone,
                                                    );

                                                    match res {
                                                        Ok(out) => {
                                                            if !out.stdout.is_empty() {
                                                                output_lines.push(
                                                                    String::from_utf8_lossy(
                                                                        &out.stdout,
                                                                    )
                                                                    .trim_end()
                                                                    .to_string(),
                                                                );
                                                            }
                                                            if !out.stderr.is_empty() {
                                                                output_lines.push(
                                                                    String::from_utf8_lossy(
                                                                        &out.stderr,
                                                                    )
                                                                    .trim_end()
                                                                    .to_string(),
                                                                );
                                                            }
                                                            if !out.status.success()
                                                                && ignore_err
                                                                && !config.silent
                                                            {
                                                                eprintln!(
                                                                    "make: [{task}] Error {} (ignored)",
                                                                    out.status.code().unwrap_or(1)
                                                                );
                                                            }
                                                            if !out.status.success() && !ignore_err
                                                            {
                                                                let code =
                                                                    out.status.code().unwrap_or(1);
                                                                let err =
                                                                    ExecutionError::BuildFailed(
                                                                        task.clone(),
                                                                        code,
                                                                    );
                                                                if config.keep_going {
                                                                    eprintln!("{err}");
                                                                } else {
                                                                    *failed_error_clone
                                                                        .lock()
                                                                        .unwrap() = Some(err);
                                                                    abort_flag_clone.store(
                                                                        true,
                                                                        Ordering::Relaxed,
                                                                    );
                                                                }
                                                                build_failed = true;
                                                                break;
                                                            }
                                                        }
                                                        Err(e) => {
                                                            if !ignore_err {
                                                                let err =
                                                                ExecutionError::CommandSpawnFailed(
                                                                    cmd,
                                                                    e.to_string(),
                                                                );
                                                                if config.keep_going {
                                                                    eprintln!("{err}");
                                                                } else {
                                                                    *failed_error_clone
                                                                        .lock()
                                                                        .unwrap() = Some(err);
                                                                    abort_flag_clone.store(
                                                                        true,
                                                                        Ordering::Relaxed,
                                                                    );
                                                                }
                                                                build_failed = true;
                                                                break;
                                                            }
                                                        }
                                                    }
                                                }
                                                num_commands_clone.fetch_add(1, Ordering::Relaxed);
                                            }
                                        }

                                        if !build_failed && !config.dry_run {
                                            if let Some(ref k) = cache_key {
                                                cache_clone.store_artifact(k, &task);
                                            }
                                        }
                                    }
                                }

                                if build_failed {
                                    TargetStatus::Failed
                                } else {
                                    if config.use_hash && !config.dry_run {
                                        let mut prereq_hashes = HashMap::new();
                                        for dep in &rule.prereqs {
                                            if let Ok(h) = crate::hash::sha256_file(dep) {
                                                prereq_hashes
                                                    .insert(dep.clone(), crate::hash::to_hex(&h));
                                            }
                                        }
                                        let target_hash = crate::hash::sha256_file(&task)
                                            .map(|h| crate::hash::to_hex(&h))
                                            .unwrap_or_default();
                                        let recipe_hash = crate::hash::to_hex(
                                            &crate::hash::sha256_bytes(all_cmds.as_bytes()),
                                        );
                                        db_clone.lock().unwrap().update_record(
                                            task.clone(),
                                            crate::hash::TargetRecord {
                                                target_hash,
                                                recipe_hash,
                                                prereq_hashes,
                                            },
                                        );
                                    }
                                    forget_mtime(&task);
                                    let mtime_after = get_file_mtime(&task);
                                    match (mtime_before, mtime_after) {
                                        (Some(b), Some(a))
                                            if a == b && !rule.is_phony && !config.dry_run =>
                                        {
                                            TargetStatus::UpToDate(Some(a))
                                        }
                                        _ => TargetStatus::Rebuilt(
                                            mtime_after.unwrap_or_else(SystemTime::now),
                                        ),
                                    }
                                }
                            }
                        };

                        let dur_us = rule_start.elapsed().as_micros() as u64;
                        let mut trace_args = HashMap::new();
                        trace_args.insert(
                            "status".to_string(),
                            match &final_status {
                                TargetStatus::Rebuilt(_) => "rebuilt".to_string(),
                                TargetStatus::UpToDate(_) => "up_to_date".to_string(),
                                TargetStatus::Failed => "failed".to_string(),
                            },
                        );
                        tracer_clone.record_complete(
                            task.clone(),
                            "rule",
                            start_ts,
                            dur_us,
                            pid,
                            worker_num,
                            trace_args,
                        );

                        let _ = done_tx_clone.send((task, final_status, output_lines));
                    }
                });
                worker_handles.push(handle);
            }
            drop(done_tx);

            // Ready targets wait in a priority queue and go to a worker only when
            // one is free, longest remaining path (bottom level) first. Durations
            // come from earlier runs; with none, the hop count stands in.
            // Computed on first use, so null builds (nothing to dispatch) skip it.
            let priority: std::cell::OnceCell<HashMap<String, u64>> = std::cell::OnceCell::new();
            let rank = |t: &String| {
                // Source files without a rule only need a stat; they never hold
                // up a recipe, so they go last and do not trigger the analysis.
                if !self.makefile.rules.contains_key(t) && self.makefile.get_rule(t).is_none() {
                    return 0;
                }
                priority
                    .get_or_init(|| {
                        let history =
                            crate::history::DurationLog::load(crate::history::LOG_FILENAME);
                        crate::history::bottom_levels(&reachable, &dependents, &history)
                    })
                    .get(t)
                    .copied()
                    .unwrap_or(1)
            };
            let mut ready: BinaryHeap<(u64, Reverse<String>)> = BinaryHeap::new();
            let mut idle_workers = num_workers;

            let mut ready_queue = VecDeque::new();
            for (node, deg) in &in_degrees {
                if *deg == 0 {
                    ready_queue.push_back(node.clone());
                }
            }

            // Targets the coordinator settles itself (already up to date) skip the
            // worker round-trip; on a dependency chain that hop was most of a
            // null build's cost.
            let mut settled: VecDeque<(String, TargetStatus, Vec<String>)> = VecDeque::new();
            for task in ready_queue.drain(..) {
                crate::decisions::note_before(&task);
                let pre = self.settle_inline(&task, &target_statuses.lock().unwrap());
                match pre {
                    Some(status) => settled.push_back((task, status, Vec::new())),
                    None => ready.push((rank(&task), Reverse(task))),
                }
            }

            let mut remaining_targets = reachable.len();
            let mut any_failed = false;

            while remaining_targets > 0 {
                while idle_workers > 0 {
                    match ready.pop() {
                        Some((_, Reverse(task))) => {
                            let _ = task_tx.send(task);
                            idle_workers -= 1;
                        }
                        None => break,
                    }
                }
                let next = match settled.pop_front() {
                    Some(item) => Ok(item),
                    None => {
                        let msg = done_rx.recv();
                        idle_workers += 1;
                        msg
                    }
                };
                match next {
                    Ok((finished_node, status, logs)) => {
                        if !logs.is_empty() {
                            for line in logs {
                                println!("{line}");
                            }
                        }

                        match status {
                            TargetStatus::Rebuilt(_) => {
                                num_rebuilt.fetch_add(1, Ordering::Relaxed);
                            }
                            TargetStatus::Failed => {
                                if !self.config.keep_going {
                                    abort_flag.store(true, Ordering::Relaxed);
                                }
                            }
                            TargetStatus::UpToDate(_) => {}
                        }

                        // A target settled before this run (remaking the
                        // makefiles) is recorded once, then.
                        let was_settled = target_statuses
                            .lock()
                            .unwrap()
                            .insert(finished_node.clone(), status.clone())
                            .is_some();
                        if !was_settled {
                            self.record_decision(&finished_node, &status);
                        }

                        remaining_targets -= 1;

                        // -k: nothing that depends on a failed target can be made;
                        // count those as finished (failed) so the build can end.
                        if matches!(status, TargetStatus::Failed) && self.config.keep_going {
                            any_failed = true;
                            let mut stack = vec![finished_node.clone()];
                            let mut statuses = target_statuses.lock().unwrap();
                            while let Some(n) = stack.pop() {
                                for d in dependents.get(&n).map(Vec::as_slice).unwrap_or(&[]) {
                                    if !statuses.contains_key(d) {
                                        statuses.insert(d.clone(), TargetStatus::Failed);
                                        self.record_decision(d, &TargetStatus::Failed);
                                        remaining_targets -= 1;
                                        stack.push(d.clone());
                                    }
                                }
                            }
                        }
                        let (status_str, was_cached) = match &status {
                            TargetStatus::Rebuilt(_) => ("rebuilt", false),
                            TargetStatus::UpToDate(_) => ("up_to_date", false),
                            TargetStatus::Failed => ("failed", false),
                        };
                        self.tui
                            .target_finished(0, &finished_node, status_str, was_cached);

                        if !matches!(status, TargetStatus::Failed)
                            && !abort_flag.load(Ordering::Relaxed)
                        {
                            if let Some(deps) = dependents.get(&finished_node) {
                                for dep in deps {
                                    if let Some(deg) = in_degrees.get_mut(dep) {
                                        *deg = deg.saturating_sub(1);
                                        if *deg == 0 {
                                            crate::decisions::note_before(dep);
                                            let pre = self.settle_inline(
                                                dep,
                                                &target_statuses.lock().unwrap(),
                                            );
                                            match pre {
                                                Some(st) => {
                                                    settled.push_back((dep.clone(), st, Vec::new()))
                                                }
                                                None => {
                                                    ready.push((rank(dep), Reverse(dep.clone())))
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(_) => break,
                }

                if abort_flag.load(Ordering::Relaxed) && failed_error.lock().unwrap().is_some() {
                    break;
                }
            }

            drop(task_tx);
            for h in worker_handles {
                let _ = h.join();
            }
            any_failed
        });

        if let Some(err) = failed_error.lock().unwrap().take() {
            return Err(err);
        }
        if any_failed {
            self.save_duration_log();
            let statuses = target_statuses.lock().unwrap();
            self.report_not_remade(root, |g| {
                !matches!(
                    statuses.get(g),
                    Some(TargetStatus::UpToDate(_) | TargetStatus::Rebuilt(_))
                )
            });
            return Err(ExecutionError::NotRemade);
        }

        let rebuilt = num_rebuilt.load(Ordering::Relaxed);
        let cached = num_cached.load(Ordering::Relaxed);
        let total = reachable.len();
        let cmds = num_commands.load(Ordering::Relaxed);

        if self.config.use_hash && !self.config.dry_run {
            let _ = self
                .db
                .lock()
                .unwrap()
                .save(crate::hash::BuildDatabase::DB_FILENAME);
        }
        self.save_duration_log();
        if let Some(ref path) = self.config.trace_file {
            let _ = self.tracer.save_to_file(path);
        }
        let (crit_us, crit_path) = self.tracer.compute_critical_path(&self.graph.adj, root);
        self.tui.finish();

        Ok(ExecutionStats {
            total_evaluated: total,
            targets_rebuilt: rebuilt,
            targets_cached: cached,
            targets_up_to_date: total - rebuilt,
            commands_executed: cmds,
            elapsed_wall_time: start_time.elapsed(),
            critical_path_duration: std::time::Duration::from_micros(crit_us),
            critical_path: crit_path,
            schedule: self.tracer.schedule_bounds(self.config.jobs, crit_us),
        })
    }

    /// Merge the recipe durations measured in this run into `.maked_log`,
    /// which orders the next parallel run. Skipped when nothing ran a recipe
    /// (`-n`, `-t`, `-q`, null builds); a write failure only loses history.
    fn save_duration_log(&self) {
        if self.config.dry_run || self.config.touch_only || self.config.question {
            return;
        }
        let measured = self.tracer.rebuilt_durations();
        if measured.is_empty() {
            return;
        }
        let mut log = crate::history::DurationLog::load(crate::history::LOG_FILENAME);
        log.merge(measured);
        let _ = log.save(crate::history::LOG_FILENAME);
    }

    /// Decide on the coordinator thread whether `target` is already up to
    /// date, given its finished prerequisites. Returns `None` whenever the
    /// target needs a worker: it must be rebuilt, has no rule, has a failed
    /// prerequisite, or freshness depends on the hash database.
    fn settle_inline(
        &self,
        target: &str,
        statuses: &crate::fxhash::FxHashMap<String, TargetStatus>,
    ) -> Option<TargetStatus> {
        if let Some(done) = statuses.get(target) {
            return Some(done.clone());
        }
        if self.config.use_hash || self.config.always_make {
            return None;
        }
        let rule = self.makefile.get_rule(target)?;
        let mut any_dep_rebuilt = false;
        let mut newest_dep_mtime: Option<SystemTime> = None;
        for dep in &rule.prereqs {
            if cached_mtime(dep).is_none() && self.makefile.resolve_path(dep).is_none() {
                any_dep_rebuilt = true;
            }
            match statuses.get(dep) {
                Some(TargetStatus::Failed) => return None,
                Some(TargetStatus::Rebuilt(t)) => {
                    any_dep_rebuilt = true;
                    if newest_dep_mtime.is_none_or(|cur| *t > cur) {
                        newest_dep_mtime = Some(*t);
                    }
                }
                Some(TargetStatus::UpToDate(Some(t))) => {
                    if newest_dep_mtime.is_none_or(|cur| *t > cur) {
                        newest_dep_mtime = Some(*t);
                    }
                }
                Some(TargetStatus::UpToDate(None)) | None => {}
            }
        }
        match evaluate_freshness_with(
            &rule,
            false,
            any_dep_rebuilt,
            newest_dep_mtime,
            cached_mtime(target),
        ) {
            FreshnessDecision::UpToDate(mtime) => Some(TargetStatus::UpToDate(Some(mtime))),
            FreshnessDecision::NeedsRebuild(_) => None,
        }
    }

    pub fn execute(&self, root: &str) -> Result<ExecutionStats, ExecutionError> {
        crate::ast::enter_execution_phase();
        // Which target each one inherits target-specific variables from.
        self.makefile.record_inheritance(root);
        // mtimes from a previous run in this process may be stale.
        if let Some(cache) = MTIMES.get() {
            cache.lock().unwrap().clear();
        }
        // -q runs nothing, so it gains nothing from workers; the sequential
        // path is the one that answers it.
        if self.config.jobs > 1 && !self.config.question {
            self.execute_parallel(root)
        } else {
            self.execute_sequential(root)
        }
    }
}

#[cfg(test)]
mod split_tests {
    use super::split_simple_command as split;

    #[test]
    fn test_split_simple_command() {
        let v = |xs: &[&str]| Some(xs.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(split("touch a b"), v(&["touch", "a", "b"]));
        assert_eq!(
            split("sh -c 'uname -s 2>/dev/null || echo not'"),
            v(&["sh", "-c", "uname -s 2>/dev/null || echo not"])
        );
        assert_eq!(split("echo \"a b\" c''d ''"), v(&["echo", "a b", "cd", ""]));
        assert_eq!(split("echo \"$HOME\""), None);
        assert_eq!(split("echo a > f"), None);
        assert_eq!(split("echo 'unterminated"), None);
        assert_eq!(split("cd dir"), None);
        assert_eq!(split("VAR=1 cmd"), None);
        assert_eq!(split(""), None);
    }

    #[test]
    fn test_noop_lines() {
        assert!(super::is_noop_line(":"));
        assert!(super::is_noop_line(": no custom templates yet"));
        assert!(!super::is_noop_line(": > truncated"));
        assert!(!super::is_noop_line(":foo"));
        assert!(!super::is_noop_line("true"));
    }
}
