#![cfg(feature = "research")]
use std::path::PathBuf;
use taskloom::runtime::rag::*;

fn config(executable: PathBuf) -> PdfConfig {
    PdfConfig {
        executable,
        extractor_id: "explicit-poppler-test-binary".into(),
        max_input_bytes: 1024 * 1024,
        max_text_bytes: 1024 * 1024,
        max_pages: 10,
        timeout_ms: 5000,
    }
}
fn fixture_pdf() -> Vec<u8> {
    let first = "BT /F1 12 Tf 72 720 Td (Research approval evidence.) Tj ET";
    let second = "BT /F1 12 Tf 72 720 Td (Recovery requires observed tool results.) Tj ET";
    let objects=["<< /Type /Catalog /Pages 2 0 R >>".to_string(),"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".into(),"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 7 0 R >> >> /Contents 5 0 R >>".into(),"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 7 0 R >> >> /Contents 6 0 R >>".into(),format!("<< /Length {} >>\nstream\n{first}\nendstream",first.len()),format!("<< /Length {} >>\nstream\n{second}\nendstream",second.len()),"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into()];
    let mut pdf = "%PDF-1.4\n".to_string();
    let mut offsets = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.push_str(&format!("{} 0 obj\n{object}\nendobj\n", i + 1));
    }
    let xref = pdf.len();
    pdf.push_str("xref\n0 8\n0000000000 65535 f \n");
    for offset in offsets {
        pdf.push_str(&format!("{offset:010} 00000 n \n"));
    }
    pdf.push_str(&format!(
        "trailer\n<< /Size 8 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
    ));
    pdf.into_bytes()
}
#[test]
fn extractor_is_explicit_and_configuration_is_bounded() {
    assert!(PdfLoader::new(config("pdftotext".into())).is_err());
    let mut cfg = config(std::env::current_exe().unwrap());
    cfg.timeout_ms = 0;
    assert!(PdfLoader::new(cfg).is_err());
}
#[tokio::test]
async fn invalid_or_oversized_pdf_is_rejected_without_starting_extractor() {
    let mut cfg = config(std::env::current_exe().unwrap());
    cfg.max_input_bytes = 32;
    let loader = PdfLoader::new(cfg).unwrap();
    assert!(loader
        .load_bytes(b"not a pdf".to_vec(), "a", "a", "fixture:a")
        .await
        .is_err());
    assert!(loader
        .load_bytes(fixture_pdf(), "a", "a", "fixture:a")
        .await
        .is_err());
}
/// Run explicitly with TASKLOOM_TEST_PDFTOTEXT=/absolute/path/to/pdftotext.
/// Absence is reported as ignored, never as successful parser validation.
#[tokio::test]
#[ignore = "requires an explicitly configured trusted Poppler pdftotext binary"]
async fn actual_poppler_extracts_two_pages_with_source_hash_and_enforces_limits() {
    let executable = PathBuf::from(
        std::env::var("TASKLOOM_TEST_PDFTOTEXT").expect("set TASKLOOM_TEST_PDFTOTEXT"),
    );
    let bytes = fixture_pdf();
    let loader = PdfLoader::new(config(executable.clone())).unwrap();
    let document = loader
        .load_bytes(
            bytes.clone(),
            "research",
            "Research PDF",
            "fixture:research.pdf",
        )
        .await
        .unwrap();
    assert!(document.text.contains("Research approval evidence."));
    assert!(document
        .text
        .contains("Recovery requires observed tool results."));
    assert_eq!(document.metadata["pages"], "2");
    assert_eq!(document.metadata["source_sha256"].len(), 64);
    let dir = tempfile::tempdir().unwrap();
    let store = FileCorpusStore::new(dir.path());
    ingest(&store, document, &WindowChunker::default(), Some(0))
        .await
        .unwrap();
    let evidence = search(&store, &LexicalRetriever, &RetrievalQuery::new("approval"))
        .await
        .unwrap();
    assert_eq!(evidence.hits.len(), 1);
    let mut cfg = config(executable.clone());
    cfg.max_pages = 1;
    assert!(PdfLoader::new(cfg)
        .unwrap()
        .load_bytes(bytes.clone(), "a", "a", "fixture:a")
        .await
        .is_err());
    let mut cfg = config(executable);
    cfg.max_text_bytes = 8;
    assert!(PdfLoader::new(cfg)
        .unwrap()
        .load_bytes(bytes, "a", "a", "fixture:a")
        .await
        .is_err());
}
