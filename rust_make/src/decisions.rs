//! A record of maked's decisions, for checking real builds against the Lean
//! model (`lean_make --check-decisions`).
//!
//! With `MAKED_DECISIONS=DIR` set, each make process writes `DIR/<pid>.txt`:
//! one `NODE` line per target it settled, in the order it settled them,
//! with what the decision was based on (the file's mtime before, phony,
//! recipe, prerequisites) and what maked did (outcome, whether the recipe
//! ran). The Lean side evaluates its `needsRebuild` on the same inputs and
//! compares, and checks that no target was settled twice in one process.
//! Sub-makes inherit the variable, so recursive builds are recorded too.
//!
//! Recording is off unless the variable is set, and only for plain builds:
//! -n, -t, -q and --hash decide differently and are not recorded.

use std::io::Write;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

struct Recorder {
    out: std::io::BufWriter<std::fs::File>,
    ran: crate::fxhash::FxHashSet<String>,
    before: crate::fxhash::FxHashMap<String, Option<SystemTime>>,
}

static RECORDER: Mutex<Option<Recorder>> = Mutex::new(None);

fn ns(t: Option<SystemTime>) -> String {
    match t.and_then(|t| t.duration_since(UNIX_EPOCH).ok()) {
        Some(d) => d.as_nanos().to_string(),
        None => "-".to_string(),
    }
}

/// Start recording if `MAKED_DECISIONS` names a directory. `mode` lists the
/// flags that change decisions (`B` for -B), written as the first line.
pub fn init(mode: &str) {
    let Some(dir) = std::env::var_os("MAKED_DECISIONS") else {
        return;
    };
    let path = std::path::Path::new(&dir).join(format!("{}.txt", std::process::id()));
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    let mut out = std::io::BufWriter::new(file);
    let cwd = std::env::current_dir().unwrap_or_default();
    let _ = writeln!(out, "RUN {} mode={mode}", cwd.display());
    *RECORDER.lock().unwrap() = Some(Recorder {
        out,
        ran: Default::default(),
        before: Default::default(),
    });
}

pub fn enabled() -> bool {
    RECORDER.lock().unwrap().is_some()
}

/// The target's recipe ran (at least one command line).
pub fn mark_ran(target: &str) {
    if let Some(r) = RECORDER.lock().unwrap().as_mut() {
        r.ran.insert(target.to_string());
    }
}

/// The target's file as it was when maked started deciding about it.
pub fn note_before(target: &str) {
    if let Some(r) = RECORDER.lock().unwrap().as_mut() {
        r.before
            .entry(target.to_string())
            .or_insert_with(|| crate::freshness::get_file_mtime(target));
    }
}

/// `outcome`: `R` rebuilt (with the new mtime), `U` up to date (with the
/// mtime, or none), `F` failed.
pub fn record(
    target: &str,
    rule: Option<&crate::ast::Rule>,
    outcome: char,
    mtime: Option<SystemTime>,
    double_colon: bool,
) {
    let mut guard = RECORDER.lock().unwrap();
    let Some(r) = guard.as_mut() else {
        return;
    };
    let before = r
        .before
        .get(target)
        .copied()
        .unwrap_or_else(|| crate::freshness::get_file_mtime(target));
    let ran = r.ran.contains(target);
    let (has_rule, phony, cmds, deps) = match rule {
        Some(rule) => (
            1,
            rule.is_phony as u8,
            (!rule.commands.is_empty()) as u8,
            rule.prereqs.join(" "),
        ),
        None => (0, 0, 0, String::new()),
    };
    let _ = writeln!(
        r.out,
        "NODE {target} rule={has_rule} phony={phony} cmds={cmds} dcolon={} before={} out={outcome}:{} ran={} deps {deps}",
        double_colon as u8,
        ns(before),
        ns(mtime),
        ran as u8
    );
}

/// Write out what is buffered (before exiting or re-executing).
pub fn flush() {
    if let Some(r) = RECORDER.lock().unwrap().as_mut() {
        let _ = r.out.flush();
    }
}
