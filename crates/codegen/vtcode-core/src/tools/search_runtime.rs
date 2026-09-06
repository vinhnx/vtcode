use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tracing::warn;

use crate::command_safety::shell_parser::prewarm_bash_parser;
use crate::tools::tree_sitter_runtime::prewarm_workspace_languages;
use crate::tools::{AstGrepStatus, RipgrepStatus};
use crate::utils::common::detect_workspace_languages;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchToolReadiness {
    Ready,
    Missing,
    Error,
}

impl SearchToolReadiness {
    #[must_use]
    pub fn is_ready(self) -> bool {
        matches!(self, Self::Ready)
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Missing => "missing",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchToolBundleStatus {
    pub ripgrep: SearchToolReadiness,
    pub ast_grep: SearchToolReadiness,
}

impl SearchToolBundleStatus {
    #[must_use]
    pub fn all_ready(self) -> bool {
        self.ripgrep.is_ready() && self.ast_grep.is_ready()
    }

    #[must_use]
    pub fn all_unavailable(self) -> bool {
        !self.ripgrep.is_ready() && !self.ast_grep.is_ready()
    }

    #[must_use]
    pub fn has_errors(self) -> bool {
        matches!(self.ripgrep, SearchToolReadiness::Error) || matches!(self.ast_grep, SearchToolReadiness::Error)
    }

    #[must_use]
    pub fn header_summary(self) -> String {
        if self.all_ready() {
            return "Search: ripgrep \u{00b7} ast-grep".to_string();
        }
        format!("Search: ripgrep {} \u{00b7} ast-grep {}", self.ripgrep.label(), self.ast_grep.label())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SearchRuntimeSnapshot {
    pub(crate) workspace_languages: Vec<String>,
    pub(crate) search_tools: SearchToolBundleStatus,
    pub(crate) ripgrep_ready: bool,
    pub(crate) ast_grep_ready: bool,
    pub(crate) code_tree_sitter_languages: Vec<String>,
    pub(crate) bash_tree_sitter_ready: bool,
}

/// Bounded per-workspace search-runtime snapshot cache. Snapshots are cheap to
/// recompute relative to the value of caching (detect/prewarm only happens on
/// miss), and keying by workspace path means an unbounded map would leak one
/// entry per distinct workspace ever seen in a long-lived process. A small LRU
/// cap bounds memory while keeping the hot workspace cached.
const SEARCH_RUNTIME_CACHE_CAP: usize = 16;

static SEARCH_RUNTIME_CACHE: OnceLock<Mutex<lru::LruCache<PathBuf, SearchRuntimeSnapshot>>> = OnceLock::new();

pub(crate) fn snapshot_for_workspace(workspace_root: &Path) -> SearchRuntimeSnapshot {
    let workspace_root = workspace_root.to_path_buf();
    let cache = SEARCH_RUNTIME_CACHE.get_or_init(|| {
        let capacity = std::num::NonZeroUsize::new(SEARCH_RUNTIME_CACHE_CAP)
            .expect("SEARCH_RUNTIME_CACHE_CAP is a non-zero const");
        Mutex::new(lru::LruCache::new(capacity))
    });

    // Check cache first; recover from poison by clearing and returning None to recompute
    {
        let mut cache_guard = cache.lock().unwrap_or_else(|poisoned| {
            warn!("search runtime cache mutex poisoned; recovering");
            poisoned.into_inner()
        });
        if let Some(snapshot) = cache_guard.get(&workspace_root).cloned() {
            return snapshot;
        }
    }

    let workspace_languages = detect_workspace_languages(&workspace_root);
    let search_tools = SearchToolBundleStatus {
        ripgrep: SearchToolReadiness::from_ripgrep_status(RipgrepStatus::check()),
        ast_grep: SearchToolReadiness::from_ast_grep_status(AstGrepStatus::check()),
    };
    let snapshot = SearchRuntimeSnapshot {
        code_tree_sitter_languages: prewarm_workspace_languages(workspace_languages.iter().map(String::as_str)),
        workspace_languages,
        search_tools,
        ripgrep_ready: search_tools.ripgrep.is_ready(),
        ast_grep_ready: search_tools.ast_grep.is_ready(),
        bash_tree_sitter_ready: prewarm_bash_parser().is_ok(),
    };

    let mut guard = cache.lock().unwrap_or_else(|poisoned| {
        warn!("search runtime cache mutex poisoned; recovering");
        poisoned.into_inner()
    });
    guard.get_or_insert(workspace_root, || snapshot.clone()).clone()
}

pub fn dominant_workspace_language(workspace_root: &Path) -> Option<String> {
    snapshot_for_workspace(workspace_root).workspace_languages.into_iter().next()
}

pub fn search_tool_bundle_status(workspace_root: &Path) -> SearchToolBundleStatus {
    snapshot_for_workspace(workspace_root).search_tools
}

impl SearchToolReadiness {
    fn from_ripgrep_status(status: RipgrepStatus) -> Self {
        match status {
            RipgrepStatus::Available { .. } => Self::Ready,
            RipgrepStatus::NotFound => Self::Missing,
            RipgrepStatus::Error { .. } => Self::Error,
        }
    }

    fn from_ast_grep_status(status: AstGrepStatus) -> Self {
        match status {
            AstGrepStatus::Available { .. } => Self::Ready,
            AstGrepStatus::NotFound => Self::Missing,
            AstGrepStatus::Error { .. } => Self::Error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SearchToolBundleStatus, SearchToolReadiness, dominant_workspace_language, snapshot_for_workspace};
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn snapshot_for_workspace_captures_languages_and_bash_parser_state() {
        let workspace = TempDir::new().expect("workspace tempdir");
        fs::create_dir_all(workspace.path().join("src")).expect("create src");
        fs::create_dir_all(workspace.path().join("web")).expect("create web");
        fs::write(workspace.path().join("src/lib.rs"), "fn alpha() {}\n").expect("write rust");
        fs::write(workspace.path().join("web/app.ts"), "const app = 1;\n").expect("write ts");

        let snapshot = snapshot_for_workspace(workspace.path());

        assert_eq!(snapshot.workspace_languages, vec!["Rust".to_string(), "TypeScript".to_string()]);
        assert_eq!(snapshot.ripgrep_ready, snapshot.search_tools.ripgrep.is_ready());
        assert_eq!(snapshot.ast_grep_ready, snapshot.search_tools.ast_grep.is_ready());
        assert_eq!(snapshot.code_tree_sitter_languages, vec!["Rust".to_string(), "TypeScript".to_string()]);
        assert!(snapshot.bash_tree_sitter_ready);
    }

    #[test]
    fn snapshot_for_workspace_reuses_cached_languages() {
        let workspace = TempDir::new().expect("workspace tempdir");
        fs::create_dir_all(workspace.path().join("src")).expect("create src");
        fs::write(workspace.path().join("src/lib.rs"), "fn alpha() {}\n").expect("write rust");

        let initial = snapshot_for_workspace(workspace.path());

        fs::create_dir_all(workspace.path().join("web")).expect("create web");
        fs::write(workspace.path().join("web/app.ts"), "const app = 1;\n").expect("write ts");

        let cached = snapshot_for_workspace(workspace.path());

        assert_eq!(initial.workspace_languages, cached.workspace_languages);
    }

    #[test]
    fn dominant_workspace_language_returns_first_detected_language() {
        let workspace = TempDir::new().expect("workspace tempdir");
        fs::create_dir_all(workspace.path().join("src")).expect("create src");
        fs::create_dir_all(workspace.path().join("web")).expect("create web");
        fs::write(workspace.path().join("src/lib.rs"), "fn alpha() {}\n").expect("write rust");
        fs::write(workspace.path().join("src/main.rs"), "fn beta() {}\n").expect("write rust");
        fs::write(workspace.path().join("web/app.ts"), "const app = 1;\n").expect("write ts");

        assert_eq!(dominant_workspace_language(workspace.path()).as_deref(), Some("Rust"));
    }

    #[test]
    fn snapshot_for_workspace_ignores_languages_without_shared_parser_support() {
        let workspace = TempDir::new().expect("workspace tempdir");
        fs::create_dir_all(workspace.path().join("ios")).expect("create ios");
        fs::write(workspace.path().join("ios/App.swift"), "struct App {}\n").expect("write swift");

        let snapshot = snapshot_for_workspace(workspace.path());

        assert_eq!(snapshot.workspace_languages, vec!["Swift".to_string()]);
        assert!(snapshot.code_tree_sitter_languages.is_empty());
    }

    #[test]
    fn search_tool_bundle_header_summary_reflects_readiness() {
        let status = SearchToolBundleStatus {
            ripgrep: SearchToolReadiness::Ready,
            ast_grep: SearchToolReadiness::Missing,
        };

        assert_eq!(status.header_summary(), "Search: ripgrep ready \u{00b7} ast-grep missing");
        assert!(!status.all_ready());
        assert!(!status.all_unavailable());
        assert!(!status.has_errors());
    }

    #[test]
    fn search_tool_bundle_header_summary_all_ready() {
        let status = SearchToolBundleStatus {
            ripgrep: SearchToolReadiness::Ready,
            ast_grep: SearchToolReadiness::Ready,
        };

        assert_eq!(status.header_summary(), "Search: ripgrep \u{00b7} ast-grep");
        assert!(status.all_ready());
    }
}
