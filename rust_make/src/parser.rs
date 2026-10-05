use crate::ast::{Makefile, PatternRule, Rule};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

#[derive(Debug)]
pub enum ParseError {
    SyntaxError(String, usize),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SyntaxError(msg, line) => write!(f, "Makefile:{line}: *** {msg}. Stop."),
        }
    }
}

impl std::error::Error for ParseError {}

pub fn expand_variables(
    text: &str,
    makefile: &Makefile,
    target: Option<&str>,
    prereqs: &[String],
) -> String {
    let mut depth = 0;
    expand_variables_internal(text, makefile, target, prereqs, None, &mut depth)
}

pub fn expand_variables_scoped(
    text: &str,
    makefile: &Makefile,
    target: Option<&str>,
    prereqs: &[String],
    scoped_vars: &HashMap<String, String>,
) -> String {
    let mut depth = 0;
    expand_variables_internal(
        text,
        makefile,
        target,
        prereqs,
        Some(scoped_vars),
        &mut depth,
    )
}

fn expand_variables_internal(
    text: &str,
    makefile: &Makefile,
    target: Option<&str>,
    prereqs: &[String],
    scoped_vars: Option<&HashMap<String, String>>,
    depth: &mut usize,
) -> String {
    // Most fragments (words, file names, flags) have nothing to expand.
    if !text.contains('$') {
        return text.to_string();
    }
    if *depth > 100 {
        return text.to_string();
    }
    *depth += 1;

    let mut result = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '$' && i + 1 < chars.len() {
            let next_ch = chars[i + 1];
            if next_ch == '$' {
                result.push('$');
                i += 2;
                continue;
            }

            // Bare automatic variables: $@ $< $^ $+ $? $* $|
            if matches!(next_ch, '@' | '<' | '^' | '+' | '?' | '*' | '|') {
                let mut buf = [0u8; 4];
                if let Some(v) =
                    automatic_var(next_ch.encode_utf8(&mut buf), makefile, target, prereqs)
                {
                    result.push_str(&v);
                }
                i += 2;
                continue;
            }

            // Parenthesized or braced variables: $(...) or ${...}
            let (open_ch, close_ch) = match next_ch {
                '(' => ('(', ')'),
                '{' => ('{', '}'),
                _ => ('\0', '\0'),
            };

            if open_ch != '\0' {
                let mut paren_depth = 1;
                let start = i + 2;
                let mut end = start;
                while end < chars.len() {
                    if chars[end] == open_ch {
                        paren_depth += 1;
                    } else if chars[end] == close_ch {
                        paren_depth -= 1;
                        if paren_depth == 0 {
                            break;
                        }
                    }
                    end += 1;
                }

                if end < chars.len() {
                    let inner: String = chars[start..end].iter().collect();
                    let expanded =
                        eval_inner(&inner, makefile, target, prereqs, scoped_vars, depth);
                    result.push_str(&expanded);
                    i = end + 1;
                    continue;
                }
            } else if next_ch.is_alphanumeric() || next_ch == '_' {
                let single_name = next_ch.to_string();
                let val_opt = if let Some(sv) = scoped_vars {
                    sv.get(&single_name).cloned()
                } else {
                    None
                };
                let val_opt = val_opt
                    .or_else(|| {
                        if let Some(t) = target {
                            makefile.get_target_var(t, &single_name)
                        } else {
                            None
                        }
                    })
                    .or_else(|| makefile.get_var(&single_name));

                if let Some(val) = val_opt {
                    if val.contains('$') {
                        result.push_str(&expand_variables_internal(
                            &val,
                            makefile,
                            target,
                            prereqs,
                            scoped_vars,
                            depth,
                        ));
                    } else {
                        result.push_str(&val);
                    }
                }
                i += 2;
                continue;
            }
        }
        result.push(chars[i]);
        i += 1;
    }

    *depth -= 1;
    result
}

fn eval_inner(
    inner: &str,
    makefile: &Makefile,
    target: Option<&str>,
    prereqs: &[String],
    scoped_vars: Option<&HashMap<String, String>>,
    depth: &mut usize,
) -> String {
    let trimmed = inner.trim();

    // Check scoped variables first
    if let Some(sv) = scoped_vars {
        if let Some(val) = sv.get(trimmed) {
            return if val.contains('$') {
                expand_variables_internal(val, makefile, target, prereqs, scoped_vars, depth)
            } else {
                val.clone()
            };
        }
    }

    // Automatic variables, including $(@D) / $(<F) forms
    if let Some(v) = automatic_var(trimmed, makefile, target, prereqs) {
        return v;
    }

    // Substitution reference: $(VAR:pattern=replacement)
    if let Some((var_name, pat, repl)) = parse_subst_ref(trimmed) {
        let expanded_var_name = if var_name.contains('$') {
            expand_variables_internal(var_name, makefile, target, prereqs, scoped_vars, depth)
        } else {
            var_name.to_string()
        };
        let var_val = if let Some(v) = automatic_var(&expanded_var_name, makefile, target, prereqs)
        {
            v
        } else if let Some(val) = scoped_vars.and_then(|sv| sv.get(&expanded_var_name)) {
            expand_variables_internal(val, makefile, target, prereqs, scoped_vars, depth)
        } else if let Some(val) =
            target.and_then(|t| makefile.get_target_var(t, &expanded_var_name))
        {
            expand_variables_internal(&val, makefile, target, prereqs, scoped_vars, depth)
        } else if let Some(val) = makefile.get_var(&expanded_var_name) {
            expand_variables_internal(&val, makefile, target, prereqs, scoped_vars, depth)
        } else {
            String::new()
        };
        let pat_exp = expand_variables_internal(pat, makefile, target, prereqs, scoped_vars, depth);
        let repl_exp =
            expand_variables_internal(repl, makefile, target, prereqs, scoped_vars, depth);
        return if pat_exp.contains('%') {
            patsubst(&pat_exp, &repl_exp, &var_val)
        } else {
            patsubst(&format!("%{pat_exp}"), &format!("%{repl_exp}"), &var_val)
        };
    }

    // GNU Make built-in functions
    let known_functions = [
        "wildcard",
        "patsubst",
        "subst",
        "filter-out",
        "filter",
        "dir",
        "notdir",
        "suffix",
        "basename",
        "addprefix",
        "addsuffix",
        "join",
        "word",
        "words",
        "firstword",
        "lastword",
        "sort",
        "strip",
        "shell",
        "if",
        "or",
        "and",
        "error",
        "warning",
        "info",
        "call",
        "eval",
        "foreach",
        "value",
        "findstring",
        "wordlist",
        "abspath",
        "realpath",
        "origin",
        "flavor",
        "file",
        "intcmp",
        "let",
    ];

    for &func in &known_functions {
        if let Some(remainder) = trimmed.strip_prefix(func) {
            if remainder.is_empty() || remainder.starts_with(char::is_whitespace) {
                let args_raw = remainder.trim_start();
                return eval_function(
                    func,
                    args_raw,
                    makefile,
                    target,
                    prereqs,
                    scoped_vars,
                    depth,
                );
            }
        }
    }

    // Standard variable lookup
    let var_name = if inner.contains('$') {
        expand_variables_internal(inner, makefile, target, prereqs, scoped_vars, depth)
    } else {
        inner.to_string()
    };

    let val_opt = scoped_vars
        .and_then(|sv| sv.get(&var_name).cloned())
        .or_else(|| target.and_then(|t| makefile.get_target_var(t, &var_name)))
        .or_else(|| makefile.get_var(&var_name));

    if let Some(val) = val_opt {
        if val.contains('$') {
            expand_variables_internal(&val, makefile, target, prereqs, scoped_vars, depth)
        } else {
            val
        }
    } else {
        String::new()
    }
}

