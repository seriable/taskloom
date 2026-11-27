#![cfg(feature = "app")]
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;
use taskloom::runtime::app::*;
use taskloom::runtime::rag::*;
use taskloom::runtime::*;

const TOKEN: &str = "research-api-test-operator";
async fn serve_app(
    host: Arc<Application>,
) -> (String, reqwest::Client, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { serve(listener, host, TOKEN.into()).await.unwrap() });
    (url, reqwest::Client::new(), server)
}
async fn post(client: &reqwest::Client, url: &str, body: Value) -> reqwest::Response {
    client
        .post(url)
        .bearer_auth(TOKEN)
        .json(&body)
        .send()
        .await
        .unwrap()
}
#[tokio::test]
async fn web_api_imports_granted_source_then_offline_run_verifies_saved_quotes() {
    let source = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", source.local_addr().unwrap());
    let source_task = tokio::spawn(async move {
        axum::serve(source,axum::Router::new().route("/page",axum::routing::get(||async{axum::response::Html("<p>Approval binds exact arguments.</p><script>never execute source scripts</script>")}))).await.unwrap()
    });
    let dir = tempfile::tempdir().unwrap();
    let cfg = AppConfig {
        state_dir: dir.path().into(),
        research: AppResearch {
            web: Some(WebAccess {
                allowed_origins: BTreeSet::from([origin.clone()]),
                allow_private: true,
                max_body_bytes: 4096,
                max_text_bytes: 4096,
                max_redirects: 0,
                timeout_ms: 1000,
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    let host = Application::open(cfg).await.unwrap();
    let (url, client, server) = serve_app(host.clone()).await;
    let capabilities: Value = client
        .get(format!("{url}/api/research/capabilities"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(capabilities["web"], true);
    assert_eq!(capabilities["pdf"], false);
    assert_eq!(post(&client,&format!("{url}/api/knowledge/web"),json!({"id":"guide","title":"Guide","url":format!("{origin}/page"),"expected_revision":0})).await.status(),200);
    assert_eq!(post(&client,&format!("{url}/api/knowledge/web"),json!({"id":"guide","title":"Guide","url":"http://127.0.0.1:1/private","expected_revision":1})).await.status(),403);
    assert_eq!(host.corpus.snapshot().await.unwrap().revision, 1);
    host.start("research", "approval").await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while host.job("research").unwrap().active {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let report = host.verified_report("research").await.unwrap();
    assert_eq!(report.sources[0].source, format!("{origin}/page"));
    assert!(!report.report.text.contains("never execute"));
    assert_eq!(post(&client,&format!("{url}/api/knowledge/pdf"),json!({"id":"pdf","title":"PDF","filename":"../host.pdf","data_base64":"","expected_revision":1})).await.status(),400);
    assert_eq!(
        post(
            &client,
            &format!("{url}/api/research/search"),
            json!({"query":"test","count":1})
        )
        .await
        .status(),
        400
    );
    source_task.abort();
    server.abort();
}
struct CustomRetriever(Arc<AtomicUsize>);
impl Retriever for CustomRetriever {
    fn identity(&self) -> Option<String> {
        Some("host-custom-retrieval-v1".into())
    }
    fn retrieve<'a>(
        &'a self,
        snapshot: &'a CorpusSnapshot,
        _: &'a RetrievalQuery,
    ) -> BoxFuture<'a, Result<Vec<ScoredChunk>, RuntimeError>> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(snapshot
                .chunks
                .first()
                .map(|c| {
                    vec![ScoredChunk {
                        chunk_id: c.id.clone(),
                        score: 1.0,
                    }]
                })
                .unwrap_or_default())
        })
    }
}
struct SearchModel;
impl Model for SearchModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            budget.reserve_before_dispatch(0).await?.settle(0);
            let turn = if request
                .messages
                .iter()
                .any(|m| matches!(m, Message::Tool { .. }))
            {
                AssistantTurn::text("fixture complete")
            } else {
                AssistantTurn {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "search".into(),
                        name: "knowledge_search".into(),
                        arguments: json!({"query":"no lexical match"}),
                    }],
                }
            };
            Ok(ModelOutput {
                turn,
                model: request.model.clone(),
            })
        })
    }
}
#[tokio::test]
async fn operator_search_and_agent_tool_share_host_injected_retriever() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = AppConfig {
        state_dir: dir.path().into(),
        ..Default::default()
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let host = Application::with_retriever(
        cfg,
        Arc::new(SearchModel),
        ToolRegistry::new(),
        ToolPolicy::default(),
        Arc::new(CustomRetriever(calls.clone())),
    )
    .await
    .unwrap();
    ingest(
        host.corpus.as_ref(),
        Document {
            id: "a".into(),
            title: "A".into(),
            source: "fixture:a".into(),
            text: "A source excerpt.".into(),
            metadata: Default::default(),
        },
        &WindowChunker::default(),
        Some(0),
    )
    .await
    .unwrap();
    let (url, client, server) = serve_app(host.clone()).await;
    let result = post(
        &client,
        &format!("{url}/api/knowledge/search"),
        serde_json::to_value(RetrievalQuery::new("no lexical match")).unwrap(),
    )
    .await
    .json::<Value>()
    .await
    .unwrap();
    assert_eq!(result["hits"][0]["chunk"]["document_id"], "a");
    host.start("custom", "search").await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while host.job("custom").unwrap().active {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(host.inspect("custom").await.unwrap().config.tools[0]
        .description
        .contains("host-custom-retrieval-v1"));
    server.abort();
}
