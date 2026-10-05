use std::fs;
use std::process::Command;

#[test]
fn test_ninja_emit_and_direct_execution() {
    let temp_dir = std::env::temp_dir().join(format!("maked_test_ninja_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    let makefile_content = r#"
all: app

app: obj1.o obj2.o
	@echo "LINKING_NINJA_APP"
	@cat obj1.o obj2.o > app

obj1.o: src1.txt
	@echo "COMPILING_OBJ1"
	@cat src1.txt > obj1.o

obj2.o: src2.txt
	@echo "COMPILING_OBJ2"
	@cat src2.txt > obj2.o
"#;
    let makefile_path = temp_dir.join("Makefile");
    fs::write(&makefile_path, makefile_content).unwrap();

    fs::write(temp_dir.join("src1.txt"), "hello_").unwrap();
    fs::write(temp_dir.join("src2.txt"), "ninja\n").unwrap();

    let maked_bin = env!("CARGO_BIN_EXE_maked");
    let ninja_file = temp_dir.join("build.ninja");

    // 1. Transpile Makefile to build.ninja using maked --emit-ninja
    let out_emit = Command::new(maked_bin)
        .arg("-f")
        .arg(&makefile_path)
        .arg(format!("--emit-ninja={}", ninja_file.display()))
        .current_dir(&temp_dir)
        .output()
        .unwrap();

    assert!(
        out_emit.status.success(),
        "Failed to emit ninja: {}",
        String::from_utf8_lossy(&out_emit.stderr)
    );
    assert!(ninja_file.exists());
    let ninja_content = fs::read_to_string(&ninja_file).unwrap();
    assert!(ninja_content.contains("default all"));
    assert!(ninja_content.contains("build all: phony app"));

    // 2. Execute the generated build.ninja directly with official Ninja if available
    let has_ninja = Command::new("ninja").arg("--version").output().is_ok();
    if has_ninja {
        let out_ninja = Command::new("ninja")
            .arg("-f")
            .arg(&ninja_file)
            .current_dir(&temp_dir)
            .output()
            .unwrap();
        assert!(
            out_ninja.status.success(),
            "Official ninja execution failed:\nSTDOUT: {}\nSTDERR: {}\nNINJA CONTENT:\n{}",
            String::from_utf8_lossy(&out_ninja.stdout),
            String::from_utf8_lossy(&out_ninja.stderr),
            ninja_content
        );
        assert!(temp_dir.join("app").exists());
        assert_eq!(
            fs::read_to_string(temp_dir.join("app")).unwrap(),
            "hello_ninja\n"
        );

        // Clean outputs for next step
        let _ = fs::remove_file(temp_dir.join("app"));
        let _ = fs::remove_file(temp_dir.join("obj1.o"));
        let _ = fs::remove_file(temp_dir.join("obj2.o"));
    }

    // 3. Execute build.ninja directly using maked (-f build.ninja)
    let out_maked_ninja = Command::new(maked_bin)
        .arg("-f")
        .arg(&ninja_file)
        .current_dir(&temp_dir)
        .output()
        .unwrap();

    assert!(
        out_maked_ninja.status.success(),
        "maked executing ninja failed: {}",
        String::from_utf8_lossy(&out_maked_ninja.stderr)
    );
    assert!(temp_dir.join("app").exists());
    assert_eq!(
        fs::read_to_string(temp_dir.join("app")).unwrap(),
        "hello_ninja\n"
    );

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_ninja_handcrafted_syntax() {
    let temp_dir = std::env::temp_dir().join(format!(
        "maked_test_ninja_handcrafted_{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    let ninja_content = r#"
# Hand-crafted Ninja specification
cflags = -O3
msg = "Building with native maked ninja engine"

rule cc
  command = echo "$msg" && cat $in > $out

rule link
  command = cat $in > $out && echo "Done linking"

build part1.o: cc part1.c
build part2.o: cc part2.c
build final_prog: link part1.o part2.o

default final_prog
"#;

    let ninja_file = temp_dir.join("build.ninja");
    fs::write(&ninja_file, ninja_content).unwrap();

    fs::write(temp_dir.join("part1.c"), "A").unwrap();
    fs::write(temp_dir.join("part2.c"), "B").unwrap();

    let maked_bin = env!("CARGO_BIN_EXE_maked");
    let out = Command::new(maked_bin)
        .arg("-f")
        .arg(&ninja_file)
        .current_dir(&temp_dir)
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "maked failed to execute handcrafted build.ninja: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(temp_dir.join("part1.o").exists());
    assert!(temp_dir.join("part2.o").exists());
    assert!(temp_dir.join("final_prog").exists());
    assert_eq!(
        fs::read_to_string(temp_dir.join("final_prog")).unwrap(),
        "AB"
    );

    let _ = fs::remove_dir_all(&temp_dir);
}
