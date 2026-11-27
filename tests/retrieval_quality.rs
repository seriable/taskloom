#![cfg(feature = "runtime")]
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use taskloom::runtime::rag::*;

#[derive(Deserialize)]
struct Dataset {
    version: u32,
    provenance: String,
    documents: Vec<Document>,
    queries: Vec<Query>,
}
#[derive(Deserialize)]
struct Query {
    id: String,
    text: String,
    relevant: BTreeSet<String>,
}

async fn evaluate(store: &FileCorpusStore, retriever: &dyn Retriever, queries: &[Query]) -> Value {
    let mut recall = 0.0;
    let mut reciprocal_rank = 0.0;
    let mut positives = 0;
    let mut negatives_correct = 0;
    let mut negatives = 0;
    let mut cases = Vec::new();
    for query in queries {
        let result = search(store, retriever, &RetrievalQuery::new(&query.text))
            .await
            .unwrap();
        let mut seen = BTreeSet::new();
        let ranked = result
            .hits
            .iter()
            .map(|hit| hit.chunk.document_id.clone())
            .filter(|id| seen.insert(id.clone()))
            .collect::<Vec<_>>();
        let matches = ranked
            .iter()
            .filter(|id| query.relevant.contains(*id))
            .count();
        let rr = ranked
            .iter()
            .position(|id| query.relevant.contains(id))
            .map_or(0.0, |n| 1.0 / (n + 1) as f64);
        if query.relevant.is_empty() {
            negatives += 1;
            negatives_correct += usize::from(ranked.is_empty());
        } else {
            positives += 1;
            recall += matches as f64 / query.relevant.len() as f64;
            reciprocal_rank += rr;
        }
        cases.push(json!({"id":query.id,"query":query.text,"relevant":query.relevant,"ranked":ranked,"reciprocal_rank":rr,"skipped_oversize":result.skipped_oversize}));
    }
    json!({"positive_queries":positives,"recall_at_5":recall/positives as f64,"mrr_at_5":reciprocal_rank/positives as f64,
        "negative_queries":negatives,"negative_correct":negatives_correct,"cases":cases})
}
#[tokio::test]
async fn labelled_operations_corpus_measures_recall_ranking_and_negative_queries() {
    let dataset: Dataset =
        serde_json::from_str(include_str!("data/retrieval-quality-v1.json")).unwrap();
    assert_eq!(dataset.version, 1);
    let ids = dataset
        .documents
        .iter()
        .map(|d| d.id.clone())
        .collect::<BTreeSet<_>>();
    assert!(dataset.queries.iter().all(|q| q.relevant.is_subset(&ids)));
    let dir = tempfile::tempdir().unwrap();
    let store = FileCorpusStore::new(dir.path());
    for doc in dataset.documents {
        ingest(&store, doc, &WindowChunker::default(), None)
            .await
            .unwrap();
    }
    let body_only = ConfiguredLexicalRetriever::new(LexicalConfig {
        title_weight: 0.0,
        metadata_weight: 0.0,
        ..Default::default()
    })
    .unwrap();
    let before = evaluate(&store, &body_only, &dataset.queries).await;
    let after = evaluate(&store, &LexicalRetriever, &dataset.queries).await;
    let report = json!({"dataset_version":dataset.version,"provenance":dataset.provenance,
        "evidence_kind":"deterministic lexical regression; no model API or competitor benchmark",
        "baseline":"body-only BM25 configuration, same corpus and queries","body_only":before,"default_fields":after,
        "semantic_claim_support":"not evaluated by retrieval rankings or exact quote checks"});
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    assert!(after["recall_at_5"].as_f64().unwrap() >= 0.95);
    assert!(after["mrr_at_5"].as_f64().unwrap() >= 0.90);
    assert_eq!(after["negative_correct"], after["negative_queries"]);
    assert!(after["recall_at_5"].as_f64().unwrap() > before["recall_at_5"].as_f64().unwrap());
}
