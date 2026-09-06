//! Read-only queries across sessions for analytics and long-term learning.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::SystemTime;

use lru::LruCache;

use crate::error::SessionStoreError;
use crate::sessions_root;

const MANIFEST_CACHE_CAPACITY: usize = 200;
const MANIFEST_CACHE_NONZERO_CAPACITY: std::num::NonZeroUsize =
    std::num::NonZeroUsize::new(MANIFEST_CACHE_CAPACITY).unwrap();
static MANIFEST_CACHE: std::sync::OnceLock<Mutex<LruCache<String, CachedManifest>>> = std::sync::OnceLock::new();

#[derive(Debug, Clone)]
struct CachedManifest {
    summary: SessionSummary,
    signature: ManifestSignature,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ManifestSignature {
    modified: Option<SystemTime>,
    len: u64,
}

fn manifest_signature(path: &Path) -> Option<ManifestSignature> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    Some(ManifestSignature {
        modified: metadata.modified().ok(),
        len: metadata.len(),
    })
}

/// Invalidate a cached manifest after an atomic replacement.
pub(crate) fn invalidate_manifest_cache(path: &Path) {
    if let Some(cache) = MANIFEST_CACHE.get()
        && let Ok(mut cache) = cache.lock()
    {
        let key = path.to_string_lossy();
        cache.pop(key.as_ref());
    }
}

/// Lightweight summary of a single session, read from its `manifest.json`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SessionSummary {
    /// Session identifier (directory name).
    pub session_id: String,
    /// Number of completed turns.
    pub turn_count: u64,
    /// Total events recorded.
    pub event_count: u64,
    /// Lifecycle status.
    pub status: String,
    /// RFC3339 last-update timestamp (used for ordering).
    pub updated_at: String,
}

/// A single grounded fact drawn from a session's memory envelope.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct FactRecord {
    /// The fact text.
    pub fact: String,
    /// Session the fact originated from.
    pub session_id: String,
}

/// One result returned from a memory search.
///
/// Mirrors the shape used by the grok-build memory subsystem so that
/// higher-level consumers (tool bridge, context injection) can share
/// formatting logic once a richer backend is available.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct MemorySearchResult {
    /// Stable identifier for this chunk (session_id + fact index).
    chunk_id: String,
    /// Source memory file path.
    path: String,
    /// 0-based start line in the source file (0 for derived facts).
    start_line: usize,
    /// 0-based end line in the source file (0 for derived facts).
    end_line: usize,
    /// Relevance score (higher = more relevant).
    score: f64,
    /// Text snippet from the chunk.
    snippet: String,
    /// Source scope: `"session"` for per-session memory files.
    source: String,
    /// Unix timestamp (seconds) when the source memory was created.
    created_at: Option<i64>,
}

/// List up to `n` most-recently-updated sessions.
#[must_use]
pub fn recent_sessions(workspace: &Path, n: usize) -> Vec<SessionSummary> {
    let root = sessions_root(workspace);
    if !root.exists() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(&root) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut cache = MANIFEST_CACHE
        .get_or_init(|| Mutex::new(LruCache::new(MANIFEST_CACHE_NONZERO_CAPACITY)))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for entry in entries.filter_map(Result::ok) {
        let manifest = entry.path().join("manifest.json");
        let key = manifest.to_string_lossy().into_owned();
        let signature = manifest_signature(&manifest);
        if let Some(cached) = cache.get(&key)
            && signature == Some(cached.signature)
        {
            out.push(cached.summary.clone());
            continue;
        }
        if let Ok(bytes) = std::fs::read(&manifest)
            && let Ok(s) = serde_json::from_slice::<SessionSummary>(&bytes)
        {
            if let Some(signature) = signature.or_else(|| manifest_signature(&manifest)) {
                cache.put(key, CachedManifest { summary: s.clone(), signature });
            }
            out.push(s);
        }
    }
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    out.truncate(n);
    out
}

