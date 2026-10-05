use makeyd::executor::{ExecutionConfig, Executor};
use makeyd::graph::DependencyGraph;
use makeyd::parser::parse_makefile_content;
use std::env;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

fn print_help() {
    println!(
        "Usage: makeyd [options] [target] ...\n\
         Options:\n  \
           -f FILE       Read FILE as a makefile\n  \
           -j [N]        Allow N jobs at once; default is 1 (or num_cpus if N omitted)\n  \
           -n, --dry-run Don't actually run any recipe; just print them\n  \
           -B            Unconditionally make all targets\n  \
           -s, --silent  Don't echo recipe commands\n  \
           -q, --question 'Question' mode: return exit code 0 if up to date, 1 otherwise\n  \
           -C DIR        Change to directory DIR before doing anything\n  \
           --profile     Display execution and profiling statistics\n  \
           --trace=FILE  Write Chrome Trace / Perfetto JSON timeline to FILE\n  \
           --hash        Use SHA-256 cryptographic content hashes instead of timestamps\n  \
           --cache       Enable content-addressable artifact caching\n  \
           --cache-dir=D Set cache directory (default .makeyd_cache)\n  \
           --emit-ninja[=F] Transpile Makefile to Ninja build file (default build.ninja)\n  \
           --emit-compdb[=F] Generate Clang JSON Compilation Database (default compile_commands.json)\n  \
           --worker-listen=A Start remote build worker daemon on TCP address A (loopback only;\n  \
                         needs MAKEYD_WORKER_TOKEN_FILE or MAKEYD_WORKER_TOKEN)\n  \
           --worker-allow-remote Let --worker-listen accept non-loopback addresses\n  \
           --remote-workers=W Dispatch compilation tasks across remote worker addresses W\n  \
           --tui         Enable live terminal execution dashboard\n  \
           -h, --help    Print this message and exit"
    );
}

/// Graph evaluation (cycle check, `doname`) recurses once per dependency
/// level, and the default 8 MiB main stack overflows on chains a few
/// thousand targets deep. Run on a thread with a large reserved stack; it is
/// virtual memory, so only the depth actually used is committed.
const MAIN_STACK_BYTES: usize = 256 * 1024 * 1024;

fn main() -> ExitCode {
    std::thread::Builder::new()
        .name("makeyd".to_string())
        .stack_size(MAIN_STACK_BYTES)
        .spawn(real_main)
        .expect("failed to spawn main thread")
        .join()
        .unwrap_or(ExitCode::from(2))
}

