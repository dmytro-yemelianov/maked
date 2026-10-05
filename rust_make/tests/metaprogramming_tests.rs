use maked::executor::{ExecutionConfig, Executor};
use maked::graph::DependencyGraph;
use maked::parser::parse_makefile_content;
use std::fs;

fn make_config() -> ExecutionConfig {
    ExecutionConfig {
        jobs: 2,
        dry_run: false,
        always_make: true,
        silent: true,
        question: false,
        use_hash: false,
        ignore_errors: false,
        touch_only: false,
        profile: false,
        trace_file: None,
        ..Default::default()
    }
}

#[test]
fn test_call_function_simple_and_nested() {
    let makefile_content = "
reverse = $(2) $(1)
concat = $(1)_$(2)
nested = $(call concat,$(call reverse,a,b),end)

all:
\t@echo RESULT=$(nested)
";
    let mf = parse_makefile_content(makefile_content, &[]).expect("parse failed");
    let val = mf.get_var("nested").expect("var nested not found");
    assert_eq!(
        maked::parser::expand_variables(&val, &mf, None, &[]),
        "b a_end"
    );
}

#[test]
fn test_foreach_function() {
    let makefile_content = "
WORDS = a b c d
MAPPED = $(foreach w,$(WORDS),$(w).o)

all:
\t@echo $(MAPPED)
";
    let mf = parse_makefile_content(makefile_content, &[]).expect("parse failed");
    let val = mf.get_var("MAPPED").expect("var MAPPED not found");
    let expanded = maked::parser::expand_variables(&val, &mf, None, &[]);
    assert_eq!(expanded, "a.o b.o c.o d.o");
}

#[test]
fn test_define_multiline_and_call() {
    let makefile_content = "
define TEMPLATE
$(1):
\t@echo Building $(1)
endef

$(eval $(call TEMPLATE,server))
$(eval $(call TEMPLATE,client))

all: server client
";
    let mf = parse_makefile_content(makefile_content, &[]).expect("parse failed");
    assert!(
        mf.rules.contains_key("server"),
        "server rule should exist from eval"
    );
    assert!(
        mf.rules.contains_key("client"),
        "client rule should exist from eval"
    );
    assert_eq!(mf.rules["server"].commands, vec!["@echo Building server"]);
    assert_eq!(mf.rules["client"].commands, vec!["@echo Building client"]);

    let graph = DependencyGraph::from_makefile(&mf);
    let executor = Executor::new(&mf, &graph, make_config());
    let stats = executor.execute("all").expect("execution failed");
    assert_eq!(stats.targets_rebuilt, 3);
}

#[test]
fn test_target_specific_variables_isolation() {
    let makefile_content = "
CFLAGS = -O0

prog_debug: CFLAGS = -g -DDEBUG
prog_debug: CFLAGS += -Wall
prog_debug:
\t@echo DEBUG_CFLAGS=$(CFLAGS)

prog_release: CFLAGS = -O3 -DNDEBUG
prog_release:
\t@echo RELEASE_CFLAGS=$(CFLAGS)

prog_default:
\t@echo DEFAULT_CFLAGS=$(CFLAGS)
";
    let mf = parse_makefile_content(makefile_content, &[]).expect("parse failed");

    // Check debug target CFLAGS
    let debug_rule = mf.get_rule("prog_debug").expect("prog_debug rule");
    let cmd_debug = maked::parser::expand_variables(
        &debug_rule.commands[0],
        &mf,
        Some("prog_debug"),
        &debug_rule.prereqs,
    );
    assert_eq!(cmd_debug, "@echo DEBUG_CFLAGS=-g -DDEBUG -Wall");

    // Check release target CFLAGS
    let release_rule = mf.get_rule("prog_release").expect("prog_release rule");
    let cmd_release = maked::parser::expand_variables(
        &release_rule.commands[0],
        &mf,
        Some("prog_release"),
        &release_rule.prereqs,
    );
    assert_eq!(cmd_release, "@echo RELEASE_CFLAGS=-O3 -DNDEBUG");

    // Check default target CFLAGS (fallback to global)
    let default_rule = mf.get_rule("prog_default").expect("prog_default rule");
    let cmd_default = maked::parser::expand_variables(
        &default_rule.commands[0],
        &mf,
        Some("prog_default"),
        &default_rule.prereqs,
    );
    assert_eq!(cmd_default, "@echo DEFAULT_CFLAGS=-O0");
}

