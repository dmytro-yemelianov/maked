use maked::ast::{Makefile, Rule};
use maked::executor::{ExecutionConfig, Executor};
use maked::freshness::{FreshnessDecision, RebuildReason, evaluate_freshness};
use maked::graph::{DependencyGraph, GraphError};
use maked::parser::{expand_variables, parse_makefile_content};
use std::time::{Duration, SystemTime};

#[test]
fn test_cycle_detection_soundness() {
    let mut makefile = Makefile::new();
    // A -> B -> C -> A
    makefile.add_rule(Rule {
        target: "A".into(),
        prereqs: vec!["B".into()],
        commands: vec![],
        is_phony: false,
        line_number: 1,
    });
    makefile.add_rule(Rule {
        target: "B".into(),
        prereqs: vec!["C".into()],
        commands: vec![],
        is_phony: false,
        line_number: 2,
    });
    makefile.add_rule(Rule {
        target: "C".into(),
        prereqs: vec!["A".into()],
        commands: vec![],
        is_phony: false,
        line_number: 3,
    });

    let graph = DependencyGraph::from_makefile(&makefile);
    let res = graph.check_cycles(&makefile, "A");
    assert!(res.is_err());
    if let Err(GraphError::CircularDependency(cycle)) = res {
        assert_eq!(cycle.first(), Some(&"A".to_string()));
        assert_eq!(cycle.last(), Some(&"A".to_string()));
        assert_eq!(cycle, vec!["A", "B", "C", "A"]);
    }
}

#[test]
fn test_self_cycle_detection() {
    let mut makefile = Makefile::new();
    makefile.add_rule(Rule {
        target: "loop".into(),
        prereqs: vec!["loop".into()],
        commands: vec![],
        is_phony: false,
        line_number: 1,
    });
    let graph = DependencyGraph::from_makefile(&makefile);
    let res = graph.check_cycles(&makefile, "loop");
    assert!(res.is_err());
    if let Err(GraphError::CircularDependency(cycle)) = res {
        assert_eq!(cycle, vec!["loop", "loop"]);
    }
}

#[test]
fn test_dag_acyclic_pass() {
    let mut makefile = Makefile::new();
    // Diamond DAG: A -> [B, C], B -> D, C -> D
    makefile.add_rule(Rule {
        target: "A".into(),
        prereqs: vec!["B".into(), "C".into()],
        commands: vec![],
        is_phony: false,
        line_number: 1,
    });
    makefile.add_rule(Rule {
        target: "B".into(),
        prereqs: vec!["D".into()],
        commands: vec![],
        is_phony: false,
        line_number: 2,
    });
    makefile.add_rule(Rule {
        target: "C".into(),
        prereqs: vec!["D".into()],
        commands: vec![],
        is_phony: false,
        line_number: 3,
    });
    makefile.add_rule(Rule {
        target: "D".into(),
        prereqs: vec![],
        commands: vec![],
        is_phony: false,
        line_number: 4,
    });

    let graph = DependencyGraph::from_makefile(&makefile);
    assert!(graph.check_cycles(&makefile, "A").is_ok());
}

#[test]
fn test_suffix_rule_conversion() {
    let content = "
.c.o:
\t$(CC) -c $(CFLAGS) $< -o $@
";
    let mf = parse_makefile_content(content, &[]).expect("parse failed");
    // Built-in rules have line_number 0; exactly one rule comes from the file.
    let user_rules: Vec<_> = mf
        .pattern_rules
        .iter()
        .filter(|r| r.line_number > 0)
        .collect();
    assert_eq!(user_rules.len(), 1);
    let user_rule = user_rules[0];
    assert_eq!(user_rule.target_pattern, "%.o");
    assert_eq!(user_rule.prereq_patterns, vec!["%.c"]);
    assert_eq!(user_rule.commands, vec!["$(CC) -c $(CFLAGS) $< -o $@"]);
}

