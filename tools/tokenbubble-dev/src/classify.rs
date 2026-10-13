//! Maps repository paths to what they can change.

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Component {
    /// The app `cargo dev` builds and runs.
    App,
    /// This development runner.
    Runner,
}

/// Paths scanned beneath the workspace root. Everything else is irrelevant.
pub const WATCHED_ROOTS: [&str; 4] = [
    "crates",
    "tools/tokenbubble-dev",
    "Cargo.toml",
    "Cargo.lock",
];

/// Components a path (relative to the workspace, `/`-separated) can change.
/// Returns an empty slice for files that cannot affect a dev binary.
pub fn classify(path: &str) -> &'static [Component] {
    use Component::*;
    if is_ignored(path) {
        &[]
    } else if path == "Cargo.toml" || path == "Cargo.lock" || path.starts_with("crates/") {
        &[App]
    } else if path.starts_with("tools/tokenbubble-dev/") {
        &[Runner]
    } else {
        &[]
    }
}

/// Whether a directory should not be descended into while scanning.
pub fn skip_directory(name: &str) -> bool {
    name == "target" || name.starts_with('.')
}

fn is_ignored(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.iter().any(|part| skip_directory(part)) {
        return true;
    }
    // A package's integration tests, examples and benches never feed the dev binaries:
    // `crates/<package>/tests/...`, `tools/<package>/examples/...`.
    if matches!(parts.first(), Some(&"crates" | &"tools"))
        && parts.len() > 3
        && matches!(parts[2], "tests" | "examples" | "benches")
    {
        return true;
    }
    let name = parts.last().copied().unwrap_or_default();
    name.starts_with('#')
        || name.ends_with('~')
        || name == "4913"
        || [".swp", ".swo", ".swx", ".tmp", ".bak", ".orig", ".md"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

#[cfg(test)]
mod tests {
    use super::{Component::*, classify};

    #[test]
    fn crate_sources_and_manifests_rebuild_the_app() {
        for path in [
            "crates/tokenbubble-core/src/providers/codex/usage.rs",
            "crates/tokenbubble-core/Cargo.toml",
            "crates/tokenbubble-core/src/providers/tests/nested.rs",
            "Cargo.toml",
            "Cargo.lock",
        ] {
            assert_eq!(classify(path), &[App], "{path}");
        }
        assert_eq!(classify("tools/tokenbubble-dev/src/runtime.rs"), &[Runner]);
    }

    #[test]
    fn build_output_tests_docs_and_editor_debris_are_irrelevant() {
        for path in [
            "target/debug/tokenbubble-collect.exe",
            "crates/tokenbubble-core/target/debug/x",
            "crates/tokenbubble-core/tests/codex.rs",
            "crates/tokenbubble-core/tests/fixtures/codex/usage_full.json",
            "tools/tokenbubble-dev/examples/probe.rs",
            "crates/tokenbubble-core/src/.model.rs.swp",
            "crates/tokenbubble-core/src/model.rs~",
            "crates/tokenbubble-core/src/4913",
            "crates/tokenbubble-core/README.md",
            ".cargo/config.toml",
            "Spec.md",
            "THIRD_PARTY_NOTICES.md",
        ] {
            assert!(classify(path).is_empty(), "{path}");
        }
    }
}
