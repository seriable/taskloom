#![cfg(feature = "runtime")]
use sha2::{Digest, Sha256};
use taskloom::runtime::rag::*;

#[tokio::test]
async fn structured_formats_preserve_raw_utf8_and_exact_byte_offsets_end_to_end() {
    let directory = tempfile::tempdir().unwrap();
    let store = FileCorpusStore::new(directory.path().join("corpus"));
    let cases:[(&str,&str,&dyn DocumentLoader);3]=[
        ("table.csv","name,note\r\n\"张三\",\"restart policy requires approval\r\nand review\"\r\n",&CsvLoader::default()),
        ("records.jsonl","{\"name\":\"张三\",\"note\":\"restart policy requires approval\"}\n",&JsonlLoader::default()),
        ("message.eml","Subject: restart policy\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n张三: restart policy requires approval\r\n",&EmlLoader::default()),
    ];
    for (index, (name, text, loader)) in cases.into_iter().enumerate() {
        let path = directory.path().join(name);
        std::fs::write(&path, text.as_bytes()).unwrap();
        let document = loader.load(&path, name, name).await.unwrap();
        assert_eq!(document.text, text);
        assert_eq!(
            document.metadata["source_sha256"],
            hex::encode(Sha256::digest(text.as_bytes()))
        );
        assert_eq!(document, loader.load(&path, name, name).await.unwrap());
        let snapshot = ingest(
            &store,
            document.clone(),
            &WindowChunker::default(),
            Some(index as u64),
        )
        .await
        .unwrap();
        for chunk in snapshot.chunks.iter().filter(|c| c.document_id == name) {
            assert_eq!(&text[chunk.start..chunk.end], chunk.text);
            chunk.validate(&document).unwrap();
        }
    }
    let found = search(
        &store,
        &LexicalRetriever,
        &RetrievalQuery::new("restart policy requires approval"),
    )
    .await
    .unwrap();
    assert_eq!(found.hits.len(), 3);
    for hit in &found.hits {
        let chunk = &hit.chunk;
        let quote = "restart policy requires approval";
        assert!(found
            .verify(CitedReport {
                text: format!("[[{}]]", chunk.id),
                citations: vec![Citation {
                    chunk_id: chunk.id.clone(),
                    quote: quote.into()
                }]
            })
            .is_ok());
    }
}
#[tokio::test]
async fn csv_validates_escaped_multiline_fields_width_and_strict_quotes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.csv");
    for text in [
        "a,b\n1,\"two, values\"\n",
        "a,b\r\n1,\"quote \"\"inside\"\"\nnext line\"\r\n",
    ] {
        std::fs::write(&path, text).unwrap();
        assert_eq!(
            CsvLoader::default()
                .load(&path, "csv", "CSV")
                .await
                .unwrap()
                .metadata["records"],
            "2"
        );
    }
    for text in [
        "a,b\n1\n",
        "a,b\n1,\"open",
        "a,b\n1,\"closed\"junk",
        "a,b\r1,2",
        "a,b\n1,qu\"ote",
        "a,\0",
    ] {
        std::fs::write(&path, text).unwrap();
        assert!(
            CsvLoader::default()
                .load(&path, "csv", "CSV")
                .await
                .is_err(),
            "{text:?}"
        );
    }
}
#[tokio::test]
async fn jsonl_rejects_incomplete_empty_scalar_records_and_honors_bounds() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.jsonl");
    for text in [
        "{\"ok\":true}\n{",
        "{\"ok\":true}\n\n",
        "null\n",
        "[1,2]\n",
        "",
    ] {
        std::fs::write(&path, text).unwrap();
        assert!(JsonlLoader::default()
            .load(&path, "jsonl", "JSONL")
            .await
            .is_err());
    }
    let loader = JsonlLoader {
        limits: StructuredTextLimits {
            max_bytes: 128,
            max_records: 1,
            max_fields: 1,
        },
    };
    for text in [
        "{\"a\":1,\"b\":2}",
        "{\"a\":1}\n{\"b\":2}",
        &format!("{{\"a\":\"{}\"}}", "x".repeat(129)),
    ] {
        std::fs::write(&path, text).unwrap();
        assert!(loader.load(&path, "jsonl", "JSONL").await.is_err());
    }
    std::fs::write(&path, [0xff, 0xfe]).unwrap();
    assert!(loader.load(&path, "jsonl", "JSONL").await.is_err());
}
#[tokio::test]
async fn email_never_silently_decodes_attachments_html_or_encoded_mime() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.eml");
    for text in [
        "Subject: incomplete",
        " badfold\n\nbody",
        "Content-Type: text/html\n\n<script>alert(1)</script>",
        "Content-Type: multipart/mixed; boundary=abc\n\nattachment",
        "Content-Type: text/plain; charset=gbk\n\nbody",
        "Content-Transfer-Encoding: base64\n\nYm9keQ==",
        "Content-Type: text/plain; charset=us-ascii\n\n非ASCII",
        "Content-Transfer-Encoding: 7bit\n\n非ASCII",
        "Subject: one\nSubject: duplicate\n\nbody",
    ] {
        std::fs::write(&path, text).unwrap();
        assert!(
            EmlLoader::default()
                .load(&path, "mail", "Mail")
                .await
                .is_err(),
            "{text:?}"
        );
    }
    let text = "Subject: safe\r\n continuation\r\n\r\nhttps://example.invalid =SHELL() remain data";
    std::fs::write(&path, text).unwrap();
    assert_eq!(
        EmlLoader::default()
            .load(&path, "mail", "Mail")
            .await
            .unwrap()
            .text,
        text
    );
}
#[tokio::test]
async fn rejected_source_never_replaces_existing_corpus_and_policy_limits_validate() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.csv");
    let store = FileCorpusStore::new(directory.path().join("corpus"));
    std::fs::write(&path, "field,value\npolicy,approved\n").unwrap();
    let document = CsvLoader::default()
        .load(&path, "csv", "CSV")
        .await
        .unwrap();
    let before = ingest(&store, document, &WindowChunker::default(), Some(0))
        .await
        .unwrap();
    std::fs::write(&path, "field,value\nwrongwidth").unwrap();
    assert!(CsvLoader::default()
        .load(&path, "csv", "CSV")
        .await
        .is_err());
    let after = store.snapshot().await.unwrap();
    assert_eq!(
        serde_json::to_value(before).unwrap(),
        serde_json::to_value(after).unwrap()
    );
    let loader = CsvLoader {
        limits: StructuredTextLimits {
            max_bytes: 0,
            ..Default::default()
        },
    };
    assert!(loader.load(&path, "csv", "CSV").await.is_err());
}
