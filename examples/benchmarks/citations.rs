//! Shared SDK quote/provenance validator for comparison harnesses. The caller
//! must supply actually observed retrieval results; this does not attest that
//! arbitrary input evidence was retrieved or judge semantic claim support.
use serde::Deserialize;
use std::io::Read;
use taskloom::runtime::rag::{CitedReport, SearchResult};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    report: CitedReport,
    evidence: Vec<SearchResult>,
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err("evidence exceeds 2 MiB".into());
    }
    let input: Input = serde_json::from_slice(&bytes)?;
    let verified = SearchResult::verify_all(&input.evidence, input.report)?;
    println!("{}", serde_json::to_string(&verified)?);
    Ok(())
}