fn eval_function(
    func: &str,
    args_raw: &str,
    makefile: &Makefile,
    target: Option<&str>,
    prereqs: &[String],
    scoped_vars: Option<&HashMap<String, String>>,
    depth: &mut usize,
) -> String {
    let mut ex =
        |text: &str| expand_variables_internal(text, makefile, target, prereqs, scoped_vars, depth);
    match func {
        "findstring" => {
            let args = split_top_level_args(args_raw);
            if args.len() < 2 {
                return String::new();
            }
            let find = ex(args[0]);
            let within = ex(&args[1..].join(","));
            if within.contains(find.as_str()) {
                find
            } else {
                String::new()
            }
        }
        "wordlist" => {
            let args = split_top_level_args(args_raw);
            if args.len() < 3 {
                return String::new();
            }
            let s: usize = ex(args[0].trim()).trim().parse().unwrap_or(0);
            let e: usize = ex(args[1].trim()).trim().parse().unwrap_or(0);
            let text = ex(&args[2..].join(","));
            if s == 0 || e < s {
                return String::new();
            }
            text.split_whitespace()
                .skip(s - 1)
                .take(e - s + 1)
                .collect::<Vec<_>>()
                .join(" ")
        }
        "abspath" | "realpath" => {
            let names = ex(args_raw);
            let cwd = std::env::current_dir().unwrap_or_default();
            names
                .split_whitespace()
                .filter_map(|n| {
                    if func == "realpath" {
                        fs::canonicalize(n)
                            .ok()
                            .map(|p| p.to_string_lossy().to_string())
                    } else {
                        Some(lexical_abspath(&cwd, n))
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        }
        "origin" => {
            let name = ex(args_raw);
            let name = name.trim();
            if automatic_var(name, makefile, None, &[]).is_some() && name.len() <= 2 {
                "automatic".to_string()
            } else if makefile.cli_overrides.contains(name) {
                "command line".to_string()
            } else if makefile.defaults.contains(name) {
                if std::env::var_os(name).is_some() {
                    "environment".to_string()
                } else {
                    "default".to_string()
                }
            } else if makefile.variables.contains_key(name) {
                "file".to_string()
            } else if std::env::var_os(name).is_some() {
                "environment".to_string()
            } else {
                "undefined".to_string()
            }
        }
        "flavor" => {
            // maked stores `=` values raw and `:=` values expanded but does
            // not record which; a value with `$` is reported as recursive.
            let name = ex(args_raw);
            let name = name.trim();
            match makefile.get_var(name) {
                None => "undefined".to_string(),
                Some(v) if v.contains('$') => "recursive".to_string(),
                Some(_) => "simple".to_string(),
            }
        }
        "file" => {
            let args = split_top_level_args(args_raw);
            let spec = ex(args.first().copied().unwrap_or("")).trim().to_string();
            let text = if args.len() > 1 {
                Some(ex(&args[1..].join(",")))
            } else {
                None
            };
            if let Some(path) = spec.strip_prefix(">>") {
                if let Ok(mut f) = fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path.trim())
                {
                    use std::io::Write;
                    if let Some(t) = text {
                        let _ = writeln!(f, "{t}");
                    }
                }
                String::new()
            } else if let Some(path) = spec.strip_prefix('>') {
                let body = text.map(|t| format!("{t}\n")).unwrap_or_default();
                let _ = fs::write(path.trim(), body);
                String::new()
            } else if let Some(path) = spec.strip_prefix('<') {
                let mut c = fs::read_to_string(path.trim()).unwrap_or_default();
                if c.ends_with('\n') {
                    c.pop();
                }
                c
            } else {
                String::new()
            }
        }
        "intcmp" => {
            let args = split_top_level_args(args_raw);
            if args.len() < 2 {
                return String::new();
            }
            let l: i128 = ex(args[0].trim()).trim().parse().unwrap_or(0);
            let r: i128 = ex(args[1].trim()).trim().parse().unwrap_or(0);
            let pick = |i: usize| args.get(i).map(|a| a.to_string());
            let branch = match l.cmp(&r) {
                std::cmp::Ordering::Less => pick(2),
                std::cmp::Ordering::Equal => pick(3).or_else(|| {
                    if args.len() == 2 {
                        None
                    } else {
                        Some(String::new())
                    }
                }),
                std::cmp::Ordering::Greater => pick(4).or_else(|| pick(3)),
            };
            match branch {
                Some(b) => ex(&b),
                None if l == r => l.to_string(),
                None => String::new(),
            }
        }
        "let" => {
            let args = split_top_level_args(args_raw);
            if args.len() < 3 {
                return String::new();
            }
            let names: Vec<String> = ex(args[0]).split_whitespace().map(str::to_string).collect();
            let words_s = ex(args[1]);
            let words: Vec<&str> = words_s.split_whitespace().collect();
            let mut scope = scoped_vars.cloned().unwrap_or_default();
            for (i, n) in names.iter().enumerate() {
                let v = if i + 1 == names.len() {
                    words.get(i..).map(|w| w.join(" ")).unwrap_or_default()
                } else {
                    words.get(i).map(|w| w.to_string()).unwrap_or_default()
                };
                scope.insert(n.clone(), v);
            }
            let body = args[2..].join(",");
            expand_variables_internal(&body, makefile, target, prereqs, Some(&scope), depth)
        }
        "call" => {
            let args = split_top_level_args(args_raw);
            if args.is_empty() {
                return String::new();
            }
            let func_name = expand_variables_internal(
                args[0].trim(),
                makefile,
                target,
                prereqs,
                scoped_vars,
                depth,
            );
            let body_opt = if let Some(sv) = scoped_vars {
                sv.get(&func_name).cloned()
            } else {
                None
            }
            .or_else(|| target.and_then(|t| makefile.get_target_var(t, &func_name)))
            .or_else(|| makefile.get_var(&func_name));

            if let Some(body) = body_opt {
                let mut child_scope = match scoped_vars {
                    Some(parent) => parent.clone(),
                    None => HashMap::new(),
                };
                child_scope.insert("0".to_string(), func_name);
                for (idx, arg) in args.iter().enumerate().skip(1) {
                    let val = expand_variables_internal(
                        arg.trim(),
                        makefile,
                        target,
                        prereqs,
                        scoped_vars,
                        depth,
                    );
                    child_scope.insert(idx.to_string(), val);
                }
                expand_variables_internal(
                    &body,
                    makefile,
                    target,
                    prereqs,
                    Some(&child_scope),
                    depth,
                )
            } else {
                String::new()
            }
        }
        "eval" => {
            let exp =
                expand_variables_internal(args_raw, makefile, target, prereqs, scoped_vars, depth);
            if crate::ast::in_execution_phase() {
                // During the build: apply simple assignments now (see
                // `set_runtime_var`); anything else cannot change the graph.
                eval_assignment_at_runtime(&exp, makefile);
            } else {
                makefile.push_eval(exp);
            }
            String::new()
        }
        "foreach" => {
            let args = split_top_level_args(args_raw);
            if args.len() >= 3 {
                let var = args[0].trim();
                let list = expand_variables_internal(
                    args[1].trim(),
                    makefile,
                    target,
                    prereqs,
                    scoped_vars,
                    depth,
                );
                let text_template = args[2..].join(",");
                let mut results = Vec::new();
                for item in list.split_whitespace() {
                    let mut child_scope = match scoped_vars {
                        Some(parent) => parent.clone(),
                        None => HashMap::new(),
                    };
                    child_scope.insert(var.to_string(), item.to_string());
                    let res = expand_variables_internal(
                        &text_template,
                        makefile,
                        target,
                        prereqs,
                        Some(&child_scope),
                        depth,
                    );
                    results.push(res);
                }
                results.join(" ")
            } else {
                String::new()
            }
        }
        "value" => {
            let var = args_raw.trim();
            if let Some(sv) = scoped_vars {
                if let Some(v) = sv.get(var) {
                    return v.clone();
                }
            }
            if let Some(t) = target {
                if let Some(v) = makefile.get_target_var(t, var) {
                    return v;
                }
            }
            makefile.get_var(var).unwrap_or_default()
        }
        "wildcard" => {
            let expanded =
                expand_variables_internal(args_raw, makefile, target, prereqs, scoped_vars, depth);
            let mut matches = Vec::new();
            for pat in expanded.split_whitespace() {
                matches.extend(expand_wildcard(pat));
            }
            matches.join(" ")
        }
        "patsubst" => {
            let args = split_top_level_args(args_raw);
            if args.len() >= 3 {
                let pat = expand_variables_internal(
                    args[0].trim(),
                    makefile,
                    target,
                    prereqs,
                    scoped_vars,
                    depth,
                );
                let repl = expand_variables_internal(
                    args[1].trim(),
                    makefile,
                    target,
                    prereqs,
                    scoped_vars,
                    depth,
                );
                let text = expand_variables_internal(
                    args[2..].join(",").trim(),
                    makefile,
                    target,
                    prereqs,
                    scoped_vars,
                    depth,
                );
                patsubst(&pat, &repl, &text)
            } else {
                String::new()
            }
        }
        "subst" => {
            let args = split_top_level_args(args_raw);
            if args.len() >= 3 {
                let from = expand_variables_internal(
                    args[0],
                    makefile,
                    target,
                    prereqs,
                    scoped_vars,
                    depth,
                );
                let to = expand_variables_internal(
                    args[1],
                    makefile,
                    target,
                    prereqs,
                    scoped_vars,
                    depth,
                );
                let text = expand_variables_internal(
                    &args[2..].join(","),
                    makefile,
                    target,
                    prereqs,
                    scoped_vars,
                    depth,
                );
                text.replace(&from, &to)
            } else {
                String::new()
            }
        }
        "filter" => {
            let args = split_top_level_args(args_raw);
            if args.len() >= 2 {
                let patterns = expand_variables_internal(
                    args[0].trim(),
                    makefile,
                    target,
                    prereqs,
                    scoped_vars,
                    depth,
                );
                let text = expand_variables_internal(
                    args[1..].join(",").trim(),
                    makefile,
                    target,
                    prereqs,
                    scoped_vars,
                    depth,
                );
                filter_words(&patterns, &text, true)
            } else {
                String::new()
            }
        }
        "filter-out" => {
            let args = split_top_level_args(args_raw);
            if args.len() >= 2 {
                let patterns = expand_variables_internal(
                    args[0].trim(),
                    makefile,
                    target,
                    prereqs,
                    scoped_vars,
                    depth,
                );
                let text = expand_variables_internal(
                    args[1..].join(",").trim(),
                    makefile,
                    target,
                    prereqs,
                    scoped_vars,
                    depth,
                );
                filter_words(&patterns, &text, false)
            } else {
                String::new()
            }
        }
        "dir" => {
            let exp =
                expand_variables_internal(args_raw, makefile, target, prereqs, scoped_vars, depth);
            dir_names(&exp)
        }
        "notdir" => {
            let exp =
                expand_variables_internal(args_raw, makefile, target, prereqs, scoped_vars, depth);
            notdir_names(&exp)
        }
        "suffix" => {
            let exp =
                expand_variables_internal(args_raw, makefile, target, prereqs, scoped_vars, depth);
            suffix_names(&exp)
        }
        "basename" => {
            let exp =
                expand_variables_internal(args_raw, makefile, target, prereqs, scoped_vars, depth);
            basename_names(&exp)
        }
        "addprefix" => {
            let args = split_top_level_args(args_raw);
            if args.len() >= 2 {
                let prefix = expand_variables_internal(
                    args[0],
                    makefile,
                    target,
                    prereqs,
                    scoped_vars,
                    depth,
                );
                let text = expand_variables_internal(
                    args[1..].join(",").trim(),
                    makefile,
                    target,
                    prereqs,
                    scoped_vars,
                    depth,
                );
                addprefix_words(&prefix, &text)
            } else {
                String::new()
            }
        }
        "addsuffix" => {
            let args = split_top_level_args(args_raw);
            if args.len() >= 2 {
                let suffix = expand_variables_internal(
                    args[0],
                    makefile,
                    target,
                    prereqs,
                    scoped_vars,
                    depth,
                );
                let text = expand_variables_internal(
                    args[1..].join(",").trim(),
                    makefile,
                    target,
                    prereqs,
                    scoped_vars,
                    depth,
                );
                addsuffix_words(&suffix, &text)
            } else {
                String::new()
            }
        }
        "join" => {
            let args = split_top_level_args(args_raw);
            if args.len() >= 2 {
                let list1 = expand_variables_internal(
                    args[0].trim(),
                    makefile,
                    target,
                    prereqs,
                    scoped_vars,
                    depth,
                );
                let list2 = expand_variables_internal(
                    args[1..].join(",").trim(),
                    makefile,
                    target,
                    prereqs,
                    scoped_vars,
                    depth,
                );
                join_words(&list1, &list2)
            } else {
                String::new()
            }
        }
        "word" => {
            let args = split_top_level_args(args_raw);
            if args.len() >= 2 {
                let n_str = expand_variables_internal(
                    args[0].trim(),
                    makefile,
                    target,
                    prereqs,
                    scoped_vars,
                    depth,
                );
                let text = expand_variables_internal(
                    args[1..].join(",").trim(),
                    makefile,
                    target,
                    prereqs,
                    scoped_vars,
                    depth,
                );
                word_n(&n_str, &text)
            } else {
                String::new()
            }
        }
        "words" => {
            let exp =
                expand_variables_internal(args_raw, makefile, target, prereqs, scoped_vars, depth);
            words_count(&exp)
        }
        "firstword" => {
            let exp =
                expand_variables_internal(args_raw, makefile, target, prereqs, scoped_vars, depth);
            firstword_str(&exp)
        }
        "lastword" => {
            let exp =
                expand_variables_internal(args_raw, makefile, target, prereqs, scoped_vars, depth);
            lastword_str(&exp)
        }
        "sort" => {
            let exp =
                expand_variables_internal(args_raw, makefile, target, prereqs, scoped_vars, depth);
            sort_words(&exp)
        }
        "strip" => {
            let exp =
                expand_variables_internal(args_raw, makefile, target, prereqs, scoped_vars, depth);
            strip_str(&exp)
        }
        "shell" => {
            let cmd =
                expand_variables_internal(args_raw, makefile, target, prereqs, scoped_vars, depth);
            shell_cmd(&cmd)
        }
        "if" => {
            let args = split_top_level_args(args_raw);
            if !args.is_empty() {
                let cond = expand_variables_internal(
                    args[0].trim(),
                    makefile,
                    target,
                    prereqs,
                    scoped_vars,
                    depth,
                );
                if !cond.trim().is_empty() {
                    if args.len() > 1 {
                        expand_variables_internal(
                            args[1].trim(),
                            makefile,
                            target,
                            prereqs,
                            scoped_vars,
                            depth,
                        )
                    } else {
                        String::new()
                    }
                } else if args.len() > 2 {
                    expand_variables_internal(
                        args[2..].join(",").trim(),
                        makefile,
                        target,
                        prereqs,
                        scoped_vars,
                        depth,
                    )
                } else {
                    String::new()
                }
            } else {
                String::new()
            }
        }
        "or" => {
            let args = split_top_level_args(args_raw);
            for arg in args {
                let val = expand_variables_internal(
                    arg.trim(),
                    makefile,
                    target,
                    prereqs,
                    scoped_vars,
                    depth,
                );
                if !val.trim().is_empty() {
                    return val;
                }
            }
            String::new()
        }
        "and" => {
            let args = split_top_level_args(args_raw);
            let mut last = String::new();
            for arg in args {
                let val = expand_variables_internal(
                    arg.trim(),
                    makefile,
                    target,
                    prereqs,
                    scoped_vars,
                    depth,
                );
                if val.trim().is_empty() {
                    return String::new();
                }
                last = val;
            }
            last
        }
        "info" => {
            let exp =
                expand_variables_internal(args_raw, makefile, target, prereqs, scoped_vars, depth);
            println!("{exp}");
            String::new()
        }
        "warning" => {
            let exp =
                expand_variables_internal(args_raw, makefile, target, prereqs, scoped_vars, depth);
            eprintln!("Makefile: [WARNING] {exp}");
            String::new()
        }
        "error" => {
            let exp =
                expand_variables_internal(args_raw, makefile, target, prereqs, scoped_vars, depth);
            eprintln!("Makefile: *** {exp}. Stop.");
            std::process::exit(2);
        }
        _ => String::new(),
    }
}

fn split_top_level_args(s: &str) -> Vec<&str> {
    let mut args = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    let bytes = s.as_bytes();

    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'(' | b'{' => depth += 1,
            b')' | b'}' => {
                if depth > 0 {
                    depth -= 1;
                }
            }
            b',' if depth == 0 => {
                args.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    args.push(&s[start..]);
    args
}

fn find_top_level_char(s: &str, target_ch: char) -> Option<usize> {
    let mut paren_depth = 0;
    let mut brace_depth = 0;
    for (i, b) in s.bytes().enumerate() {
        match b {
            b'(' => paren_depth += 1,
            b')' => {
                if paren_depth > 0 {
                    paren_depth -= 1;
                }
            }
            b'{' => brace_depth += 1,
            b'}' => {
                if brace_depth > 0 {
                    brace_depth -= 1;
                }
            }
            _ => {
                if paren_depth == 0 && brace_depth == 0 && b == target_ch as u8 {
                    return Some(i);
                }
            }
        }
    }
    None
}

fn process_pending_evals(
    makefile: &mut Makefile,
    cli_vars: &[(String, String)],
) -> Result<(), ParseError> {
    loop {
        let pending = makefile.drain_eval_queue();
        if pending.is_empty() {
            break;
        }
        for chunk in pending {
            if !chunk.trim().is_empty() {
                parse_makefile_into(makefile, &chunk, cli_vars)?;
            }
        }
    }
    Ok(())
}

fn parse_subst_ref(s: &str) -> Option<(&str, &str, &str)> {
    let mut d = 0;
    let mut colon = None;
    for (i, b) in s.bytes().enumerate() {
        match b {
            b'(' | b'{' => d += 1,
            b')' | b'}' => {
                if d > 0 {
                    d -= 1;
                }
            }
            b':' if d == 0 && colon.is_none() => colon = Some(i),
            b',' if d == 0 && colon.is_none() => return None,
            _ => {}
        }
    }
    if let Some(c_idx) = colon {
        let var_name = s[..c_idx].trim();
        if var_name.is_empty() || var_name.contains(char::is_whitespace) {
            return None;
        }
        let rest = &s[c_idx + 1..];
        let mut eq = None;
        d = 0;
        for (i, b) in rest.bytes().enumerate() {
            match b {
                b'(' | b'{' => d += 1,
                b')' | b'}' => {
                    if d > 0 {
                        d -= 1;
                    }
                }
                b'=' if d == 0 && eq.is_none() => {
                    eq = Some(i);
                    break;
                }
                _ => {}
            }
        }
        if let Some(eq_idx) = eq {
            let pat = &rest[..eq_idx];
            let repl = &rest[eq_idx + 1..];
            return Some((var_name, pat, repl));
        }
    }
    None
}

fn match_pattern_stem(pattern: &str, target: &str) -> Option<String> {
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

fn patsubst(pattern: &str, replacement: &str, text: &str) -> String {
    let mut words = Vec::new();
    for word in text.split_whitespace() {
        if pattern.contains('%') {
            if let Some(stem) = match_pattern_stem(pattern, word) {
                words.push(replacement.replace('%', &stem));
            } else {
                words.push(word.to_string());
            }
        } else if word == pattern {
            words.push(replacement.to_string());
        } else {
            words.push(word.to_string());
        }
    }
    words.join(" ")
}

/// GNU make automatic variables: `@ < ^ + ? * |` and their `D`/`F` forms
/// (`$(@D)`, `$(^F)`, ...). `$?` compares mtimes at expansion time, which is
/// when recipes are expanded: after prerequisites were brought up to date.
fn automatic_var(
    name: &str,
    makefile: &Makefile,
    target: Option<&str>,
    prereqs: &[String],
) -> Option<String> {
    let (base, part) = match name.len() {
        1 => (name, None),
        2 if name.ends_with('D') || name.ends_with('F') => (&name[..1], name.chars().nth(1)),
        _ => return None,
    };
    let order_only: &[String] = target
        .and_then(|t| makefile.order_only.get(t))
        .map_or(&[], Vec::as_slice);
    let normal: Vec<String> = prereqs
        .iter()
        .filter(|d| !order_only.contains(d))
        .cloned()
        .collect();
    let unique = || {
        let mut seen = HashSet::new();
        normal
            .iter()
            .filter(|d| seen.insert(d.as_str()))
            .cloned()
            .collect::<Vec<_>>()
    };
    let words: Vec<String> = match base {
        "@" => target.map(|t| vec![t.to_string()]).unwrap_or_default(),
        "<" => normal.first().cloned().into_iter().collect(),
        "^" => unique(),
        "+" => normal.clone(),
        "?" => {
            let tm = target.and_then(|t| fs::metadata(t).and_then(|m| m.modified()).ok());
            unique()
                .into_iter()
                .filter(|d| match (tm, fs::metadata(d).and_then(|m| m.modified())) {
                    (Some(t), Ok(dm)) => dm > t,
                    (None, _) => true,
                    (Some(_), Err(_)) => true,
                })
                .collect()
        }
        "*" => target
            .map(|t| match makefile.static_stems.get(t) {
                Some(stem) => vec![stem.clone()],
                None => vec![t.rfind('.').map_or(t, |i| &t[..i]).to_string()],
            })
            .unwrap_or_default(),
        "|" => order_only.to_vec(),
        _ => return None,
    };
    let mapped: Vec<String> = match part {
        None => words,
        Some('D') => words
            .iter()
            .map(|w| match w.rfind('/') {
                Some(0) => "/".to_string(),
                Some(i) => w[..i].to_string(),
                None => ".".to_string(),
            })
            .collect(),
        Some(_) => words
            .iter()
            .map(|w| w.rsplit('/').next().unwrap_or(w).to_string())
            .collect(),
    };
    Some(mapped.join(" "))
}

/// `NAME := v`, `NAME ::= v`, `NAME = v`, `NAME += v`, `NAME ?= v` from an
/// `$(eval)` run during the build, applied to the runtime overlay.
fn eval_assignment_at_runtime(text: &str, makefile: &Makefile) {
    for line in text.lines() {
        let line = line.trim();
        let Some(eq) = find_top_level_char(line, '=') else {
            continue;
        };
        let lhs = &line[..eq];
        let value = line[eq + 1..].trim();
        let (name, op) = if let Some(n) = lhs.strip_suffix("::") {
            (n, ':')
        } else if let Some(n) = lhs.strip_suffix(':') {
            (n, ':')
        } else if let Some(n) = lhs.strip_suffix('+') {
            (n, '+')
        } else if let Some(n) = lhs.strip_suffix('?') {
            (n, '?')
        } else {
            (lhs, '=')
        };
        let name = name.trim();
        if name.is_empty() || name.contains(char::is_whitespace) {
            continue;
        }
        let val = match op {
            ':' => expand_variables(value, makefile, None, &[]),
            '+' => {
                let prev = makefile.get_var(name).unwrap_or_default();
                if prev.is_empty() {
                    value.to_string()
                } else {
                    format!("{prev} {value}")
                }
            }
            '?' if makefile.get_var(name).is_some() => continue,
            _ => value.to_string(),
        };
        crate::ast::set_runtime_var(name.to_string(), val);
    }
}

/// `$(abspath)`: make `name` absolute and fold `.`/`..` without touching
/// the filesystem (no symlink resolution, unlike `$(realpath)`).
fn lexical_abspath(cwd: &Path, name: &str) -> String {
    let joined = if Path::new(name).is_absolute() {
        Path::new(name).to_path_buf()
    } else {
        cwd.join(name)
    };
    let mut parts: Vec<String> = Vec::new();
    for c in joined.components() {
        match c {
            std::path::Component::ParentDir => {
                parts.pop();
            }
            std::path::Component::Normal(p) => parts.push(p.to_string_lossy().to_string()),
            _ => {}
        }
    }
    format!("/{}", parts.join("/"))
}

/// Marks a backslash-newline joined by the first parsing pass.
const CONTINUATION: char = '\u{1}';

/// A line ending in an odd number of backslashes continues on the next line.
fn ends_with_unescaped_backslash(line: &str) -> bool {
    line.bytes().rev().take_while(|&b| b == b'\\').count() % 2 == 1
}

/// Recipe lines keep `\`+newline for the shell, dropping one leading tab
/// from each continuation line. Other lines turn each continuation and the
/// whitespace around it into a single space, as GNU make does.
fn resolve_continuations(line: &str, is_recipe: bool) -> String {
    if !line.contains(CONTINUATION) {
        return line.to_string();
    }
    let mut parts = line.split(CONTINUATION);
    let mut out = parts.next().unwrap_or_default().to_string();
    for part in parts {
        if is_recipe {
            out.push_str("\\\n");
            out.push_str(part.strip_prefix('\t').unwrap_or(part));
        } else {
            let kept = out.trim_end().len();
            out.truncate(kept);
            out.push(' ');
            out.push_str(part.trim_start());
        }
    }
    out
}

/// Drop a make comment: everything from the first `#` not written as `\#`.
/// `\#` becomes a literal `#`.
fn strip_comment(line: &str) -> std::borrow::Cow<'_, str> {
    if !line.contains('#') {
        return std::borrow::Cow::Borrowed(line);
    }
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'#') => {
                out.push('#');
                chars.next();
            }
            '#' => break,
            _ => out.push(c),
        }
    }
    std::borrow::Cow::Owned(out)
}

