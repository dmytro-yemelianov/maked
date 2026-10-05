use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub target: String,
    pub prereqs: Vec<String>,
    pub commands: Vec<String>,
    pub is_phony: bool,
    pub line_number: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternRule {
    pub target_pattern: String,
    pub prereq_patterns: Vec<String>,
    pub commands: Vec<String>,
    pub line_number: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VpathDirective {
    pub pattern: String,
    pub directories: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Makefile {
    pub rules: HashMap<String, Rule>,
    pub rule_order: Vec<String>,
    pub pattern_rules: Vec<PatternRule>,
    pub variables: HashMap<String, String>,
    pub cli_overrides: HashSet<String>,
    pub default_target: Option<String>,
    pub vpath_directives: Vec<VpathDirective>,
    pub target_variables: HashMap<String, HashMap<String, String>>,
    pub has_second_expansion: bool,
    pub eval_queue: Arc<Mutex<Vec<String>>>,
}

impl Makefile {
    pub fn new() -> Self {
        let mut mf = Self {
            rules: HashMap::new(),
            rule_order: Vec::new(),
            pattern_rules: Vec::new(),
            variables: HashMap::new(),
            cli_overrides: HashSet::new(),
            default_target: None,
            vpath_directives: Vec::new(),
            target_variables: HashMap::new(),
            has_second_expansion: false,
            eval_queue: Arc::new(Mutex::new(Vec::new())),
        };

        // Built-in POSIX/GNU Make variables
        let cur_exe = std::env::current_exe()
            .ok()
            .and_then(|p| p.to_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "makeyd".to_string());
        mf.set_var("MAKE".to_string(), cur_exe);
        mf.set_var("CC".to_string(), "cc".to_string());
        mf.set_var("AR".to_string(), "ar".to_string());
        mf.set_var("ARFLAGS".to_string(), "rv".to_string());
        mf.set_var("RANLIB".to_string(), "ranlib".to_string());
        mf.set_var("RM".to_string(), "rm -f".to_string());

        // Built-in implicit pattern rule: %.o: %.c
        mf.add_pattern_rule(PatternRule {
            target_pattern: "%.o".to_string(),
            prereq_patterns: vec!["%.c".to_string()],
            commands: vec!["$(CC) $(CFLAGS) -c $< -o $@".to_string()],
            line_number: 0,
        });

        mf
    }

    pub fn add_rule(&mut self, rule: Rule) {
        // POSIX: First target not starting with '.' is default goal, regardless of .PHONY!
        if self.default_target.is_none() && !rule.target.starts_with('.') {
            self.default_target = Some(rule.target.clone());
        }

        if let Some(existing) = self.rules.get_mut(&rule.target) {
            // Accumulate prerequisites
            for dep in rule.prereqs {
                if !existing.prereqs.contains(&dep) {
                    existing.prereqs.push(dep);
                }
            }
            if existing.commands.is_empty() && !rule.commands.is_empty() {
                existing.commands = rule.commands;
                existing.line_number = rule.line_number;
            } else if !rule.commands.is_empty() {
                eprintln!(
                    "make: [WARNING] Makefile:{}: overriding recipe for target '{}'",
                    rule.line_number, rule.target
                );
                existing.commands = rule.commands;
                existing.line_number = rule.line_number;
            }
            if rule.is_phony {
                existing.is_phony = true;
            }
        } else {
            self.rule_order.push(rule.target.clone());
            self.rules.insert(rule.target.clone(), rule);
        }
    }

    pub fn add_pattern_rule(&mut self, rule: PatternRule) -> usize {
        let idx = self.pattern_rules.len();
        self.pattern_rules.push(rule);
        idx
    }

    pub fn set_var(&mut self, key: String, val: String) {
        if self.cli_overrides.contains(&key) {
            return;
        }
        self.variables.insert(key, val);
    }

    pub fn set_cli_var(&mut self, key: String, val: String) {
        self.cli_overrides.insert(key.clone());
        self.variables.insert(key, val);
    }

    pub fn get_var(&self, key: &str) -> Option<String> {
        if let Some(val) = self.variables.get(key) {
            return Some(val.clone());
        }
        std::env::var(key).ok()
    }

    pub fn add_vpath(&mut self, pattern: String, directories: Vec<String>) {
        self.vpath_directives.push(VpathDirective {
            pattern,
            directories,
        });
    }

    pub fn clear_vpath_pattern(&mut self, pattern: &str) {
        self.vpath_directives.retain(|d| d.pattern != pattern);
    }

    pub fn clear_all_vpaths(&mut self) {
        self.vpath_directives.clear();
    }

    pub fn push_eval(&self, s: String) {
        if let Ok(mut q) = self.eval_queue.lock() {
            q.push(s);
        }
    }

    pub fn drain_eval_queue(&self) -> Vec<String> {
        if let Ok(mut q) = self.eval_queue.lock() {
            std::mem::take(&mut *q)
        } else {
            Vec::new()
        }
    }

    pub fn set_target_var(&mut self, target: String, key: String, val: String) {
        self.target_variables
            .entry(target)
            .or_default()
            .insert(key, val);
    }

    pub fn get_target_var(&self, target: &str, key: &str) -> Option<String> {
        // 1. Exact target match
        if let Some(m) = self.target_variables.get(target) {
            if let Some(v) = m.get(key) {
                return Some(v.clone());
            }
        }
        // 2. Pattern-specific target match (e.g. %.o)
        for (pat, vars) in &self.target_variables {
            if pat.contains('%') && match_pattern(pat, target).is_some() {
                if let Some(v) = vars.get(key) {
                    return Some(v.clone());
                }
            }
        }
        None
    }

    pub fn expand_prerequisites(&self, target: &str, raw_prereqs: &[String]) -> Vec<String> {
        // Without vpath/VPATH, resolve_path can only return the name it was
        // given, so skip its stat(2): get_rule runs several times per node.
        if !self.has_vpath() {
            if !self.has_second_expansion {
                return raw_prereqs.to_vec();
            }
            let mut result = Vec::with_capacity(raw_prereqs.len());
            for dep in raw_prereqs {
                if dep.contains('$') {
                    let expanded = crate::parser::expand_variables(dep, self, Some(target), &[]);
                    result.extend(expanded.split_whitespace().map(str::to_string));
                } else {
                    result.push(dep.clone());
                }
            }
            return result;
        }
        let mut result = Vec::new();
        for dep in raw_prereqs {
            if self.has_second_expansion && dep.contains('$') {
                let expanded = crate::parser::expand_variables(dep, self, Some(target), &[]);
                for token in expanded.split_whitespace() {
                    let resolved = self
                        .resolve_path(token)
                        .unwrap_or_else(|| token.to_string());
                    result.push(resolved);
                }
            } else {
                let resolved = self.resolve_path(dep).unwrap_or_else(|| dep.clone());
                result.push(resolved);
            }
        }
        result
    }

    /// True when any `vpath` directive or a non-empty `VPATH` is in effect.
    pub fn has_vpath(&self) -> bool {
        !self.vpath_directives.is_empty()
            || self.get_var("VPATH").is_some_and(|v| !v.trim().is_empty())
    }

    /// Resolves a file path using vpath directives and VPATH environment/Makefile variable
    pub fn resolve_path(&self, file: &str) -> Option<String> {
        if Path::new(file).exists() {
            return Some(file.to_string());
        }

        for vpath in &self.vpath_directives {
            let matched = if vpath.pattern == "%" {
                true
            } else if vpath.pattern.contains('%') {
                match_pattern(&vpath.pattern, file).is_some()
            } else {
                vpath.pattern == file
            };

            if matched {
                for dir in &vpath.directories {
                    let candidate = Path::new(dir).join(file);
                    if candidate.exists() {
                        return candidate.to_str().map(|s| s.replace('\\', "/"));
                    }
                }
            }
        }

        if let Some(vpath_var) = self.get_var("VPATH") {
            let dirs: Vec<&str> = vpath_var
                .split(|c| c == ':' || c == ';' || c == ' ' || c == '\t')
                .filter(|s| !s.is_empty())
                .collect();
            for dir in dirs {
                let candidate = Path::new(dir).join(file);
                if candidate.exists() {
                    return candidate.to_str().map(|s| s.replace('\\', "/"));
                }
            }
        }

        None
    }

    /// Try to find an explicit rule or synthesize one from pattern rules (e.g. %.o: %.c)
    pub fn get_rule(&self, target: &str) -> Option<Rule> {
        let explicit = self.rules.get(target);
        if let Some(r) = explicit {
            if !r.commands.is_empty() {
                let mut r_clone = r.clone();
                r_clone.prereqs = self.expand_prerequisites(target, &r.prereqs);
                return Some(r_clone);
            }
        }

        // If target has no commands, or no explicit rule at all:
        // Try pattern matching (e.g. %.o: %.c), prioritizing user-defined rules over built-ins
        let mut ordered_pattern_rules: Vec<&PatternRule> =
            Vec::with_capacity(self.pattern_rules.len());
        for pr in &self.pattern_rules {
            if pr.line_number > 0 {
                ordered_pattern_rules.push(pr);
            }
        }
        for pr in &self.pattern_rules {
            if pr.line_number == 0 {
                ordered_pattern_rules.push(pr);
            }
        }

        for p_rule in ordered_pattern_rules {
            if let Some(stem) = match_pattern(&p_rule.target_pattern, target) {
                let mut concrete_prereqs = Vec::new();
                let mut all_prereqs_viable = true;

                for p_dep in &p_rule.prereq_patterns {
                    let dep_name = p_dep.replace('%', &stem);
                    let dep_candidates = self.expand_prerequisites(target, &[dep_name]);
                    for candidate in dep_candidates {
                        if Path::new(&candidate).exists()
                            || self.rules.contains_key(&candidate)
                            || self.resolve_path(&candidate).is_some()
                        {
                            concrete_prereqs.push(candidate);
                        } else {
                            all_prereqs_viable = false;
                            break;
                        }
                    }
                    if !all_prereqs_viable {
                        break;
                    }
                }

                if all_prereqs_viable {
                    let mut prereqs = concrete_prereqs;
                    if let Some(r) = explicit {
                        let exp_prereqs = self.expand_prerequisites(target, &r.prereqs);
                        for dep in exp_prereqs {
                            if !prereqs.contains(&dep) {
                                prereqs.push(dep);
                            }
                        }
                    }
                    return Some(Rule {
                        target: target.to_string(),
                        prereqs,
                        commands: p_rule.commands.clone(),
                        is_phony: explicit.map_or(false, |r| r.is_phony),
                        line_number: explicit.map_or(p_rule.line_number, |r| r.line_number),
                    });
                }
            }
        }

        // Return explicit rule if present (e.g. alias rule with prerequisites but no commands)
        if let Some(r) = explicit {
            let mut r_clone = r.clone();
            r_clone.prereqs = self.expand_prerequisites(target, &r.prereqs);
            return Some(r_clone);
        }

        None
    }
}

pub fn match_pattern(pattern: &str, target: &str) -> Option<String> {
    if let Some(pct_idx) = pattern.find('%') {
        let prefix = &pattern[..pct_idx];
        let suffix = &pattern[pct_idx + 1..];

        if target.starts_with(prefix)
            && target.ends_with(suffix)
            && target.len() >= prefix.len() + suffix.len()
        {
            let stem = &target[prefix.len()..target.len() - suffix.len()];
            return Some(stem.to_string());
        }
    }
    None
}