#[test]
fn test_multi_target_rules() {
    let content = "
target1 target2: dep1
\techo building $@
";
    let mf = parse_makefile_content(content, &[]).expect("parse failed");
    let r1 = mf.rules.get("target1").expect("target1 missing");
    let r2 = mf.rules.get("target2").expect("target2 missing");
    assert_eq!(r1.commands, vec!["echo building $@"]);
    assert_eq!(r2.commands, vec!["echo building $@"]);
    assert_eq!(r1.prereqs, vec!["dep1"]);
    assert_eq!(r2.prereqs, vec!["dep1"]);
}

#[test]
fn test_automatic_variables_expansion() {
    let mut makefile = Makefile::new();
    makefile.set_var("CC".into(), "clang".into());
    let prereqs = vec![
        "foo.c".to_string(),
        "bar.h".to_string(),
        "foo.c".to_string(),
    ];

    // $@ test
    assert_eq!(
        expand_variables("$@", &makefile, Some("app"), &prereqs),
        "app"
    );
    assert_eq!(
        expand_variables("$(@)", &makefile, Some("app"), &prereqs),
        "app"
    );

    // $< test (first prereq)
    assert_eq!(
        expand_variables("$<", &makefile, Some("app"), &prereqs),
        "foo.c"
    );
    assert_eq!(
        expand_variables("$(<)", &makefile, Some("app"), &prereqs),
        "foo.c"
    );

    // $^ test (deduplicated prereqs)
    assert_eq!(
        expand_variables("$^", &makefile, Some("app"), &prereqs),
        "foo.c bar.h"
    );
    assert_eq!(
        expand_variables("$(^)", &makefile, Some("app"), &prereqs),
        "foo.c bar.h"
    );

    // $* test (stem)
    assert_eq!(
        expand_variables("$*", &makefile, Some("main.o"), &prereqs),
        "main"
    );
    assert_eq!(
        expand_variables("$(*)", &makefile, Some("main.o"), &prereqs),
        "main"
    );
}

#[test]
fn test_conditionals_parsing() {
    let content = "
MODE = debug

ifeq ($(MODE), debug)
CFLAGS = -g -O0
else
CFLAGS = -O3
endif
";
    let mf = parse_makefile_content(content, &[]).expect("parse failed");
    assert_eq!(mf.get_var("CFLAGS"), Some("-g -O0".into()));
}

#[test]
fn test_cli_variable_overrides_priority() {
    let content = "
VAR = default_value
";
    let cli_vars = vec![("VAR".to_string(), "cli_override".to_string())];
    let mf = parse_makefile_content(content, &cli_vars).expect("parse failed");
    assert_eq!(mf.get_var("VAR"), Some("cli_override".into()));
}

#[test]
fn test_freshness_high_resolution_subsecond() {
    let now = SystemTime::now();
    let earlier = now - Duration::from_nanos(100);
    let later = now + Duration::from_nanos(100);

    let rule = Rule {
        target: "dummy_target_nonexistent".into(),
        prereqs: vec!["dep".into()],
        commands: vec!["echo build".into()],
        is_phony: false,
        line_number: 1,
    };

    // Missing target must rebuild
    let dec = evaluate_freshness(&rule, false, false, Some(earlier));
    assert_eq!(
        dec,
        FreshnessDecision::NeedsRebuild(RebuildReason::TargetMissing)
    );

    // A missing target without a recipe is remade too (GNU make, and
    // `needsRebuild` in the Lean model): its dependents count it as rebuilt.
    let alias_rule = Rule {
        target: "alias".into(),
        prereqs: vec!["dep".into()],
        commands: vec![],
        is_phony: false,
        line_number: 1,
    };
    let dec_alias = evaluate_freshness(&alias_rule, false, false, Some(later));
    assert_eq!(
        dec_alias,
        FreshnessDecision::NeedsRebuild(RebuildReason::TargetMissing)
    );
}