fn filter_words(patterns: &str, text: &str, include: bool) -> String {
    let pats: Vec<&str> = patterns.split_whitespace().collect();
    let mut result = Vec::new();

    for word in text.split_whitespace() {
        let matches = pats.iter().any(|pat| {
            if pat.contains('%') {
                match_pattern_stem(pat, word).is_some()
            } else {
                *pat == word
            }
        });

        if matches == include {
            result.push(word);
        }
    }

    result.join(" ")
}

fn dir_names(names: &str) -> String {
    let mut res = Vec::new();
    for word in names.split_whitespace() {
        if let Some(pos) = word.rfind('/') {
            res.push(word[..=pos].to_string());
        } else {
            res.push("./".to_string());
        }
    }
    res.join(" ")
}

fn notdir_names(names: &str) -> String {
    let mut res = Vec::new();
    for word in names.split_whitespace() {
        if let Some(pos) = word.rfind('/') {
            res.push(word[pos + 1..].to_string());
        } else {
            res.push(word.to_string());
        }
    }
    res.join(" ")
}

fn suffix_names(names: &str) -> String {
    let mut res = Vec::new();
    for word in names.split_whitespace() {
        let filename = word.rfind('/').map_or(word, |p| &word[p + 1..]);
        if let Some(dot_pos) = filename.rfind('.') {
            res.push(filename[dot_pos..].to_string());
        }
    }
    res.join(" ")
}

