//! Host CLI only: bounded explicit files -> fixed isolated OCR -> JSON audit.
//! cargo run --features ocr --example agent_ocr -- CONFIG_JSON INPUT_PNG ID TITLE
use std::{io::Read, path::Path};
use taskloom::runtime::{
    ocr::{DockerOcr, DockerOcrConfig},
    ToolContext,
};
fn bounded(path: &Path, limit: usize) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let file = std::fs::File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err("source must be a regular host-selected file".into());
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err("source exceeds host byte allowance".into());
    }
    Ok(bytes)
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 4 {
        return Err("usage: agent_ocr CONFIG_JSON INPUT_PNG ID TITLE".into());
    }
    let config: DockerOcrConfig = serde_json::from_slice(&bounded(Path::new(&args[0]), 65536)?)?;
    config.validate()?;
    let bytes = bounded(Path::new(&args[1]), config.max_image_bytes)?;
    let worker = DockerOcr::new(config)?;
    let context = ToolContext {
        run_id: format!("ocr-cli-{}", uuid::Uuid::new_v4().simple()),
        call_id: "extract".into(),
    };
    let outcome = worker.extract(&bytes, &context).await?;
    let document = outcome
        .clone()
        .into_document(args[2].to_string_lossy(), args[3].to_string_lossy())
        .ok();
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({"outcome":outcome,"document":document}))?
    );
    if document.is_none() {
        return Err("OCR output incomplete/empty or cleanup unconfirmed; no document".into());
    }
    Ok(())
}
