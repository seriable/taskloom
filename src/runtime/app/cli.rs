//! CLI entry point, also used by the packaged `taskloom` binary.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::super::rag::{
    self, CorpusStore, DocumentLoader, LexicalRetriever, RetrievalQuery, TextLoader, WindowChunker,
};
use super::super::{ApprovalDecision, RecoveryDecision, RunStatus, RuntimeError};
use super::{AppConfig, Application};
use serde::Serialize;

const HELP: &str = "taskloom [--config FILE] COMMAND\n\
  init FILE                         write offline configuration (create only)\n\
  serve [127.0.0.1:8080]             API/UI; TASKLOOM_API_TOKEN required\n\
  run ID PROMPT                     create and execute a durable run\n\
  resume ID                         resume a persisted run\n\
  inspect ID | events ID | list      read persisted state without dispatch\n\
  approve ID TICKET ACTOR NOTE       record approval, then explicitly resume\n\
  reject ID TICKET ACTOR NOTE        record rejection\n\
  cancel ID ACTOR NOTE               cancel an unowned persisted run\n\
  reconcile ID CALL DECISION.json    record observed tool outcome\n\
  report ID                         verify saved cited report\n\
  import ID TITLE FILE               import UTF-8 text/Markdown\n\
  knowledge | search QUERY          inspect/query local corpus\n\
Use API/UI to pause or cancel work owned by the running server.\n";
pub async fn run(
    arguments: impl IntoIterator<Item = String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut args = arguments.into_iter().collect::<Vec<_>>();
    if args.is_empty() || args[0] == "--help" || args[0] == "-h" {
        print!("{HELP}");
        return Ok(());
    }
    if args[0] == "--version" {
        println!("taskloom {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let config_path = if args[0] == "--config" {
        if args.len() < 3 {
            return Err(HELP.into());
        }
        let path = PathBuf::from(args.remove(1));
        args.remove(0);
        path
    } else {
        PathBuf::from("taskloom.json")
    };
    if args[0] == "init" {
        if args.len() != 2 {
            return Err(HELP.into());
        }
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&args[1])?;
        file.write_all(&serde_json::to_vec_pretty(&AppConfig::default())?)?;
        println!("Created {}", args[1]);
        return Ok(());
    }
    let config = AppConfig::load(&config_path).await?;
    // Inspection/import do not initialize a model or contact MCP services.
    let runs = super::super::FileRunStore::new(config.state_dir.join("runs"));
    let corpus = super::super::rag::FileCorpusStore::new(config.state_dir.join("knowledge"));
    match args[0].as_str() {
        "list" if args.len() == 1 => {
            output(&super::list_persisted(&runs).await?)?;
            return Ok(());
        }
        "report" if args.len() == 2 => {
            use super::super::RunStore;
            let state = runs.inspect(&args[1]).await?.ok_or("run does not exist")?;
            output(&super::verify_report(&state)?)?;
            return Ok(());
        }
        "inspect" | "events" if args.len() == 2 => {
            use super::super::RunStore;
            let state = runs.inspect(&args[1]).await?.ok_or("run does not exist")?;
            if args[0] == "events" {
                output(&state.report.events)?;
            } else {
                output(&state)?;
            }
            return Ok(());
        }
        "knowledge" if args.len() == 1 => {
            output(&corpus.snapshot().await?)?;
            return Ok(());
        }
        "import" if args.len() == 4 => {
            let document = TextLoader
                .load(std::path::Path::new(&args[3]), &args[1], &args[2])
                .await?;
            let snapshot = rag::ingest(&corpus, document, &WindowChunker::default(), None).await?;
            println!(
                "revision {}; {} documents; {} chunks",
                snapshot.revision,
                snapshot.documents.len(),
                snapshot.chunks.len()
            );
            return Ok(());
        }
        "search" if args.len() == 2 => {
            output(
                &rag::search(&corpus, &LexicalRetriever, &RetrievalQuery::new(&args[1])).await?,
            )?;
            return Ok(());
        }
        _ => {}
    }
    let app = Application::open(config).await?;
    match args[0].as_str() {
        "serve" if args.len() <= 2 => {
            let address = args
                .get(1)
                .map_or("127.0.0.1:8080", String::as_str)
                .parse::<std::net::SocketAddr>()?;
            if !address.ip().is_loopback() {
                return Err("CLI serve binds loopback only; use the library behind your authenticated TLS deployment for remote access".into());
            }
            let token = super::config::secret("TASKLOOM_API_TOKEN")?;
            let listener = tokio::net::TcpListener::bind(address).await?;
            println!(
                "taskloom UI/API: http://{} (bearer token from TASKLOOM_API_TOKEN)",
                listener.local_addr()?
            );
            let router = super::router(Arc::clone(&app), token)?;
            let shutdown_app = Arc::clone(&app);
            axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = tokio::signal::ctrl_c().await;
                    let _ = shutdown_app.shutdown(Duration::from_secs(5)).await;
                })
                .await?;
        }
        "run" if args.len() == 3 => {
            app.start(&args[1], &args[2]).await?;
            wait(&app, &args[1]).await?;
        }
        "resume" if args.len() == 2 => {
            app.resume(&args[1]).await?;
            wait(&app, &args[1]).await?;
        }
        "approve" | "reject" if args.len() == 5 => output(
            &app.decide(
                &args[1],
                &args[2],
                ApprovalDecision {
                    approved: args[0] == "approve",
                    actor: args[3].clone(),
                    note: args[4].clone(),
                },
            )
            .await?,
        )?,
        "cancel" if args.len() == 4 => {
            app.cancel(&args[1], &args[2], &args[3]).await?;
            output(&app.inspect(&args[1]).await?)?;
        }
        "reconcile" if args.len() == 4 => {
            let bytes = tokio::fs::read(&args[3]).await?;
            if bytes.len() > 1024 * 1024 {
                return Err("decision exceeds 1 MiB".into());
            }
            let decision: RecoveryDecision = serde_json::from_slice(&bytes)?;
            output(&app.reconcile(&args[1], &args[2], decision).await?)?;
        }
        _ => return Err(HELP.into()),
    }
    Ok(())
}
fn output(value: &impl Serialize) -> Result<(), serde_json::Error> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
async fn wait(app: &Application, id: &str) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        let job = app.job(id)?;
        if !job.active {
            if let Some(error) = job.error {
                return Err(error.into());
            }
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let state = app.inspect(id).await?;
    output(&state)?;
    if state.status == RunStatus::Finished
        && state.report.stop_reason != super::super::StopReason::Completed
    {
        return Err(
            RuntimeError::Conflict(format!("run stopped: {:?}", state.report.stop_reason)).into(),
        );
    }
    Ok(())
}
