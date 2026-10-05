use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// One target- or pattern-specific assignment.
#[derive(Debug, Clone)]
pub struct TargetVarOp {
    pub name: String,
    /// `=` (recursive), `:` (simple, already expanded), `+` or `?`.
    pub kind: char,
    pub value: String,
    pub is_override: bool,
}

/// One rule of a double-colon target (`t:: prereqs`): GNU make runs each
/// on its own, when the target is older than its own prerequisites.
#[derive(Debug, Clone, Default)]
pub struct DcRule {
    pub prereqs: Vec<String>,
    pub commands: Vec<String>,
}

/// Marks a recipe line of a double-colon rule: `\u{1}<index>\u{1}<line>`.
pub const DC_MARK: char = '\u{1}';

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
    pub rules: crate::fxhash::FxHashMap<String, Rule>,
    pub rule_order: Vec<String>,
    pub pattern_rules: Vec<PatternRule>,
    pub variables: crate::fxhash::FxHashMap<String, String>,
    pub cli_overrides: HashSet<String>,
    /// Variables whose value was expanded when set (`:=`, `::=`): read as
    /// is, never expanded again; `+=` expands the new text at once.
    pub simple_vars: crate::fxhash::FxHashSet<String>,
    /// The separate rules of each double-colon target, in order.
    pub double_colon: HashMap<String, Vec<DcRule>>,
    /// Set with `override`: later plain assignments are ignored, and the
    /// origin is "override".
    pub override_vars: crate::fxhash::FxHashSet<String>,
    pub default_target: Option<String>,
    pub vpath_directives: Vec<VpathDirective>,
    /// Target- and pattern-specific assignments, in the order written,
    /// applied when a variable is read for that target (`get_target_var`).
    pub target_variables: HashMap<String, Vec<TargetVarOp>>,
    /// Names assigned by any target- or pattern-specific line: lookups of
    /// other names skip the per-target chain.
    pub target_var_names: crate::fxhash::FxHashSet<String>,
    /// The target each target inherits target-specific variables from: the
    /// one that first asked for it (`record_inheritance`).
    pub inherit_parent: Arc<Mutex<crate::fxhash::FxHashMap<String, String>>>,
    pub has_second_expansion: bool,
    pub eval_queue: Arc<Mutex<Vec<String>>>,
    /// `export NAME` (true) / `unexport NAME` (false).
    pub exported: HashMap<String, bool>,
    /// A bare `export` line: export every variable.
    pub export_all: bool,
    /// `$*` for targets of static pattern rules.
    pub static_stems: HashMap<String, String>,
    /// Order-only prerequisites per target (`target: normal | order-only`).
    pub order_only: HashMap<String, Vec<String>>,
    /// Built-in variables still at their default value.
    pub defaults: HashSet<String>,
    /// Makefiles read through `include` (as written).
    pub included: Vec<String>,
    /// `include`d files that did not exist: (name, optional, line).
    pub missing_includes: Vec<(String, bool, usize)>,
    /// `get_rule` results during the build. Implicit-rule search stats every
    /// candidate prerequisite, and the executor asks for the same target
    /// several times (git: 116k stat calls for a null build against GNU
    /// make's 16k). GNU make also searches once per target per run. Shared
    /// by clones; used only in the execution phase, when rules are fixed.
    pub rule_cache: Arc<Mutex<crate::fxhash::FxHashMap<String, Option<Arc<Rule>>>>>,
    /// `$*` for targets made by a pattern rule: the directory and the stem,
    /// recorded when the rule is chosen.
    pub implicit_stems: Arc<Mutex<crate::fxhash::FxHashMap<String, String>>>,
}

// `$(eval NAME := value)` met while expanding recipes: the makefile is
// shared and immutable during a build, so such assignments go to a per-thread
// overlay that `get_var` consults first. That is enough for GNU idioms like
// git's self-memoizing `X = $(eval X := $$(shell ...))$(X)`.
static EXECUTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
thread_local! {
    static RUNTIME_VARS: std::cell::RefCell<HashMap<String, String>> =
        std::cell::RefCell::new(HashMap::new());
}

