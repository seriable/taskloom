//! Explicit local identity administration / project-scoped JSON service.
//! This is not the durable queue/worker or SQL-backed Application increment.
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use taskloom::runtime::{
    app::{
        multiuser::{self, ProjectApplication},
        AppConfig, Application,
    },
    database::*,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Host {
    listen: String,
    projects: Vec<Project>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Project {
    tenant: String,
    project: String,
    config: PathBuf,
}
fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, Box<dyn std::error::Error>> {
    use std::io::Read;
    let mut data = Vec::new();
    std::fs::File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut data)?;
    if data.len() > 1024 * 1024 {
        return Err("configuration exceeds 1MiB".into());
    }
    Ok(serde_json::from_slice(&data)?)
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    const HELP:&str="multiuser_host migrate | catalog | put-user USER.json REV OPERATOR | issue USER NEW_PRIVATE_TOKEN_FILE OPERATOR | revoke TOKEN_ID OPERATOR | serve HOST.json; TASKLOOM_DATABASE_URL required";
    if args.is_empty() || args[0] == "--help" {
        println!("{HELP}");
        return Ok(());
    }
    let url = std::env::var("TASKLOOM_DATABASE_URL")
        .map_err(|_| "TASKLOOM_DATABASE_URL is required (value never logged)")?;
    let db = SqlDatabase::connect(&url, DatabaseOptions::default()).await?;
    let identities = db.identities();
    match args[0].as_str() {
        "migrate" if args.len() == 1 => {
            db.migrate().await?;
            println!("schema v1 ready");
        }
        "catalog" if args.len() == 1 => {
            // Trusted local admin output includes token hashes/metadata, never bearers.
            println!(
                "{}",
                serde_json::to_string_pretty(&identities.snapshot().await?)?
            );
        }
        "put-user" if args.len() == 4 => {
            identities
                .put_user(read(Path::new(&args[1]))?, args[2].parse()?, &args[3])
                .await?;
            println!("user configuration saved");
        }
        "issue" if args.len() == 4 => {
            use std::io::Write;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            // Create-only before issuance; never overwrite an existing secret.
            let mut file = options.open(&args[2])?;
            let token = identities
                .issue_token(&args[1], Duration::from_secs(86400), &args[3])
                .await?;
            if file
                .write_all(token.bearer.as_str().as_bytes())
                .and_then(|_| file.sync_all())
                .is_err()
            {
                let _ = identities.revoke_token(&token.id, &args[3]).await;
                return Err("token file persistence failed; issuance revocation attempted; inspect the local catalog".into());
            }
            println!(
                "{}",
                serde_json::json!({"token_id":token.id,"expires_ms":token.expires_ms,"bearer":"written only to the requested new private file"})
            );
        }
        "revoke" if args.len() == 3 => {
            identities.revoke_token(&args[1], &args[2]).await?;
            println!("token revoked");
        }
        "serve" if args.len() == 2 => {
            identities.snapshot().await?; // refuse an absent/unsupported schema
            let path = std::fs::canonicalize(&args[1])?;
            let host: Host = read(&path)?;
            let mut projects = Vec::new();
            let mut apps = Vec::new();
            if host.projects.is_empty() || host.projects.len() > 64 {
                return Err("host requires 1..64 projects".into());
            }
            for project in host.projects {
                let path = if project.config.is_absolute() {
                    project.config
                } else {
                    path.parent().unwrap().join(project.config)
                };
                let app = Application::open(AppConfig::load(&path).await?).await?;
                projects.push(ProjectApplication {
                    tenant: project.tenant,
                    project: project.project,
                    application: Arc::clone(&app),
                });
                apps.push(app);
            }
            let router = multiuser::router(identities, projects)?;
            let listener = tokio::net::TcpListener::bind(&host.listen).await?;
            println!(
                "Project-scoped JSON API listening on {}",
                listener.local_addr()?
            );
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await?;
            for app in apps {
                app.shutdown(Duration::from_secs(5)).await?;
            }
        }
        _ => return Err(HELP.into()),
    }
    db.close().await;
    Ok(())
}
