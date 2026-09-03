use std::{fs, path::Path};

use anyhow::{Context, Result};
use farhelm_lifecycle::write_atomic;

const WORKER_FILES: &[(&str, &[u8])] = &[
    (
        "pyproject.toml",
        include_bytes!("../../farhelm-worker-codex/pyproject.toml"),
    ),
    (
        "uv.lock",
        include_bytes!("../../farhelm-worker-codex/uv.lock"),
    ),
    (
        "src/farhelm_worker_codex/__init__.py",
        include_bytes!("../../farhelm-worker-codex/src/farhelm_worker_codex/__init__.py"),
    ),
    (
        "src/farhelm_worker_codex/__main__.py",
        include_bytes!("../../farhelm-worker-codex/src/farhelm_worker_codex/__main__.py"),
    ),
    (
        "src/farhelm_worker_codex/framing.py",
        include_bytes!("../../farhelm-worker-codex/src/farhelm_worker_codex/framing.py"),
    ),
    (
        "src/farhelm_worker_codex/worker.py",
        include_bytes!("../../farhelm-worker-codex/src/farhelm_worker_codex/worker.py"),
    ),
];

pub fn materialize_worker(root: &Path) -> Result<()> {
    fs::create_dir_all(root)
        .with_context(|| format!("failed to create Worker runtime {}", root.display()))?;
    for (relative, contents) in WORKER_FILES {
        let path = root.join(relative);
        if fs::read(&path).ok().as_deref() == Some(*contents) {
            continue;
        }
        write_atomic(&path, contents, 0o600)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_worker_can_be_materialized() {
        let directory = tempfile::tempdir().unwrap();
        materialize_worker(directory.path()).unwrap();
        assert!(
            directory
                .path()
                .join("src/farhelm_worker_codex/__main__.py")
                .is_file()
        );
        assert!(directory.path().join("uv.lock").is_file());
    }
}
