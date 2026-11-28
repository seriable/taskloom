#![cfg(feature = "office")]
use std::io::{Cursor, Write};
use taskloom::runtime::rag::*;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

const S: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const P: &str = "http://schemas.openxmlformats.org/presentationml/2006/main";
const A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const CT: &str = "http://schemas.openxmlformats.org/package/2006/content-types";
const PR: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
type Parts = Vec<(String, Vec<u8>)>;
fn add(parts: &mut Parts, name: &str, text: &str) {
    parts.push((name.into(), text.as_bytes().to_vec()));
}
fn replace(parts: &mut Parts, name: &str, from: &str, to: &str) {
    let bytes = &mut parts.iter_mut().find(|p| p.0 == name).unwrap().1;
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(text.contains(from));
    *bytes = text.replace(from, to).into_bytes();
}
fn rels(entries: &[(&str, &str, &str)]) -> String {
    format!(
        "<Relationships xmlns=\"{PR}\">{}</Relationships>",
        entries
            .iter()
            .map(|(id, kind, target)| format!(
                "<Relationship Id=\"{id}\" Type=\"{R}/{kind}\" Target=\"{target}\"/>"
            ))
            .collect::<String>()
    )
}
fn types(entries: &[(&str, &str)]) -> String {
    format!("<Types xmlns=\"{CT}\">{}</Types>", entries.iter().map(|(name,kind)|
        format!("<Override PartName=\"/{name}\" ContentType=\"application/vnd.openxmlformats-officedocument.{kind}+xml\"/>")).collect::<String>())
}
fn xlsx_parts() -> Parts {
    let mut p = Vec::new();
    add(
        &mut p,
        "[Content_Types].xml",
        &types(&[
            ("xl/workbook.xml", "spreadsheetml.sheet.main"),
            ("xl/worksheets/sheet7.xml", "spreadsheetml.worksheet"),
            ("xl/worksheets/sheet1.xml", "spreadsheetml.worksheet"),
            ("xl/sharedStrings.xml", "spreadsheetml.sharedStrings"),
        ]),
    );
    add(
        &mut p,
        "_rels/.rels",
        &rels(&[("doc", "officeDocument", "xl/workbook.xml")]),
    );
    add(&mut p,"xl/workbook.xml",&format!("<workbook xmlns=\"{S}\" xmlns:r=\"{R}\"><sheets><sheet name=\"政策 &amp; Prices\" sheetId=\"7\" r:id=\"b\"/><sheet name=\"Hidden\" sheetId=\"1\" state=\"hidden\" r:id=\"a\"/></sheets></workbook>"));
    add(
        &mut p,
        "xl/_rels/workbook.xml.rels",
        &rels(&[
            ("a", "worksheet", "worksheets/sheet1.xml"),
            ("b", "worksheet", "worksheets/sheet7.xml"),
            ("ss", "sharedStrings", "sharedStrings.xml"),
        ]),
    );
    add(&mut p,"xl/sharedStrings.xml",&format!("<sst xmlns=\"{S}\"><si><t>中文 &amp; policy</t></si><si><r><t>rich </t></r><r><t>text</t></r></si></sst>"));
    add(&mut p,"xl/worksheets/sheet7.xml",&format!("<worksheet xmlns=\"{S}\"><sheetData><row r=\"2\"><c r=\"A2\" t=\"s\"><v>0</v></c><c r=\"C2\" t=\"inlineStr\"><is><t>line&#10;[sheet fake]</t></is></c><c r=\"XFD2\"><v>45200</v></c></row></sheetData></worksheet>"));
    add(&mut p,"xl/worksheets/sheet1.xml",&format!("<worksheet xmlns=\"{S}\"><sheetData><row r=\"1\"><c r=\"B1\" t=\"s\"><v>1</v></c></row></sheetData></worksheet>"));
    p
}
fn pptx_parts() -> Parts {
    let mut p = Vec::new();
    add(
        &mut p,
        "[Content_Types].xml",
        &types(&[
            ("ppt/presentation.xml", "presentationml.presentation.main"),
            ("ppt/slides/slide1.xml", "presentationml.slide"),
            ("ppt/slides/slide2.xml", "presentationml.slide"),
        ]),
    );
    add(
        &mut p,
        "_rels/.rels",
        &rels(&[("doc", "officeDocument", "ppt/presentation.xml")]),
    );
    add(&mut p,"ppt/presentation.xml",&format!("<p:presentation xmlns:p=\"{P}\" xmlns:r=\"{R}\"><p:sldIdLst><p:sldId id=\"257\" r:id=\"second\"/><p:sldId id=\"256\" r:id=\"first\"/></p:sldIdLst></p:presentation>"));
    add(
        &mut p,
        "ppt/_rels/presentation.xml.rels",
        &rels(&[
            ("first", "slide", "slides/slide1.xml"),
            ("second", "slide", "slides/slide2.xml"),
        ]),
    );
    add(&mut p,"ppt/slides/slide2.xml",&slide("<p:sp><p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>先读中文 &amp; </a:t></a:r><a:r><a:t>policy</a:t></a:r><a:br/><a:r><a:t>next</a:t></a:r></a:p></p:txBody></p:sp><p:graphicFrame><a:graphic><a:graphicData><a:tbl><a:tblGrid><a:gridCol w=\"10\"/><a:gridCol w=\"10\"/></a:tblGrid><a:tr h=\"10\"><a:tc><a:txBody><a:p><a:r><a:t>左</a:t></a:r></a:p></a:txBody><a:tcPr/></a:tc><a:tc><a:txBody><a:p><a:r><a:t>右</a:t></a:r></a:p></a:txBody><a:tcPr/></a:tc></a:tr></a:tbl></a:graphicData></a:graphic></p:graphicFrame>"));
    add(&mut p,"ppt/slides/slide1.xml",&slide("<p:grpSp><p:sp><p:txBody><a:p><a:r><a:t>后读 slide1</a:t></a:r></a:p></p:txBody></p:sp></p:grpSp>"));
    p
}
fn slide(body: &str) -> String {
    format!("<p:sld xmlns:p=\"{P}\" xmlns:a=\"{A}\"><p:cSld><p:spTree>{body}</p:spTree></p:cSld></p:sld>")
}
fn archive(parts: &Parts, compression: CompressionMethod) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in parts {
        zip.start_file(
            name,
            SimpleFileOptions::default().compression_method(compression),
        )
        .unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}