#[test]
fn test_phony_freshness() {
    let rule_with_cmd = Rule {
        target: "clean".into(),
        prereqs: vec![],
        commands: vec!["rm -f *.o".into()],
        is_phony: true,
        line_number: 1,
    };
    // Phony with commands always rebuilds
    assert_eq!(
        evaluate_freshness(&rule_with_cmd, false, false, None),
        FreshnessDecision::NeedsRebuild(RebuildReason::PhonyTarget)
    );

    let rule_alias = Rule {
        target: "all".into(),
        prereqs: vec!["app".into()],
        commands: vec![],
        is_phony: true,
        line_number: 1,
    };
    let dep_time = SystemTime::now();
    // A phony target without a recipe is remade too (GNU make).
    assert_eq!(
        evaluate_freshness(&rule_alias, false, false, Some(dep_time)),
        FreshnessDecision::NeedsRebuild(RebuildReason::PhonyTarget)
    );
}

#[test]
fn test_monotonicity_execution_stats() {
    let content = "
step1:
\t@echo step1

step2: step1
\t@echo step2
";
    let mf = parse_makefile_content(content, &[]).expect("parse failed");
    let graph = DependencyGraph::from_makefile(&mf);
    let config = ExecutionConfig {
        keep_going: false,
        jobs: 2,
        dry_run: true,
        always_make: true,
        silent: true,
        question: false,
        use_hash: false,
        ignore_errors: false,
        touch_only: false,
        profile: false,
        trace_file: None,
        ..Default::default()
    };
    let executor = Executor::new(&mf, &graph, config);
    let stats = executor.execute("step2").expect("execution failed");
    assert_eq!(stats.total_evaluated, 2);
    assert_eq!(stats.targets_rebuilt, 2);
    assert_eq!(stats.commands_executed, 2);
}

#[test]
fn test_cryptographic_hash_freshness() {
    use maked::freshness::evaluate_freshness_hash;
    use maked::hash::{BuildDatabase, TargetRecord};
    use std::collections::HashMap;

    let mut db = BuildDatabase::default();
    let mut prereq_hashes = HashMap::new();
    prereq_hashes.insert("src.c".to_string(), "hash_c_v1".to_string());

    db.update_record(
        "app.o".to_string(),
        TargetRecord {
            target_hash: "hash_o_v1".to_string(),
            recipe_hash: "recipe_v1".to_string(),
            prereq_hashes,
        },
    );

    let rule = Rule {
        target: "app.o".into(),
        prereqs: vec!["src.c".into()],
        commands: vec!["cc -c src.c".into()],
        is_phony: false,
        line_number: 1,
    };

    // If target does not exist on disk, NeedsRebuild(TargetMissing)
    let dec = evaluate_freshness_hash(&rule, false, false, "cc -c src.c", &db);
    assert_eq!(
        dec,
        FreshnessDecision::NeedsRebuild(RebuildReason::TargetMissing)
    );
}

#[test]
fn test_gnu_functions_text_manipulation() {
    let mut mf = Makefile::new();
    mf.set_var("TEXT".into(), "feet on the street".into());
    assert_eq!(
        expand_variables("$(subst ee,EE,$(TEXT))", &mf, None, &[]),
        "fEEt on the strEEt"
    );

    assert_eq!(
        expand_variables("$(patsubst %.c,%.o,x.c.c bar.c)", &mf, None, &[]),
        "x.c.o bar.o"
    );

    assert_eq!(
        expand_variables("$(filter %.c %.s,foo.c bar.s baz.h qux.c)", &mf, None, &[]),
        "foo.c bar.s qux.c"
    );

    assert_eq!(
        expand_variables(
            "$(filter-out %.c %.s,foo.c bar.s baz.h qux.c)",
            &mf,
            None,
            &[]
        ),
        "baz.h"
    );

    assert_eq!(
        expand_variables("$(strip   a   b    c   )", &mf, None, &[]),
        "a b c"
    );

    assert_eq!(
        expand_variables("$(sort foo bar lose foo)", &mf, None, &[]),
        "bar foo lose"
    );

    assert_eq!(
        expand_variables("$(word 2,foo bar baz)", &mf, None, &[]),
        "bar"
    );

    assert_eq!(
        expand_variables("$(words foo bar baz qux)", &mf, None, &[]),
        "4"
    );

    assert_eq!(
        expand_variables("$(firstword foo bar baz)", &mf, None, &[]),
        "foo"
    );

    assert_eq!(
        expand_variables("$(lastword foo bar baz)", &mf, None, &[]),
        "baz"
    );
}

