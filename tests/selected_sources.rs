#![cfg(feature = "runtime")]
//! Source integrity/selection controls, not native semantic quality evidence.
use taskloom::runtime::rag::*;

fn evidence(id: &str, text: &str, revision: u64) -> SearchResult {
    let document = Document {
        id: id.into(),
        title: id.into(),
        source: format!("fixture:{id}"),
        text: text.into(),
        metadata: Default::default(),
    };
    SearchResult {
        corpus_revision: revision,
        hits: vec![Evidence {
            chunk: Chunk::from_range(&document, 0, text.len()).unwrap(),
            score: 1.0,
        }],
        skipped_oversize: 0,
    }
}
#[test]
fn host_quotes_exact_wrapped_bodies_and_keeps_model_selection_unchanged() {
    let first = evidence(
        "a",
        "Approval binds the exact tool\nname and arguments. 中文原文。",
        7,
    );
    let second = evidence("b", "Observe the external outcome before resuming.", 9);
    let a = &first.hits[0].chunk;
    let b = &second.hits[0].chunk;
    let selection = SourceSelectionReport {
        text: format!("Approval [[{}]], recovery [[{}]].", a.id, b.id),
        sources: vec![a.id.clone(), b.id.clone()],
    };
    let original = selection.clone();
    let verified =
        SearchResult::verify_selection_all(&[first.clone(), second.clone()], selection).unwrap();
    assert_eq!(verified.report.text, original.text);
    assert_eq!(verified.report.citations[0].quote, a.text);
    assert!(verified.report.citations[0].quote.contains("tool\nname"));
    assert_eq!(verified.corpus_revisions, [7, 9]);
    assert_eq!(original.sources, vec![a.id.clone(), b.id.clone()]);
    // Selecting a source does not mean the prose follows it.
    assert!(SearchResult::verify_selection_all(
        &[first],
        SourceSelectionReport {
            text: format!(
                "Approval permits every changed payload [[{}]].",
                original.sources[0]
            ),
            sources: vec![original.sources[0].clone()]
        }
    )
    .is_ok());
}
#[test]
fn unknown_duplicate_changed_conflicting_and_unmarked_selections_are_refused() {
    let source = evidence("a", "The exact source states an approval requirement.", 1);
    let id = source.hits[0].chunk.id.clone();
    for selected in [
        SourceSelectionReport {
            text: "claim".into(),
            sources: vec![id.clone()],
        },
        SourceSelectionReport {
            text: format!("[[{id}]]"),
            sources: vec!["0".repeat(64)],
        },
        SourceSelectionReport {
            text: format!("[[{id}]]"),
            sources: vec![id.clone(), id.clone()],
        },
        SourceSelectionReport {
            text: format!("[[{id}]][[undeclared]]"),
            sources: vec![id.clone()],
        },
        SourceSelectionReport {
            text: "[[1]]".into(),
            sources: vec![id[..12].into()],
        },
    ] {
        assert!(
            SearchResult::verify_selection_all(std::slice::from_ref(&source), selected).is_err()
        );
    }
    let selection = SourceSelectionReport {
        text: format!("[[{id}]]"),
        sources: vec![id],
    };
    let mut corrupt = source.clone();
    corrupt.hits[0].chunk.text.push('!');
    assert!(
        SearchResult::verify_selection_all(std::slice::from_ref(&corrupt), selection.clone())
            .is_err()
    );
    assert!(SearchResult::verify_selection_all(&[source, corrupt], selection).is_err());
    assert!(serde_json::from_value::<SourceSelectionReport>(serde_json::json!({"text":"x","sources":[],"quote":"model quote must not be silently ignored"})).is_err());
}
#[test]
fn source_body_selection_is_bounded_before_host_quote_allocation() {
    let source = evidence("large", &"a".repeat(1024 * 1024 + 1), 1);
    let id = source.hits[0].chunk.id.clone();
    assert!(SearchResult::verify_selection_all(
        &[source],
        SourceSelectionReport {
            text: format!("[[{id}]]"),
            sources: vec![id]
        }
    )
    .is_err());
}

#[test]
fn explicit_host_bibliography_renders_sources_without_forgiving_invalid_model_markers() {
    let source = evidence(
        "a",
        "The exact body contains\na wrapped source sentence.",
        1,
    );
    let id = source.hits[0].chunk.id.clone();
    let original = SourceSelectionReport {
        text: "A summary with answer-level selected evidence.".into(),
        sources: vec![id.clone()],
    };
    assert!(
        SearchResult::verify_selection_all(std::slice::from_ref(&source), original.clone())
            .is_err()
    );
    let rendered = SearchResult::render_selection_bibliography_all(
        std::slice::from_ref(&source),
        original.clone(),
    )
    .unwrap();
    assert!(rendered.report.text.starts_with(&original.text));
    assert!(rendered
        .report
        .text
        .contains("Source list rendered by the host"));
    assert!(rendered.report.text.contains(&format!("[[{id}]]")));
    assert_eq!(
        rendered.report.citations[0].quote,
        source.hits[0].chunk.text
    );
    for marker in ["", "  ", "[[unknown]]", "[[1]]", "[[unclosed"] {
        assert!(SearchResult::render_selection_bibliography_all(
            std::slice::from_ref(&source),
            SourceSelectionReport {
                text: marker.into(),
                sources: vec![id.clone()]
            }
        )
        .is_err());
    }
    let mut duplicate = original;
    duplicate.sources.push(id);
    assert!(SearchResult::render_selection_bibliography_all(&[source], duplicate).is_err());
}
