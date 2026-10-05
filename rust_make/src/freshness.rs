use crate::ast::Rule;
use std::fs;
use std::path::Path;
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreshnessDecision {
    UpToDate(SystemTime),
    NeedsRebuild(RebuildReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebuildReason {
    PhonyTarget,
    TargetMissing,
    PrerequisiteRebuilt,
    PrerequisiteNewer,
    AlwaysMakeFlag,
    RecipeChanged,
    PrerequisiteHashChanged(String),
    TargetHashMissing,
}

impl std::fmt::Display for RebuildReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PhonyTarget => write!(f, "target is .PHONY"),
            Self::TargetMissing => write!(f, "target does not exist"),
            Self::PrerequisiteRebuilt => write!(f, "prerequisite was rebuilt in this run"),
            Self::PrerequisiteNewer => write!(f, "prerequisite timestamp is newer than target"),
            Self::AlwaysMakeFlag => write!(f, "-B / --always-make specified"),
            Self::RecipeChanged => write!(f, "recipe commands changed"),
            Self::PrerequisiteHashChanged(p) => {
                write!(f, "prerequisite '{p}' content hash changed")
            }
            Self::TargetHashMissing => {
                write!(f, "target hash not found or target modified externally")
            }
        }
    }
}

pub fn get_file_mtime<P: AsRef<Path>>(path: P) -> Option<SystemTime> {
    fs::metadata(path)
        .ok()
        .and_then(|meta| meta.modified().ok())
}

/// Evaluates whether a target is fresh or needs to be rebuilt according to Make semantics.
pub fn evaluate_freshness(
    rule: &Rule,
    always_make: bool,
    rebuilt_prereqs: bool,
    newest_prereq_time: Option<SystemTime>,
) -> FreshnessDecision {
    if always_make {
        return FreshnessDecision::NeedsRebuild(RebuildReason::AlwaysMakeFlag);
    }

    if rule.is_phony {
        if let (true, false, Some(t)) = (
            rule.commands.is_empty(),
            rebuilt_prereqs,
            newest_prereq_time,
        ) {
            return FreshnessDecision::UpToDate(t);
        }
        return FreshnessDecision::NeedsRebuild(RebuildReason::PhonyTarget);
    }

    let target_mtime = match get_file_mtime(&rule.target) {
        Some(t) => t,
        None => {
            // POSIX Alias Rule: If target has no commands and no file on disk,
            // it acts as a virtual group/alias. If prerequisites are all up to date,
            // the alias target inherits the newest prerequisite timestamp and is UP TO DATE!
            if let (true, false, Some(t)) = (
                rule.commands.is_empty(),
                rebuilt_prereqs,
                newest_prereq_time,
            ) {
                return FreshnessDecision::UpToDate(t);
            }
            return FreshnessDecision::NeedsRebuild(RebuildReason::TargetMissing);
        }
    };

    if rebuilt_prereqs {
        return FreshnessDecision::NeedsRebuild(RebuildReason::PrerequisiteRebuilt);
    }

    if let Some(dep_time) = newest_prereq_time {
        // High-resolution sub-second comparison!
        if dep_time > target_mtime {
            return FreshnessDecision::NeedsRebuild(RebuildReason::PrerequisiteNewer);
        }
    }

    FreshnessDecision::UpToDate(target_mtime)
}

/// Evaluates target freshness using cryptographic content hashes and recipe digest
pub fn evaluate_freshness_hash(
    rule: &Rule,
    always_make: bool,
    rebuilt_prereqs: bool,
    recipe_str: &str,
    db: &crate::hash::BuildDatabase,
) -> FreshnessDecision {
    if always_make {
        return FreshnessDecision::NeedsRebuild(RebuildReason::AlwaysMakeFlag);
    }

    if rule.is_phony {
        if rule.commands.is_empty() && !rebuilt_prereqs {
            return FreshnessDecision::UpToDate(SystemTime::now());
        }
        return FreshnessDecision::NeedsRebuild(RebuildReason::PhonyTarget);
    }

    if !Path::new(&rule.target).exists() {
        if rule.commands.is_empty() && !rebuilt_prereqs {
            return FreshnessDecision::UpToDate(SystemTime::now());
        }
        return FreshnessDecision::NeedsRebuild(RebuildReason::TargetMissing);
    }

    if rebuilt_prereqs {
        return FreshnessDecision::NeedsRebuild(RebuildReason::PrerequisiteRebuilt);
    }

    let record = match db.get_record(&rule.target) {
        Some(rec) => rec,
        None => return FreshnessDecision::NeedsRebuild(RebuildReason::TargetHashMissing),
    };

    // 1. Verify recipe hash
    let cur_recipe_hash = crate::hash::to_hex(&crate::hash::sha256_bytes(recipe_str.as_bytes()));
    if cur_recipe_hash != record.recipe_hash {
        return FreshnessDecision::NeedsRebuild(RebuildReason::RecipeChanged);
    }

    // 2. Verify prerequisite content hashes
    for prereq in &rule.prereqs {
        if Path::new(prereq).exists() {
            if let Ok(cur_hash) = crate::hash::sha256_file(prereq) {
                let cur_hex = crate::hash::to_hex(&cur_hash);
                if let Some(recorded_hex) = record.prereq_hashes.get(prereq) {
                    if &cur_hex != recorded_hex {
                        return FreshnessDecision::NeedsRebuild(
                            RebuildReason::PrerequisiteHashChanged(prereq.clone()),
                        );
                    }
                } else {
                    return FreshnessDecision::NeedsRebuild(
                        RebuildReason::PrerequisiteHashChanged(prereq.clone()),
                    );
                }
            }
        }
    }

    // 3. Verify target file hash
    if let Ok(cur_target_hash) = crate::hash::sha256_file(&rule.target) {
        let cur_hex = crate::hash::to_hex(&cur_target_hash);
        if cur_hex != record.target_hash {
            return FreshnessDecision::NeedsRebuild(RebuildReason::TargetHashMissing);
        }
    }

    let current_mtime = get_file_mtime(&rule.target).unwrap_or_else(SystemTime::now);
    FreshnessDecision::UpToDate(current_mtime)
}
