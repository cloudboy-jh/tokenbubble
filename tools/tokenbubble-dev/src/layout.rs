//! Where the development runtime lives: `<target>/tokenbubble-dev`, so `cargo clean`
//! resets it and nothing touches an installed TokenBubble.

use crate::APP_BINARY;
use std::fs::File;
use std::path::{Path, PathBuf};

pub struct Layout {
    pub workspace: PathBuf,
    /// `<target>/tokenbubble-dev`
    pub root: PathBuf,
    /// Runnable binaries. Cargo never writes here, so a running app cannot block builds.
    pub bin: PathBuf,
}

impl Layout {
    pub fn discover() -> Result<Self, String> {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let workspace = simplify(
            workspace
                .canonicalize()
                .map_err(|error| format!("cannot locate workspace: {error}"))?,
        );
        let target = match std::env::var_os("CARGO_TARGET_DIR") {
            Some(path) => workspace.join(path),
            None => workspace.join("target"),
        };
        Ok(Self::at(workspace, &target))
    }

    pub fn at(workspace: PathBuf, target: &Path) -> Self {
        let root = target.join("tokenbubble-dev");
        Self {
            workspace,
            bin: root.join("bin"),
            root,
        }
    }

    pub fn app_exe(&self) -> PathBuf {
        self.bin.join(executable(APP_BINARY))
    }

    pub fn prepare(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.bin)
            .map_err(|error| format!("{}: {error}", self.bin.display()))
    }

    /// One runner per checkout: a second one would fight over the same binary.
    pub fn lock_runner(&self) -> Result<File, String> {
        let path = self.root.join("runner.lock");
        let file = File::create(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        match file.try_lock() {
            Ok(()) => Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => {
                Err("another `cargo dev` is already running for this checkout".into())
            }
            Err(std::fs::TryLockError::Error(error)) => Err(error.to_string()),
        }
    }
}

/// The `executable` name with the platform suffix.
pub fn executable(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// Drop the Windows `\\?\` prefix so paths stay readable and accepted everywhere.
pub fn simplify(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    if let Some(rest) = path.to_str().and_then(|text| text.strip_prefix(r"\\?\"))
        && !rest.starts_with("UNC\\")
    {
        return PathBuf::from(rest);
    }
    path
}