fn real_main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let mut makefile_path = "Makefile".to_string();
    let mut target_names: Vec<String> = Vec::new();
    let mut jobs = 1usize;
    let mut jobs_explicit = false;
    let mut dry_run = false;
    let mut always_make = false;
    let mut silent = false;
    let mut question = false;
    let mut use_hash = false;
    let mut profile = false;
    let mut ignore_errors = false;
    let mut touch_only = false;
    let mut env_overrides = false;
    let mut print_database = false;
    let mut chdir: Option<String> = None;
    let mut cli_vars: Vec<(String, String)> = Vec::new();
    let mut jobserver_auth: Option<String> = None;
    let mut trace_file: Option<String> = None;
    let mut cache = false;
    let mut cache_dir: Option<String> = None;
    let mut emit_ninja: Option<String> = None;
    let mut emit_compdb: Option<String> = None;
    let mut worker_listen: Option<String> = None;
    let mut worker_allow_remote = false;
    let mut remote_workers: Vec<String> = Vec::new();
    let mut tui = false;

    let mut i = 1;
    while i < args.len() {
        let arg = &args[i];
        if arg == "-h" || arg == "--help" {
            print_help();
            return ExitCode::SUCCESS;
        } else if arg == "-v" || arg == "--version" {
            println!(
                "makeyd {}\nPOSIX IEEE Std 1003.1 conforming Make with Lean 4 formal semantics",
                env!("CARGO_PKG_VERSION")
            );
            return ExitCode::SUCCESS;
        } else if arg == "-b" || arg == "-m" {
            // POSIX compatibility no-ops
        } else if arg == "-e" || arg == "--environment-overrides" {
            env_overrides = true;
        } else if arg == "-i" || arg == "--ignore-errors" {
            ignore_errors = true;
        } else if arg == "-k" || arg == "--keep-going" {
            // Keep going on errors (POSIX compatibility)
        } else if arg == "-p" || arg == "--print-data-base" {
            print_database = true;
        } else if arg == "-t" || arg == "--touch" {
            touch_only = true;
        } else if arg == "--hash" {
            use_hash = true;
        } else if arg == "-f" && i + 1 < args.len() {
            i += 1;
            makefile_path = args[i].clone();
        } else if arg == "-j" {
            jobs_explicit = true;
            if i + 1 < args.len()
                && !args[i + 1].starts_with('-')
                && args[i + 1].parse::<usize>().is_ok()
            {
                i += 1;
                jobs = args[i].parse().unwrap();
            } else {
                jobs = std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(4);
            }
        } else if arg.starts_with("-j") {
            jobs_explicit = true;
            let num_str = &arg[2..];
            if num_str.is_empty() {
                jobs = std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(4);
            } else if let Ok(n) = num_str.parse::<usize>() {
                jobs = n;
            }
        } else if arg == "-n" || arg == "--dry-run" {
            dry_run = true;
        } else if arg == "-B" || arg == "--always-make" {
            always_make = true;
        } else if arg == "-s" || arg == "--silent" || arg == "--quiet" {
            silent = true;
        } else if arg == "-q" || arg == "--question" {
            question = true;
        } else if arg == "--profile" {
            profile = true;
        } else if arg == "-C" && i + 1 < args.len() {
            i += 1;
            chdir = Some(args[i].clone());
        } else if arg.starts_with("--trace=") {
            trace_file = Some(arg["--trace=".len()..].to_string());
        } else if arg == "--trace" && i + 1 < args.len() {
            i += 1;
            trace_file = Some(args[i].clone());
        } else if arg.starts_with("--jobserver-auth=") {
            jobserver_auth = Some(arg["--jobserver-auth=".len()..].to_string());
        } else if arg.starts_with("--jobserver-fds=") {
            jobserver_auth = Some(arg["--jobserver-fds=".len()..].to_string());
        } else if arg == "--cache" {
            cache = true;
        } else if let Some(dir) = arg.strip_prefix("--cache-dir=") {
            cache_dir = Some(dir.to_string());
            cache = true;
        } else if arg == "--cache-dir" && i + 1 < args.len() {
            i += 1;
            cache_dir = Some(args[i].clone());
            cache = true;
        } else if arg == "--emit-ninja" {
            emit_ninja = Some("build.ninja".to_string());
        } else if let Some(path) = arg.strip_prefix("--emit-ninja=") {
            emit_ninja = Some(path.to_string());
        } else if arg == "--emit-compdb" {
            emit_compdb = Some("compile_commands.json".to_string());
        } else if let Some(path) = arg.strip_prefix("--emit-compdb=") {
            emit_compdb = Some(path.to_string());
        } else if let Some(addr) = arg.strip_prefix("--worker-listen=") {
            worker_listen = Some(addr.to_string());
        } else if arg == "--worker-allow-remote" {
            worker_allow_remote = true;
        } else if let Some(workers) = arg.strip_prefix("--remote-workers=") {
            for w in workers.split(',') {
                let trimmed = w.trim();
                if !trimmed.is_empty() {
                    remote_workers.push(trimmed.to_string());
                }
            }
        } else if arg == "--tui" {
            tui = true;
        } else if !arg.starts_with('-') {
            if let Some(eq_idx) = arg.find('=') {
                let k = arg[..eq_idx].trim().to_string();
                let raw_v = arg[eq_idx + 1..].trim();
                let clean_v =
                    if (raw_v.starts_with('"') && raw_v.ends_with('"') && raw_v.len() >= 2)
                        || (raw_v.starts_with('\'') && raw_v.ends_with('\'') && raw_v.len() >= 2)
                    {
                        &raw_v[1..raw_v.len() - 1]
                    } else {
                        raw_v
                    };
                cli_vars.push((k, clean_v.to_string()));
            } else {
                target_names.push(arg.clone());
            }
        }
        i += 1;
    }

    if let Some(ref addr) = worker_listen {
        let auth = match makeyd::distributed::WorkerAuth::from_env() {
            Ok(a) => a,
            Err(e) => {
                eprintln!("make: *** {e}. Stop.");
                return ExitCode::from(2);
            }
        };
        if let Err(e) = makeyd::distributed::run_worker_daemon(addr, auth, worker_allow_remote) {
            eprintln!("make: *** worker daemon error on {addr}: {e}. Stop.");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }

    if !remote_workers.is_empty() {
        if let Err(e) = makeyd::distributed::WorkerAuth::from_env() {
            eprintln!("make: *** --remote-workers: {e}. Stop.");
            return ExitCode::from(2);
        }
    }

    if let Some(ref dir) = chdir {
        if let Err(e) = env::set_current_dir(dir) {
            eprintln!("make: *** chdir to '{dir}' failed: {e}. Stop.");
            return ExitCode::from(2);
        }
    }

    if env_overrides {
        for (k, v) in env::vars() {
            if !cli_vars.iter().any(|(ck, _)| ck == &k) {
                cli_vars.push((k, v));
            }
        }
    }

    // Check Makefile existence
    let path = Path::new(&makefile_path);
    let chosen_path = if path.exists() {
        path
    } else if makefile_path == "Makefile" && Path::new("makefile").exists() {
        Path::new("makefile")
    } else {
        eprintln!("make: *** No targets specified and no makefile found. Stop.");
        return ExitCode::from(2);
    };

    let content = match fs::read_to_string(chosen_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("make: *** Error reading '{makefile_path}': {e}. Stop.");
            return ExitCode::from(2);
        }
    };

    let is_ninja = chosen_path
        .to_str()
        .map_or(false, |s| s.ends_with(".ninja"));
    let makefile = if is_ninja {
        match makeyd::ninja::parse_ninja_content(&content) {
            Ok(mf) => mf,
            Err(e) => {
                eprintln!("ninja: {e}");
                return ExitCode::from(2);
            }
        }
    } else {
        match parse_makefile_content(&content, &cli_vars) {
            Ok(mf) => mf,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(2);
            }
        }
    };

    if print_database {
        println!("# Variables");
        for (k, v) in &makefile.variables {
            println!("{k} = {v}");
        }
        println!("\n# Pattern Rules");
        for pr in &makefile.pattern_rules {
            println!("{}: {}", pr.target_pattern, pr.prereq_patterns.join(" "));
            for cmd in &pr.commands {
                println!("\t{cmd}");
            }
        }
        println!("\n# Explicit Rules");
        for (tgt, r) in &makefile.rules {
            println!("{}: {}", tgt, r.prereqs.join(" "));
            for cmd in &r.commands {
                println!("\t{cmd}");
            }
        }
    }

    // If no targets given on command line, select default target
    let targets_to_build: Vec<String> = if target_names.is_empty() {
        match makefile.default_target {
            Some(ref dt) => vec![dt.clone()],
            None => {
                eprintln!("make: *** No targets. Stop.");
                return ExitCode::from(2);
            }
        }
    } else {
        target_names
    };

    // Construct Dependency Graph
    let graph = DependencyGraph::from_makefile(&makefile);

    if let Some(ref ninja_out) = emit_ninja {
        let default_goal = targets_to_build.first().map(|s| s.as_str());
        let ninja_text = makeyd::ninja::emit_ninja(&makefile, &graph, default_goal);
        if let Err(e) = fs::write(ninja_out, ninja_text) {
            eprintln!("make: *** Error writing '{ninja_out}': {e}. Stop.");
            return ExitCode::from(2);
        }
        println!("Generated Ninja build file: {ninja_out}");
        return ExitCode::SUCCESS;
    }

    if let Some(ref compdb_out) = emit_compdb {
        let entries = makeyd::compdb::generate_compilation_database(&makefile, &graph, None);
        let compdb_json = makeyd::compdb::emit_compdb_json(&entries);
        if let Err(e) = fs::write(compdb_out, compdb_json) {
            eprintln!("make: *** Error writing '{compdb_out}': {e}. Stop.");
            return ExitCode::from(2);
        }
        println!(
            "Generated Clang compilation database: {compdb_out} ({} entries)",
            entries.len()
        );
        return ExitCode::SUCCESS;
    }

    // Verify DAG acyclicity for all requested goals
    for tgt in &targets_to_build {
        if let Err(e) = graph.check_cycles(&makefile, tgt) {
            eprintln!("make: *** {e}");
            return ExitCode::from(2);
        }
    }

    // A sub-make started through $(MAKE) gets its job slots from MAKEFLAGS,
    // not argv; without this it would schedule at -j1 under a -jN parent.
    if !jobs_explicit {
        let mf = env::var("MAKEFLAGS").unwrap_or_default();
        if let Some(n) = makeyd::jobserver::inherited_jobs(&mf) {
            jobs = n;
        }
    }

    let config = ExecutionConfig {
        jobs,
        dry_run,
        always_make,
        silent,
        question,
        use_hash,
        ignore_errors,
        touch_only,
        profile,
        trace_file: trace_file.clone(),
        cache,
        cache_dir,
        remote_workers,
        tui,
    };

    let jobserver =
        match makeyd::jobserver::JobServer::detect_or_create(jobs, jobserver_auth.as_deref()) {
            Ok(js) => std::sync::Arc::new(js),
            Err(e) => {
                eprintln!("make: [WARNING] failed to initialize jobserver: {e}");
                std::sync::Arc::new(
                    makeyd::jobserver::JobServer::detect_or_create(1, None).unwrap(),
                )
            }
        };

    let executor = Executor::with_jobserver(&makefile, &graph, config, jobserver);

    for tgt in &targets_to_build {
        match executor.execute(tgt) {
            Ok(stats) => {
                if question {
                    if stats.targets_rebuilt > 0 {
                        return ExitCode::from(1);
                    }
                }

                if stats.targets_rebuilt == 0 && !silent {
                    println!("make: '{tgt}' is up to date.");
                }

                if profile {
                    println!("--------------------------------------------------");
                    println!("makeyd Execution Profile:");
                    println!("  Target:               {tgt}");
                    println!("  Concurrency (-j):     {jobs}");
                    println!("  Targets evaluated:    {}", stats.total_evaluated);
                    println!("  Targets rebuilt:      {}", stats.targets_rebuilt);
                    if stats.targets_cached > 0 {
                        println!("  Targets from cache:   {}", stats.targets_cached);
                    }
                    println!("  Targets up to date:   {}", stats.targets_up_to_date);
                    println!("  Commands executed:    {}", stats.commands_executed);
                    println!("  Elapsed wall time:    {:?}", stats.elapsed_wall_time);
                    if !stats.critical_path.is_empty() {
                        println!(
                            "  Critical path:        {}",
                            stats.critical_path.join(" -> ")
                        );
                        println!("  Critical path dur:    {:?}", stats.critical_path_duration);
                    }
                    if let Some(b) = stats.schedule {
                        let ms = |us: u64| us as f64 / 1000.0;
                        println!("  Schedule (-j{}):", b.jobs);
                        println!("    Total work:         {:.1} ms", ms(b.work_us));
                        println!("    Measured span:      {:.1} ms", ms(b.span_us));
                        println!(
                            "    Lower bound:        {:.1} ms  (max of critical path, work / {})",
                            ms(b.lower_bound_us),
                            b.jobs
                        );
                        println!(
                            "    Greedy bound:       {:.1} ms  (work / {} + critical path)",
                            ms(b.graham_bound_us),
                            b.jobs
                        );
                        println!(
                            "    Gap to optimum:     <= {:.2}x  (span / lower bound)",
                            b.gap()
                        );
                    }
                    if let Some(ref tf) = trace_file {
                        println!("  Perfetto trace log:   {tf}");
                    }
                    println!("--------------------------------------------------");
                }
            }
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(1);
            }
        }
    }

    ExitCode::SUCCESS
}
