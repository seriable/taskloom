//! Explicit, ephemeral semantic recall of reviewed private memory. No automatic
//! provider, embedding upload, approval, ontology merge or persistent vector cache.
use super::*;
use crate::runtime::rag::{EmbeddingBatch, EmbeddingModel};
fn invalid_json(error: serde_json::Error) -> RuntimeError {
    invalid(&format!("semantic memory JSON: {error}"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticMemoryPolicy {
    pub max_records: usize,
    pub max_input_bytes: usize,
    pub min_score: f64,
    pub recall: MemoryRecallPolicy,
}
impl SemanticMemoryPolicy {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        self.recall.validate()?;
        if !(1..=256).contains(&self.max_records)
            || !(1..=128 * 1024).contains(&self.max_input_bytes)
            || !self.min_score.is_finite()
            || !(0.0..=1.0).contains(&self.min_score)
        {
            return Err(invalid("invalid explicit semantic memory bounds"));
        }
        Ok(())
    }
    /// Pure compound-operation preflight: check before building a paid index
    /// for a query whose fixed response envelope can never fit the output cap.
    pub fn validate_query_envelope(
        &self,
        scope: &MemoryScope,
        revision: u64,
        query: &str,
    ) -> Result<(), RuntimeError> {
        self.validate()?;
        scope.validate()?;
        bounded_text(query, 4096)?;
        let envelope = SemanticMemoryRecall {
            memory: MemoryRecall {
                scope: scope.clone(),
                revision,
                query: query.into(),
                hits: Vec::new(),
                skipped_oversize: self.max_records,
                omitted: self.max_records,
            },
            scores: BTreeMap::new(),
            index_binding: "f".repeat(64),
        };
        if serde_json::to_vec(&envelope).map_err(invalid_json)?.len() > self.recall.max_bytes {
            return Err(invalid(
                "semantic memory envelope exceeds recall bytes; no dispatch",
            ));
        }
        Ok(())
    }
}

/// Constructed only from a validated scoped snapshot and the actual host model.
/// Deliberately not Serialize/Deserialize: do not accept client-owned vectors.
pub struct SemanticMemoryIndex {
    snapshot: String,
    model: String,
    binding: String,
    vectors: Vec<(String, Vec<f32>)>,
    policy: SemanticMemoryPolicy,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticMemoryRecall {
    pub memory: MemoryRecall,
    pub scores: BTreeMap<String, f64>,
    pub index_binding: String,
}

fn check_batch(
    batch: EmbeddingBatch,
    model: &str,
    count: usize,
    dimension: Option<usize>,
) -> Result<Vec<Vec<f32>>, RuntimeError> {
    if batch.identity != model || batch.vectors.len() != count {
        return Err(invalid(
            "semantic memory embedding identity/cardinality mismatch",
        ));
    }
    let size = dimension.or_else(|| batch.vectors.first().map(Vec::len));
    for vector in &batch.vectors {
        if vector.is_empty()
            || vector.len() > 4096
            || Some(vector.len()) != size
            || vector.iter().any(|n| !n.is_finite())
            || vector.iter().all(|n| *n == 0.0)
        {
            return Err(invalid("invalid semantic memory vector shape/value"));
        }
    }
    Ok(batch.vectors)
}
fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let dot = a
        .iter()
        .zip(b)
        .map(|(x, y)| f64::from(*x) * f64::from(*y))
        .sum::<f64>();
    let norm = |v: &[f32]| v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
    (dot / (norm(a) * norm(b))).clamp(-1.0, 1.0)
}
impl MemoryManager<'_> {
    /// This explicit operation can upload reviewed values to the supplied host
    /// embedding service. Its transport owns durable admission and service costs;
    /// these calls do not consume the generation RunBudget or retry on failure.
    pub async fn semantic_index(
        &self,
        access: &MemoryAccess,
        model: &dyn EmbeddingModel,
        policy: SemanticMemoryPolicy,
    ) -> Result<SemanticMemoryIndex, RuntimeError> {
        policy.validate()?;
        let snapshot = self.snapshot(access).await?;
        let snapshot_binding = fingerprint(&snapshot)?;
        let identity = model.identity();
        bounded_text(&identity, 4096)?;
        let records = snapshot
            .records
            .values()
            .filter(|r| r.status == MemoryStatus::Active)
            .collect::<Vec<_>>();
        let texts = records
            .iter()
            .map(|r| format!("{}\n{}", r.key, r.value))
            .collect::<Vec<_>>();
        if records.len() > policy.max_records
            || texts.iter().map(String::len).sum::<usize>() > policy.max_input_bytes
        {
            return Err(invalid(
                "reviewed memory exceeds explicit semantic index allowance; no dispatch",
            ));
        }
        let mut vectors = Vec::new();
        let mut dimension = None;
        for batch in texts.chunks(32) {
            // Stop before a later paid batch if the owner edited/forgot memory.
            if fingerprint(&self.snapshot(access).await?)? != snapshot_binding
                || model.identity() != identity
            {
                return Err(RuntimeError::Conflict(
                    "memory/model changed during explicit semantic build".into(),
                ));
            }
            let values = check_batch(model.embed(batch).await?, &identity, batch.len(), dimension)?;
            dimension = values.first().map(Vec::len).or(dimension);
            vectors.extend(values);
        }
        if fingerprint(&self.snapshot(access).await?)? != snapshot_binding
            || model.identity() != identity
        {
            return Err(RuntimeError::Conflict(
                "memory/model changed during semantic build; no index returned".into(),
            ));
        }
        Ok(SemanticMemoryIndex {
            binding: fingerprint(&("semantic-memory-v1", &snapshot_binding, &identity, &policy))?,
            snapshot: snapshot_binding,
            model: identity,
            vectors: records.iter().map(|r| r.id.clone()).zip(vectors).collect(),
            policy,
        })
    }
    pub async fn semantic_recall(
        &self,
        access: &MemoryAccess,
        index: &SemanticMemoryIndex,
        model: &dyn EmbeddingModel,
        query: &str,
    ) -> Result<SemanticMemoryRecall, RuntimeError> {
        bounded_text(query, 4096)?;
        let snapshot = self.snapshot(access).await?;
        if fingerprint(&snapshot)? != index.snapshot || model.identity() != index.model {
            return Err(RuntimeError::Conflict(
                "semantic memory index is stale or belongs to another owner/model".into(),
            ));
        }
        let mut result = SemanticMemoryRecall {
            memory: MemoryRecall {
                scope: access.scope.clone(),
                revision: snapshot.revision,
                query: query.into(),
                hits: Vec::new(),
                skipped_oversize: index.vectors.len(),
                omitted: index.vectors.len(),
            },
            scores: BTreeMap::new(),
            index_binding: index.binding.clone(),
        };
        // Check the entire fixed envelope and worst counter widths before any
        // paid query. Oversized output cannot become a post-dispatch surprise.
        if serde_json::to_vec(&result).map_err(invalid_json)?.len() > index.policy.recall.max_bytes
        {
            return Err(invalid(
                "semantic memory envelope exceeds recall bytes; no dispatch",
            ));
        }
        result.memory.skipped_oversize = 0;
        let mut ranked = Vec::new();
        if !index.vectors.is_empty() {
            let query_batch = check_batch(
                model.embed(&[query.to_owned()]).await?,
                &index.model,
                1,
                Some(index.vectors[0].1.len()),
            )?;
            // A concurrent forget/review makes the entire old result unusable.
            if fingerprint(&self.snapshot(access).await?)? != index.snapshot
                || model.identity() != index.model
            {
                return Err(RuntimeError::Conflict(
                    "memory/model changed during semantic recall; result discarded".into(),
                ));
            }
            for (id, vector) in &index.vectors {
                let record = &snapshot.records[id];
                let score = cosine(&query_batch[0], vector);
                let preference = index.policy.recall.include_preferences
                    && record.kind == MemoryKind::Preference;
                if score >= index.policy.min_score || preference {
                    ranked.push((score, record, preference));
                }
            }
        }
        ranked.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));
        let eligible = ranked.len();
        result.memory.omitted = eligible;
        for (score, record, preference) in ranked {
            if result.memory.hits.len() == index.policy.recall.max_items {
                break;
            }
            let mut candidate = result.clone();
            let terms = crate::runtime::rag::memory_terms(query);
            let text =
                crate::runtime::rag::memory_terms(&format!("{} {}", record.key, record.value));
            candidate.memory.hits.push(MemoryHit {
                record: record.clone(),
                preference,
                matched_terms: terms.keys().filter(|term| text.contains_key(*term)).count(),
            });
            candidate.scores.insert(record.id.clone(), score);
            candidate.memory.skipped_oversize = eligible;
            candidate.memory.omitted = eligible;
            if serde_json::to_vec(&candidate).map_err(invalid_json)?.len()
                > index.policy.recall.max_bytes
            {
                result.memory.skipped_oversize += 1;
            } else {
                result.memory.hits = candidate.memory.hits;
                result.scores = candidate.scores;
            }
        }
        result.memory.omitted = eligible - result.memory.hits.len();
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::future::BoxFuture;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    };
    fn access(owner: &str) -> MemoryAccess {
        MemoryAccess::curator(
            MemoryScope {
                tenant: "t".into(),
                project: "p".into(),
                owner: owner.into(),
            },
            owner,
        )
        .unwrap()
    }
    async fn fixture() -> (tempfile::TempDir, Arc<FileMemoryStore>, MemoryAccess) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(FileMemoryStore::new(dir.path()));
        let auth = access("alice");
        let mut snapshot = MemorySnapshot::empty(auth.scope.clone());
        snapshot.revision = 1;
        for (key, value, status) in [
            ("language", "中文", MemoryStatus::Active),
            ("reply_language", "中文", MemoryStatus::Active),
            ("pending", "unreviewed-secret", MemoryStatus::Candidate),
        ] {
            let mut record = MemoryRecord {
                id: String::new(),
                kind: MemoryKind::Fact,
                key: key.into(),
                value: value.into(),
                source: MemorySource {
                    session_id: "source".into(),
                    turn_id: "turn".into(),
                    run_id: "run".into(),
                    message_index: 1,
                    message_fingerprint: "a".repeat(64),
                    turn_fingerprint: "b".repeat(64),
                    quote: "fixture source text".into(),
                },
                origin: MemoryOrigin::Extraction {
                    run_id: "extract".into(),
                },
                status,
                created_revision: 1,
                updated_revision: 1,
                decision: if status == MemoryStatus::Candidate {
                    None
                } else {
                    Some(MemoryDecision {
                        actor: "reviewer".into(),
                        note: "reviewed".into(),
                        revision: 1,
                    })
                },
            };
            record.id = record.identity(auth.scope()).unwrap();
            snapshot.records.insert(record.id.clone(), record);
        }
        snapshot.events.push(MemoryEvent {
            revision: 1,
            actor: "host".into(),
            action: MemoryAction::Propose,
            record_ids: snapshot.records.keys().cloned().collect(),
        });
        store.save(&snapshot, 0).await.unwrap();
        (dir, store, auth)
    }
    struct FixtureModel {
        calls: AtomicUsize,
        inputs: Mutex<Vec<String>>,
        store: Arc<FileMemoryStore>,
        erase: AtomicBool,
        bad: AtomicBool,
    }
    impl EmbeddingModel for FixtureModel {
        fn identity(&self) -> String {
            "semantic-control-fixture-v1".into()
        }
        fn embed<'a>(
            &'a self,
            texts: &'a [String],
        ) -> BoxFuture<'a, Result<EmbeddingBatch, RuntimeError>> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.inputs.lock().unwrap().extend(texts.iter().cloned());
                if self.erase.load(Ordering::SeqCst) {
                    MemoryManager::new(self.store.as_ref())
                        .forget(&access("alice"), 1, MemoryKind::Fact, "language")
                        .await?;
                }
                Ok(EmbeddingBatch {
                    identity: self.identity(),
                    vectors: texts
                        .iter()
                        .map(|_| {
                            if self.bad.load(Ordering::SeqCst) {
                                vec![f32::NAN]
                            } else {
                                vec![1.0, 0.5]
                            }
                        })
                        .collect(),
                })
            })
        }
    }
    fn policy() -> SemanticMemoryPolicy {
        SemanticMemoryPolicy {
            max_records: 8,
            max_input_bytes: 8192,
            min_score: 0.8,
            recall: MemoryRecallPolicy {
                include_preferences: false,
                ..Default::default()
            },
        }
    }
    fn model(store: Arc<FileMemoryStore>) -> FixtureModel {
        FixtureModel {
            calls: AtomicUsize::new(0),
            inputs: Mutex::new(Vec::new()),
            store,
            erase: AtomicBool::new(false),
            bad: AtomicBool::new(false),
        }
    }
    #[tokio::test]
    async fn semantic_recall_uses_only_reviewed_scope_and_refuses_stale_index_before_dispatch() {
        let (_dir, store, auth) = fixture().await;
        let manager = MemoryManager::new(store.as_ref());
        let model = model(store.clone());
        let mut bounded = policy();
        bounded.max_records = 1;
        assert!(manager
            .semantic_index(&auth, &model, bounded)
            .await
            .is_err());
        assert_eq!(model.calls.load(Ordering::SeqCst), 0);
        let index = manager
            .semantic_index(&auth, &model, policy())
            .await
            .unwrap();
        assert_eq!(model.inputs.lock().unwrap().len(), 2);
        assert!(!model
            .inputs
            .lock()
            .unwrap()
            .iter()
            .any(|text| text.contains("unreviewed-secret")));
        assert!(manager
            .semantic_recall(&access("bob"), &index, &model, "reply style")
            .await
            .is_err());
        assert_eq!(model.calls.load(Ordering::SeqCst), 1);
        let recall = manager
            .semantic_recall(&auth, &index, &model, "reply style")
            .await
            .unwrap();
        assert_eq!(recall.memory.hits.len(), 2);
        assert_eq!(recall.memory.hits[0].matched_terms, 0);
        assert!(serde_json::to_vec(&recall).unwrap().len() <= index.policy.recall.max_bytes);
        manager
            .forget(&auth, 1, MemoryKind::Fact, "language")
            .await
            .unwrap();
        assert!(manager
            .semantic_recall(&auth, &index, &model, "reply style")
            .await
            .is_err());
        assert_eq!(model.calls.load(Ordering::SeqCst), 2);
    }
    #[tokio::test]
    async fn impossible_response_envelope_is_refused_before_paid_query() {
        let (_dir, store, auth) = fixture().await;
        let manager = MemoryManager::new(store.as_ref());
        let model = model(store.clone());
        let mut bounds = policy();
        bounds.recall.max_bytes = 512;
        let index = manager.semantic_index(&auth, &model, bounds).await.unwrap();
        assert_eq!(model.calls.load(Ordering::SeqCst), 1);
        assert!(manager
            .semantic_recall(&auth, &index, &model, &"x".repeat(4096))
            .await
            .is_err());
        assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn concurrent_forget_discards_semantic_result_and_invalid_vectors_are_rejected() {
        let (_dir, store, auth) = fixture().await;
        let manager = MemoryManager::new(store.as_ref());
        let model = model(store.clone());
        model.bad.store(true, Ordering::SeqCst);
        assert!(manager
            .semantic_index(&auth, &model, policy())
            .await
            .is_err());
        model.bad.store(false, Ordering::SeqCst);
        let index = manager
            .semantic_index(&auth, &model, policy())
            .await
            .unwrap();
        model.erase.store(true, Ordering::SeqCst);
        assert!(manager
            .semantic_recall(&auth, &index, &model, "reply style")
            .await
            .is_err());
        assert_eq!(manager.snapshot(&auth).await.unwrap().revision, 2);
    }
    #[tokio::test]
    async fn explicit_ontology_only_suggests_duplicates_and_does_not_merge_or_approve() {
        let (_dir, store, auth) = fixture().await;
        let manager = MemoryManager::new(store.as_ref());
        let ontology = MemoryOntology {
            aliases: BTreeMap::from([("reply_language".into(), "language".into())]),
        };
        let plan = manager.curation_plan(&auth, &ontology).await.unwrap();
        assert_eq!(plan.groups.len(), 1);
        assert!(plan.groups[0].exact_value_duplicates);
        assert_eq!(manager.snapshot(&auth).await.unwrap().revision, 1);
        assert!(manager
            .curation_plan(&access("bob"), &ontology)
            .await
            .unwrap()
            .groups
            .is_empty());
        let snapshot = manager.snapshot(&auth).await.unwrap();
        let id = &snapshot
            .records
            .values()
            .find(|r| r.key == "language")
            .unwrap()
            .id;
        manager
            .edit(&auth, 1, id, "English", "explicitly reviewed correction")
            .await
            .unwrap();
        let conflicts = manager.curation_plan(&auth, &ontology).await.unwrap();
        assert_eq!(conflicts.groups.len(), 1);
        assert!(!conflicts.groups[0].exact_value_duplicates);
        assert_eq!(manager.snapshot(&auth).await.unwrap().revision, 2);
        let cycle = MemoryOntology {
            aliases: BTreeMap::from([("a".into(), "b".into()), ("b".into(), "a".into())]),
        };
        assert!(cycle.validate().is_err());
    }
}

