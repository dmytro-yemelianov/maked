use crate::ast::Makefile;
use crate::freshness::{
    FreshnessDecision, evaluate_freshness, evaluate_freshness_hash, get_file_mtime,
};
use crate::graph::DependencyGraph;
use crate::parser::expand_variables;
use std::collections::{HashMap, HashSet, VecDeque};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Instant, SystemTime};

#[derive(Debug, Clone, Default)]
pub struct ExecutionConfig {
    pub jobs: usize,
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
}

#[derive(Debug, Clone)]
pub enum ExecutionError {
    BuildFailed(String, i32),
    NoRuleToMake(String),
    CommandSpawnFailed(String, String),
}

impl std::fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BuildFailed(target, code) => {
                write!(f, "make: *** [{target}] Error {code}")
            }
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
}

/// Cross-platform shell command builder
pub fn create_shell_command(cmd: &str) -> Command {
    #[cfg(unix)]
    {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let mut c = Command::new(shell);
        if !cmd.is_empty() {
            c.arg("-c").arg(cmd);
        }
        c
    }
    #[cfg(windows)]
    {
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

/// Fast-path process execution: bypasses shell if no shell metacharacters exist!
fn run_command_status_fast(
    cmd: &str,
    makeflags: Option<&str>,
) -> std::io::Result<std::process::ExitStatus> {
    let needs_shell = cmd.chars().any(|c| {
        matches!(
            c,
            '*' | '?'
                | '['
                | ']'
                | '~'
                | '='
                | '|'
                | '&'
                | ';'
                | '<'
                | '>'
                | '('
                | ')'
                | '$'
                | '`'
                | '\\'
                | '"'
                | '\''
                | '\n'
        )
    });

    let mut command = if !needs_shell {
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        if let Some((program, args)) = parts.split_first() {
            let mut c = Command::new(program);
            c.args(args);
            c
        } else {
            create_shell_command("")
        }
    } else {
        create_shell_command(cmd)
    };

    if let Some(mf) = makeflags {
        command.env("MAKEFLAGS", mf);
    }
    command.status()
}

fn run_command_output_fast(
    cmd: &str,
    makeflags: Option<&str>,
) -> std::io::Result<std::process::Output> {
    let needs_shell = cmd.chars().any(|c| {
        matches!(
            c,
            '*' | '?'
                | '['
                | ']'
                | '~'
                | '='
                | '|'
                | '&'
                | ';'
                | '<'
                | '>'
                | '('
                | ')'
                | '$'
                | '`'
                | '\\'
                | '"'
                | '\''
                | '\n'
        )
    });

    let mut command = if !needs_shell {
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        if let Some((program, args)) = parts.split_first() {
            let mut c = Command::new(program);
            c.args(args);
            c
        } else {
            create_shell_command("")
        }
    } else {
        create_shell_command(cmd)
    };

    if let Some(mf) = makeflags {
        command.env("MAKEFLAGS", mf);
    }
    command.output()
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
                .unwrap_or_else(|| std::path::PathBuf::from(".makeyd_cache")),
        };
        let cache = Arc::new(crate::cache::ContentAddressableCache::new(cache_config));
        let remote_pool = Arc::new(crate::distributed::RemoteWorkerPool::new(
            config.remote_workers.clone(),
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
            tui,
        }
    }

    /// Single-threaded post-order recursive execution
    pub fn execute_sequential(&self, root: &str) -> Result<ExecutionStats, ExecutionError> {
        let start_time = Instant::now();
        let mut target_statuses: HashMap<String, TargetStatus> = HashMap::new();
        let mut stats = ExecutionStats {
            total_evaluated: 0,
            targets_rebuilt: 0,
            targets_cached: 0,
            targets_up_to_date: 0,
            commands_executed: 0,
            elapsed_wall_time: std::time::Duration::ZERO,
            critical_path_duration: std::time::Duration::ZERO,
            critical_path: Vec::new(),
        };

        let status = self.doname(root, &mut target_statuses, &mut stats)?;
        if matches!(status, TargetStatus::Failed) {
            return Err(ExecutionError::BuildFailed(root.to_string(), 1));
        }
        if self.config.use_hash && !self.config.dry_run {
            let _ = self
                .db
                .lock()
                .unwrap()
                .save(crate::hash::BuildDatabase::DB_FILENAME);
        }
        if let Some(ref path) = self.config.trace_file {
            let _ = self.tracer.save_to_file(path);
        }
        let (crit_us, crit_path) = self.tracer.compute_critical_path(&self.graph.adj, root);
        stats.critical_path_duration = std::time::Duration::from_micros(crit_us);
        stats.critical_path = crit_path;
        stats.elapsed_wall_time = start_time.elapsed();
        self.tui.finish();
        Ok(stats)
    }

    fn doname(
        &self,
        target: &str,
        statuses: &mut HashMap<String, TargetStatus>,
        stats: &mut ExecutionStats,
    ) -> Result<TargetStatus, ExecutionError> {
        if let Some(status) = statuses.get(target) {
            return Ok(status.clone());
        }

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

        for dep in &rule.prereqs {
            let dep_status = self.doname(dep, statuses, stats)?;
            if get_file_mtime(dep).is_none() && self.makefile.resolve_path(dep).is_none() {
                any_dep_rebuilt = true;
            }
            match dep_status {
                TargetStatus::Failed => {
                    statuses.insert(target.to_string(), TargetStatus::Failed);
                    return Ok(TargetStatus::Failed);
                }
                TargetStatus::Rebuilt(new_time) => {
                    any_dep_rebuilt = true;
                    if newest_dep_mtime.map_or(true, |t| new_time > t) {
                        newest_dep_mtime = Some(new_time);
                    }
                }
                TargetStatus::UpToDate(Some(dep_mtime)) => {
                    if newest_dep_mtime.map_or(true, |t| dep_mtime > t) {
                        newest_dep_mtime = Some(dep_mtime);
                    }
                }
                TargetStatus::UpToDate(None) => {}
            }
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
            evaluate_freshness(
                &rule,
                self.config.always_make,
                any_dep_rebuilt,
                newest_dep_mtime,
            )
        };

        let final_status = match freshness {
            FreshnessDecision::UpToDate(mtime) => {
                stats.targets_up_to_date += 1;
                TargetStatus::UpToDate(Some(mtime))
            }
            FreshnessDecision::NeedsRebuild(_reason) => {
                if self.config.question {
                    stats.targets_rebuilt += 1;
                    return Ok(TargetStatus::Rebuilt(SystemTime::now()));
                }

                if self.config.touch_only {
                    let _ = std::fs::OpenOptions::new()
                        .create(true)
                        .write(true)
                        .truncate(false)
                        .open(target);
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
                            println!("[makeyd] Restored {target} from cache ({k})");
                        }
                    }
                    Some(k)
                } else {
                    None
                };

                if !restored_from_cache {
                    let mut ran_remotely = false;
                    if !self.remote_pool.is_empty() && !self.config.dry_run {
                        if let Some(worker) = self.remote_pool.acquire_worker() {
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
                            let mut cmd_str = raw_cmd.trim_start();
                            let mut is_silent = self.config.silent;
                            let mut ignore_err = self.config.ignore_errors;

                            while cmd_str.starts_with('@') || cmd_str.starts_with('-') {
                                if cmd_str.starts_with('@') {
                                    if !self.config.dry_run {
                                        is_silent = true;
                                    }
                                    cmd_str = cmd_str[1..].trim_start();
                                } else if cmd_str.starts_with('-') {
                                    ignore_err = true;
                                    cmd_str = cmd_str[1..].trim_start();
                                }
                            }

                            let cmd = expand_variables(
                                cmd_str,
                                self.makefile,
                                Some(target),
                                &rule.prereqs,
                            );
                            if !is_silent {
                                println!("{cmd}");
                            }
                            if !self.config.dry_run {
                                let cur_mf = std::env::var("MAKEFLAGS").unwrap_or_default();
                                let child_mf = self.jobserver.child_makeflags(&cur_mf);
                                let status = run_command_status_fast(&cmd, Some(&child_mf))
                                    .map_err(|e| {
                                        ExecutionError::CommandSpawnFailed(
                                            cmd.clone(),
                                            e.to_string(),
                                        )
                                    })?;

                                if !status.success() && !ignore_err {
                                    let code = status.code().unwrap_or(1);
                                    statuses.insert(target.to_string(), TargetStatus::Failed);
                                    return Err(ExecutionError::BuildFailed(
                                        target.to_string(),
                                        code,
                                    ));
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
                let current_time = get_file_mtime(target).unwrap_or_else(SystemTime::now);
                TargetStatus::Rebuilt(current_time)
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

        let mut in_degrees: HashMap<String, usize> = HashMap::new();
        let mut dependents: HashMap<String, Vec<String>> = HashMap::new();

        for node in &reachable {
            let prereqs = if let Some(rule) = self.makefile.get_rule(node) {
                rule.prereqs
            } else {
                self.graph.adj.get(node).cloned().unwrap_or_default()
            };

            let mut unique_prereqs = HashSet::new();
            for dep in prereqs {
                if reachable.contains(&dep) {
                    unique_prereqs.insert(dep);
                }
            }
            in_degrees.insert(node.clone(), unique_prereqs.len());
            for dep in unique_prereqs {
                dependents.entry(dep).or_default().push(node.clone());
            }
        }

        let makefile_arc = Arc::new(self.makefile.clone());
        let target_statuses = Arc::new(Mutex::new(HashMap::<String, TargetStatus>::new()));
        let failed_error = Arc::new(Mutex::new(Option::<ExecutionError>::None));
        let abort_flag = Arc::new(AtomicBool::new(false));
        let num_rebuilt = Arc::new(AtomicUsize::new(0));
        let num_cached = Arc::new(AtomicUsize::new(0));
        let num_commands = Arc::new(AtomicUsize::new(0));

        let (task_tx, task_rx) = mpsc::channel::<String>();
        let (done_tx, done_rx) = mpsc::channel::<(String, TargetStatus, Vec<String>)>();

        let task_rx = Arc::new(Mutex::new(task_rx));
        let num_workers = self.config.jobs.max(1);

        let mut worker_handles = Vec::new();
        let db_arc = Arc::clone(&self.db);
        let jobserver_arc = Arc::clone(&self.jobserver);
        let cache_arc = Arc::clone(&self.cache);
        for worker_id in 0..num_workers {
            let task_rx_clone = Arc::clone(&task_rx);
            let done_tx_clone = done_tx.clone();
            let failed_error_clone = Arc::clone(&failed_error);
            let abort_flag_clone = Arc::clone(&abort_flag);
            let makefile = Arc::clone(&makefile_arc);
            let config = self.config.clone();
            let target_statuses_clone = Arc::clone(&target_statuses);
            let num_commands_clone = Arc::clone(&num_commands);
            let num_cached_clone = Arc::clone(&num_cached);
            let cache_clone = Arc::clone(&cache_arc);
            let db_clone = Arc::clone(&db_arc);
            let jobserver_clone = Arc::clone(&jobserver_arc);
            let tracer_clone = self.tracer.clone();
            let remote_pool_clone = Arc::clone(&self.remote_pool);
            let tui_clone = self.tui.clone();
            let worker_num = (worker_id + 1) as u32;

            let handle = std::thread::spawn(move || {
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
                    let start_ts = tracer_clone.start_micros();
                    let rule_start = Instant::now();

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
                                let _ =
                                    done_tx_clone.send((task, TargetStatus::Failed, Vec::new()));
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
                            if get_file_mtime(dep).is_none() && makefile.resolve_path(dep).is_none()
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
                                        if newest_dep_mtime.map_or(true, |cur| *t > cur) {
                                            newest_dep_mtime = Some(*t);
                                        }
                                    }
                                    TargetStatus::UpToDate(Some(t)) => {
                                        if newest_dep_mtime.map_or(true, |cur| *t > cur) {
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
                        evaluate_freshness(
                            &rule,
                            config.always_make,
                            any_dep_rebuilt,
                            newest_dep_mtime,
                        )
                    };

                    let mut output_lines = Vec::new();
                    let mut build_failed = false;

                    let final_status = match decision {
                        FreshnessDecision::UpToDate(mtime) => TargetStatus::UpToDate(Some(mtime)),
                        FreshnessDecision::NeedsRebuild(_) => {
                            let _job_token = match jobserver_clone.acquire() {
                                Ok(t) => t,
                                Err(_) => {
                                    abort_flag_clone.store(true, Ordering::Relaxed);
                                    break;
                                }
                            };

                            if config.touch_only {
                                let _ = std::fs::OpenOptions::new()
                                    .create(true)
                                    .write(true)
                                    .truncate(false)
                                    .open(&task);
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
                                                "[makeyd] Restored {task} from cache ({k})"
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
                                        if let Some(worker) = remote_pool_clone.acquire_worker() {
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
                                                    &makefile,
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

                                            let mut cmd_str = raw_cmd.trim_start();
                                            let mut is_silent = config.silent;
                                            let mut ignore_err = config.ignore_errors;

                                            while cmd_str.starts_with('@')
                                                || cmd_str.starts_with('-')
                                            {
                                                if cmd_str.starts_with('@') {
                                                    if !config.dry_run {
                                                        is_silent = true;
                                                    }
                                                    cmd_str = cmd_str[1..].trim_start();
                                                } else if cmd_str.starts_with('-') {
                                                    ignore_err = true;
                                                    cmd_str = cmd_str[1..].trim_start();
                                                }
                                            }

                                            let cmd = expand_variables(
                                                cmd_str,
                                                &makefile,
                                                Some(&task),
                                                &rule.prereqs,
                                            );
                                            if !is_silent {
                                                output_lines.push(cmd.clone());
                                            }
                                            if !config.dry_run {
                                                let cur_mf =
                                                    std::env::var("MAKEFLAGS").unwrap_or_default();
                                                let child_mf =
                                                    jobserver_clone.child_makeflags(&cur_mf);
                                                let res =
                                                    run_command_output_fast(&cmd, Some(&child_mf));

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
                                                        if !out.status.success() && !ignore_err {
                                                            let code =
                                                                out.status.code().unwrap_or(1);
                                                            *failed_error_clone.lock().unwrap() =
                                                                Some(ExecutionError::BuildFailed(
                                                                    task.clone(),
                                                                    code,
                                                                ));
                                                            abort_flag_clone
                                                                .store(true, Ordering::Relaxed);
                                                            build_failed = true;
                                                            break;
                                                        }
                                                    }
                                                    Err(e) => {
                                                        if !ignore_err {
                                                            *failed_error_clone.lock().unwrap() =
                                                                Some(ExecutionError::CommandSpawnFailed(cmd, e.to_string()));
                                                            abort_flag_clone
                                                                .store(true, Ordering::Relaxed);
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
                                let current_time =
                                    get_file_mtime(&task).unwrap_or_else(SystemTime::now);
                                TargetStatus::Rebuilt(current_time)
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

        let mut ready_queue = VecDeque::new();
        for (node, deg) in &in_degrees {
            if *deg == 0 {
                ready_queue.push_back(node.clone());
            }
        }

        for task in ready_queue.drain(..) {
            let _ = task_tx.send(task);
        }

        let mut remaining_targets = reachable.len();

        while remaining_targets > 0 {
            match done_rx.recv() {
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
                            abort_flag.store(true, Ordering::Relaxed);
                        }
                        TargetStatus::UpToDate(_) => {}
                    }

                    target_statuses
                        .lock()
                        .unwrap()
                        .insert(finished_node.clone(), status.clone());

                    remaining_targets -= 1;
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
                                        let _ = task_tx.send(dep.clone());
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

        if let Some(err) = failed_error.lock().unwrap().take() {
            return Err(err);
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
        })
    }

    pub fn execute(&self, root: &str) -> Result<ExecutionStats, ExecutionError> {
        if self.config.jobs > 1 {
            self.execute_parallel(root)
        } else {
            self.execute_sequential(root)
        }
    }
}