async fn parse(parts: &Parts, pptx: bool) -> Result<Document, taskloom::runtime::RuntimeError> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.office");
    std::fs::write(&path, archive(parts, CompressionMethod::Deflated)).unwrap();
    if pptx {
        PptxLoader::default()
            .load(&path, "office", "Office fixture")
            .await
    } else {
        XlsxLoader::default()
            .load(&path, "office", "Office fixture")
            .await
    }
}

#[tokio::test]
async fn xlsx_real_zip_resolves_book_order_sparse_coordinates_shared_and_inline_text() {
    let mut parts = xlsx_parts();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    add(&mut parts,"xl/worksheets/_rels/sheet7.xml.rels",&format!("<Relationships xmlns=\"{PR}\"><Relationship Id=\"url\" Type=\"{R}/hyperlink\" Target=\"http://{}/not-fetched\" TargetMode=\"External\"/></Relationships>",listener.local_addr().unwrap()));
    let document = parse(&parts, false).await.unwrap();
    assert!(!document.text.starts_with("[sheet 1 name=\"政策 &amp;"));
    assert!(document.text.starts_with("[sheet 1 name=\"政策 & Prices\""));
    assert!(document
        .text
        .contains("row=2 cell=A2 type=s value=\"中文 & policy\""));
    assert!(document
        .text
        .contains("cell=C2 type=inlineStr value=\"line\\n[sheet fake]\""));
    assert!(document.text.contains("cell=XFD2 type=n value=\"45200\""));
    assert!(document.text.find("sheet7.xml").unwrap() < document.text.find("sheet1.xml").unwrap());
    assert!(document.text.contains("state=\"hidden\""));
    assert!(document.text.contains("rich text"));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert!(document.metadata["citation_offsets"].contains("not ZIP/XML"));
    assert_eq!(document.metadata["extraction_policy_sha256"].len(), 64);
}
#[tokio::test]
async fn pptx_real_zip_resolves_presentation_order_groups_paragraphs_and_tables() {
    let document = parse(&pptx_parts(), true).await.unwrap();
    assert!(document
        .text
        .starts_with("[slide 1 id=257 part=\"ppt/slides/slide2.xml\""));
    assert!(document
        .text
        .contains("shape=1 paragraph=1 text=\"先读中文 & policy\\nnext\""));
    assert!(document
        .text
        .contains("shape=2 table_row=1 table_cell=1 text=\"左\""));
    assert!(document
        .text
        .contains("shape=2 table_row=1 table_cell=2 text=\"右\""));
    assert!(document.text.find("先读").unwrap() < document.text.find("后读").unwrap());
}
#[tokio::test]
async fn formulas_bad_references_duplicate_cells_and_external_structural_targets_fail_closed() {
    let mut cases = Vec::new();
    for (from, to) in [
        (
            "<v>45200</v>",
            "<f>WEBSERVICE(&quot;http://not-executed&quot;)</f><v>45200</v>",
        ),
        ("<v>0</v>", "<v>999</v>"),
        ("r=\"C2\"", "r=\"A2\""),
        ("r=\"C2\"", "r=\"C3\""),
        ("r=\"XFD2\"", "r=\"XFE2\""),
        ("<v>45200</v>", "<v>NaN</v>"),
    ] {
        let mut p = xlsx_parts();
        replace(&mut p, "xl/worksheets/sheet7.xml", from, to);
        cases.push(p);
    }
    let mut p = xlsx_parts();
    replace(
        &mut p,
        "xl/_rels/workbook.xml.rels",
        "Target=\"worksheets/sheet7.xml\"",
        "Target=\"https://never-fetched.invalid/a.xml\" TargetMode=\"External\"",
    );
    cases.push(p);
    let mut p = xlsx_parts();
    replace(
        &mut p,
        "xl/_rels/workbook.xml.rels",
        "worksheets/sheet7.xml",
        "../../outside",
    );
    cases.push(p);
    let mut p = xlsx_parts();
    replace(&mut p, "xl/workbook.xml", "r:id=\"b\"", "r:id=\"a\"");
    cases.push(p);
    for (i, p) in cases.iter().enumerate() {
        assert!(parse(p, false).await.is_err(), "case {i}");
    }
    let mut p = pptx_parts();
    replace(
        &mut p,
        "ppt/slides/slide2.xml",
        "<a:r><a:t>next</a:t></a:r>",
        "<a:fld><a:t>cached date</a:t></a:fld>",
    );
    assert!(parse(&p, true).await.is_err());
    let mut p = pptx_parts();
    replace(
        &mut p,
        "ppt/presentation.xml",
        "r:id=\"second\"",
        "r:id=\"first\"",
    );
    assert!(parse(&p, true).await.is_err());
}
#[tokio::test]
async fn every_xml_part_rejects_dtd_unknown_entities_malformed_namespaces_and_incomplete_xml() {
    for bad in [
        "<!DOCTYPE unused [<!ENTITY x SYSTEM 'file:///never-read'>]><unused/>",
        "<unused>&unknown;</unused>",
        "<unused>",
        "<a/><b/>",
        "<unbound:a/>",
        "<a x='1' x='2'/>",
        "<a xmlns:x='urn:test' xmlns:y='urn:test' x:a='1' y:a='2'/>",
        "<a>&#0;</a>",
        "<?xml version='1.0' encoding='UTF-16'?><a/>",
        "<a><?run something?></a>",
    ] {
        let mut p = xlsx_parts();
        add(&mut p, "unused.xml", bad);
        assert!(parse(&p, false).await.is_err(), "{bad}");
    }
    let mut p = xlsx_parts();
    replace(&mut p, "xl/workbook.xml", S, "urn:spoof");
    assert!(parse(&p, false).await.is_err());
    // Expanded namespace resolution supports alternate prefixes without trusting local names.
    let mut p = xlsx_parts();
    replace(
        &mut p,
        "xl/workbook.xml",
        &format!("xmlns=\"{S}\""),
        &format!("xmlns:x=\"{S}\""),
    );
    for tag in ["workbook", "sheets", "sheet"] {
        replace(
            &mut p,
            "xl/workbook.xml",
            &format!("<{tag}"),
            &format!("<x:{tag}"),
        );
        if tag != "sheet" {
            replace(
                &mut p,
                "xl/workbook.xml",
                &format!("</{tag}>"),
                &format!("</x:{tag}>"),
            );
        }
    }
    assert!(parse(&p, false).await.is_ok());
}
#[tokio::test]
async fn zip_paths_macros_binary_embeddings_and_content_types_are_refused() {
    for name in [
        "../escape",
        "/absolute",
        "C:drive",
        "xl\\bad",
        "xl/%2e%2e/bad",
        "XL/workbook.xml",
        "xl/vbaProject.bin",
        "ppt/embeddings/ole.dat",
        "xl/externalLinks/link.xml",
    ] {
        let mut p = xlsx_parts();
        add(&mut p, name, "ignored");
        assert!(parse(&p, false).await.is_err(), "{name}");
    }
    let mut p = xlsx_parts();
    replace(
        &mut p,
        "[Content_Types].xml",
        "spreadsheetml.sheet.main",
        "spreadsheetml.sheet.macroEnabled.main",
    );
    assert!(parse(&p, false).await.is_err());
    let mut p = pptx_parts();
    replace(
        &mut p,
        "[Content_Types].xml",
        "presentationml.slide+xml",
        "presentationml.slideLayout+xml",
    );
    assert!(parse(&p, true).await.is_err());
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, body) in xlsx_parts() {
        zip.start_file(name, SimpleFileOptions::default()).unwrap();
        zip.write_all(&body).unwrap();
    }
    zip.add_symlink("xl/link", "../../outside", SimpleFileOptions::default())
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("link.xlsx");
    std::fs::write(&path, zip.finish().unwrap().into_inner()).unwrap();
    assert!(XlsxLoader::default()
        .load(&path, "id", "Title")
        .await
        .is_err());
}
#[tokio::test]
async fn archive_decompression_nodes_depth_units_items_and_unused_crc_are_bounded() {
    let mut p = xlsx_parts();
    add(
        &mut p,
        "unused.xml",
        &format!("<a>{}</a>", "x".repeat(50000)),
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bounded.xlsx");
    let bytes = archive(&p, CompressionMethod::Deflated);
    std::fs::write(&path, &bytes).unwrap();
    for limits in [
        OfficeLimits {
            max_archive_bytes: bytes.len() - 1,
            ..Default::default()
        },
        OfficeLimits {
            max_entries: 2,
            ..Default::default()
        },
        OfficeLimits {
            max_entry_bytes: 2048,
            ..Default::default()
        },
        OfficeLimits {
            max_uncompressed_bytes: 4096,
            ..Default::default()
        },
        OfficeLimits {
            max_xml_depth: 3,
            ..Default::default()
        },
        OfficeLimits {
            max_xml_nodes: 20,
            ..Default::default()
        },
        OfficeLimits {
            max_units: 1,
            ..Default::default()
        },
        OfficeLimits {
            max_items: 1,
            ..Default::default()
        },
    ] {
        assert!(
            XlsxLoader { limits }
                .load(&path, "id", "Title")
                .await
                .is_err(),
            "{limits:?}"
        );
    }
    let mut bytes = archive(&p, CompressionMethod::Stored);
    let offset = {
        let mut zip = ZipArchive::new(Cursor::new(&bytes)).unwrap();
        let offset = zip.by_name("unused.xml").unwrap().data_start() as usize;
        offset
    };
    bytes[offset + 10] ^= 1;
    std::fs::write(&path, bytes).unwrap();
    assert!(XlsxLoader::default()
        .load(&path, "id", "Title")
        .await
        .unwrap_err()
        .to_string()
        .contains("CRC"));
}
#[tokio::test]
async fn shared_string_amplification_hits_extracted_output_cap_without_dense_allocation() {
    let mut p = xlsx_parts();
    replace(
        &mut p,
        "xl/sharedStrings.xml",
        "中文 &amp; policy",
        &"x".repeat(100_000),
    );
    let rows = (1..=100)
        .map(|r| format!("<row r=\"{r}\"><c r=\"A{r}\" t=\"s\"><v>0</v></c></row>"))
        .collect::<String>();
    let sheet = p
        .iter_mut()
        .find(|p| p.0 == "xl/worksheets/sheet7.xml")
        .unwrap();
    sheet.1 =
        format!("<worksheet xmlns=\"{S}\"><sheetData>{rows}</sheetData></worksheet>").into_bytes();
    assert!(parse(&p, false)
        .await
        .unwrap_err()
        .to_string()
        .contains("snapshot exceeds byte"));
}

#[tokio::test]
async fn provenance_and_exact_snapshot_citations_change_on_nontext_archive_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("provenance.pptx");
    let mut p = pptx_parts();
    add(&mut p, "ppt/media/ignored.dat", "image-one");
    std::fs::write(&path, archive(&p, CompressionMethod::Stored)).unwrap();
    let first = PptxLoader::default()
        .load(&path, "deck", "Deck")
        .await
        .unwrap();
    let store = FileCorpusStore::new(dir.path().join("corpus"));
    let before = ingest(&store, first.clone(), &WindowChunker::default(), Some(0))
        .await
        .unwrap();
    let result = search(&store, &LexicalRetriever, &RetrievalQuery::new("policy"))
        .await
        .unwrap();
    let chunk = &result.hits[0].chunk;
    assert_eq!(first.text.get(chunk.start..chunk.end).unwrap(), chunk.text);
    result
        .verify(CitedReport {
            text: format!("[[{}]]", chunk.id),
            citations: vec![Citation {
                chunk_id: chunk.id.clone(),
                quote: "shape=1 paragraph=1 text=\"先读中文 & policy\\nnext\"".into(),
            }],
        })
        .unwrap();
    replace(&mut p, "ppt/media/ignored.dat", "image-one", "image-two");
    std::fs::write(&path, archive(&p, CompressionMethod::Stored)).unwrap();
    let second = PptxLoader::default()
        .load(&path, "deck", "Deck")
        .await
        .unwrap();
    assert_eq!(first.text, second.text);
    assert_ne!(first.source, second.source);
    assert_ne!(
        Chunk::from_range(&first, 0, first.text.len()).unwrap().id,
        Chunk::from_range(&second, 0, second.text.len()).unwrap().id
    );
    replace(
        &mut p,
        "ppt/slides/slide2.xml",
        "<a:t>next</a:t>",
        "<a:t>&unknown;</a:t>",
    );
    std::fs::write(&path, archive(&p, CompressionMethod::Deflated)).unwrap();
    assert!(PptxLoader::default()
        .load(&path, "deck", "Deck")
        .await
        .is_err());
    assert_eq!(
        serde_json::to_value(before).unwrap(),
        serde_json::to_value(store.snapshot().await.unwrap()).unwrap()
    );
}