fn basename_names(names: &str) -> String {
    let mut res = Vec::new();
    for word in names.split_whitespace() {
        let dir_part = word.rfind('/').map(|p| &word[..=p]).unwrap_or("");
        let filename = word.rfind('/').map_or(word, |p| &word[p + 1..]);
        if let Some(dot_pos) = filename.rfind('.') {
            res.push(format!("{}{}", dir_part, &filename[..dot_pos]));
        } else {
            res.push(word.to_string());
        }
    }
    res.join(" ")
}

fn addprefix_words(prefix: &str, names: &str) -> String {
    names
        .split_whitespace()
        .map(|w| format!("{prefix}{w}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn addsuffix_words(suffix: &str, names: &str) -> String {
    names
        .split_whitespace()
        .map(|w| format!("{w}{suffix}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn join_words(list1: &str, list2: &str) -> String {
    let w1: Vec<&str> = list1.split_whitespace().collect();
    let w2: Vec<&str> = list2.split_whitespace().collect();
    let max_len = w1.len().max(w2.len());
    let mut res = Vec::new();

    for i in 0..max_len {
        let s1 = w1.get(i).copied().unwrap_or("");
        let s2 = w2.get(i).copied().unwrap_or("");
        res.push(format!("{s1}{s2}"));
    }
    res.join(" ")
}

fn word_n(n_str: &str, text: &str) -> String {
    let n = n_str.trim().parse::<usize>().unwrap_or(0);
    if n == 0 {
        return String::new();
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    words.get(n - 1).copied().unwrap_or("").to_string()
}

fn words_count(text: &str) -> String {
    text.split_whitespace().count().to_string()
}

fn firstword_str(text: &str) -> String {
    text.split_whitespace().next().unwrap_or("").to_string()
}

fn lastword_str(text: &str) -> String {
    text.split_whitespace().last().unwrap_or("").to_string()
}

fn sort_words(text: &str) -> String {
    let mut words: Vec<&str> = text.split_whitespace().collect();
    words.sort();
    words.dedup();
    words.join(" ")
}

fn strip_str(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn shell_cmd(cmd: &str) -> String {
    let output = crate::executor::create_shell_command(cmd).output();

    match output {
        Ok(out) => {
            let mut s = String::from_utf8_lossy(&out.stdout).to_string();
            s = s.replace("\r\n", " ").replace('\n', " ");
            s.trim_end().to_string()
        }
        Err(_) => String::new(),
    }
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let p_chars: Vec<char> = pattern.chars().collect();
    let t_chars: Vec<char> = text.chars().collect();
    let mut p = 0;
    let mut t = 0;
    let mut star_p = None;
    let mut star_t = 0;

    while t < t_chars.len() {
        if p < p_chars.len() && p_chars[p] == '[' {
            let mut close_idx = p + 1;
            while close_idx < p_chars.len() && p_chars[close_idx] != ']' {
                close_idx += 1;
            }
            if close_idx < p_chars.len() {
                let class_chars = &p_chars[p + 1..close_idx];
                let mut matched = false;
                let ch = t_chars[t];
                let mut ci = 0;
                while ci < class_chars.len() {
                    if ci + 2 < class_chars.len() && class_chars[ci + 1] == '-' {
                        let start = class_chars[ci];
                        let end = class_chars[ci + 2];
                        if ch >= start && ch <= end {
                            matched = true;
                            break;
                        }
                        ci += 3;
                    } else {
                        if class_chars[ci] == ch {
                            matched = true;
                            break;
                        }
                        ci += 1;
                    }
                }
                if matched {
                    p = close_idx + 1;
                    t += 1;
                    continue;
                }
            }
        }

        if p < p_chars.len() && (p_chars[p] == '?' || p_chars[p] == t_chars[t]) {
            p += 1;
            t += 1;
        } else if p < p_chars.len() && p_chars[p] == '*' {
            star_p = Some(p);
            p += 1;
            star_t = t;
        } else if let Some(sp) = star_p {
            p = sp + 1;
            star_t += 1;
            t = star_t;
        } else {
            return false;
        }
    }

    while p < p_chars.len() && p_chars[p] == '*' {
        p += 1;
    }

    p == p_chars.len()
}

fn expand_wildcard(pattern: &str) -> Vec<String> {
    if !pattern.contains('*') && !pattern.contains('?') && !pattern.contains('[') {
        if Path::new(pattern).exists() {
            return vec![pattern.to_string()];
        } else {
            return vec![];
        }
    }

    let parts: Vec<&str> = pattern.split('/').collect();
    let mut current_paths = vec![String::new()];

    for (part_idx, &part) in parts.iter().enumerate() {
        let is_last = part_idx == parts.len() - 1;
        let mut next_paths = Vec::new();

        for base in current_paths {
            let search_dir = if base.is_empty() {
                Path::new(".")
            } else {
                Path::new(&base)
            };

            if !part.contains('*') && !part.contains('?') && !part.contains('[') {
                let candidate = if base.is_empty() {
                    part.to_string()
                } else {
                    format!("{base}/{part}")
                };
                if Path::new(&candidate).exists() {
                    next_paths.push(candidate);
                }
            } else if let Ok(entries) = fs::read_dir(search_dir) {
                for entry in entries.flatten() {
                    let fname = entry.file_name();
                    let fname_str = fname.to_string_lossy();
                    if glob_match(part, &fname_str) {
                        let path_str = if base.is_empty() {
                            fname_str.to_string()
                        } else {
                            format!("{base}/{fname_str}")
                        };

                        if is_last || entry.path().is_dir() {
                            next_paths.push(path_str);
                        }
                    }
                }
            }
        }

        current_paths = next_paths;
        if current_paths.is_empty() {
            break;
        }
    }

    current_paths.sort();
    current_paths
}

enum TargetType {
    Normal(Vec<String>),
    Pattern(usize), // index into pattern_rules
}

pub fn parse_makefile_content(
    content: &str,
    cli_vars: &[(String, String)],
) -> Result<Makefile, ParseError> {
    let mut makefile = Makefile::new();
    parse_makefile_into(&mut makefile, content, cli_vars)?;
    Ok(makefile)
}

pub fn parse_makefile_into(
    makefile: &mut Makefile,
    content: &str,
    cli_vars: &[(String, String)],
) -> Result<(), ParseError> {
    for (k, v) in cli_vars {
        makefile.set_cli_var(k.clone(), v.clone());
    }
    let mut phony_targets: HashSet<String> = HashSet::new();
    let mut current_target: Option<TargetType> = None;
    // Prerequisites written on the rule line now being read, per target:
    // when its recipe starts they move to the front, because GNU make takes
    // `$<` and the start of `$^` from the rule that has the recipe.
    let mut line_prereqs: HashMap<String, Vec<String>> = HashMap::new();

    // First pass: join line continuations (lines ending with backslash \)
    let raw_lines: Vec<&str> = content.lines().collect();
    let mut combined_lines: Vec<(usize, String)> = Vec::new();
    let mut i = 0;

    while i < raw_lines.len() {
        let line_num = i + 1;
        let mut line = raw_lines[i].to_string();
        // Keep each backslash-newline as a marker: a recipe line passes it to
        // the shell as `\`+newline, any other line turns it and the space
        // around it into one space (`resolve_continuations`).
        while ends_with_unescaped_backslash(&line) && i + 1 < raw_lines.len() {
            line.pop(); // remove \
            i += 1;
            line.push(CONTINUATION);
            line.push_str(raw_lines[i]);
        }
        combined_lines.push((line_num, line));
        i += 1;
    }

    // Conditionals stack: (condition_is_active, branch_has_matched)
    let mut cond_stack: Vec<(bool, bool)> = Vec::new();

    let mut line_idx = 0;
    while line_idx < combined_lines.len() {
        let (line_num, ref joined) = combined_lines[line_idx];
        line_idx += 1;
        let resolved =
            resolve_continuations(joined, joined.starts_with('\t') && current_target.is_some());
        let line = &resolved;

        // In a rule, a tab-led line is recipe text and keeps its '#'. Every
        // other line loses its comment first, as in GNU make (`\#` is a
        // literal '#'), so directives like `include x # c` and `endif # c` work.
        let is_recipe_line = line.starts_with('\t') && current_target.is_some();
        let uncommented;
        let trimmed: &str = if is_recipe_line {
            line.trim()
        } else {
            uncommented = strip_comment(line);
            uncommented.trim()
        };

        // Check conditional directives: ifeq, ifneq, ifdef, ifndef, else, endif
        if is_recipe_line {
            // recipe text: no directive processing
        } else if trimmed.starts_with("ifeq")
            || trimmed.starts_with("ifneq")
            || trimmed.starts_with("ifdef")
            || trimmed.starts_with("ifndef")
        {
            let currently_active = cond_stack.last().is_none_or(|(act, _)| *act);
            let branch_result = if currently_active {
                eval_condition(trimmed, makefile)
            } else {
                false
            };
            cond_stack.push((currently_active && branch_result, branch_result));
            continue;
        } else if trimmed == "else" || trimmed.starts_with("else ") {
            if let Some((_act, matched)) = cond_stack.pop() {
                let parent_active = cond_stack.last().is_none_or(|(p_act, _)| *p_act);
                if trimmed == "else" {
                    let new_active = parent_active && !matched;
                    cond_stack.push((new_active, matched || new_active));
                } else {
                    let sub_cond = trimmed[5..].trim();
                    let sub_res = if parent_active && !matched {
                        eval_condition(sub_cond, makefile)
                    } else {
                        false
                    };
                    cond_stack.push((sub_res, matched || sub_res));
                }
            } else {
                return Err(ParseError::SyntaxError(
                    "extraneous `else'".into(),
                    line_num,
                ));
            }
            continue;
        } else if trimmed == "endif" {
            if cond_stack.pop().is_none() {
                return Err(ParseError::SyntaxError(
                    "extraneous `endif'".into(),
                    line_num,
                ));
            }
            continue;
        }

        // If currently in an inactive conditional branch, skip this line
        if cond_stack.iter().any(|(act, _)| !*act) {
            continue;
        }

        if let Some(def_header) = trimmed.strip_prefix("define ") {
            let def_header = def_header.trim();
            let (var_name, is_immediate) = if let Some(eq_pos) = def_header.find('=') {
                let name = def_header[..eq_pos].trim();
                let imm = name.ends_with(':');
                let clean_name = if imm {
                    name[..name.len() - 1].trim()
                } else {
                    name
                };
                (clean_name.to_string(), imm)
            } else {
                (def_header.to_string(), false)
            };
            let mut def_body = Vec::new();
            while line_idx < combined_lines.len() {
                let (_, ref dline) = combined_lines[line_idx];
                line_idx += 1;
                if dline.trim() == "endef" {
                    break;
                }
                def_body.push(dline.replace(CONTINUATION, "\\\n"));
            }
            let full_val = def_body.join("\n");
            let final_val = if is_immediate {
                expand_variables(&full_val, makefile, None, &[])
            } else {
                full_val
            };
            makefile.set_var(var_name, final_val);
            current_target = None;
            continue;
        }

        let line_trimmed_end = line.trim_end();
        if line_trimmed_end.is_empty() {
            continue;
        }

        // Recipe line: tab-led *inside a rule*. Outside one, GNU make parses a
        // tab-indented line as ordinary makefile text (redis indents
        // assignments and conditionals that way).
        if line.starts_with('\t') && current_target.is_some() {
            let cmd = line[1..].trim();
            if cmd.is_empty() {
                continue;
            }
            match current_target {
                Some(TargetType::Normal(ref target_names)) => {
                    for target_name in target_names {
                        if let Some(rule) = makefile.rules.get_mut(target_name) {
                            if rule.commands.is_empty() {
                                if let Some(first) = line_prereqs.get(target_name) {
                                    let rest: Vec<String> = rule
                                        .prereqs
                                        .iter()
                                        .filter(|p| !first.contains(p))
                                        .cloned()
                                        .collect();
                                    let mut ordered: Vec<String> = first
                                        .iter()
                                        .filter(|p| rule.prereqs.contains(p))
                                        .cloned()
                                        .collect();
                                    ordered.extend(rest);
                                    rule.prereqs = ordered;
                                }
                            }
                            rule.commands.push(cmd.to_string());
                        }
                    }
                }
                Some(TargetType::Pattern(idx)) => {
                    if let Some(p_rule) = makefile.pattern_rules.get_mut(idx) {
                        p_rule.commands.push(cmd.to_string());
                    }
                }
                None => {
                    return Err(ParseError::SyntaxError(
                        "recipe commences before first target".into(),
                        line_num,
                    ));
                }
            }
            continue;
        }

        if trimmed.starts_with('#') {
            continue;
        }

        // Include directive: include file1 file2 ... or -include ... / sinclude ...
        if trimmed.starts_with("include ")
            || trimmed.starts_with("-include ")
            || trimmed.starts_with("sinclude ")
        {
            let is_optional = trimmed.starts_with('-') || trimmed.starts_with("sinclude");
            // "include " is 8 bytes; "-include " and "sinclude " are 9.
            let prefix_len = if trimmed.starts_with("include ") {
                8
            } else {
                9
            };
            let files_str = trimmed[prefix_len..].trim();
            let expanded_files = expand_variables(files_str, makefile, None, &[]);

            for inc_token in expanded_files.split_whitespace() {
                let candidates = if inc_token.contains('*')
                    || inc_token.contains('?')
                    || inc_token.contains('[')
                {
                    let matched = expand_wildcard(inc_token);
                    if matched.is_empty() && !is_optional {
                        vec![inc_token.to_string()]
                    } else {
                        matched
                    }
                } else {
                    vec![inc_token.to_string()]
                };

                for inc_file in &candidates {
                    let resolved_inc = makefile
                        .resolve_path(inc_file)
                        .unwrap_or_else(|| inc_file.to_string());
                    if Path::new(&resolved_inc).exists() {
                        makefile.included.push(inc_file.to_string());
                        match fs::read_to_string(&resolved_inc) {
                            Ok(sub_content) => {
                                parse_makefile_into(makefile, &sub_content, cli_vars)?;
                            }
                            Err(e) => {
                                if !is_optional {
                                    return Err(ParseError::SyntaxError(
                                        format!("failed to include '{inc_file}': {e}"),
                                        line_num,
                                    ));
                                }
                            }
                        }
                    } else {
                        // It may still be made by a rule: `main` tries to
                        // remake included makefiles and restarts (GNU make's
                        // "How Makefiles Are Remade").
                        makefile.missing_includes.push((
                            inc_file.to_string(),
                            is_optional,
                            line_num,
                        ));
                    }
                }
            }
            current_target = None;
            continue;
        }

        // Comments were stripped above.
        let mut effective_line = trimmed;

        // export / unexport / override prefixes and directives.
        let mut export_this = false;
        let mut force_override = false;
        if effective_line == "export" {
            makefile.export_all = true;
            current_target = None;
            continue;
        }
        if effective_line == "unexport" {
            makefile.export_all = false;
            current_target = None;
            continue;
        }
        if let Some(rest) = effective_line.strip_prefix("override ") {
            force_override = true;
            effective_line = rest.trim_start();
        }
        if let Some(rest) = effective_line.strip_prefix("unexport ") {
            if find_top_level_char(rest, ':').is_none() {
                for name in expand_variables(rest, makefile, None, &[]).split_whitespace() {
                    makefile.exported.insert(name.to_string(), false);
                }
                current_target = None;
                continue;
            }
        }
        if let Some(rest) = effective_line.strip_prefix("export ") {
            let rest = rest.trim_start();
            if find_top_level_char(rest, '=').is_some() {
                export_this = true;
                effective_line = rest;
            } else if find_top_level_char(rest, ':').is_none() {
                for name in expand_variables(rest, makefile, None, &[]).split_whitespace() {
                    makefile.exported.insert(name.to_string(), true);
                }
                current_target = None;
                continue;
            }
        }

        if effective_line.is_empty() {
            continue;
        }

        // vpath directive
        if effective_line == "vpath" || effective_line.starts_with("vpath ") {
            if effective_line == "vpath" {
                makefile.clear_all_vpaths();
            } else {
                let rest = effective_line[6..].trim();
                let expanded_rest = expand_variables(rest, makefile, None, &[]);
                let mut parts = expanded_rest.split_whitespace();
                if let Some(pattern) = parts.next() {
                    let dirs: Vec<String> = parts
                        .flat_map(|p| p.split(':'))
                        .filter(|d| !d.is_empty())
                        .map(|d| d.to_string())
                        .collect();
                    if dirs.is_empty() {
                        makefile.clear_vpath_pattern(pattern);
                    } else {
                        makefile.add_vpath(pattern.to_string(), dirs);
                    }
                }
            }
            current_target = None;
            continue;
        }

        let colon_pos = find_top_level_char(effective_line, ':');
        let eq_pos = find_top_level_char(effective_line, '=');

        // Target-specific variable assignment: target: VAR = VAL, target: VAR := VAL, target: VAR += VAL, target: VAR ?= VAL
        if let Some(cp) = colon_pos {
            let after_colon = effective_line[cp + 1..].trim();
            if let Some(sub_eq) = find_top_level_char(after_colon, '=') {
                let var_part_raw = after_colon[..sub_eq].trim();
                let is_imm = var_part_raw.ends_with(':');
                let is_app = var_part_raw.ends_with('+');
                let is_cond = var_part_raw.ends_with('?');
                let var_name = if is_imm || is_app || is_cond {
                    var_part_raw[..var_part_raw.len() - 1].trim()
                } else {
                    var_part_raw
                };

                let clean_var = if let Some(v) = var_name.strip_prefix("override ") {
                    v.trim()
                } else if let Some(v) = var_name.strip_prefix("export ") {
                    v.trim()
                } else {
                    var_name
                };

                if !clean_var.is_empty() && !clean_var.contains(char::is_whitespace) {
                    let target_part = effective_line[..cp].trim();
                    let expanded_targets_str = expand_variables(target_part, makefile, None, &[]);
                    let raw_val = after_colon[sub_eq + 1..].trim();

                    for tgt in expanded_targets_str.split_whitespace() {
                        let val = if is_imm {
                            expand_variables(raw_val, makefile, Some(tgt), &[])
                        } else if is_app {
                            let prev = makefile
                                .get_target_var(tgt, clean_var)
                                .or_else(|| makefile.get_var(clean_var))
                                .unwrap_or_default();
                            if prev.is_empty() {
                                raw_val.to_string()
                            } else {
                                format!("{prev} {raw_val}")
                            }
                        } else if is_cond {
                            if makefile.get_target_var(tgt, clean_var).is_some()
                                || makefile.get_var(clean_var).is_some()
                            {
                                continue;
                            }
                            raw_val.to_string()
                        } else {
                            raw_val.to_string()
                        };
                        makefile.set_target_var(tgt.to_string(), clean_var.to_string(), val);
                    }
                    current_target = None;
                    process_pending_evals(makefile, cli_vars)?;
                    continue;
                }
            }
        }

        // Variable assignment: VAR = VAL, VAR := VAL, VAR += VAL
        if let Some(ep) = eq_pos {
            let is_var_assign = match colon_pos {
                None => true,
                Some(cp) => cp + 1 == ep || ep < cp,
            };

            if is_var_assign {
                let is_immediate = effective_line[..ep].ends_with(':');
                let is_append = effective_line[..ep].ends_with('+');
                let is_cond = effective_line[..ep].ends_with('?');

                let key_end = if is_immediate || is_append || is_cond {
                    ep - 1
                } else {
                    ep
                };

                // Variable names are expanded: `$(N)_FLAGS = x`.
                let key_raw = effective_line[..key_end].trim();
                let key = if key_raw.contains('$') {
                    expand_variables(key_raw, makefile, None, &[])
                        .trim()
                        .to_string()
                } else {
                    key_raw.to_string()
                };
                let raw_val = effective_line[ep + 1..].trim();
                if export_this {
                    makefile.exported.insert(key.clone(), true);
                }

                if is_cond && makefile.get_var(&key).is_some() {
                    current_target = None;
                    continue;
                }

                let val = if is_immediate {
                    expand_variables(raw_val, makefile, None, &[])
                } else if is_append {
                    let prev = makefile.get_var(&key).unwrap_or_default();
                    if prev.is_empty() {
                        raw_val.to_string()
                    } else {
                        format!("{prev} {raw_val}")
                    }
                } else {
                    raw_val.to_string()
                };

                if force_override {
                    makefile.set_var_override(key, val);
                } else {
                    makefile.set_var(key, val);
                }
                current_target = None;
                process_pending_evals(makefile, cli_vars)?;
                continue;
            }
        }

        // Target rule line: target: prereqs or %.o: %.c or .c.o:
        if let Some(cp) = colon_pos {
            let target_part = effective_line[..cp].trim();
            let mut prereqs_part = effective_line[cp + 1..].trim();

            // `target:: prereqs` (double-colon). Approximation: the rules are
            // merged, so all their prerequisites come before their recipes,
            // which then run in order. GNU make runs them as separate rules.
            let is_double_colon = prereqs_part.starts_with(':');
            if is_double_colon {
                prereqs_part = prereqs_part[1..].trim();
            }

            // `a b: | dir` order-only prerequisites. A missing one is built
            // first like a normal prerequisite; an existing one is ignored,
            // so it never makes the target out of date.
            let order_only_owned;
            let mut order_only_names: Vec<String> = Vec::new();
            if let Some(bar) = find_top_level_char(prereqs_part, '|') {
                let normal = &prereqs_part[..bar];
                let order_only = expand_variables(&prereqs_part[bar + 1..], makefile, None, &[]);
                order_only_names = order_only.split_whitespace().map(str::to_string).collect();
                let missing: Vec<&str> = order_only
                    .split_whitespace()
                    .filter(|p| !Path::new(p).exists())
                    .collect();
                order_only_owned = format!("{} {}", normal, missing.join(" "));
                prereqs_part = order_only_owned.trim();
            }

            // Static pattern rule: `targets: target-pattern: prereq-patterns`.
            if !is_double_colon {
                if let Some(c2) = find_top_level_char(prereqs_part, ':') {
                    let targets = expand_variables(target_part, makefile, None, &[]);
                    let tpat = expand_variables(prereqs_part[..c2].trim(), makefile, None, &[]);
                    let ppats =
                        expand_variables(prereqs_part[c2 + 1..].trim(), makefile, None, &[]);
                    let tpat = tpat.trim();
                    let mut targets_vec = Vec::new();
                    line_prereqs.clear();
                    for tgt in targets.split_whitespace() {
                        let Some(stem) = crate::ast::match_pattern(tpat, tgt) else {
                            eprintln!(
                                "make: Makefile:{line_num}: target '{tgt}' doesn't match the target pattern"
                            );
                            continue;
                        };
                        let prereqs: Vec<String> = ppats
                            .split_whitespace()
                            .map(|p| p.replacen('%', &stem, 1))
                            .collect();
                        line_prereqs.insert(tgt.to_string(), prereqs.clone());
                        let rule = Rule {
                            target: tgt.to_string(),
                            prereqs,
                            commands: Vec::new(),
                            is_phony: phony_targets.contains(tgt),
                            line_number: line_num,
                        };
                        makefile.add_rule(rule);
                        makefile.static_stems.insert(tgt.to_string(), stem);
                        if !order_only_names.is_empty() {
                            makefile
                                .order_only
                                .entry(tgt.to_string())
                                .or_default()
                                .extend(order_only_names.iter().cloned());
                        }
                        targets_vec.push(tgt.to_string());
                    }
                    current_target = Some(TargetType::Normal(targets_vec));
                    process_pending_evals(makefile, cli_vars)?;
                    continue;
                }
            }

            let expanded_targets_str = expand_variables(target_part, makefile, None, &[]);
            let expanded_prereqs_str = expand_variables(prereqs_part, makefile, None, &[]);

            if expanded_targets_str == ".PHONY" {
                for token in expanded_prereqs_str.split_whitespace() {
                    phony_targets.insert(token.to_string());
                    if let Some(rule) = makefile.rules.get_mut(token) {
                        rule.is_phony = true;
                    }
                }
                current_target = None;
                process_pending_evals(makefile, cli_vars)?;
                continue;
            }

            if expanded_targets_str == ".SECONDEXPANSION" {
                makefile.has_second_expansion = true;
                current_target = None;
                process_pending_evals(makefile, cli_vars)?;
                continue;
            }

            // Glob patterns in prerequisites expand like GNU make's: sorted
            // matches, or the word itself when nothing matches.
            let prereqs: Vec<String> = expanded_prereqs_str
                .split_whitespace()
                .flat_map(|s| {
                    if !s.contains('%') && (s.contains('*') || s.contains('?') || s.contains('[')) {
                        let mut m = expand_wildcard(s);
                        m.sort();
                        if m.is_empty() { vec![s.to_string()] } else { m }
                    } else {
                        vec![s.to_string()]
                    }
                })
                .collect();

            // Classic suffix rule: e.g. .c.o:
            if expanded_targets_str.starts_with('.')
                && !expanded_targets_str.contains('/')
                && !expanded_targets_str.contains(' ')
            {
                let parts: Vec<&str> = expanded_targets_str.split('.').collect();
                if parts.len() == 3
                    && parts[0].is_empty()
                    && !parts[1].is_empty()
                    && !parts[2].is_empty()
                {
                    let from_ext = parts[1];
                    let to_ext = parts[2];
                    let p_rule = PatternRule {
                        target_pattern: format!("%.{to_ext}"),
                        prereq_patterns: vec![format!("%.{from_ext}")],
                        commands: Vec::new(),
                        line_number: line_num,
                    };
                    let idx = makefile.add_pattern_rule(p_rule);
                    current_target = Some(TargetType::Pattern(idx));
                    process_pending_evals(makefile, cli_vars)?;
                    continue;
                } else if expanded_targets_str == ".SUFFIXES" {
                    // Ignore .SUFFIXES declaration
                    current_target = None;
                    process_pending_evals(makefile, cli_vars)?;
                    continue;
                }
            }

            // Pattern rule: target contains '%'
            if expanded_targets_str.contains('%') {
                let p_rule = PatternRule {
                    target_pattern: expanded_targets_str.clone(),
                    prereq_patterns: prereqs,
                    commands: Vec::new(),
                    line_number: line_num,
                };
                let idx = makefile.add_pattern_rule(p_rule);
                current_target = Some(TargetType::Pattern(idx));
                process_pending_evals(makefile, cli_vars)?;
                continue;
            }

            let target_tokens: Vec<&str> = expanded_targets_str.split_whitespace().collect();
            if target_tokens.is_empty() {
                if target_part.is_empty() {
                    return Err(ParseError::SyntaxError(
                        "missing target before colon".into(),
                        line_num,
                    ));
                }
                // Targets that expand to nothing (`$(EMPTY): x`): GNU make
                // drops the rule and its recipe.
                current_target = Some(TargetType::Normal(Vec::new()));
                process_pending_evals(makefile, cli_vars)?;
                continue;
            }

            let mut targets_vec = Vec::new();
            line_prereqs.clear();
            for &tgt in &target_tokens {
                line_prereqs.insert(tgt.to_string(), prereqs.clone());
                let is_phony = phony_targets.contains(tgt);
                let rule = Rule {
                    target: tgt.to_string(),
                    prereqs: prereqs.clone(),
                    commands: Vec::new(),
                    is_phony,
                    line_number: line_num,
                };
                makefile.add_rule(rule);
                if !order_only_names.is_empty() {
                    makefile
                        .order_only
                        .entry(tgt.to_string())
                        .or_default()
                        .extend(order_only_names.iter().cloned());
                }
                targets_vec.push(tgt.to_string());
            }

            current_target = Some(TargetType::Normal(targets_vec));
            process_pending_evals(makefile, cli_vars)?;
            continue;
        }

        // Top-level macro or function call (e.g. $(eval ...), $(info ...), $(call ...))
        if effective_line.contains('$') {
            let expanded_line = expand_variables(effective_line, makefile, None, &[]);
            process_pending_evals(makefile, cli_vars)?;
            let trimmed_exp = expanded_line.trim();
            if trimmed_exp.is_empty() {
                current_target = None;
                continue;
            }
            parse_makefile_into(makefile, trimmed_exp, cli_vars)?;
            current_target = None;
            continue;
        }

        return Err(ParseError::SyntaxError(
            format!("missing separator in line: '{line}'"),
            line_num,
        ));
    }

    process_pending_evals(makefile, cli_vars)?;

    for phony in &phony_targets {
        if let Some(rule) = makefile.rules.get_mut(phony) {
            rule.is_phony = true;
        } else {
            // `.PHONY: FORCE` with no rule: always out of date, no recipe.
            makefile.rules.insert(
                phony.clone(),
                Rule {
                    target: phony.clone(),
                    prereqs: Vec::new(),
                    commands: Vec::new(),
                    is_phony: true,
                    line_number: 0,
                },
            );
        }
    }

    Ok(())
}

fn eval_condition(line: &str, makefile: &Makefile) -> bool {
    let trimmed = line.trim();
    if let Some(var) = trimmed.strip_prefix("ifdef ") {
        let var = var.trim();
        return makefile.get_var(var).is_some_and(|v| !v.trim().is_empty());
    } else if let Some(var) = trimmed.strip_prefix("ifndef ") {
        let var = var.trim();
        return makefile.get_var(var).is_none_or(|v| v.trim().is_empty());
    }

    let is_eq = trimmed.starts_with("ifeq");
    let content = if is_eq {
        trimmed[4..].trim()
    } else {
        trimmed[5..].trim()
    };

    let (v1, v2) = if content.starts_with('(') && content.ends_with(')') {
        let inner = &content[1..content.len() - 1];
        let args = split_top_level_args(inner);
        if args.len() >= 2 {
            (
                args[0].trim().to_string(),
                args[1..].join(",").trim().to_string(),
            )
        } else {
            (inner.trim().to_string(), String::new())
        }
    } else if content.starts_with('"') || content.starts_with('\'') {
        let quote = content.chars().next().unwrap();
        let rest = &content[1..];
        if let Some(close1) = rest.find(quote) {
            let s1 = &rest[..close1];
            let after1 = rest[close1 + 1..].trim();
            if after1.starts_with(quote) {
                let rest2 = &after1[1..];
                if let Some(close2) = rest2.find(quote) {
                    let s2 = &rest2[..close2];
                    (s1.to_string(), s2.to_string())
                } else {
                    (s1.to_string(), String::new())
                }
            } else {
                (s1.to_string(), String::new())
            }
        } else {
            (content.to_string(), String::new())
        }
    } else {
        (content.to_string(), String::new())
    };

    let exp1 = expand_variables(&v1, makefile, None, &[]);
    let exp2 = expand_variables(&v2, makefile, None, &[]);

    if is_eq { exp1 == exp2 } else { exp1 != exp2 }
}
