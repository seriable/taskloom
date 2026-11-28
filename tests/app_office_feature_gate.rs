#![cfg(all(feature = "app", not(feature = "office")))]

#[tokio::test]
async fn office_command_without_feature_refuses_before_opening_missing_config_or_source() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config-must-not-be-read.json");
    let source = dir.path().join("source-must-not-be-read.xlsx");
    let error = taskloom::runtime::app::cli::run(vec![
        "--config".into(),
        config.to_string_lossy().into_owned(),
        "ingest-office".into(),
        source.to_string_lossy().into_owned(),
        "sheet".into(),
        "Spreadsheet".into(),
        "xlsx".into(),
    ])
    .await
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Office ingestion requires the office feature"
    );
    assert!(!config.exists());
    assert!(!source.exists());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}
