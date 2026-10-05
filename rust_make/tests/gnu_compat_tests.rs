//! Differential cases found by building real projects (benchmarks/realworld):
//! each Makefile runs under maked and GNU make, and stdout and the exit
//! status must match. GNU make is `gmake` if present (macOS), else `make`.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static SEQ: AtomicUsize = AtomicUsize::new(0);

/// (major, minor) of the GNU make in use.
fn gnu_version() -> Option<(u32, u32)> {
    let out = Command::new(gnu_make()?).arg("--version").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let v = text.lines().next()?.rsplit(' ').next()?.to_string();
    let mut it = v.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    Some((it.next()?, it.next().unwrap_or(0)))
}

fn gnu_make() -> Option<String> {
    for cand in ["gmake", "make"] {
        if let Ok(out) = Command::new(cand).arg("--version").output() {
            if String::from_utf8_lossy(&out.stdout).contains("GNU Make") {
                return Some(cand.to_string());
            }
        }
    }
    None
}

fn scratch(files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "maked_compat_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    for (name, body) in files {
        let p = dir.join(name);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, body).unwrap();
    }
    dir
}

/// Run `args` under both tools in fresh copies of `files`; compare.
fn assert_same(files: &[(&str, &str)], args: &[&str]) {
    assert_same_after(files, "", args);
}

