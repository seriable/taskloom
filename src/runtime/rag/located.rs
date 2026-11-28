//! Explicit source locators and deterministic verified rendering. This is a
//! distinct input contract, never a correction of an invalid chunk hash.
use super::{invalid, Citation, CitedReport, SearchResult, VerifiedReport};
use crate::runtime::RuntimeError;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceLocator {
    pub document_id: String,
    pub corpus_revision: u64,
    /// Exact chunk start, in UTF-8 bytes, as returned by retrieval.
    pub start: usize,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocatedCitation {
    pub document_id: String,
    pub corpus_revision: u64,
    pub start: usize,
    pub quote: String,
}
impl LocatedCitation {
    pub fn locator(&self) -> SourceLocator {
        SourceLocator {
            document_id: self.document_id.clone(),
            corpus_revision: self.corpus_revision,
            start: self.start,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocatedReport {
    /// `[[1]]` refers to the first citation, `[[2]]` to the second, and so on.
    pub text: String,
    pub citations: Vec<LocatedCitation>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedCitation {
    pub ordinal: usize,
    pub source: SourceLocator,
    pub chunk_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedLocatedReport {
    pub original: LocatedReport,
    pub mappings: Vec<ResolvedCitation>,
    pub verified: VerifiedReport,
}
impl SearchResult {
    /// Resolve only exact locators in the supplied evidence snapshots. Missing,
    /// conflicting or ambiguous locators, invented quotes and invalid ordinals
    /// fail. No fuzzy ID, revision, offset or quotation repair occurs.
    pub fn verify_located_all(
        results: &[Self],
        report: LocatedReport,
    ) -> Result<ResolvedLocatedReport, RuntimeError> {
        if results.len() > 1024
            || report.text.trim().is_empty()
            || report.text.len() > 1024 * 1024
            || report.citations.is_empty()
            || report.citations.len() > 20
        {
            return Err(invalid(
                "located report needs bounded text, evidence and citations",
            ));
        }
        let mut mappings = Vec::new();
        let mut citations = Vec::new();
        for (i, citation) in report.citations.iter().enumerate() {
            if citation.document_id.trim().is_empty() || citation.document_id.len() > 512 {
                return Err(invalid("invalid source locator document identity"));
            }
            let mut candidates = BTreeMap::new();
            for result in results
                .iter()
                .filter(|r| r.corpus_revision == citation.corpus_revision)
            {
                for hit in result.hits.iter().filter(|h| {
                    h.chunk.document_id == citation.document_id && h.chunk.start == citation.start
                }) {
                    if let Some(previous) = candidates.insert(hit.chunk.id.as_str(), &hit.chunk) {
                        if previous != &hit.chunk {
                            return Err(invalid("conflicting source locator evidence"));
                        }
                    }
                }
            }
            if candidates.len() != 1 {
                return Err(invalid(&format!("citation {} source locator was not retrieved or is ambiguous; use exact document_id, corpus_revision and start", i + 1)));
            }
            // Give the repair model the failed ordinal, not an opaque combined
            // marker/provenance error. Never replace its quote with source text.
            let chunk = candidates.first_key_value().unwrap().1;
            if !chunk.text.contains(&citation.quote) {
                return Err(invalid(&format!(
                    "citation {} quote is not an exact substring of its retrieved chunk.text; copy the source body exactly or remove that citation and its marker",
                    i + 1
                )));
            }
            let id = (*candidates.first_key_value().unwrap().0).to_string();
            citations.push(Citation {
                chunk_id: id.clone(),
                quote: citation.quote.clone(),
            });
            mappings.push(ResolvedCitation {
                ordinal: i + 1,
                source: citation.locator(),
                chunk_id: id,
            });
        }
        let mut rendered = String::new();
        let mut tail = report.text.as_str();
        let mut seen = BTreeSet::new();
        while let Some((before, rest)) = tail.split_once("[[") {
            let (ordinal, after) = rest
                .split_once("]]")
                .ok_or_else(|| invalid("unclosed ordinal citation marker"))?;
            let number = ordinal
                .parse::<usize>()
                .map_err(|_| invalid("located reports require [[1]], [[2]], ... markers"))?;
            if number == 0 || number > mappings.len() || number.to_string() != ordinal {
                return Err(invalid("invalid or undeclared ordinal citation marker"));
            }
            rendered.push_str(before);
            rendered.push_str("[[");
            rendered.push_str(&mappings[number - 1].chunk_id);
            rendered.push_str("]]");
            seen.insert(number);
            tail = after;
        }
        rendered.push_str(tail);
        if seen.len() != mappings.len() {
            let missing = (1..=mappings.len())
                .filter(|ordinal| !seen.contains(ordinal))
                .map(|ordinal| format!("[[{ordinal}]]"))
                .collect::<Vec<_>>();
            return Err(invalid(&format!(
                "every located citation needs its ordinal text marker; missing {}",
                missing.join(", ")
            )));
        }
        let verified = Self::verify_all(
            results,
            CitedReport {
                text: rendered,
                citations,
            },
        )?;
        Ok(ResolvedLocatedReport {
            original: report,
            mappings,
            verified,
        })
    }
}