#[cfg(feature = "app")]
#[tokio::test]
async fn cli_explicit_office_import_needs_no_model_secret_and_rejected_import_preserves_corpus() {
    use taskloom::runtime::app::{AppConfig, AppProvider};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cli.xlsx");
    let config_path = dir.path().join("config.json");
    let state = dir.path().join("state");
    let config = AppConfig {
        state_dir: state.clone(),
        provider: AppProvider::Openai {
            base_url: "http://127.0.0.1:9/v1".into(),
            key_env: "TASKLOOM_OFFICE_TEST_ABSENT_KEY_20261128".into(),
            namespace: "office-cold-import".into(),
        },
        ..Default::default()
    };
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let mut p = xlsx_parts();
    std::fs::write(&path, archive(&p, CompressionMethod::Deflated)).unwrap();
    let cli = |format: &str| {
        vec![
            "--config".into(),
            config_path.to_string_lossy().into_owned(),
            "ingest-office".into(),
            path.to_string_lossy().into_owned(),
            "sheet".into(),
            "Spreadsheet".into(),
            format.into(),
        ]
    };
    taskloom::runtime::app::cli::run(cli("xlsx")).await.unwrap();
    let store = FileCorpusStore::new(state.join("knowledge"));
    let before = store.snapshot().await.unwrap();
    assert_eq!(before.revision, 1);
    assert_eq!(before.documents.len(), 1);
    replace(
        &mut p,
        "xl/worksheets/sheet7.xml",
        "<v>45200</v>",
        "<f>1+1</f><v>2</v>",
    );
    std::fs::write(&path, archive(&p, CompressionMethod::Deflated)).unwrap();
    assert!(taskloom::runtime::app::cli::run(cli("xlsx")).await.is_err());
    assert!(taskloom::runtime::app::cli::run(cli("guess"))
        .await
        .is_err());
    assert_eq!(
        serde_json::to_value(before).unwrap(),
        serde_json::to_value(store.snapshot().await.unwrap()).unwrap()
    );
}