#[test]
fn test_gnu_functions_file_names() {
    let mf = Makefile::new();
    assert_eq!(
        expand_variables("$(dir src/foo.c hacks)", &mf, None, &[]),
        "src/ ./"
    );

    assert_eq!(
        expand_variables("$(notdir src/foo.c hacks)", &mf, None, &[]),
        "foo.c hacks"
    );

    assert_eq!(
        expand_variables("$(suffix src/foo.c src-1.0/bar.h hacks)", &mf, None, &[]),
        ".c .h"
    );

    assert_eq!(
        expand_variables("$(basename src/foo.c src-1.0/bar hacks)", &mf, None, &[]),
        "src/foo src-1.0/bar hacks"
    );

    assert_eq!(
        expand_variables("$(addprefix src/,foo bar)", &mf, None, &[]),
        "src/foo src/bar"
    );

    assert_eq!(
        expand_variables("$(addsuffix .c,foo bar)", &mf, None, &[]),
        "foo.c bar.c"
    );

    assert_eq!(
        expand_variables("$(join a b,.c .o)", &mf, None, &[]),
        "a.c b.o"
    );
}

#[test]
fn test_gnu_functions_substitution_references() {
    let mut mf = Makefile::new();
    mf.set_var("SRCS".into(), "main.c utils.c".into());

    // Suffix replacement $(VAR:old=new)
    assert_eq!(
        expand_variables("$(SRCS:.c=.o)", &mf, None, &[]),
        "main.o utils.o"
    );

    // Pattern replacement $(VAR:%.c=build/%.o)
    assert_eq!(
        expand_variables("$(SRCS:%.c=build/%.o)", &mf, None, &[]),
        "build/main.o build/utils.o"
    );
}

#[test]
fn test_gnu_functions_conditionals_and_shell() {
    let mf = Makefile::new();
    assert_eq!(expand_variables("$(if 1,yes,no)", &mf, None, &[]), "yes");

    assert_eq!(expand_variables("$(if ,yes,no)", &mf, None, &[]), "no");

    assert_eq!(
        expand_variables("$(or ,first,second)", &mf, None, &[]),
        "first"
    );

    assert_eq!(
        expand_variables("$(and 1,2,final)", &mf, None, &[]),
        "final"
    );

    assert_eq!(expand_variables("$(and 1,,final)", &mf, None, &[]), "");

    let shell_res = expand_variables("$(shell echo 'hello make')", &mf, None, &[]);
    assert_eq!(shell_res, "hello make");
}

#[test]
fn test_gnu_functions_wildcard() {
    let mf = Makefile::new();
    let res = expand_variables("$(wildcard src/*.rs)", &mf, None, &[]);
    assert!(res.contains("src/ast.rs"));
    assert!(res.contains("src/parser.rs"));
    assert!(res.contains("src/executor.rs"));
}

#[test]
fn test_vpath_file_resolution() {
    use std::fs;
    let temp_dir = std::env::temp_dir().join("maked_vpath_test");
    let src_dir = temp_dir.join("src");
    let _ = fs::create_dir_all(&src_dir);
    let c_file = src_dir.join("test_module.c");
    fs::write(&c_file, "int x = 42;").expect("write test file");

    let makefile_content = format!(
        "
vpath %.c {}

test_module.o: test_module.c
\t@echo Building from $< to $@
",
        src_dir.display()
    );

    let mf = parse_makefile_content(&makefile_content, &[]).expect("parse vpath makefile");
    let rule = mf.get_rule("test_module.o").expect("find rule");
    assert_eq!(rule.prereqs, vec![c_file.to_str().unwrap().to_string()]);

    let _ = fs::remove_dir_all(&temp_dir);
}
