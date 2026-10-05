use std::fs::{self, File};
use std::io::Write;
use std::process::Command;

fn get_makeyd_bin() -> String {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // drop test binary name
    if path.ends_with("deps") {
        path.pop();
    }
    path.push("makeyd");
    path.to_str().unwrap().to_string()
}

#[test]
fn test_c_depfile_dynamic_header_dependency_injection() {
    let makeyd = get_makeyd_bin();
    let temp_dir = std::env::temp_dir().join(format!("test_depfile_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    let main_c = temp_dir.join("main.c");
    let foo_h = temp_dir.join("foo.h");
    let bar_h = temp_dir.join("bar.h");

    File::create(&main_c)
        .unwrap()
        .write_all(b"int main() { return 0; }\n")
        .unwrap();
    File::create(&foo_h)
        .unwrap()
        .write_all(b"#define FOO 1\n")
        .unwrap();
    File::create(&bar_h)
        .unwrap()
        .write_all(b"#define BAR 2\n")
        .unwrap();

    let mf_path = temp_dir.join("Makefile");
    let mut mf = File::create(&mf_path).unwrap();
    // Makefile with pattern rule producing .d depfile and -include $(DEPS)
    writeln!(mf, "OBJS = main.o").unwrap();
    writeln!(mf, "DEPS = $(OBJS:.o=.d)").unwrap();
    writeln!(mf, "all: prog").unwrap();
    writeln!(mf, "prog: $(OBJS)").unwrap();
    writeln!(mf, "\t@echo \"LINK prog\"").unwrap();
    writeln!(mf, "\t@touch prog").unwrap();
    writeln!(mf, "%.o: %.c").unwrap();
    writeln!(mf, "\t@echo \"COMPILE $<\"").unwrap();
    writeln!(mf, "\t@touch $@").unwrap();
    writeln!(mf, "\t@echo 'main.o: main.c foo.h bar.h' > $(@:.o=.d)").unwrap();
    writeln!(mf, "\t@echo 'foo.h:' >> $(@:.o=.d)").unwrap();
    writeln!(mf, "\t@echo 'bar.h:' >> $(@:.o=.d)").unwrap();
    writeln!(mf, "-include $(DEPS)").unwrap();
    drop(mf);

    // 1. Initial cold build: main.o and prog should be compiled and linked
    let out1 = Command::new(&makeyd)
        .arg("-C")
        .arg(&temp_dir)
        .output()
        .expect("failed to run makeyd");
    assert!(out1.status.success());
    let stdout1 = String::from_utf8_lossy(&out1.stdout);
    assert!(stdout1.contains("COMPILE main.c"));
    assert!(stdout1.contains("LINK prog"));
    assert!(
        temp_dir.join("main.d").exists(),
        "main.d depfile must be created"
    );

    // 2. Second build without changes: must be up to date!
    let out2 = Command::new(&makeyd)
        .arg("-C")
        .arg(&temp_dir)
        .output()
        .expect("failed to run makeyd");
    assert!(out2.status.success());
    let stdout2 = String::from_utf8_lossy(&out2.stdout);
    assert!(
        stdout2.contains("is up to date"),
        "expected up to date, got: {stdout2}"
    );

    // 3. Touch foo.h (which is only listed in main.d, not in the explicit Makefile prerequisites!)
    // Wait a brief moment to ensure sub-second or timestamp advancement
    std::thread::sleep(std::time::Duration::from_millis(50));
    let mut f_foo = File::create(&foo_h).unwrap();
    f_foo.write_all(b"#define FOO 42\n").unwrap();
    drop(f_foo);

    let out3 = Command::new(&makeyd)
        .arg("-C")
        .arg(&temp_dir)
        .output()
        .expect("failed to run makeyd");
    assert!(out3.status.success());
    let stdout3 = String::from_utf8_lossy(&out3.stdout);
    assert!(
        stdout3.contains("COMPILE main.c"),
        "touching foo.h must trigger rebuild of main.o: {stdout3}"
    );
    assert!(
        stdout3.contains("LINK prog"),
        "rebuilding main.o must trigger re-linking of prog: {stdout3}"
    );

    // 4. Touch bar.h (also in main.d)
    std::thread::sleep(std::time::Duration::from_millis(50));
    let mut f_bar = File::create(&bar_h).unwrap();
    f_bar.write_all(b"#define BAR 99\n").unwrap();
    drop(f_bar);

    let out4 = Command::new(&makeyd)
        .arg("-C")
        .arg(&temp_dir)
        .output()
        .expect("failed to run makeyd");
    assert!(out4.status.success());
    let stdout4 = String::from_utf8_lossy(&out4.stdout);
    assert!(
        stdout4.contains("COMPILE main.c"),
        "touching bar.h must trigger rebuild of main.o: {stdout4}"
    );
    assert!(
        stdout4.contains("LINK prog"),
        "rebuilding main.o must trigger re-linking of prog: {stdout4}"
    );

    // 5. Subsequent run: up to date again!
    let out5 = Command::new(&makeyd)
        .arg("-C")
        .arg(&temp_dir)
        .output()
        .expect("failed to run makeyd");
    assert!(out5.status.success());
    let stdout5 = String::from_utf8_lossy(&out5.stdout);
    assert!(
        stdout5.contains("is up to date"),
        "expected up to date, got: {stdout5}"
    );

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_wildcard_include_pattern() {
    let makeyd = get_makeyd_bin();
    let temp_dir = std::env::temp_dir().join(format!("test_wildcard_inc_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    let inc1 = temp_dir.join("rules1.mk");
    let inc2 = temp_dir.join("rules2.mk");
    File::create(&inc1)
        .unwrap()
        .write_all(b"VAR1 = alpha\n")
        .unwrap();
    File::create(&inc2)
        .unwrap()
        .write_all(b"VAR2 = beta\n")
        .unwrap();

    let mf_path = temp_dir.join("Makefile");
    let mut mf = File::create(&mf_path).unwrap();
    writeln!(mf, "include rules*.mk").unwrap();
    writeln!(mf, "all:").unwrap();
    writeln!(mf, "\t@echo $(VAR1)_$(VAR2)").unwrap();
    drop(mf);

    let out = Command::new(&makeyd)
        .arg("-C")
        .arg(&temp_dir)
        .output()
        .expect("failed to run makeyd");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("alpha_beta"),
        "expected alpha_beta, got: {stdout}"
    );

    let _ = fs::remove_dir_all(&temp_dir);
}
