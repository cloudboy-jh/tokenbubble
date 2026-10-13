//! Polling source watcher. Polling a small tree is cheap, needs no extra dependency, and
//! behaves the same on every platform and editor (atomic renames, swap files, etc.).

use crate::classify::{self, Component};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Digest([u8; 32]);

#[cfg(test)]
impl Digest {
    pub(crate) fn of(byte: u8) -> Self {
        Self([byte; 32])
    }
}

/// Content identity of everything that feeds each dev binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Fingerprints {
    pub app: Digest,
    pub runner: Digest,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    modified: Option<SystemTime>,
    len: u64,
}

struct Entry {
    stamp: Stamp,
    content: [u8; 32],
    components: &'static [Component],
}

/// Snapshot of watched files keyed by workspace-relative `/` path.
pub struct Tree {
    root: PathBuf,
    files: BTreeMap<String, Entry>,
}

/// Paths whose content changed in one poll.
#[derive(Debug, Default)]
pub struct Changes {
    pub paths: Vec<String>,
}

impl Tree {
    pub fn scan(root: &Path) -> Self {
        let mut tree = Self {
            root: root.to_owned(),
            files: BTreeMap::new(),
        };
        tree.refresh();
        tree
    }

    /// Re-stat every watched file; re-hash only files whose stamp moved, so a save
    /// that leaves content unchanged is not reported.
    pub fn refresh(&mut self) -> Changes {
        let mut seen = BTreeMap::new();
        for root in classify::WATCHED_ROOTS {
            collect(&self.root, root, &mut seen);
        }
        let mut changes = Changes::default();
        let mut next = BTreeMap::new();
        for (path, (stamp, components)) in seen {
            let previous = self.files.remove(&path);
            let entry = match previous {
                Some(entry) if entry.stamp == stamp => entry,
                previous => {
                    let Ok(bytes) = std::fs::read(self.root.join(&path)) else {
                        // Mid-write or just deleted: keep the old view until it settles.
                        if let Some(previous) = previous {
                            next.insert(path, previous);
                        }
                        continue;
                    };
                    let content: [u8; 32] = Sha256::digest(&bytes).into();
                    if previous.is_none_or(|previous| previous.content != content) {
                        changes.paths.push(path.clone());
                    }
                    Entry {
                        stamp,
                        content,
                        components,
                    }
                }
            };
            next.insert(path, entry);
        }
        // Whatever is left was deleted.
        changes
            .paths
            .extend(std::mem::take(&mut self.files).into_keys());
        self.files = next;
        changes
    }

    pub fn fingerprints(&self) -> Fingerprints {
        let mut app = Sha256::new();
        let mut runner = Sha256::new();
        for (path, entry) in &self.files {
            for component in entry.components {
                let hasher = match component {
                    Component::App => &mut app,
                    Component::Runner => &mut runner,
                };
                hasher.update(path.as_bytes());
                hasher.update([0]);
                hasher.update(entry.content);
            }
        }
        Fingerprints {
            app: Digest(app.finalize().into()),
            runner: Digest(runner.finalize().into()),
        }
    }
}

fn collect(
    root: &Path,
    relative: &str,
    seen: &mut BTreeMap<String, (Stamp, &'static [Component])>,
) {
    let path = root.join(relative);
    let Ok(metadata) = std::fs::metadata(&path) else {
        return;
    };
    if metadata.is_dir() {
        let Ok(entries) = std::fs::read_dir(&path) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
            if is_dir && classify::skip_directory(&name) {
                continue;
            }
            collect(root, &format!("{relative}/{name}"), seen);
        }
        return;
    }
    let components = classify::classify(relative);
    if components.is_empty() {
        return;
    }
    let stamp = Stamp {
        modified: metadata.modified().ok(),
        len: metadata.len(),
    };
    seen.insert(relative.to_owned(), (stamp, components));
}

/// Coalesces an editor's burst of writes into one rebuild: fires once the tree has been
/// quiet for `quiet`, or after `limit` if changes keep arriving.
pub struct Debouncer {
    quiet: Duration,
    limit: Duration,
    first: Option<Instant>,
    last: Option<Instant>,
}

impl Debouncer {
    pub fn new(quiet: Duration, limit: Duration) -> Self {
        Self {
            quiet,
            limit,
            first: None,
            last: None,
        }
    }

