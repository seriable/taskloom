#![cfg(feature = "runtime")]
use serde_json::json;
use taskloom::runtime::rag::*;

fn evidence(id: &str, text: &str, start: usize, revision: u64) -> SearchResult {
    let document = Document {
        id: id.into(),
        title: "Source title".into(),
        source: format!("fixture:{id}"),
        text: text.into(),
        metadata: Default::default(),
    };
    SearchResult {
        corpus_revision: revision,
        hits: vec![Evidence {
            chunk: Chunk::from_range(&document, start, text.len()).unwrap(),
            score: 1.0,
        }],
        skipped_oversize: 0,
    }
}
fn located(result: &SearchResult) -> LocatedCitation {
    let chunk = &result.hits[0].chunk;
    LocatedCitation {
        document_id: chunk.document_id.clone(),
        corpus_revision: result.corpus_revision,
        start: chunk.start,
        quote: chunk.text.clone(),
    }
}
fn report(result: &SearchResult) -> LocatedReport {
    LocatedReport {
        text: "Evidence [[1]].".into(),
        citations: vec![located(result)],
    }
}

#[test]
fn exact_multilingual_locators_preserve_original_and_render_only_markers() {
    let a = evidence(
        "approval",
        "Approval requires the exact operation arguments.",
        0,
        7,
    );
    let b = evidence("恢复", "前缀：未知副作用需要人工核对。", "前缀：".len(), 9);
    let input = LocatedReport {
        text: "Approval [[1]]，恢复 [[2]]；再次引用 [[1]].".into(),
        citations: vec![located(&a), located(&b)],
    };
    let original = input.clone();
    // Repeated reads of the same snapshot are harmless.
    let resolved =
        SearchResult::verify_located_all(&[a.clone(), b.clone(), a.clone()], input).unwrap();
    assert_eq!(resolved.original, original);
    assert_eq!(resolved.mappings[1].source.start, 9); // bytes, not character offset
    assert_eq!(resolved.verified.corpus_revision, None);
    assert_eq!(resolved.verified.corpus_revisions, [7, 9]);
    assert_eq!(
        resolved.verified.report.text,
        format!(
            "Approval [[{}]]，恢复 [[{}]]；再次引用 [[{}]].",
            a.hits[0].chunk.id, b.hits[0].chunk.id, a.hits[0].chunk.id
        )
    );
    assert_eq!(
        resolved.verified.report.citations[1].quote,
        original.citations[1].quote
    );
}

#[test]
fn wrong_locator_quote_and_duplicate_source_never_receive_fuzzy_repair() {
    let e = evidence(
        "approval",
        "Approval requires the exact operation arguments.",
        0,
        7,
    );
    let input = report(&e);
    for invalid in [
        LocatedCitation {
            document_id: "Approval".into(),
            ..located(&e)
        },
        LocatedCitation {
            corpus_revision: 6,
            ..located(&e)
        },
        LocatedCitation {
            start: 1,
            ..located(&e)
        },
        LocatedCitation {
            quote: "Automatically approve everything.".into(),
            ..located(&e)
        },
        LocatedCitation {
            quote: "the".into(),
            ..located(&e)
        },
    ] {
        assert!(SearchResult::verify_located_all(
            std::slice::from_ref(&e),
            LocatedReport {
                citations: vec![invalid],
                ..input.clone()
            }
        )
        .is_err());
    }
    let mut duplicate = input;
    duplicate.text.push_str(" [[2]]");
    duplicate.citations.push(duplicate.citations[0].clone());
    assert!(SearchResult::verify_located_all(&[e], duplicate).is_err());
}

#[test]
fn ordinal_contract_rejects_missing_noncanonical_unknown_and_hash_markers() {
    let e = evidence(
        "approval",
        "Approval requires the exact operation arguments.",
        0,
        7,
    );
    for text in [
        "missing",
        "[[0]]",
        "[[2]]",
        "[[01]]",
        "[[+1]]",
        "[[ 1]]",
        "[[1",
        "[[1]][[invented]]",
        &format!("[[{}]]", e.hits[0].chunk.id),
    ] {
        assert!(
            SearchResult::verify_located_all(
                std::slice::from_ref(&e),
                LocatedReport {
                    text: text.into(),
                    ..report(&e)
                }
            )
            .is_err(),
            "{text}"
        );
    }
}

#[test]
fn ambiguous_source_is_rejected_even_when_only_one_candidate_contains_quote() {
    let a = evidence(
        "approval",
        "Approval requires the exact operation arguments.",
        0,
        7,
    );
    let b = evidence(
        "approval",
        "A different source under the same document and revision.",
        0,
        7,
    );
    assert!(
        SearchResult::verify_located_all(&[a.clone(), b], report(&a))
            .unwrap_err()
            .to_string()
            .contains("ambiguous")
    );
    let mut corrupt = a.clone();
    corrupt.hits[0].chunk.source = "forged".into();
    assert!(SearchResult::verify_located_all(&[a.clone(), corrupt.clone()], report(&a)).is_err());
    assert!(SearchResult::verify_located_all(&[corrupt], report(&a)).is_err());
}

#[test]
fn locator_fields_are_strict_and_legacy_input_is_not_implicitly_migrated() {
    let e = evidence(
        "approval",
        "Approval requires the exact operation arguments.",
        0,
        0,
    );
    let input = report(&e);
    assert!(SearchResult::verify_located_all(std::slice::from_ref(&e), input.clone()).is_ok());
    assert!(serde_json::from_value::<CitedReport>(serde_json::to_value(&input).unwrap()).is_err());
    let mut wire = serde_json::to_value(&input).unwrap();
    wire["citations"][0]["chunk_id"] = json!(e.hits[0].chunk.id);
    assert!(serde_json::from_value::<LocatedReport>(wire).is_err());
    let mut wire = serde_json::to_value(input).unwrap();
    wire["sources"] = json!([]);
    assert!(serde_json::from_value::<LocatedReport>(wire).is_err());
}
