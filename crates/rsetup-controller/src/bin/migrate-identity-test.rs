use rsetup_controller::{TestMigrationConfig, TestMigrationMode, run_identity_test_migration};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mode = match (args.next().as_deref(), args.next().as_deref(), args.next()) {
        (Some("--mode"), Some("upgrade"), None) => TestMigrationMode::Upgrade,
        (Some("--mode"), Some("fixture-v1"), None) => TestMigrationMode::FixtureV1,
        (Some("--mode"), Some("fixture-partial-v1"), None) => TestMigrationMode::FixturePartialV1,
        _ => return Err("explicit --mode upgrade|fixture-v1|fixture-partial-v1 required".into()),
    };
    // Never log the config, environment, database URL, credentials, or administrator secret.
    let config = TestMigrationConfig::from_test_env()?;
    run_identity_test_migration(&config, mode).await?;
    Ok(())
}