#[test]
fn test_pattern_specific_variables() {
    let makefile_content = "
%.o: CFLAGS = -fPIC
main.o: CFLAGS = -g -fPIC

main.o: main.c
\t@echo CFLAGS=$(CFLAGS)

util.o: util.c
\t@echo CFLAGS=$(CFLAGS)
";
    let mf = parse_makefile_content(makefile_content, &[]).expect("parse failed");

    // main.o should use target-specific CFLAGS
    let main_rule = mf.get_rule("main.o").expect("main.o rule");
    let cmd_main = maked::parser::expand_variables(
        &main_rule.commands[0],
        &mf,
        Some("main.o"),
        &main_rule.prereqs,
    );
    assert_eq!(cmd_main, "@echo CFLAGS=-g -fPIC");

    // util.o should match %.o pattern-specific CFLAGS
    let util_rule = mf.get_rule("util.o").expect("util.o rule");
    let cmd_util = maked::parser::expand_variables(
        &util_rule.commands[0],
        &mf,
        Some("util.o"),
        &util_rule.prereqs,
    );
    assert_eq!(cmd_util, "@echo CFLAGS=-fPIC");
}

#[test]
fn test_second_expansion_prerequisites() {
    let test_dir = std::env::temp_dir().join("maked_second_expansion_test");
    let _ = fs::create_dir_all(&test_dir);
    let f1 = test_dir.join("sub1.c");
    let f2 = test_dir.join("sub2.c");
    fs::write(&f1, "int a = 1;").unwrap();
    fs::write(&f2, "int b = 2;").unwrap();

    let makefile_content = format!(
        "
.SECONDEXPANSION:

DEPS = {} {}

prog: $$(DEPS)
\t@echo PREREQS=$^
",
        f1.display(),
        f2.display()
    );

    let mf = parse_makefile_content(&makefile_content, &[]).expect("parse failed");
    assert!(
        mf.has_second_expansion,
        "Makefile should flag has_second_expansion"
    );

    let rule = mf.get_rule("prog").expect("prog rule not found");
    assert_eq!(
        rule.prereqs.len(),
        2,
        "Prerequisites should be expanded via .SECONDEXPANSION"
    );
    assert!(rule.prereqs.contains(&f1.to_str().unwrap().to_string()));
    assert!(rule.prereqs.contains(&f2.to_str().unwrap().to_string()));

    let cmd_expanded =
        maked::parser::expand_variables(&rule.commands[0], &mf, Some("prog"), &rule.prereqs);
    assert_eq!(
        cmd_expanded,
        format!("@echo PREREQS={} {}", f1.display(), f2.display())
    );

    let _ = fs::remove_dir_all(&test_dir);
}

#[test]
fn test_second_expansion_with_automatic_variables() {
    let test_dir = std::env::temp_dir().join("maked_second_expansion_auto_test");
    let _ = fs::create_dir_all(&test_dir);
    let app_src = test_dir.join("app.c");
    fs::write(&app_src, "int main() {}").unwrap();

    let app_obj = test_dir.join("app.o");

    let makefile_content = format!(
        "
.SECONDEXPANSION:

{}: $$(@:.o=.c)
\t@echo COMPILE $< to $@
",
        app_obj.display()
    );

    let mf = parse_makefile_content(&makefile_content, &[]).expect("parse failed");
    let rule = mf.get_rule(app_obj.to_str().unwrap()).expect("app.o rule");
    assert_eq!(rule.prereqs, vec![app_src.to_str().unwrap().to_string()]);

    let cmd_exp = maked::parser::expand_variables(
        &rule.commands[0],
        &mf,
        Some(app_obj.to_str().unwrap()),
        &rule.prereqs,
    );
    assert_eq!(
        cmd_exp,
        format!(
            "@echo COMPILE {} to {}",
            app_src.display(),
            app_obj.display()
        )
    );

    let _ = fs::remove_dir_all(&test_dir);
}