/// Called when the build starts: from now on `$(eval)` assignments apply at once.
pub fn enter_execution_phase() {
    EXECUTING.store(true, std::sync::atomic::Ordering::Relaxed);
}

pub fn in_execution_phase() -> bool {
    EXECUTING.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn set_runtime_var(key: String, val: String) {
    RUNTIME_VARS.with(|m| {
        m.borrow_mut().insert(key, val);
    });
}

thread_local! {
    /// While the makefile is read, `$(eval)` text is queued and parsed after
    /// the current line; its simple assignments also go here at once, so
    /// the rest of the line sees them, as in GNU make (where `$(eval)` is
    /// immediate). Cleared when the queue is applied.
    static EVAL_PREVIEW: std::cell::RefCell<HashMap<String, String>> =
        std::cell::RefCell::new(HashMap::new());
}

pub fn set_eval_preview(key: String, val: String) {
    EVAL_PREVIEW.with(|m| {
        m.borrow_mut().insert(key, val);
    });
}

pub fn clear_eval_preview() {
    EVAL_PREVIEW.with(|m| m.borrow_mut().clear());
}

fn runtime_var(key: &str) -> Option<String> {
    if !in_execution_phase() {
        return EVAL_PREVIEW.with(|m| m.borrow().get(key).cloned());
    }
    RUNTIME_VARS.with(|m| m.borrow().get(key).cloned())
}

/// How recipes are run: the shell and the environment changes GNU make
/// applies (exported variables, unexported ones removed).
#[derive(Debug, Clone, Default)]
pub struct RecipeEnv {
    pub shell: String,
    pub set: Vec<(String, String)>,
    pub unset: Vec<String>,
}

fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl Default for Makefile {
    fn default() -> Self {
        Self::new()
    }
}

impl Makefile {
    pub fn new() -> Self {
        let mut mf = Self {
            rules: Default::default(),
            rule_order: Vec::new(),
            pattern_rules: Vec::new(),
            variables: Default::default(),
            cli_overrides: HashSet::new(),
            simple_vars: Default::default(),
            double_colon: HashMap::new(),
            override_vars: Default::default(),
            default_target: None,
            vpath_directives: Vec::new(),
            exported: HashMap::new(),
            export_all: false,
            static_stems: HashMap::new(),
            order_only: HashMap::new(),
            defaults: HashSet::new(),
            included: Vec::new(),
            missing_includes: Vec::new(),
            rule_cache: Arc::new(Mutex::new(crate::fxhash::FxHashMap::default())),
            implicit_stems: Default::default(),
            target_variables: HashMap::new(),
            target_var_names: Default::default(),
            inherit_parent: Default::default(),
            has_second_expansion: false,
            eval_queue: Arc::new(Mutex::new(Vec::new())),
        };

        // Built-in POSIX/GNU Make variables
        let cur_exe = std::env::current_exe()
            .ok()
            .and_then(|p| p.to_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "maked".to_string());
        mf.set_var("MAKE".to_string(), cur_exe);
        mf.defaults.insert("MAKE".to_string()); // $(origin MAKE) is default
        // GNU make's built-in defaults: the environment overrides them, and
        // `$(origin)` reports them as "default".
        for (k, v) in [
            ("CC", "cc"),
            ("CXX", "c++"),
            ("CPP", "$(CC) -E"),
            ("AR", "ar"),
            ("ARFLAGS", "rv"),
            ("AS", "as"),
            ("RANLIB", "ranlib"),
            ("RM", "rm -f"),
            ("LEX", "lex"),
            ("YACC", "yacc"),
            ("COMPILE.c", "$(CC) $(CFLAGS) $(CPPFLAGS) $(TARGET_ARCH) -c"),
            (
                "COMPILE.cc",
                "$(CXX) $(CXXFLAGS) $(CPPFLAGS) $(TARGET_ARCH) -c",
            ),
            ("LINK.o", "$(CC) $(LDFLAGS) $(TARGET_ARCH)"),
            ("OUTPUT_OPTION", "-o $@"),
        ] {
            mf.variables.insert(k.to_string(), v.to_string());
            mf.defaults.insert(k.to_string());
        }

        // Built-in implicit rules (GNU make's, without match-anything ones).
        for (tp, pp, cmd) in [
            ("%.o", "%.c", "$(COMPILE.c) $(OUTPUT_OPTION) $<"),
            ("%.o", "%.cc", "$(COMPILE.cc) $(OUTPUT_OPTION) $<"),
            ("%.o", "%.cpp", "$(COMPILE.cc) $(OUTPUT_OPTION) $<"),
            ("%.o", "%.C", "$(COMPILE.cc) $(OUTPUT_OPTION) $<"),
        ] {
            mf.add_pattern_rule(PatternRule {
                target_pattern: tp.to_string(),
                prereq_patterns: vec![pp.to_string()],
                commands: vec![cmd.to_string()],
                line_number: 0,
            });
        }

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

    /// `VAR += text` for a variable defined in the makefile (not a built-in
    /// default, not only in the environment): append without copying the
    /// value. Returns false when the caller must take the general path.
    pub fn append_in_place(&mut self, key: &str, text: &str) -> bool {
        if self.defaults.contains(key) {
            return false;
        }
        match self.variables.get_mut(key) {
            Some(v) => {
                if !v.is_empty() && !text.is_empty() {
                    v.push(' ');
                }
                v.push_str(text);
                true
            }
            None => false,
        }
    }

    /// `override VAR = value`: set even over a command-line definition.
    pub fn set_var_override(&mut self, key: String, val: String, simple: bool) {
        self.override_vars.insert(key.clone());
        self.defaults.remove(&key);
        self.set_flavor(&key, simple);
        self.variables.insert(key, val);
    }

    fn set_flavor(&mut self, key: &str, simple: bool) {
        if simple {
            self.simple_vars.insert(key.to_string());
        } else {
            self.simple_vars.remove(key);
        }
    }

    /// A plain assignment cannot change a command-line or `override` variable.
    pub fn is_protected(&self, key: &str) -> bool {
        self.cli_overrides.contains(key) || self.override_vars.contains(key)
    }

    /// A recipe line and, if it belongs to one rule of a double-colon
    /// target, that rule's prerequisites (for `$^`, `$<`, `$?`).
    pub fn dc_split<'a>(&'a self, raw: &'a str, target: &str) -> (&'a str, Option<&'a [String]>) {
        if let Some(rest) = raw.strip_prefix(DC_MARK) {
            if let Some((idx, line)) = rest.split_once(DC_MARK) {
                let pre = idx
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| self.double_colon.get(target)?.get(i))
                    .map(|r| r.prereqs.as_slice());
                return (line, pre);
            }
        }
        (raw, None)
    }

    /// The order-only prerequisites of `target` (`target: normal | these`).
    pub fn order_only_of(&self, target: &str) -> &[String] {
        self.order_only.get(target).map_or(&[], Vec::as_slice)
    }

    /// True for `:=` variables, whose values are not expanded again.
    pub fn is_simple(&self, key: &str) -> bool {
        self.simple_vars.contains(key)
    }

    /// `VAR := value` (already expanded).
    pub fn set_simple_var(&mut self, key: String, val: String) {
        if self.is_protected(&key) {
            return;
        }
        self.defaults.remove(&key);
        self.set_flavor(&key, true);
        self.variables.insert(key, val);
    }

    /// Recipe shell and environment, as GNU make builds them. Exported are:
    /// `export`ed names, everything after a bare `export`, variables from the
    /// command line, and makefile variables that override one from the
    /// environment. `unexport`ed names are removed. The shell is the
    /// makefile's `SHELL`, never `$SHELL` from the environment, else /bin/sh.
    pub fn recipe_env(&self) -> RecipeEnv {
        let mut names: Vec<&String> = Vec::new();
        for name in self.variables.keys() {
            let explicit = self.exported.get(name).copied();
            let export = match explicit {
                Some(e) => e,
                None => {
                    self.export_all
                        || self.cli_overrides.contains(name)
                        || std::env::var_os(name).is_some()
                }
            };
            if export && name != "SHELL" && is_env_name(name) {
                names.push(name);
            }
        }
        names.sort();
        let set = names
            .into_iter()
            .map(|n| {
                let raw = self.variables.get(n).cloned().unwrap_or_default();
                (
                    n.clone(),
                    crate::parser::expand_variables(&raw, self, None, &[]),
                )
            })
            .collect();
        let mut unset: Vec<String> = self
            .exported
            .iter()
            .filter(|(_, e)| !**e)
            .map(|(n, _)| n.clone())
            .collect();
        unset.sort();
        let shell = self
            .variables
            .get("SHELL")
            .map(|v| crate::parser::expand_variables(v, self, None, &[]))
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "/bin/sh".to_string());
        RecipeEnv { shell, set, unset }
    }

    /// `VAR = value` (recursive: expanded each time it is used).
    pub fn set_var(&mut self, key: String, val: String) {
        if self.is_protected(&key) {
            return;
        }
        self.defaults.remove(&key);
        self.set_flavor(&key, false);
        self.variables.insert(key, val);
    }

    pub fn set_cli_var(&mut self, key: String, val: String) {
        // Command-line variables are applied again for every included or
        // eval'd text; an `override` in the makefile still wins.
        if self.override_vars.contains(&key) {
            return;
        }
        self.cli_overrides.insert(key.clone());
        self.defaults.remove(&key);
        self.variables.insert(key, val);
    }

    pub fn get_var(&self, key: &str) -> Option<String> {
        if let Some(val) = runtime_var(key) {
            return Some(val);
        }
        if self.defaults.contains(key) {
            if let Ok(env_val) = std::env::var(key) {
                return Some(env_val);
            }
        }
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

    /// Record `target: [override] NAME op value` (op `=`, `:`, `+` or `?`).
    pub fn add_target_var(&mut self, target: String, op: TargetVarOp) {
        self.target_var_names.insert(op.name.clone());
        self.target_variables.entry(target).or_default().push(op);
    }

    /// The value of `key` for `target`, if a target- or pattern-specific
    /// assignment applies to it or to a target it inherits from. As in GNU
    /// make, the value is built up from the global value, then the
    /// inherited chain, then matching patterns, then the target's own
    /// assignments; `+=` appends to what is below it, and a command-line
    /// variable wins unless the assignment says `override`.
    pub fn get_target_var(&self, target: &str, key: &str) -> Option<String> {
        if !self.target_var_names.contains(key) {
            return None;
        }
        let (v, touched) = self.target_value(target, key, 0);
        if touched { v } else { None }
    }

    fn target_value(&self, target: &str, key: &str, depth: usize) -> (Option<String>, bool) {
        let parent = if depth < 256 {
            self.inherit_parent.lock().unwrap().get(target).cloned()
        } else {
            None
        };
        let (mut v, mut touched) = match parent {
            Some(p) => {
                let (pv, pt) = self.target_value(&p, key, depth + 1);
                if pt {
                    (pv, true)
                } else {
                    (self.get_var(key), false)
                }
            }
            None => (self.get_var(key), false),
        };
        let cli = self.cli_overrides.contains(key);
        let mut apply = |op: &TargetVarOp| {
            if op.name != key || (cli && !op.is_override) {
                return;
            }
            touched = true;
            v = match op.kind {
                '+' => Some(match v.take() {
                    Some(prev) if !prev.is_empty() => format!("{prev} {}", op.value),
                    _ => op.value.clone(),
                }),
                '?' if v.is_some() => v.take(),
                _ => Some(op.value.clone()),
            };
        };
        for (pat, ops) in &self.target_variables {
            if pat.contains('%') && match_pattern(pat, target).is_some() {
                ops.iter().for_each(&mut apply);
            }
        }
        if let Some(ops) = self.target_variables.get(target) {
            ops.iter().for_each(&mut apply);
        }
        (v, touched)
    }

    /// Before a build: record, for every target reachable from `root`, the
    /// target that first asks for it, in GNU make's depth-first order. Its
    /// target-specific variables are inherited.
    pub fn record_inheritance(&self, root: &str) {
        if self.target_variables.is_empty() {
            return;
        }
        let mut parents = self.inherit_parent.lock().unwrap();
        let mut seen: crate::fxhash::FxHashSet<String> = Default::default();
        seen.insert(root.to_string());
        let mut stack: Vec<(String, Vec<String>, usize)> = Vec::new();
        let prereqs_of = |t: &str| {
            self.get_rule(t)
                .map(|r| r.prereqs.clone())
                .unwrap_or_default()
        };
        stack.push((root.to_string(), prereqs_of(root), 0));
        while let Some((node, deps, i)) = stack.last_mut() {
            if *i >= deps.len() {
                stack.pop();
                continue;
            }
            let dep = deps[*i].clone();
            *i += 1;
            let node = node.clone();
            if seen.insert(dep.clone()) {
                parents.entry(dep.clone()).or_insert(node);
                let next = prereqs_of(&dep);
                stack.push((dep, next, 0));
            }
        }
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
                .split([':', ';', ' ', '\t'])
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
    /// The rule that builds `target` (explicit, implicit or `.DEFAULT`).
    /// Shared, not copied: the executor asks for the same rule several
    /// times, and git's objects carry ~100 prerequisites each.
    pub fn get_rule(&self, target: &str) -> Option<Arc<Rule>> {
        if !in_execution_phase() {
            return self.find_rule(target).map(Arc::new);
        }
        if let Some(hit) = self.rule_cache.lock().unwrap().get(target) {
            return hit.clone();
        }
        let found = self.find_rule(target).map(Arc::new);
        self.rule_cache
            .lock()
            .unwrap()
            .insert(target.to_string(), found.clone());
        found
    }

    /// Forget cached `get_rule` results (after rules change).
    pub fn clear_rule_cache(&self) {
        self.rule_cache.lock().unwrap().clear();
        self.implicit_stems.lock().unwrap().clear();
    }

    fn find_rule(&self, target: &str) -> Option<Rule> {
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

        // Without vpath, `resolve_path` would only stat the same name again.
        let has_vpath = self.has_vpath();
        for p_rule in ordered_pattern_rules {
            if let Some((dir, stem)) = match_rule_target(&p_rule.target_pattern, target) {
                let mut concrete_prereqs = Vec::new();
                let mut all_prereqs_viable = true;

                for p_dep in &p_rule.prereq_patterns {
                    // GNU make: the target's directory goes back in front of
                    // each prerequisite made from a pattern (`%.o: src/%.c`
                    // makes `a/b.o` from `a/src/b.c`); a plain name is as is.
                    let dep_name = if p_dep.contains('%') {
                        format!("{dir}{}", p_dep.replacen('%', &stem, 1))
                    } else {
                        p_dep.clone()
                    };
                    let dep_candidates = self.expand_prerequisites(target, &[dep_name]);
                    for candidate in dep_candidates {
                        if self.rules.contains_key(&candidate)
                            || Path::new(&candidate).exists()
                            || (has_vpath && self.resolve_path(&candidate).is_some())
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
                    self.implicit_stems
                        .lock()
                        .unwrap()
                        .insert(target.to_string(), format!("{dir}{stem}"));
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
                        is_phony: explicit.is_some_and(|r| r.is_phony),
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

        // `.DEFAULT`: the recipe for targets with no rule and no file.
        if target != ".DEFAULT" && !target.starts_with('.') && !Path::new(target).exists() {
            if let Some(d) = self.rules.get(".DEFAULT") {
                if !d.commands.is_empty() && self.resolve_path(target).is_none() {
                    return Some(Rule {
                        target: target.to_string(),
                        prereqs: Vec::new(),
                        commands: d.commands.clone(),
                        is_phony: false,
                        line_number: d.line_number,
                    });
                }
            }
        }

        None
    }
}

/// Match a pattern rule's target pattern, as GNU make does: a pattern with
/// no `/` is matched against the file name alone. Returns the directory
/// part (with its trailing `/`, or empty) and the stem.
fn match_rule_target<'t>(pattern: &str, target: &'t str) -> Option<(&'t str, String)> {
    if !pattern.contains('/') {
        if let Some(i) = target.rfind('/') {
            let (dir, name) = target.split_at(i + 1);
            return match_pattern(pattern, name).map(|stem| (dir, stem));
        }
    }
    match_pattern(pattern, target).map(|stem| ("", stem))
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