/// Host-authored aliases only. Model-proposed keys cannot change this mapping.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryOntology {
    pub aliases: BTreeMap<String, String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryCurationGroup {
    pub kind: MemoryKind,
    pub canonical_key: String,
    pub records: Vec<String>,
    pub exact_value_duplicates: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryCurationPlan {
    pub snapshot_binding: String,
    pub ontology_binding: String,
    pub groups: Vec<MemoryCurationGroup>,
}
impl MemoryOntology {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if self.aliases.len() > 256 {
            return Err(invalid("memory ontology exceeds256 aliases"));
        }
        for (from, to) in &self.aliases {
            validate_key(from)?;
            validate_key(to)?;
            if from == to || self.aliases.contains_key(to) {
                return Err(invalid(
                    "ontology aliases must point directly to canonical slots without chains/cycles",
                ));
            }
        }
        Ok(())
    }
}
impl MemoryManager<'_> {
    /// Read-only suggestions. Similarity and aliases never accept, merge, edit,
    /// forget, grant tools or override the existing explicit review CAS.
    pub async fn curation_plan(
        &self,
        access: &MemoryAccess,
        ontology: &MemoryOntology,
    ) -> Result<MemoryCurationPlan, RuntimeError> {
        ontology.validate()?;
        let snapshot = self.snapshot(access).await?;
        let mut slots: BTreeMap<(MemoryKind, String), Vec<&MemoryRecord>> = BTreeMap::new();
        for record in snapshot
            .records
            .values()
            .filter(|r| matches!(r.status, MemoryStatus::Active | MemoryStatus::Candidate))
        {
            let canonical = ontology
                .aliases
                .get(&record.key)
                .unwrap_or(&record.key)
                .clone();
            slots
                .entry((record.kind, canonical))
                .or_default()
                .push(record);
        }
        let groups = slots
            .into_iter()
            .filter(|(_, records)| records.len() > 1)
            .map(|((kind, canonical_key), records)| MemoryCurationGroup {
                kind,
                canonical_key,
                exact_value_duplicates: records.iter().all(|r| r.value == records[0].value),
                records: records.into_iter().map(|r| r.id.clone()).collect(),
            })
            .collect();
        Ok(MemoryCurationPlan {
            snapshot_binding: fingerprint(&snapshot)?,
            ontology_binding: fingerprint(ontology)?,
            groups,
        })
    }
}
