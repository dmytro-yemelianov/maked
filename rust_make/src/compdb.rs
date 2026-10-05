use crate::ast::Makefile;
use crate::graph::DependencyGraph;
use crate::parser::expand_variables;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompDbEntry {
    pub directory: String,
    pub command: String,
    pub file: String,
    pub output: Option<String>,
}

/// Identifies whether a binary name represents a C/C++ compiler
fn is_compiler_command(first_token: &str) -> bool {
    let name = Path::new(first_token)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(first_token);

    name == "cc"
        || name == "gcc"
        || name == "clang"
        || name == "c++"
        || name == "g++"
        || name == "clang++"
        || name == "icc"
        || name == "icpc"
        || name.ends_with("-gcc")
        || name.ends_with("-clang")
        || name.ends_with("-g++")
        || name.ends_with("-clang++")
        || name.ends_with("-cc")
}

/// Checks if a file path is a C/C++ or assembly source file
fn is_source_file(path: &str) -> bool {
    let ext = Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase());

    matches!(
        ext.as_deref(),
        Some("c")
            | Some("cc")
            | Some("cpp")
            | Some("cxx")
            | Some("m")
            | Some("mm")
            | Some("s")
            | Some("asm")
    )
}

/// Extracts compilation database entries from an evaluated Makefile AST
pub fn generate_compilation_database(
    makefile: &Makefile,
    _graph: &DependencyGraph,
    base_dir: Option<&Path>,
) -> Vec<CompDbEntry> {
    let current_dir = base_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .to_string_lossy()
        .to_string();

    let mut entries = Vec::new();

    // Sort target names for deterministic JSON output
    let mut targets: Vec<&String> = makefile.rules.keys().collect();
    targets.sort();

    for target in targets {
        let rule = &makefile.rules[target];
        if rule.commands.is_empty() || rule.is_phony {
            continue;
        }

        for raw_cmd in &rule.commands {
            let mut s = raw_cmd.trim();
            while s.starts_with('@') || s.starts_with('-') || s.starts_with('+') {
                s = s[1..].trim_start();
            }

            let expanded_cmd = expand_variables(s, makefile, Some(target), &rule.prereqs);
            let tokens: Vec<&str> = expanded_cmd.split_whitespace().collect();
            if tokens.is_empty() {
                continue;
            }

            let first = tokens[0];
            let is_cc = is_compiler_command(first);
            let has_compile_flag = tokens.iter().any(|&t| t == "-c");

            // Look for source file
            let mut source_file = None;
            for &token in &tokens[1..] {
                if !token.starts_with('-') && is_source_file(token) {
                    source_file = Some(token.to_string());
                    break;
                }
            }

            // Fallback: check rule prerequisites for a source file
            if source_file.is_none() {
                for dep in &rule.prereqs {
                    if is_source_file(dep) {
                        source_file = Some(dep.clone());
                        break;
                    }
                }
            }

            // Look for explicit -o output
            let mut output_file = None;
            let mut i = 0;
            while i < tokens.len() {
                if tokens[i] == "-o" && i + 1 < tokens.len() {
                    output_file = Some(tokens[i + 1].to_string());
                    break;
                }
                i += 1;
            }

            if output_file.is_none() && !rule.target.is_empty() {
                output_file = Some(rule.target.clone());
            }

            if let Some(src) = source_file {
                // If it's a compiler invocation or has -c with a source file, include it
                if is_cc || has_compile_flag {
                    entries.push(CompDbEntry {
                        directory: current_dir.clone(),
                        command: expanded_cmd,
                        file: src,
                        output: output_file,
                    });
                }
            }
        }
    }

    entries
}

/// Serializes CompDb entries into formatted JSON adhering to Clang Compilation Database spec
pub fn emit_compdb_json(entries: &[CompDbEntry]) -> String {
    let mut out = String::from("[\n");

    for (idx, entry) in entries.iter().enumerate() {
        out.push_str("  {\n");
        out.push_str(&format!(
            "    \"directory\": \"{}\",\n",
            escape_json(&entry.directory)
        ));
        out.push_str(&format!(
            "    \"command\": \"{}\",\n",
            escape_json(&entry.command)
        ));
        out.push_str(&format!("    \"file\": \"{}\"", escape_json(&entry.file)));

        if let Some(ref o) = entry.output {
            out.push_str(",\n");
            out.push_str(&format!("    \"output\": \"{}\"\n", escape_json(o)));
        } else {
            out.push('\n');
        }

        if idx + 1 < entries.len() {
            out.push_str("  },\n");
        } else {
            out.push_str("  }\n");
        }
    }

    out.push_str("]\n");
    out
}

fn escape_json(s: &str) -> String {
    let mut res = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => res.push_str("\\\""),
            '\\' => res.push_str("\\\\"),
            '\n' => res.push_str("\\n"),
            '\r' => res.push_str("\\r"),
            '\t' => res.push_str("\\t"),
            _ => res.push(c),
        }
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Rule;

    #[test]
    fn test_generate_compdb_simple() {
        let mut makefile = Makefile::new();
        makefile.set_var("CC".to_string(), "gcc".to_string());
        makefile.set_var("CFLAGS".to_string(), "-O2 -Wall".to_string());

        makefile.add_rule(Rule {
            target: "foo.o".to_string(),
            prereqs: vec!["foo.c".to_string(), "foo.h".to_string()],
            commands: vec!["$(CC) $(CFLAGS) -c $< -o $@".to_string()],
            is_phony: false,
            line_number: 1,
        });

        makefile.add_rule(Rule {
            target: "app".to_string(),
            prereqs: vec!["foo.o".to_string()],
            commands: vec!["gcc foo.o -o app".to_string()],
            is_phony: false,
            line_number: 5,
        });

        let graph = DependencyGraph::from_makefile(&makefile);
        let entries =
            generate_compilation_database(&makefile, &graph, Some(Path::new("/tmp/test_proj")));

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].file, "foo.c");
        assert_eq!(entries[0].output.as_deref(), Some("foo.o"));
        assert_eq!(entries[0].directory, "/tmp/test_proj");
        assert_eq!(entries[0].command, "gcc -O2 -Wall -c foo.c -o foo.o");

        let json = emit_compdb_json(&entries);
        assert!(json.contains("\"directory\": \"/tmp/test_proj\""));
        assert!(json.contains("\"file\": \"foo.c\""));
        assert!(json.contains("\"output\": \"foo.o\""));
        assert!(json.contains("\"command\": \"gcc -O2 -Wall -c foo.c -o foo.o\""));
    }
}
