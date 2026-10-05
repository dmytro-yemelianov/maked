use std::fs;
use std::process::Command;

#[test]
fn test_tui_dashboard_execution() {
    let temp_dir = std::env::temp_dir().join(format!("maked_test_tui_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    let makefile_content = r#"
all: target1 target2 target3

target1:
	@echo "BUILDING_1"
	@echo "res1" > target1

target2:
	@echo "BUILDING_2"
	@echo "res2" > target2

target3: target1 target2
	@echo "BUILDING_3"
	@cat target1 target2 > target3
"#;
    let makefile_path = temp_dir.join("Makefile");
    fs::write(&makefile_path, makefile_content).unwrap();

    let maked_bin = env!("CARGO_BIN_EXE_maked");

    // Run with --tui in non-interactive / piped environment (assert fallback line printing works without panic)
    let out = Command::new(maked_bin)
        .arg("-f")
        .arg(&makefile_path)
        .arg("-j4")
        .arg("--tui")
        .current_dir(&temp_dir)
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "maked --tui build failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout_str = String::from_utf8_lossy(&out.stdout);
    // In non-interactive piped environment, it emits clean progress lines
    assert!(stdout_str.contains("Finished") || stdout_str.contains("target3"));
    assert!(temp_dir.join("target3").exists());
    assert_eq!(
        fs::read_to_string(temp_dir.join("target3")).unwrap(),
        "res1\nres2\n"
    );

    let _ = fs::remove_dir_all(&temp_dir);
}
