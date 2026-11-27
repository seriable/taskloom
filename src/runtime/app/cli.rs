//! CLI entry point, also used by the packaged `taskloom` binary.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::super::rag::{
    self, CorpusStore, DocumentLoader, RetrievalQuery, TextLoader, WindowChunker,
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
  import-pdf ID TITLE FILE           import PDF using configured pdftotext\n\
  import-web ID TITLE URL            ingest a host-granted webpage\n\
  web-search QUERY                   query configured Brave-compatible search\n\
  knowledge | search QUERY          inspect/query local corpus\n\
  memory | memory-recall QUERY      export scoped memory / bounded recall (no model call)\n\
  memory-review FILE.json           review a candidate, explicit revision and replacement\n\
  memory-edit FILE.json             edit active memory with operator provenance\n\
  memory-forget FILE.json           erase all versions of a memory slot\n\
  memory-extract FILE.json          explicit metered extraction from a closed session turn\n\
  index-status                      inspect index/config/embedding spending\n\
  index-build REV BINDING            explicit full build/update; may incur embedding charges\n\
  benchmark SUITE OUT MAX_MICROS    execute a fresh task-evaluation campaign\n\
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
    if args[0] == "benchmark" {
        if args.len() != 4 {
            return Err(HELP.into());
        }
        let suite_path = PathBuf::from(&args[1]);
        let bytes = super::super::store::blocking(move || {
            use std::io::Read;
            let file = std::fs::File::open(suite_path).map_err(super::super::store::storage)?;
            let mut bytes = Vec::new();
            file.take(16 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(super::super::store::storage)?;
            if bytes.len() > 16 * 1024 * 1024 {
                return Err(super::config::config_error("suite exceeds 16 MiB"));
            }
            Ok(bytes)
        })
        .await?;
        let suite: super::super::benchmark::BenchmarkSuite = serde_json::from_slice(&bytes)?;
        suite.validate()?;
        let total = args[3].parse::<u64>()?;
        let kind = if matches!(&config.provider, super::AppProvider::Offline) {
            super::super::benchmark::ExecutionKind::OfflineDemo
        } else {
            super::super::benchmark::ExecutionKind::ConfiguredProvider
        };
        let model = config.model()?;
        let evaluation = super::super::benchmark::BenchmarkConfig {
            origin: super::super::benchmark::BenchmarkOrigin {
                kind,
                model: config.model.clone(),
                configuration_fingerprint: config.label()?,
            },
            limits: config.limits.clone(),
            context: Some(config.context.clone()),
            output_tokens: config.output_tokens,
            max_total_cost_micros: Some(total),
        };
        let report = super::super::benchmark::BenchmarkRunner::new(model.as_ref(), evaluation)
            .run(&suite, &args[2])
            .await?;
        output(&report)?;
        if report.passed != report.requested {
            return Err(format!(
                "benchmark: {} passed, {} failed, {} not run; report saved",
                report.passed, report.failed, report.not_run
            )
            .into());
        }
        return Ok(());
    }
    // Inspection/import do not initialize a model or contact MCP services.
    let runs = super::super::FileRunStore::new(config.state_dir.join("runs"));
    let corpus = super::super::rag::FileCorpusStore::new(config.state_dir.join("knowledge"));
    if args[0].starts_with("memory") {
        use super::super::memory::*;
        let memory = config
            .memory
            .as_ref()
            .ok_or("long-term memory is not enabled by the host")?;
        let access = memory.access()?;
        let store = FileMemoryStore::new(config.state_dir.join("memory"));
        let manager = MemoryManager::new(&store);
        match args[0].as_str() {
            "memory" if args.len() == 1 => output(&manager.snapshot(&access).await?)?,
            "memory-recall" if args.len() == 2 => {
                output(&manager.recall(&access, &args[1], &memory.recall).await?)?
            }
            "memory-review" if args.len() == 2 => {
                let input: super::MemoryReview = decision_file(&args[1]).await?;
                output(
                    &manager
                        .review(
                            &access,
                            input.expected_revision,
                            &input.id,
                            MemoryReviewDecision {
                                accept: input.accept,
                                replace: input.replace,
                                note: input.note,
                            },
                        )
                        .await?,
                )?;
            }
            "memory-edit" if args.len() == 2 => {
                let input: super::MemoryEdit = decision_file(&args[1]).await?;
                output(
                    &manager
                        .edit(
                            &access,
                            input.expected_revision,
                            &input.id,
                            &input.value,
                            &input.note,
                        )
                        .await?,
                )?;
            }
            "memory-forget" if args.len() == 2 => {
                let input: super::MemoryForget = decision_file(&args[1]).await?;
                output(
                    &manager
                        .forget(&access, input.expected_revision, input.kind, &input.key)
                        .await?,
                )?;
            }
            "memory-extract" if args.len() == 2 => {
                access.require_curator()?;
                let input: super::MemoryExtract = decision_file(&args[1]).await?;
                // Load no MCP/tool credentials for this tool-less operation.
                let sessions =
                    super::super::FileSessionStore::new(config.state_dir.join("sessions"));
                let session = super::super::SessionManager::new(&sessions)
                    .load(&input.session_id)
                    .await?;
                let model = config.model()?;
                let tools = super::super::ToolRegistry::new();
                let agent = super::super::AgentLoop::new(model.as_ref(), &tools, &config.model)
                    .with_label(config.label()?)
                    .with_limits(config.limits.clone())
                    .with_context(config.context.clone())
                    .with_output_tokens(config.output_tokens);
                let extracted = manager
                    .extract_from_turn(
                        &access,
                        &session,
                        &MemoryExtractionRequest {
                            expected_revision: input.expected_revision,
                            turn_id: input.turn_id,
                            request_id: input.request_id,
                        },
                        &agent,
                        &runs,
                    )
                    .await?;
                output(&extracted)?;
                if !extracted.proposals_recorded {
                    return Err(
                        "memory extraction did not complete; inspect the retained run outcome"
                            .into(),
                    );
                }
            }
            _ => return Err(HELP.into()),
        }
        return Ok(());
    }
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
            let retriever = super::retrieval::configured(&config).await?;
            output(
                &rag::search(&corpus, retriever.as_ref(), &RetrievalQuery::new(&args[1])).await?,
            )?;
            return Ok(());
        }
        "index-status" if args.len() == 1 => {
            output(&super::retrieval::index_status(&config).await?)?;
            return Ok(());
        }
        "index-build" if args.len() == 3 => {
            output(&super::retrieval::build_index(&config, args[1].parse()?, &args[2]).await?)?;
            return Ok(());
        }
        "import-pdf" if args.len() == 4 => {
            let pdf = rag::PdfLoader::new(
                config
                    .research
                    .pdf
                    .clone()
                    .ok_or("PDF ingestion is not configured")?,
            )?;
            let document = pdf
                .load(std::path::Path::new(&args[3]), &args[1], &args[2])
                .await?;
            let snapshot = rag::ingest(&corpus, document, &WindowChunker::default(), None).await?;
            output(
                &serde_json::json!({"revision":snapshot.revision,"documents":snapshot.documents.len(),"chunks":snapshot.chunks.len()}),
            )?;
            return Ok(());
        }
        "import-web" if args.len() == 4 => {
            let web = rag::WebClient::new(
                config
                    .research
                    .web
                    .clone()
                    .ok_or("web ingestion is not configured")?,
            )?;
            let document = web.fetch(&args[3], &args[1], &args[2]).await?;
            let snapshot = rag::ingest(&corpus, document, &WindowChunker::default(), None).await?;
            output(
                &serde_json::json!({"revision":snapshot.revision,"documents":snapshot.documents.len(),"chunks":snapshot.chunks.len()}),
            )?;
            return Ok(());
        }
        "web-search" if args.len() == 2 => {
            let search = config
                .research
                .search
                .clone()
                .ok_or("web search is not configured")?;
            let key = super::config::secret(&search.key_env)?;
            output(
                &rag::WebSearch::new(search, &key)?
                    .search(&args[1], 5)
                    .await?,
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
            // Ctrl+C first stops the listener (axum stops accepting as soon as
            // the graceful-shutdown future completes and closes keep-alive
            // connections after their current request); only then are workers
            // signalled, so no new job can start during the shutdown grace.
            let (stopped, accepting_stopped) = tokio::sync::oneshot::channel::<()>();
            let server = axum::serve(listener, router).with_graceful_shutdown(async move {
                let _ = tokio::signal::ctrl_c().await;
                let _ = stopped.send(());
            });
            let workers = async {
                if accepting_stopped.await.is_ok() {
                    let _ = app.shutdown(Duration::from_secs(5)).await;
                }
            };
            let server = std::future::IntoFuture::into_future(server);
            let (served, ()) = tokio::join!(server, workers);
            served?;
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
async fn decision_file<T: serde::de::DeserializeOwned>(
    path: &str,
) -> Result<T, Box<dyn std::error::Error>> {
    // Bound file I/O itself rather than allocating an arbitrary local file.
    let path = PathBuf::from(path);
    let bytes = super::super::store::blocking(move || {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(super::super::store::storage)?
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(super::super::store::storage)?;
        if bytes.len() > 1024 * 1024 {
            return Err(super::config::config_error(
                "memory request file exceeds 1 MiB",
            ));
        }
        Ok(bytes)
    })
    .await?;
    Ok(serde_json::from_slice(&bytes)?)
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