/// Like `assert_same`, after running the shell command `setup` in each copy
/// (to arrange file timestamps).
fn assert_same_after(files: &[(&str, &str)], setup: &str, args: &[&str]) {
    let Some(gmake) = gnu_make() else {
        eprintln!("GNU make not found; skipping differential case");
        return;
    };
    let run = |bin: &str| {
        let dir = scratch(files);
        if !setup.is_empty() {
            let ok = Command::new("/bin/sh")
                .arg("-c")
                .arg(setup)
                .current_dir(&dir)
                .status()
                .unwrap();
            assert!(ok.success(), "setup failed: {setup}");
        }
        let out = Command::new(bin)
            .arg("-C")
            .arg(&dir)
            .arg("--no-print-directory")
            .args(args)
            .env_remove("MAKEFLAGS")
            .env_remove("MAKELEVEL")
            .output()
            .unwrap();
        let _ = fs::remove_dir_all(&dir);
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };
    // `$(MAKE)` is how each tool was invoked; compare it as MAKE.
    let norm = |out: String, name: &str| {
        let base = std::path::Path::new(name)
            .file_name()
            .map(|b| b.to_string_lossy().to_string())
            .unwrap_or_default();
        out.lines()
            .map(|l| match l.strip_prefix(name) {
                Some(rest) if rest.starts_with(' ') => format!("MAKE{rest}"),
                _ => match l.strip_prefix(&format!("{base}:")) {
                    Some(rest) => format!("MAKE:{rest}"),
                    None => l.to_string(),
                },
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let (g_ok, g_out, g_err) = run(&gmake);
    let g_out = norm(g_out, &gmake);
    let (m_ok, m_out, m_err) = run(env!("CARGO_BIN_EXE_maked"));
    let m_out = norm(m_out, env!("CARGO_BIN_EXE_maked"));
    assert_eq!(
        (m_ok, &m_out),
        (g_ok, &g_out),
        "maked differs from GNU make\n--- maked stderr:\n{m_err}\n--- GNU make stderr:\n{g_err}"
    );
}

#[test]
fn comment_after_include_and_conditionals() {
    assert_same(
        &[
            ("inc.mk", "X = included\n"),
            (
                "Makefile",
                "include inc.mk # am--include-marker\n\
                 ifeq ($(X),included) # trailing comment\n\
                 Y = yes\n\
                 else # another\n\
                 Y = no\n\
                 endif # done\n\
                 all:\n\
                 \t@echo $(X) $(Y)\n",
            ),
        ],
        &[],
    );
}

#[test]
fn escaped_hash_is_literal() {
    assert_same(&[("Makefile", "H = a\\#b\nall:\n\t@echo '$(H)'\n")], &[]);
}

#[test]
fn space_indented_line_in_rule_is_not_a_recipe() {
    assert_same(
        &[(
            "Makefile",
            "all: dep\n\t@echo all $(V)\n\
             dep:\n\t@echo dep\n  V = set-later\n",
        )],
        &[],
    );
}

#[test]
fn default_special_target() {
    assert_same(
        &[(
            "Makefile",
            "default: all\n.DEFAULT:\n\t@echo default-recipe for $@\ninstall:\n\t@echo install\n",
        )],
        &[],
    );
    assert_same(
        &[(
            "Makefile",
            ".DEFAULT:\n\t@echo made $@\nall: x y\n\t@echo all\n",
        )],
        &["all"],
    );
}

#[test]
fn tab_indented_lines_outside_a_rule_are_makefile_text() {
    assert_same(
        &[(
            "Makefile",
            "OPT ?= -O3\nifeq ($(OPT),-O3)\n\tifeq (a,a)\n\t\tFLAGS += inner\n\telse\n\t\tFLAGS += other\n\tendif\n\tFLAGS += outer\nendif\nall:\n\t@echo $(FLAGS)\n",
        )],
        &[],
    );
}

#[test]
fn export_unexport_override_and_recipe_environment() {
    assert_same(
        &[(
            "Makefile",
            "export A = exported\n\
             B = plain\n\
             export B\n\
             C = not-exported\n\
             export D := imm\n\
             unexport E\n\
             override F = forced\n\
             N = X\n\
             $(N)_FLAGS = computed\n\
             all:\n\
             \t@echo A=$$A B=$$B C=$${C:-unset} D=$$D E=$${E:-unset} F=$(F) $(X_FLAGS)\n",
        )],
        &["F=cli"],
    );
}

#[test]
fn recipes_use_bin_sh_not_the_login_shell() {
    // GNU make ignores $SHELL from the environment; recipes run under /bin/sh
    // unless the makefile itself sets SHELL.
    let Some(_) = gnu_make() else { return };
    let dir = scratch(&[("Makefile", "all:\n\t@echo $$0\n")]);
    let out = Command::new(env!("CARGO_BIN_EXE_maked"))
        .arg("-C")
        .arg(&dir)
        .env("SHELL", "/bin/zsh-does-not-exist")
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "/bin/sh");

    assert_same(&[("Makefile", "SHELL = /bin/sh\nall:\n\t@echo $$0\n")], &[]);
}

#[test]
fn glob_in_prerequisites() {
    assert_same(
        &[
            ("cmds/b.json", "b"),
            ("cmds/a.json", "a"),
            (
                "Makefile",
                "out: cmds/*.json\n\t@echo $^\nnone: nothing/*.x\n\t@echo $^\n",
            ),
        ],
        &["out"],
    );
}

#[test]
fn automatic_variables_and_d_f_variants() {
    assert_same(
        &[
            ("src/a.c", "a"),
            ("src/b.c", "b"),
            ("old.h", "h"),
            (
                "Makefile",
                "out/lib.a: src/a.c src/b.c src/a.c old.h\n\
                 \t@echo '@=$@ <=$< ^=$^ +=$+'\n\
                 \t@echo '@D=$(@D) @F=$(@F) <D=$(<D) <F=$(<F)'\n\
                 \t@echo '^D=$(^D) ^F=$(^F) +F=$(+F)'\n\
                 \t@echo '?=$?'\n",
            ),
        ],
        &["out/lib.a"],
    );
}

#[test]
fn recipe_prefixes_after_expansion_and_under_dry_run() {
    let files = [
        ("sub/Makefile", "all:\n\t@echo sub-ran\n"),
        (
            "Makefile",
            "Q = @\nQUIET_CC = @printf 'CC %s\\n' x;\n\
             all:\n\t$(QUIET_CC) echo compiled\n\t$(Q)echo quiet\n\t+@echo forced\n\t@$(MAKE) -s -C sub\n",
        ),
    ];
    assert_same(&files, &[]);
    assert_same(&files, &["-n"]);
}

#[test]
fn several_goals_share_prerequisites_once() {
    assert_same(
        &[(
            "Makefile",
            "a: shared\n\t@echo a\nb: shared\n\t@echo b\nshared:\n\t@echo shared-ran\n",
        )],
        &["a", "b"],
    );
}

#[test]
fn nothing_to_be_done_and_up_to_date_messages() {
    assert_same(&[("f.txt", "x"), ("Makefile", "all: f.txt\n")], &[]);
    assert_same(
        &[("src", "x"), ("Makefile", "out: src\n\tcp src out\n")],
        &["out", "out"],
    );
}

#[test]
fn failed_recipe_exits_like_gnu_make() {
    // Both must fail; stdout (nothing) must match.
    assert_same(&[("Makefile", "all: f\nf:\n\t@false\n")], &[]);
}

#[test]
fn static_pattern_rules() {
    assert_same(
        &[
            ("a.c", "a"),
            ("b.c", "b"),
            (
                "Makefile",
                "OBJS = a.o b.o\nall: $(OBJS)\n\t@echo linked $^\n\
                 $(OBJS): %.o: %.c common.h\n\t@echo cc $< '->' $@ stem=$*\n\
                 common.h:\n\t@echo gen $@\n",
            ),
        ],
        &[],
    );
}

#[test]
fn double_colon_rules_accumulate() {
    assert_same(
        &[(
            "Makefile",
            // maked merges double-colon rules (all prerequisites first, then
            // the recipes in order); this case behaves the same either way.
            "all:: extra\n\t@echo first\nall::\n\t@echo second\nextra:\n\t@echo extra\n",
        )],
        &[],
    );
}

#[test]
fn order_only_prerequisites() {
    assert_same(
        &[
            ("src.c", "x"),
            (
                "Makefile",
                "out/obj: src.c | out\n\t@echo build $@ from $^\nout:\n\t@echo mkdir $@\n",
            ),
        ],
        &[],
    );
    assert_same(
        &[
            ("src.c", "x"),
            ("out/.keep", ""),
            (
                "Makefile",
                "out/obj: src.c | out\n\t@echo build $@ from $^\nout:\n\t@echo mkdir $@\n",
            ),
        ],
        &[],
    );
}

#[test]
fn phony_target_without_a_rule_forces_rebuild() {
    assert_same(
        &[
            ("v", "old"),
            ("Makefile", "v: FORCE\n\t@echo regen $@\n.PHONY: FORCE\n"),
        ],
        &[],
    );
}

#[test]
fn self_memoizing_eval_variable_in_recipes() {
    // git's idiom: computed on first use, then a simple variable.
    assert_same(
        &[(
            "Makefile",
            "CFG = $(eval CFG := $$(shell echo computed))$(CFG)\n\
             FLAGS = -x $(CFG)\n\
             all: a b\n\
             a:\n\t@echo a $(FLAGS)\n\
             b:\n\t@echo b $(FLAGS) $(CFG)\n",
        )],
        &[],
    );
}

#[test]
fn first_prerequisite_comes_from_the_rule_with_the_recipe() {
    assert_same(
        &[
            ("x", ""),
            ("y", ""),
            ("z", ""),
            (
                "Makefile",
                "a: x\na: y z\n\t@echo \"<=$< ^=$^\"\nb.o: x\nOBJ = b.o\n$(OBJ): %.o: y\n\t@echo \"<=$< ^=$^\"\n",
            ),
        ],
        &["a", "b.o"],
    );
}

#[test]
fn ifeq_with_shell_quotes_backticks_and_parens() {
    assert_same(
        &[(
            "Makefile",
            "R = 23.1.0\n\
             ifeq ($(shell expr \"$(R)\" : '[15678]\\.'),2)\n\tOLD = yes\n\tendif\n\
             ifeq ($(shell test \"`expr \"$(R)\" : '\\([0-9][0-9]*\\)\\.'`\" -ge 11 && echo 1),1)\n\tGETDELIM = yes\nendif\n\
             METHOD = arc4random\n\
             ifneq ($(findstring arc4random,$(METHOD)),)\nFLAGS += -DARC\nendif\n\
             all:\n\t@echo old=$(OLD) getdelim=$(GETDELIM) flags=$(FLAGS)\n",
        )],
        &[],
    );
}

#[test]
fn text_and_file_functions() {
    assert_same(
        &[(
            "Makefile",
            "W = a b c d e\n\
             all:\n\
             \t@echo '[$(findstring arc4,arc4random)] [$(findstring x,abc)]'\n\
             \t@echo '[$(wordlist 2,4,$(W))] [$(wordlist 4,2,$(W))] [$(wordlist 3,9,$(W))]'\n\
             \t@echo '[$(notdir $(abspath ./sub/../x.c))] [$(origin W)] [$(origin CC)] [$(origin NOPE)] [$(origin @)]'\n\
             \t@echo '[$(file >out.txt,hello)$(file <out.txt)]'\n",
        )],
        &[],
    );
}

#[test]
fn builtin_c_rule_uses_cppflags_and_cc_from_environment() {
    let files = [
        ("hello.c", "int main(void){return 0;}\n"),
        ("Makefile", "CPPFLAGS = -DX=1\n"),
    ];
    assert_same(&files, &["-n", "hello.o"]);
}

#[test]
fn gnu_44_functions_intcmp_and_let() {
    if gnu_version().is_none_or(|v| v < (4, 4)) {
        eprintln!("GNU make < 4.4 has no intcmp/let; skipping");
        return;
    }
    assert_same(
        &[(
            "Makefile",
            "all:\n\t@echo '[$(intcmp 1,2,lt,eq,gt)] [$(intcmp 2,2,lt,eq,gt)] [$(intcmp 3,2,lt,eq,gt)] [$(intcmp 4,4)]'\n\t@echo '[$(let a b,1 2 3,$(b)-$(a))]'\n",
        )],
        &[],
    );
}

#[test]
fn shell_builtins_in_recipes() {
    assert_same(
        &[(
            "Makefile",
            "all:\n\t: no custom templates yet\n\tcd /\n\ttest -d /\n\techo done\n",
        )],
        &[],
    );
}

#[test]
fn rule_whose_targets_expand_to_nothing_is_dropped() {
    assert_same(
        &[(
            "Makefile",
            "EMPTY =\nall:\n\t@echo ok\n$(EMPTY): all\n\t@echo never\n",
        )],
        &[],
    );
}

#[test]
fn recipe_that_leaves_target_untouched_does_not_rebuild_dependents() {
    // automake's `config.h: stamp-h1` with `test -f config.h || ...`.
    assert_same_after(
        &[
            ("hdr", ""),
            ("out", ""),
            ("stamp", ""),
            (
                "Makefile",
                "out: hdr\n\t@echo rebuilt out\nhdr: stamp\n\t@test -f hdr || touch hdr\n",
            ),
        ],
        "touch -t 202001010000 hdr && touch -t 202001010001 out && touch -t 202001010002 stamp",
        &[],
    );
}

#[test]
fn included_makefiles_are_remade_and_reread() {
    assert_same(
        &[(
            "Makefile",
            "all:\n\t@echo v=$(V)\n-include gen.mk\ngen.mk:\n\t@echo 'V = generated' > gen.mk\n",
        )],
        &[],
    );
    assert_same(
        &[(
            "Makefile",
            // (A `FORCE` prerequisite here would make GNU make restart forever.)
            "all:\n\t@echo w=$(W)\ninclude req.mk\nreq.mk:\n\t@echo 'W = required' > req.mk\n",
        )],
        &[],
    );
    // A missing include nothing can make is an error in both.
    assert_same(&[("Makefile", "all:\n\t@echo hi\ninclude nope.mk\n")], &[]);
}

#[test]
fn variable_flavors_overrides_and_target_specific_inheritance() {
    // Found by benchmarks/fuzzer/directive_fuzz.py.
    let mf = "S := s\nS += $(LATE)\nLATE = late\nR = r\nR += $(LATE)\n\
              override O = file\nO = ignored\nP ?= dflt\n\
              define D +=\nline\nendef\ndefine D2 :=\n$(S)\nendef\n\
              all: top\n\t@echo 'all [$(S)] [$(R)] [$(O)] [$(origin O)] [$(flavor S)] [$(flavor R)] [$(P)]'\n\
              top: V := 1 2\ntop: dep\n\t@echo 'top [$(V)] [$(D2)]'\n\
              dep: V += dep\ndep: x.pp\n\t@echo 'dep [$(V)]'\n\
              %.pp: W := pat  \nx.pp:\n\t@echo 'x.pp [$(V)] [$(W)]'\n";
    assert_same(&[("Makefile", mf)], &[]);
    assert_same(&[("Makefile", mf)], &["O=cli", "P=cli", "V=cli"]);
}

#[test]
fn expression_whitespace_and_function_semantics() {
    // Each line is a GNU make rule maked got wrong until the expression
    // fuzzer (benchmarks/fuzzer/expr_fuzz.py) compared them.
    assert_same(
        &[(
            "Makefile",
            "E :=\nSP := $(E) $(E)\nT := a b  \nFN = <$(1)|$(2)>\n\
             $(info 1[$(T)])\n\
             $(info 2[$(or $(E),$(SP))][$(if $(SP),yes,no)][$(and $(SP),x)])\n\
             $(info 3[$(call FN, a , b )])\n\
             $(info 4[$(subst ,X,ab)][$(subst a,b, x a )])\n\
             $(info 5[$(eval V := now)$(V)])\n\
             $(info 6[$(wordlist 1,2,a\tb  c)][$(patsubst %c,%.%,abc)])\n\
             $(info 7[$(foreach w,$$(E)x,$(w))][$(origin MAKE)])\n\
             ifdef SP\n$(info 8 sp defined)\nendif\n\
             ifeq ( $(E),)\n$(info 9 eq)\nelse\n$(info 9 ne)\nendif\n\
             ifeq ($(E),$(E) )\n$(info 10 eq)\nelse\n$(info 10 ne)\nendif\n\
             ifeq \"a\" 'a'\n$(info 11 eq)\nendif\n\
             all: ; @:\n",
        )],
        &[],
    );
    // A dotfile is not matched by `*`.
    assert_same(
        &[
            (".h.c", ""),
            ("v.c", ""),
            (
                "Makefile",
                "$(info [$(wildcard *.c)][$(wildcard .*.c)])\nall: ; @:\n",
            ),
        ],
        &[],
    );
}

#[test]
fn recipe_after_semicolon_and_empty_recipes() {
    // `t: p ; recipe`, an `=` after the `;` (recipe text) and before it (a
    // target-specific variable, `;` included); an empty recipe (`b.o: ;`)
    // stops implicit rule search; a line expanding to nothing is skipped.
    assert_same(
        &[
            (
                "Makefile",
                "all: x y t u w b.o e\nx: ; @echo x-ran\ny:;@echo y: ran a=b\n\
             t: V = a;b\nt:\n\t@echo 't V=[$(V)]'\nu: p ; @echo u $^\np: ; @echo p\n\
             w: ; @echo w1\n\t@echo w2\nb.o: ;\nE :=\ne:\n\t$(E)\n\t\n\t@echo e\n",
            ),
            ("b.c", ""),
        ],
        &[],
    );
}

#[test]
fn question_and_touch_skip_targets_without_a_recipe() {
    // `x` has no recipe: -q does not count it as work, -t does not create
    // it (a file named FORCE would break the idiom for good).
    let mf = "x: a FORCE\ny: x\n\t@touch y\nFORCE:\n";
    for args in [
        &["-q", "x"][..],
        &["-q", "-j4", "x"],
        &["-q", "y"],
        &["-t", "y"],
        &["-t", "-j4", "y"],
    ] {
        assert_same_after(&[("a", ""), ("Makefile", mf)], "", args);
    }
    // What -t left behind must match too.
    assert_same_after(&[("a", ""), ("Makefile", mf)], "", &["-t", "y"]);
    assert_same(
        &[
            ("a", ""),
            (
                "Makefile",
                &format!("{mf}check:\n\t@ls FORCE x 2>&1 | sort\n"),
            ),
        ],
        &["-t", "y", "check"],
    );
}

#[test]
fn pattern_without_slash_matches_the_file_name() {
    // The directory comes off before matching and goes back in front of
    // each prerequisite made from a pattern; `$*` keeps it.
    assert_same(
        &[
            ("a/src/b.c", ""),
            ("src/a/b.c", ""),
            ("hdr.h", ""),
            ("a/src/eat.in", ""),
            (
                "Makefile",
                "all: a/b.o a/eat\n%.o: src/%.c hdr.h\n\t@echo '$@ <- $^ stem=$*'\n\
                 e%t: src/e%t.in\n\t@echo '$@ <- $^ stem=$*'\n",
            ),
        ],
        &[],
    );
}

#[test]
fn target_remade_with_the_makefiles_is_not_remade_again() {
    // git's GIT-VERSION-FILE: a FORCE rule for an included file whose
    // recipe leaves it unchanged. GNU make runs it once, while remaking the
    // makefiles, and takes it as done for the goals.
    assert_same(
        &[
            ("ver.mk", "V = 1\n"),
            (
                "Makefile",
                "all: out\n\t@echo v=$(V)\nout: ver.mk\n\t@touch $@\n\
                 ver.mk: FORCE\n\t@echo gen\nFORCE:\n-include ver.mk\n",
            ),
        ],
        &[],
    );
}

#[test]
fn backslash_newline_in_variables_and_recipes() {
    assert_same(
        &[(
            "Makefile",
            "FLAGS = -a   \\\n      -b \\\n\t-c\n\
             all:\n\t@echo '[$(FLAGS)]'\n\t@echo one\\\n\ttwo\n\t@echo 'in\\\n\tquotes'\n\t@x=1; \\\n\techo x=$$x\n",
        )],
        &[],
    );
}

#[test]
fn touch_flag_updates_existing_targets() {
    // -t must bump `out` past `src`; a second run then has nothing to do.
    let files = [
        ("src", ""),
        ("out", ""),
        ("Makefile", "out: src\n\t@echo should-not-run\n"),
    ];
    assert_same_after(
        &files,
        "touch -t 202001010000 out && touch -t 202001010001 src",
        &["-t"],
    );
}

#[test]
fn keep_going_builds_what_does_not_depend_on_a_failure() {
    let files = [(
        "Makefile",
        "all: bad dependent good\nbad:\n\t@false\ndependent: bad\n\t@echo never\ngood:\n\t@echo good\n",
    )];
    assert_same(&files, &["-k"]);
    assert_same(&files, &["-k", "-j4"]);
    assert_same(&files, &[]);
}

#[test]
fn expansion_keeps_non_ascii_text_intact() {
    assert_same(
        &[(
            "Makefile",
            "ІМЯ = світ\nX = ü$(ІМЯ)ß\nall:\n\t@echo 'Привіт, $(X)! $$HOME-літерал ${ІМЯ}'\n",
        )],
        &[],
    );
}

#[test]
fn colon_noop_and_colon_redirect() {
    assert_same(
        &[
            ("f", "content\n"),
            (
                "Makefile",
                "all:\n\t: just a comment\n\t: > f\n\t@wc -c < f | tr -d ' '\n",
            ),
        ],
        &[],
    );
}
