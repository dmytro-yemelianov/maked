use std::fs;
use std::process::Command;

#[test]
fn test_emit_compdb_cli() {
    let temp_dir = std::env::temp_dir().join(format!("maked_test_compdb_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    let makefile_content = r#"
CC = clang
CFLAGS = -Wall -Wextra -O3 -DDEBUG=1

all: main_app

main_app: math.o string_utils.o main.o
	$(CC) $^ -o $@

math.o: math.c math.h
	$(CC) $(CFLAGS) -c math.c -o math.o

string_utils.o: string_utils.c
	$(CC) $(CFLAGS) -c $< -o $@

main.o: main.c
	$(CC) $(CFLAGS) -c $< -o $@
"#;

    let makefile_path = temp_dir.join("Makefile");
    fs::write(&makefile_path, makefile_content).unwrap();

    fs::write(temp_dir.join("math.h"), "// header").unwrap();
    fs::write(
        temp_dir.join("math.c"),
        "int add(int a, int b) { return a + b; }",
    )
    .unwrap();
    fs::write(
        temp_dir.join("string_utils.c"),
        "int slen(const char* s) { return 0; }",
    )
    .unwrap();
    fs::write(temp_dir.join("main.c"), "int main() { return 0; }").unwrap();

    let maked_bin = env!("CARGO_BIN_EXE_maked");
    let compdb_file = temp_dir.join("compile_commands.json");

    // 1. Generate compilation database via --emit-compdb
    let out = Command::new(maked_bin)
        .arg("-f")
        .arg(&makefile_path)
        .arg(format!("--emit-compdb={}", compdb_file.display()))
        .current_dir(&temp_dir)
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "maked --emit-compdb failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(compdb_file.exists());

    let json_content = fs::read_to_string(&compdb_file).unwrap();
    assert!(json_content.starts_with("[\n"));
    assert!(json_content.ends_with("]\n"));

    // Check that all 3 compile units are recorded
    assert!(json_content.contains("\"file\": \"math.c\""));
    assert!(json_content.contains("\"file\": \"string_utils.c\""));
    assert!(json_content.contains("\"file\": \"main.c\""));

    assert!(json_content.contains("\"output\": \"math.o\""));
    assert!(json_content.contains("\"output\": \"string_utils.o\""));
    assert!(json_content.contains("\"output\": \"main.o\""));

    assert!(json_content.contains("clang -Wall -Wextra -O3 -DDEBUG=1 -c math.c -o math.o"));
    assert!(
        json_content
            .contains("clang -Wall -Wextra -O3 -DDEBUG=1 -c string_utils.c -o string_utils.o")
    );
    assert!(json_content.contains("clang -Wall -Wextra -O3 -DDEBUG=1 -c main.c -o main.o"));

    // Check directory is populated
    let cur_dir_str = temp_dir.to_string_lossy();
    assert!(json_content.contains(&*cur_dir_str));

    // Verify it doesn't include the linker step 'main_app' (not a single file -c compilation)
    assert!(!json_content.contains("\"file\": \"main_app\""));

    let _ = fs::remove_dir_all(&temp_dir);
}