/// Cross-session long-term-learning query: collect grounded facts from every
/// session's derived memory envelope. This is how the agent learns across
/// sessions without loading any history into context.
pub fn query_facts(workspace: &Path, limit: usize) -> Result<Vec<FactRecord>, SessionStoreError> {
    let root = sessions_root(workspace);
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut facts: Vec<FactRecord> = Vec::new();
    let entries = std::fs::read_dir(&root).map_err(|e| SessionStoreError::io(root.clone(), e))?;
    for entry in entries.filter_map(Result::ok) {
        let memory = entry.path().join(crate::DERIVED_DIR).join("memory.json");
        let Ok(bytes) = std::fs::read(&memory) else {
            continue;
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        let session_id = entry.file_name().to_string_lossy().into_owned();
        if let Some(arr) = value.get("grounded_facts").and_then(|v| v.as_array()) {
            for item in arr {
                if let Some(fact) = item.get("fact").and_then(|f| f.as_str()) {
                    facts.push(FactRecord {
                        fact: fact.to_string(),
                        session_id: session_id.clone(),
                    });
                }
            }
        }
    }
    facts.truncate(limit);
    Ok(facts)
}

/// Cross-session memory search: scan every session's derived memory envelope
/// for facts matching `query`. Returns up to `max_results` results with
/// score >= `min_score`, sorted by descending relevance.
///
/// Scoring uses BM25 (`k1=1.2`, `b=0.75`) over tokenized facts. Ties are
/// deterministic by the stable `session_id:index` chunk identifier.
pub fn search_memory(
    workspace: &Path,
    query: &str,
    max_results: usize,
    min_score: f64,
) -> Result<Vec<MemorySearchResult>, SessionStoreError> {
    if query.is_empty() {
        return Ok(Vec::new());
    }

    let root = sessions_root(workspace);
    if !root.exists() {
        return Ok(Vec::new());
    }

    let query_terms = tokenize(query);
    if query_terms.is_empty() {
        return Ok(Vec::new());
    }
    let mut results: Vec<MemorySearchResult> = Vec::new();
    let session_source = String::from("session");
    let mut documents = Vec::new();

    let entries = std::fs::read_dir(&root).map_err(|e| SessionStoreError::io(root.clone(), e))?;
    for entry in entries.filter_map(Result::ok) {
        let session_dir = entry.path();
        let memory = session_dir.join(crate::DERIVED_DIR).join("memory.json");
        let Ok(bytes) = std::fs::read(&memory) else {
            continue;
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        let session_id = entry.file_name().to_string_lossy().into_owned();
        let memory_path = memory.to_string_lossy().into_owned();
        let created_at = value
            .get("created_at")
            .and_then(|v| v.as_i64())
            .or_else(|| value.get("updated_at").and_then(|v| v.as_i64()));

        if let Some(arr) = value.get("grounded_facts").and_then(|v| v.as_array()) {
            for (idx, item) in arr.iter().enumerate() {
                let Some(fact) = item.get("fact").and_then(|f| f.as_str()) else {
                    continue;
                };
                documents.push((
                    format!("{session_id}:{idx}"),
                    memory_path.clone(),
                    fact.to_owned(),
                    tokenize(fact),
                    created_at,
                ));
            }
        }
    }

    let document_count = documents.len();
    if document_count == 0 {
        return Ok(Vec::new());
    }
    let average_length =
        documents.iter().map(|(_, _, _, terms, _)| terms.len() as f64).sum::<f64>() / document_count as f64;
    let mut document_frequency: HashMap<String, usize> = HashMap::new();
    for (_, _, _, terms, _) in &documents {
        let mut seen = std::collections::HashSet::new();
        for term in terms {
            if seen.insert(term.as_str()) {
                *document_frequency.entry(term.clone()).or_insert(0) += 1;
            }
        }
    }
    let k1 = 1.2;
    let b = 0.75;
    for (chunk_id, path, fact, terms, created_at) in documents {
        let length = terms.len() as f64;
        let mut term_frequency = HashMap::<&str, usize>::new();
        for term in &terms {
            *term_frequency.entry(term.as_str()).or_insert(0) += 1;
        }
        let mut score = 0.0;
        for query_term in &query_terms {
            let Some(&frequency) = term_frequency.get(query_term.as_str()) else {
                continue;
            };
            let Some(&frequency_in_documents) = document_frequency.get(query_term) else {
                continue;
            };
            let idf = (((document_count - frequency_in_documents) as f64 + 0.5)
                / (frequency_in_documents as f64 + 0.5)
                + 1.0)
                .ln();
            let denominator = frequency as f64 + k1 * (1.0 - b + b * length / average_length.max(1.0));
            score += idf * (frequency as f64 * (k1 + 1.0)) / denominator;
        }
        if score <= 0.0 || score < min_score {
            continue;
        }
        results.push(MemorySearchResult {
            chunk_id,
            path,
            start_line: 0,
            end_line: 0,
            score,
            snippet: fact,
            source: session_source.clone(),
            created_at,
        });
    }

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.chunk_id.cmp(&b.chunk_id))
    });
    results.truncate(max_results);
    Ok(results)
}

