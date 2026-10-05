use std::fs;
use std::process::Command;

#[test]
fn test_content_addressable_cache_workflow() {
    let temp_dir =
        std::env::temp_dir().join(format!("maked_test_cas_workflow_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    let makefile_content = r#"
all: app

app: obj.o
	@echo "LINKING APP"
	@cat obj.o > app
	@echo "app_binary" >> app

obj.o: src.txt
	@echo "COMPILING OBJ"
	@cat src.txt > obj.o
"#;
    let makefile_path = temp_dir.join("Makefile");
    fs::write(&makefile_path, makefile_content).unwrap();

    let src_path = temp_dir.join("src.txt");
    fs::write(&src_path, "source_v1\n").unwrap();

    let maked_bin = env!("CARGO_BIN_EXE_maked");
    let cache_dir = temp_dir.join(".custom_cas");

    // 1. Initial Cold Build with --cache
    let out1 = Command::new(maked_bin)
        .arg("-f")
        .arg(&makefile_path)
        .arg("--cache")
        .arg(format!("--cache-dir={}", cache_dir.display()))
        .arg("--profile")
        .current_dir(&temp_dir)
        .output()
        .unwrap();

    assert!(out1.status.success());
    let stdout1 = String::from_utf8_lossy(&out1.stdout);
    assert!(stdout1.contains("COMPILING OBJ"));
    assert!(stdout1.contains("LINKING APP"));
    assert!(temp_dir.join("app").exists());
    assert!(temp_dir.join("obj.o").exists());

    // 2. Delete the built artifacts completely!
    fs::remove_file(temp_dir.join("app")).unwrap();
    fs::remove_file(temp_dir.join("obj.o")).unwrap();
    assert!(!temp_dir.join("app").exists());
    assert!(!temp_dir.join("obj.o").exists());

    // 3. Re-run build with --cache: both app and obj.o should be RESTORED FROM CACHE!
    let out2 = Command::new(maked_bin)
        .arg("-f")
        .arg(&makefile_path)
        .arg("--cache")
        .arg(format!("--cache-dir={}", cache_dir.display()))
        .arg("--profile")
        .current_dir(&temp_dir)
        .output()
        .unwrap();

    assert!(out2.status.success());
    let stdout2 = String::from_utf8_lossy(&out2.stdout);
    // Recipes must NOT be executed!
    assert!(!stdout2.contains("COMPILING OBJ"));
    assert!(!stdout2.contains("LINKING APP"));
    // Cache restoration message should appear
    assert!(stdout2.contains("Restored obj.o from cache"));
    assert!(stdout2.contains("Restored app from cache"));
    assert!(stdout2.contains("Targets from cache:"));

    // Artifacts must be successfully restored on disk
    assert!(temp_dir.join("app").exists());
    assert!(temp_dir.join("obj.o").exists());
    let app_content = fs::read_to_string(temp_dir.join("app")).unwrap();
    assert!(app_content.contains("source_v1"));
    assert!(app_content.contains("app_binary"));

    let _ = fs::remove_dir_all(&temp_dir);
}
