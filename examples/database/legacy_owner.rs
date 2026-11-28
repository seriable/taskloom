//! Copy to an archived schema-v2 SDK's examples/old_queue_owner.rs, then
//! compile there. Lease-only migration acceptance; no model/tool calls.
use std::{io::Write, time::Duration};
use taskloom::runtime::{database::*, RunStore};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if DATABASE_SCHEMA_VERSION != 2 {
        return Err("compile this migration fixture against the archived schema-v2 SDK".into());
    }
    let url = std::env::var("TASKLOOM_DATABASE_URL")
        .map_err(|_| "private database environment required")?;
    let db = SqlDatabase::connect(&url, DatabaseOptions::default()).await?;
    let runs = db.runs(DatabaseNamespace::new("legacy-v2-owner", "migration-probe")?);
    let owner = runs.acquire("old-v2-lease").await?;
    owner.verify_ownership().await?;
    println!("old-v2-owner-ready");
    std::io::stdout().flush()?;
    tokio::time::sleep(Duration::from_secs(30)).await;
    drop(owner);
    db.close().await;
    Ok(())
}