/// Return the configured default for `max_results` in search queries.
pub fn default_search_max_results() -> usize {
    6
}

/// Return the configured default for `min_score` in search queries.
pub fn default_search_min_score() -> f64 {
    0.0
}

fn count_substring_matches(text: &str, lowered_query: &str) -> usize {
    if lowered_query.is_empty() {
        return 0;
    }
    let lowered = text.to_ascii_lowercase();
    let mut count = 0;
    let mut start = 0;
    while let Some(pos) = lowered[start..].find(lowered_query) {
        count += 1;
        start += pos + lowered_query.len();
    }
    count
}

fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for character in text.chars() {
        if character.is_alphanumeric() {
            current.extend(character.to_lowercase());
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn count_substring_matches_counts_overlapping() {
        assert_eq!(count_substring_matches("aaaa", "aa"), 2);
        assert_eq!(count_substring_matches("ababa", "aba"), 1);
        assert_eq!(count_substring_matches("hello world", "ll"), 1);
        assert_eq!(count_substring_matches("", "x"), 0);
    }

    #[test]
    fn search_memory_returns_matching_facts() {
        let dir = TempDir::new().expect("tempdir");
        let sess = crate::session_dir(dir.path(), "s1");
        std::fs::create_dir_all(sess.join(crate::DERIVED_DIR)).expect("mkdir");
        let memory = serde_json::json!({
            "grounded_facts": [
                {"fact": "the widget is blue"},
                {"fact": "the server runs on port 8080"},
                {"fact": "use PostgreSQL for persistence"},
            ]
        });
        std::fs::write(sess.join(crate::DERIVED_DIR).join("memory.json"), serde_json::to_string(&memory).expect("ser"))
            .expect("write");

        let results = search_memory(dir.path(), "blue", 10, 0.0).expect("search");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].snippet, "the widget is blue");
        assert_eq!(results[0].chunk_id, "s1:0");
        assert!(results[0].score > 0.0);
    }

    #[test]
    fn search_memory_scores_multiple_matches() {
        let dir = TempDir::new().expect("tempdir");
        let sess = crate::session_dir(dir.path(), "s2");
        std::fs::create_dir_all(sess.join(crate::DERIVED_DIR)).expect("mkdir");
        let memory = serde_json::json!({
            "grounded_facts": [
                {"fact": "rust uses rustc and cargo"},
                {"fact": "cargo is the rust build tool"},
            ]
        });
        std::fs::write(sess.join(crate::DERIVED_DIR).join("memory.json"), serde_json::to_string(&memory).expect("ser"))
            .expect("write");

        let results = search_memory(dir.path(), "cargo", 10, 0.0).expect("search");
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.score > 0.0));
    }

    #[test]
    fn search_memory_respects_min_score() {
        let dir = TempDir::new().expect("tempdir");
        let sess = crate::session_dir(dir.path(), "s3");
        std::fs::create_dir_all(sess.join(crate::DERIVED_DIR)).expect("mkdir");
        let memory = serde_json::json!({
            "grounded_facts": [
                {"fact": "alpha beta gamma"},
            ]
        });
        std::fs::write(sess.join(crate::DERIVED_DIR).join("memory.json"), serde_json::to_string(&memory).expect("ser"))
            .expect("write");

        let results = search_memory(dir.path(), "beta", 10, 2.0).expect("search");
        assert!(results.is_empty());
    }

    #[test]
    fn search_memory_empty_query_returns_empty() {
        let dir = TempDir::new().expect("tempdir");
        let results = search_memory(dir.path(), "", 10, 0.0).expect("search");
        assert!(results.is_empty());
    }

    #[test]
    fn search_memory_sorts_by_score_descending() {
        let dir = TempDir::new().expect("tempdir");
        for i in 0..3 {
            let sess = crate::session_dir(dir.path(), &format!("s{i}"));
            std::fs::create_dir_all(sess.join(crate::DERIVED_DIR)).expect("mkdir");
            let memory = serde_json::json!({
                "grounded_facts": [
                    {"fact": format!("fact {i} appears twice twice")},
                ]
            });
            std::fs::write(
                sess.join(crate::DERIVED_DIR).join("memory.json"),
                serde_json::to_string(&memory).expect("ser"),
            )
            .expect("write");
        }

        let results = search_memory(dir.path(), "twice", 10, 0.0).expect("search");
        assert_eq!(results.len(), 3);
        assert!(results.windows(2).all(|w| w[0].score >= w[1].score));
    }

    #[test]
    fn search_memory_uses_bm25_term_coverage_and_deterministic_ties() {
        let dir = TempDir::new().expect("tempdir");
        for (session_id, facts) in [
            ("s1", vec!["rust cargo tool", "unrelated note"]),
            ("s2", vec!["cargo build tool"]),
        ] {
            let session = crate::session_dir(dir.path(), session_id);
            std::fs::create_dir_all(session.join(crate::DERIVED_DIR)).expect("mkdir");
            let memory = serde_json::json!({
                "grounded_facts": facts.into_iter().map(|fact| serde_json::json!({"fact": fact})).collect::<Vec<_>>()
            });
            std::fs::write(
                session.join(crate::DERIVED_DIR).join("memory.json"),
                serde_json::to_string(&memory).expect("serialize"),
            )
            .expect("write");
        }

        let results = search_memory(dir.path(), "rust cargo", 10, 0.0).expect("search");
        assert_eq!(results.first().map(|result| result.chunk_id.as_str()), Some("s1:0"));
        assert!(results[0].score > results[1].score);
    }

    #[test]
    fn recent_sessions_invalidates_manifest_cache_after_replacement() {
        let dir = TempDir::new().expect("tempdir");
        let session = crate::session_dir(dir.path(), "cache-session");
        std::fs::create_dir_all(&session).expect("mkdir");
        let manifest = serde_json::json!({
            "session_id": "cache-session",
            "schema_version": 1,
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
            "turn_count": 1,
            "event_count": 1,
            "status": "active"
        });
        let path = session.join("manifest.json");
        std::fs::write(&path, serde_json::to_vec(&manifest).expect("serialize")).expect("write");
        assert_eq!(recent_sessions(dir.path(), 1)[0].updated_at, "2026-01-01T00:00:00Z");

        let mut replaced = manifest;
        replaced["updated_at"] = serde_json::json!("2099-01-01T00:00:00Z");
        std::fs::write(&path, serde_json::to_vec(&replaced).expect("serialize")).expect("replace");
        assert_eq!(recent_sessions(dir.path(), 1)[0].updated_at, "2099-01-01T00:00:00Z");
    }
}