    pub fn record(&mut self, now: Instant) {
        self.first.get_or_insert(now);
        self.last = Some(now);
    }

    pub fn due(&self, now: Instant) -> bool {
        match (self.first, self.last) {
            (Some(first), Some(last)) => {
                now.duration_since(last) >= self.quiet || now.duration_since(first) >= self.limit
            }
            _ => false,
        }
    }

    pub fn reset(&mut self) {
        self.first = None;
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Workspace(PathBuf);

    impl Workspace {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "tokenbubble-dev-watch-{name}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }

        fn write(&self, path: &str, content: &str) {
            let path = self.0.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
    }

    impl Drop for Workspace {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn app_edit_moves_only_the_app_fingerprint() {
        let workspace = Workspace::new("app");
        workspace.write("crates/tokenbubble-core/src/lib.rs", "fn a() {}");
        workspace.write("tools/tokenbubble-dev/src/lib.rs", "fn b() {}");
        let mut tree = Tree::scan(&workspace.0);
        let before = tree.fingerprints();

        workspace.write("crates/tokenbubble-core/src/lib.rs", "fn a() { 1; }");
        let changes = tree.refresh();
        let after = tree.fingerprints();

        assert_eq!(changes.paths, ["crates/tokenbubble-core/src/lib.rs"]);
        assert_ne!(before.app, after.app);
        assert_eq!(before.runner, after.runner);
    }

    #[test]
    fn rewriting_identical_content_and_build_output_are_not_changes() {
        let workspace = Workspace::new("noop");
        workspace.write("crates/tokenbubble-core/src/lib.rs", "fn a() {}");
        let mut tree = Tree::scan(&workspace.0);
        let before = tree.fingerprints();

        // Same bytes, new mtime (editors often save unchanged buffers).
        std::thread::sleep(Duration::from_millis(20));
        workspace.write("crates/tokenbubble-core/src/lib.rs", "fn a() {}");
        workspace.write("target/debug/tokenbubble-collect.exe", "binary");
        workspace.write("crates/tokenbubble-core/tests/codex.rs", "test");
        workspace.write("Spec.md", "doc");

        assert!(tree.refresh().paths.is_empty());
        assert_eq!(tree.fingerprints(), before);
    }

    #[test]
    fn deleting_and_restoring_a_file_round_trips_the_fingerprint() {
        let workspace = Workspace::new("delete");
        workspace.write("crates/tokenbubble-core/src/lib.rs", "fn a() {}");
        workspace.write("crates/tokenbubble-core/src/extra.rs", "fn b() {}");
        let mut tree = Tree::scan(&workspace.0);
        let before = tree.fingerprints();

        std::fs::remove_file(workspace.0.join("crates/tokenbubble-core/src/extra.rs")).unwrap();
        assert_eq!(
            tree.refresh().paths,
            ["crates/tokenbubble-core/src/extra.rs"]
        );
        assert_ne!(tree.fingerprints().app, before.app);

        workspace.write("crates/tokenbubble-core/src/extra.rs", "fn b() {}");
        tree.refresh();
        assert_eq!(tree.fingerprints(), before);
    }

    #[test]
    fn burst_of_writes_fires_once_after_quiet_period() {
        let start = Instant::now();
        let ms = Duration::from_millis;
        let mut debouncer = Debouncer::new(ms(250), ms(2000));
        assert!(!debouncer.due(start));

        // Editor writes temp file, renames, then formats: three events in 120 ms.
        debouncer.record(start);
        debouncer.record(start + ms(60));
        debouncer.record(start + ms(120));
        assert!(
            !debouncer.due(start + ms(300)),
            "quiet period restarts on each event"
        );
        assert!(debouncer.due(start + ms(370)));

        debouncer.reset();
        assert!(!debouncer.due(start + ms(10_000)));
    }

    #[test]
    fn continuous_changes_still_fire_at_the_limit() {
        let start = Instant::now();
        let ms = Duration::from_millis;
        let mut debouncer = Debouncer::new(ms(250), ms(2000));
        for step in 0..20 {
            debouncer.record(start + ms(step * 100));
        }
        assert!(!debouncer.due(start + ms(1900)));
        assert!(debouncer.due(start + ms(2000)));
    }
}
