use std::{
    fs,
    io::Read,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail, ensure};
use farhelm_lifecycle::write_atomic;
use farhelm_updater::Updater;
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};

const MAX_RUNTIME_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_RUNTIME_UNPACKED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const CODEX_SDK_VERSION: &str = "0.147.0";

const WORKER_FILES: &[(&str, &[u8])] = &[
    (
        "README.md",
        include_bytes!("../../farhelm-worker-codex/README.md"),
    ),
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

pub async fn ensure_worker_environment(root: &Path, _configured_python: &str) -> Result<PathBuf> {
    materialize_worker(root)?;
    let managed_python = root.join(".venv/bin/python");
    if validate_runtime(root, &managed_python).is_ok() {
        return Ok(managed_python);
    }

    if let Some(archive) = std::env::var_os("FARHELM_CODEX_RUNTIME_ARCHIVE") {
        let archive = PathBuf::from(archive);
        let expected_size = std::env::var("FARHELM_CODEX_RUNTIME_SIZE")
            .context("offline runtime import requires FARHELM_CODEX_RUNTIME_SIZE")?
            .parse::<u64>()
            .context("FARHELM_CODEX_RUNTIME_SIZE is invalid")?;
        let expected_digest = parse_sha256(
            &std::env::var("FARHELM_CODEX_RUNTIME_SHA256")
                .context("offline runtime import requires FARHELM_CODEX_RUNTIME_SHA256")?,
        )?;
        verify_local_archive(&archive, expected_size, expected_digest)?;
        install_runtime_archive(root, &archive)?;
    } else {
        let updater = Updater::new()?;
        let candidate = updater.runtime(farhelm_core::PRODUCT_VERSION).await?;
        let archive = updater.download_runtime(&candidate).await?;
        install_runtime_archive(root, &archive.path)?;
    }

    validate_runtime(root, &managed_python)?;
    Ok(managed_python)
}

fn verify_local_archive(path: &Path, expected_size: u64, expected_digest: [u8; 32]) -> Result<()> {
    ensure!(path.is_file(), "offline Codex runtime archive is missing");
    ensure!(
        expected_size > 0 && expected_size <= MAX_RUNTIME_ARCHIVE_BYTES,
        "offline Codex runtime size is outside the allowed range"
    );
    let metadata = fs::metadata(path)?;
    ensure!(
        metadata.len() == expected_size,
        "offline Codex runtime length does not match trusted metadata"
    );
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual: [u8; 32] = hasher.finalize().into();
    ensure!(
        actual == expected_digest,
        "offline Codex runtime SHA-256 does not match trusted metadata"
    );
    Ok(())
}

fn install_runtime_archive(root: &Path, archive_path: &Path) -> Result<()> {
    let parent = root.parent().context("Worker runtime path has no parent")?;
    fs::create_dir_all(parent)?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    let staging = tempfile::Builder::new()
        .prefix(".codex-runtime-")
        .tempdir_in(parent)?;
    let archive = fs::File::open(archive_path)?;
    let mut archive = tar::Archive::new(GzDecoder::new(archive));
    let mut unpacked = 0_u64;
    for entry in archive.entries()? {
        let mut entry = entry?;
        unpacked = unpacked
            .checked_add(entry.size())
            .context("Codex runtime unpacked size overflowed")?;
        ensure!(
            unpacked <= MAX_RUNTIME_UNPACKED_BYTES,
            "Codex runtime expands beyond its size limit"
        );
        ensure!(
            entry.unpack_in(staging.path())?,
            "Codex runtime contains an unsafe path"
        );
    }
    sanitize_permissions(staging.path())?;
    validate_runtime(staging.path(), &staging.path().join(".venv/bin/python"))?;

    if root.exists() {
        let metadata = fs::symlink_metadata(root)?;
        ensure!(metadata.is_dir(), "Worker runtime path is not a directory");
        fs::remove_dir_all(root)?;
    }
    let staged_path = staging.keep();
    fs::rename(&staged_path, root).inspect_err(|_| {
        let _ = fs::remove_dir_all(&staged_path);
    })?;
    Ok(())
}

fn sanitize_permissions(path: &Path) -> Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
            sanitize_permissions(&path)?;
        } else if metadata.is_file() {
            let mode = if metadata.permissions().mode() & 0o111 == 0 {
                0o600
            } else {
                0o700
            };
            fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
        }
    }
    Ok(())
}

fn validate_runtime(root: &Path, python: &Path) -> Result<()> {
    ensure!(python.is_file(), "managed Codex runtime omitted Python");
    let status = Command::new(python)
        .arg("-c")
        .arg(format!(
            "import importlib.metadata as m,sys; assert sys.version_info[:2] == (3, 12); assert m.version('openai-codex') == '{CODEX_SDK_VERSION}'; assert m.version('farhelm-worker-codex') == '{}'; import openai_codex,farhelm_worker_codex",
            farhelm_core::PRODUCT_VERSION
        ))
        .env("PYTHONPATH", root.join("src"))
        .status()
        .context("failed to execute managed Codex runtime")?;
    ensure!(
        status.success(),
        "managed Codex runtime failed version or import validation"
    );
    Ok(())
}

fn parse_sha256(value: &str) -> Result<[u8; 32]> {
    let value = value.strip_prefix("sha256:").unwrap_or(value);
    if value.len() != 64 {
        bail!("FARHELM_CODEX_RUNTIME_SHA256 must contain 64 hexadecimal characters");
    }
    let mut digest = [0_u8; 32];
    for (index, pair) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        digest[index] = u8::from_str_radix(std::str::from_utf8(pair)?, 16)
            .context("FARHELM_CODEX_RUNTIME_SHA256 is invalid")?;
    }
    Ok(digest)
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
        assert!(directory.path().join("README.md").is_file());
    }

    #[test]
    fn offline_digest_parser_is_strict() {
        assert_eq!(parse_sha256(&"ab".repeat(32)).unwrap(), [0xab; 32]);
        assert!(parse_sha256("ab").is_err());
        assert!(parse_sha256(&"zz".repeat(32)).is_err());
    }
}
