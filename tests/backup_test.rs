use std::fs;
use std::path::Path;

#[test]
fn creates_backup_file_when_saving() {
    let temp_dir = std::env::temp_dir();
    let test_file = temp_dir.join("test_backup.txt");
    let backup_file = temp_dir.join("test_backup.txt.bak");

    // Clean up any existing files
    let _ = fs::remove_file(&test_file);
    let _ = fs::remove_file(&backup_file);

    // Write initial content
    fs::write(&test_file, "original").unwrap();

    // Simulate saving with backup (this behavior should be implemented)
    // For now, just check if backup exists after save
    // This test will fail until backup logic is implemented
    let content = fs::read_to_string(&test_file).unwrap();
    assert_eq!(content, "original");

    // This assertion will fail because backup is not yet created
    assert!(backup_file.exists(), "Backup file should exist after saving");

    // Cleanup
    let _ = fs::remove_file(&test_file);
    let _ = fs::remove_file(&backup_file);
}