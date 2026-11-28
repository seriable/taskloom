#![cfg(feature = "office")]
use std::io::{Cursor, Write};
use taskloom::runtime::rag::*;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

const TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
const PREFIX: &str = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#;
fn main_xml(body: &str) -> String {
    format!("{PREFIX}{body}</w:body></w:document>")
}
fn docx(xml: &str, extras: &[(&str, &[u8])], compression: CompressionMethod) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(compression);
    for (name, bytes) in [
        ("[Content_Types].xml", TYPES.as_bytes()),
        ("word/document.xml", xml.as_bytes()),
    ]
    .into_iter()
    .chain(extras.iter().copied())
    {
        writer.start_file(name, options).unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}
#[tokio::test]
async fn actual_docx_unicode_paragraph_table_and_entities_preserve_extracted_citations() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.docx");
    let xml=main_xml("<w:p><w:r><w:t>标题 &amp; 内容</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>alpha beta</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>安全审批</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:t>link text &lt;script&gt;</w:t></w:r></w:p>");
    let bytes=docx(&xml,&[("word/_rels/document.xml.rels",br#"<Relationships><Relationship Target="http://127.0.0.1:9/never-fetched" TargetMode="External"/></Relationships>"#)],CompressionMethod::Deflated);
    std::fs::write(&path, &bytes).unwrap();
    let document = DocxLoader::default()
        .load(&path, "docx", "Word policy")
        .await
        .unwrap();
    assert_eq!(
        document.text,
        "标题 & 内容\nalpha beta\t安全审批\nlink text <script>"
    );
    assert!(document
        .source
        .contains("#taskloom-docx-extracted-v1;archive-sha256="));
    assert!(document.metadata["citation_offsets"].contains("not ZIP/XML"));
    let store = FileCorpusStore::new(directory.path().join("corpus"));
    let snapshot = ingest(&store, document.clone(), &WindowChunker::default(), Some(0))
        .await
        .unwrap();
    for chunk in &snapshot.chunks {
        assert_eq!(&document.text[chunk.start..chunk.end], chunk.text);
    }
    let found = search(&store, &LexicalRetriever, &RetrievalQuery::new("安全审批"))
        .await
        .unwrap();
    let chunk = &found.hits[0].chunk;
    assert!(found
        .verify(CitedReport {
            text: format!("[[{}]]", chunk.id),
            citations: vec![Citation {
                chunk_id: chunk.id.clone(),
                quote: "标题 & 内容\nalpha beta\t安全审批".into()
            }]
        })
        .is_ok());
    assert_eq!(
        document,
        DocxLoader::default()
            .load(&path, "docx", "Word policy")
            .await
            .unwrap()
    );
}
#[tokio::test]
async fn zip_paths_symlinks_macro_parts_and_duplicate_case_paths_are_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.docx");
    let xml = main_xml("<w:p><w:r><w:t>safe text</w:t></w:r></w:p>");
    for name in [
        "../escape",
        "/absolute",
        "word/../../escape",
        "word\\evil",
        "C:drive",
        "word/vbaProject.bin",
        "word/activeX/item.xml",
        "word/embeddings/ole.bin",
        "WORD/document.xml",
    ] {
        let bytes = docx(&xml, &[(name, b"data")], CompressionMethod::Stored);
        std::fs::write(&path, bytes).unwrap();
        assert!(
            DocxLoader::default()
                .load(&path, "docx", "Word")
                .await
                .is_err(),
            "{name}"
        );
    }
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default();
    writer.start_file("[Content_Types].xml", options).unwrap();
    writer.write_all(TYPES.as_bytes()).unwrap();
    writer.start_file("word/document.xml", options).unwrap();
    writer.write_all(xml.as_bytes()).unwrap();
    writer
        .add_symlink("word/linked", "../../outside", options)
        .unwrap();
    std::fs::write(&path, writer.finish().unwrap().into_inner()).unwrap();
    assert!(DocxLoader::default()
        .load(&path, "docx", "Word")
        .await
        .is_err());
}
#[tokio::test]
async fn xml_dtd_unknown_entities_invalid_namespaces_depth_and_unused_corruption_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.docx");
    let safe = main_xml("<w:p><w:r><w:t>safe text</w:t></w:r></w:p>");
    for xml in [
        format!("<!DOCTYPE w:document [<!ENTITY x SYSTEM 'file:///not-read'>]>{safe}"),
        main_xml("<w:p><w:r><w:t>&unknown;</w:t></w:r></w:p>"),
        safe.replace(
            "http://schemas.openxmlformats.org/wordprocessingml/2006/main",
            "https://untrusted.invalid/namespace",
        ),
        safe.replace("</w:document>", ""),
        format!("{safe}{safe}"),
    ] {
        std::fs::write(&path, docx(&xml, &[], CompressionMethod::Deflated)).unwrap();
        assert!(DocxLoader::default()
            .load(&path, "docx", "Word")
            .await
            .is_err());
    }
    std::fs::write(
        &path,
        docx(
            &safe,
            &[(
                "word/styles.xml",
                b"<!DOCTYPE styles [<!ENTITY x SYSTEM 'https://no-fetch.invalid'>]><styles/>",
            )],
            CompressionMethod::Deflated,
        ),
    )
    .unwrap();
    assert!(DocxLoader::default()
        .load(&path, "docx", "Word")
        .await
        .is_err());
    std::fs::write(&path, docx(&safe, &[], CompressionMethod::Stored)).unwrap();
    let loader = DocxLoader {
        limits: DocxLimits {
            max_xml_depth: 4,
            ..Default::default()
        },
    };
    assert!(loader.load(&path, "docx", "Word").await.is_err());
}
#[tokio::test]
async fn compressed_uncompressed_entry_count_and_crc_limits_are_enforced() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.docx");
    let xml = main_xml(&format!(
        "<w:p><w:r><w:t>{}</w:t></w:r></w:p>",
        "text ".repeat(1000)
    ));
    let bytes = docx(&xml, &[], CompressionMethod::Deflated);
    std::fs::write(&path, &bytes).unwrap();
    for limits in [
        DocxLimits {
            max_archive_bytes: bytes.len() - 1,
            ..Default::default()
        },
        DocxLimits {
            max_entry_bytes: 128,
            ..Default::default()
        },
        DocxLimits {
            max_uncompressed_bytes: 256,
            ..Default::default()
        },
        DocxLimits {
            max_entries: 1,
            ..Default::default()
        },
    ] {
        assert!(DocxLoader { limits }
            .load(&path, "docx", "Word")
            .await
            .is_err());
    }
    let mut bytes = docx(&xml, &[], CompressionMethod::Stored);
    let offset = {
        let mut archive = ZipArchive::new(Cursor::new(&bytes)).unwrap();
        let offset = archive.by_name("word/document.xml").unwrap().data_start() as usize;
        offset
    };
    bytes[offset + 10] ^= 1;
    std::fs::write(&path, bytes).unwrap();
    assert!(DocxLoader::default()
        .load(&path, "docx", "Word")
        .await
        .unwrap_err()
        .to_string()
        .contains("CRC"));
}
#[tokio::test]
async fn archive_provenance_changes_even_when_extracted_text_is_equal_and_bad_input_keeps_corpus() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.docx");
    let xml = main_xml("<w:p><w:r><w:t>stable extracted policy</w:t></w:r></w:p>");
    let loader = DocxLoader::default();
    std::fs::write(
        &path,
        docx(
            &xml,
            &[("word/media/image.bin", b"image-one")],
            CompressionMethod::Stored,
        ),
    )
    .unwrap();
    let first = loader.load(&path, "docx", "Word").await.unwrap();
    let store = FileCorpusStore::new(directory.path().join("corpus"));
    let before = ingest(&store, first.clone(), &WindowChunker::default(), Some(0))
        .await
        .unwrap();
    std::fs::write(
        &path,
        docx(
            &xml,
            &[("word/media/image.bin", b"image-two")],
            CompressionMethod::Stored,
        ),
    )
    .unwrap();
    let second = loader.load(&path, "docx", "Word").await.unwrap();
    assert_eq!(first.text, second.text);
    assert_ne!(first.source, second.source);
    assert_ne!(
        Chunk::from_range(&first, 0, first.text.len()).unwrap().id,
        Chunk::from_range(&second, 0, second.text.len()).unwrap().id
    );
    std::fs::write(&path, b"not a DOCX ZIP").unwrap();
    assert!(loader.load(&path, "docx", "Word").await.is_err());
    assert_eq!(
        serde_json::to_value(before).unwrap(),
        serde_json::to_value(store.snapshot().await.unwrap()).unwrap()
    );
}
