//! Real Qdrant REST smoke with explicit offline vectors. Not a semantic-quality
//! benchmark. The host pre-creates an isolated unnamed cosine collection(size2).
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use taskloom::runtime::{rag::*, RuntimeError};

#[derive(Default)]
struct Offline(AtomicUsize);
impl EmbeddingModel for Offline {
    fn identity(&self) -> String {
        "qdrant-example-offline-fixture-v1".into()
    }
    fn embed<'a>(
        &'a self,
        texts: &'a [String],
    ) -> BoxFuture<'a, Result<EmbeddingBatch, RuntimeError>> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(EmbeddingBatch {
                identity: self.identity(),
                vectors: texts.iter().map(|_| vec![1.0, 0.5]).collect(),
            })
        })
    }
}
fn save(path: &Path, value: &impl serde::Serialize) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    Ok(())
}
async fn ingest_fixture(store: &FileCorpusStore, id: &str, text: &str) -> Result<(), RuntimeError> {
    ingest(
        store,
        Document {
            id: id.into(),
            title: id.into(),
            source: format!("fixture:qdrant:{id}"),
            text: text.into(),
            metadata: Default::default(),
        },
        &WindowChunker::default(),
        None,
    )
    .await?;
    Ok(())
}
async fn scenario(config: QdrantConfig, root: &Path) -> Result<Value, Box<dyn std::error::Error>> {
    let store = FileCorpusStore::new(root.join("corpus"));
    ingest_fixture(
        &store,
        "alpha",
        "Alpha source is resolved from the local frozen corpus.",
    )
    .await?;
    ingest_fixture(
        &store,
        "beta",
        "Beta source retains exact UTF-8 provenance and source offsets.",
    )
    .await?;
    let snapshot = store.snapshot().await?;
    let model = Arc::new(Offline::default());
    let index = Arc::new(EmbeddingIndex::build(&snapshot, model.as_ref()).await?);
    save(&root.join("index.json"), index.as_ref())?;
    let client = Arc::new(QdrantClient::new(config.clone())?);
    let journal = FileQdrantPublishJournal::new(root.join("publish"))?;
    client.publish(&snapshot, &index, &journal).await?;
    let receipt = client
        .completed_receipt(&snapshot, &index, &journal)
        .await?;
    let retriever = QdrantRetriever::new(
        client,
        &snapshot,
        index.clone(),
        receipt,
        model.clone(),
        0.5,
    )?;
    let query = RetrievalQuery {
        text: "source".into(),
        top_k: 20,
        max_bytes: 16 * 1024,
    };
    let first = search(&store, &retriever, &query).await?;
    save(&root.join("search-before-foreign.json"), &first)?;
    let before = model.0.load(Ordering::SeqCst);
    let mut stale = snapshot.clone();
    stale.revision += 1;
    let stale_refused = retriever.retrieve(&stale, &query).await.is_err()
        && model.0.load(Ordering::SeqCst) == before;
    // Separate explicit host publication. Query itself never upserts points.
    let foreign_store = FileCorpusStore::new(root.join("foreign-corpus"));
    ingest_fixture(
        &foreign_store,
        "foreign",
        "A foreign namespace cannot enter the original source selection.",
    )
    .await?;
    let foreign_snapshot = foreign_store.snapshot().await?;
    let foreign_index = EmbeddingIndex::build(&foreign_snapshot, model.as_ref()).await?;
    let mut foreign_config = config;
    foreign_config.namespace.push_str("::foreign");
    let foreign_client = QdrantClient::new(foreign_config)?;
    let foreign_journal = FileQdrantPublishJournal::new(root.join("foreign-publish"))?;
    foreign_client
        .publish(&foreign_snapshot, &foreign_index, &foreign_journal)
        .await?;
    let after = search(&store, &retriever, &query).await?;
    save(&root.join("search-after-foreign.json"), &after)?;
    let checks = json!({"real_qdrant_publish_and_query":first.hits.len()==2,"stale_snapshot_refused_before_embedding":stale_refused,"foreign_namespace_does_not_enter_original_query":after.hits.len()==2&&after.hits.iter().all(|hit|snapshot.chunks.iter().any(|chunk|chunk==&hit.chunk)),"sources_resolve_from_exact_local_snapshot":first.hits.iter().all(|hit|snapshot.chunks.iter().any(|chunk|chunk==&hit.chunk)),"complete_publish_journals":journal.snapshot().await?.is_some_and(|state|state.complete)&&foreign_journal.snapshot().await?.is_some_and(|state|state.complete)});
    if !checks
        .as_object()
        .expect("checks object")
        .values()
        .all(|v| v == &Value::Bool(true))
    {
        return Err("Qdrant example acceptance check failed".into());
    }
    Ok(
        json!({"checks":checks,"embedding_model":"explicit offline protocol fixture; no cloud request","fixture_embedding_calls":model.0.load(Ordering::SeqCst),"published_main_points":2,"published_foreign_points":1,"automatic_query_writes":false}),
    )
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 2 {
        return Err("usage: agent_qdrant HOST-QDRANT-CONFIG.json NEW-OUTPUT-DIRECTORY (requires existing unnamed cosine collection, size2)".into());
    }
    let config: QdrantConfig = serde_json::from_slice(&std::fs::read(&args[0])?)?;
    config.validate()?;
    let root = std::path::absolute(PathBuf::from(&args[1]))?;
    std::fs::create_dir(&root)?;
    let result = scenario(config, &root).await;
    let passed = result.is_ok();
    let report = match result {
        Ok(value) => {
            json!({"version":1,"status":"passed","completed":true,"scope":"real Qdrant REST and local provenance with offline vectors, not model effectiveness/cloud/SLO","result":value})
        }
        Err(error) => {
            json!({"version":1,"status":"failed","completed":false,"error":error.to_string(),"pending_write_journals":"retain and inspect; no implicit retry or query receipt"})
        }
    };
    save(&root.join("report.json"), &report)?;
    println!("{}", serde_json::to_string(&report)?);
    if !passed {
        return Err("Qdrant example acceptance failed; local report retained".into());
    }
    Ok(())
}
