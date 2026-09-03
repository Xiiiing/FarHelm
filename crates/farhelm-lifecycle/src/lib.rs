//! Shared native-program installation and systemd lifecycle primitives.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Output},
};

use anyhow::{Context, Result, bail, ensure};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceScope {
    System,
    User,
}

pub fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .is_ok_and(|output| output.status.success() && output.stdout == b"0\n")
}

pub fn systemctl(scope: ServiceScope, arguments: &[&str]) -> Result<Output> {
    let mut command = Command::new("systemctl");
    if scope == ServiceScope::User {
        command.arg("--user");
    }
    command
        .args(arguments)
        .output()
        .with_context(|| format!("failed to run systemctl {}", arguments.join(" ")))
}

pub fn systemctl_checked(scope: ServiceScope, arguments: &[&str]) -> Result<()> {
    let output = systemctl(scope, arguments)?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if detail.is_empty() {
            bail!("systemctl {} failed", arguments.join(" "));
        }
        bail!("systemctl {} failed: {detail}", arguments.join(" "));
    }
    Ok(())
}

pub fn service_is_active(scope: ServiceScope, unit: &str) -> Result<bool> {
    Ok(systemctl(scope, &["is-active", "--quiet", unit])?
        .status
        .success())
}

pub fn write_atomic(path: &Path, contents: &[u8], mode: u32) -> Result<()> {
    let parent = path
        .parent()
        .context("managed file has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let temporary = sibling(path, "new")?;
    remove_regular_file_if_exists(&temporary)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&temporary)
        .with_context(|| format!("failed to create {}", temporary.display()))?;
    output
        .write_all(contents)
        .with_context(|| format!("failed to write {}", temporary.display()))?;
    output
        .sync_all()
        .with_context(|| format!("failed to sync {}", temporary.display()))?;
    drop(output);
    fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))?;
    fs::rename(&temporary, path)
        .with_context(|| format!("failed to activate {}", path.display()))?;
    sync_directory(parent)
}

pub fn install_binary(source: &Path, destination: &Path, previous: &Path) -> Result<bool> {
    ensure!(source.is_file(), "source executable is missing");
    if same_file(source, destination)? {
        return Ok(false);
    }
    let parent = destination
        .parent()
        .context("binary destination has no parent directory")?;
    ensure!(
        previous.parent() == Some(parent),
        "previous binary must share the destination directory"
    );
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let staged = sibling(destination, "new")?;
    copy_executable(source, &staged)?;

    let replaced = destination.exists();
    if replaced {
        ensure!(
            destination.is_file(),
            "managed binary is not a regular file"
        );
        remove_regular_file_if_exists(previous)?;
        fs::rename(destination, previous).context("failed to preserve previous binary")?;
    }
    if let Err(error) = fs::rename(&staged, destination) {
        if replaced && previous.is_file() {
            let _ = fs::rename(previous, destination);
        }
        return Err(error).context("failed to activate managed binary");
    }
    sync_directory(parent)?;
    Ok(replaced)
}

pub fn swap_with_previous(destination: &Path, previous: &Path) -> Result<()> {
    ensure!(destination.is_file(), "current managed binary is missing");
    ensure!(
        previous.is_file(),
        "no previous FarHelm binary is available"
    );
    let parent = destination
        .parent()
        .context("binary destination has no parent directory")?;
    ensure!(
        previous.parent() == Some(parent),
        "previous binary must share the destination directory"
    );
    let displaced = sibling(destination, "displaced")?;
    remove_regular_file_if_exists(&displaced)?;
    fs::rename(destination, &displaced).context("failed to stage current binary")?;
    if let Err(error) = fs::rename(previous, destination) {
        let _ = fs::rename(&displaced, destination);
        return Err(error).context("failed to restore previous binary");
    }
    if let Err(error) = fs::rename(&displaced, previous) {
        let _ = fs::rename(destination, previous);
        let _ = fs::rename(&displaced, destination);
        return Err(error).context("failed to preserve displaced binary");
    }
    sync_directory(parent)
}

pub fn remove_regular_file_if_exists(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure!(
                metadata.file_type().is_file(),
                "refusing to remove non-file"
            );
            fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to inspect {}", path.display())),
    }
}

pub fn read_regular_file(path: &Path) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    ensure!(
        metadata.file_type().is_file(),
        "managed path is not a regular file"
    );
    let mut input = File::open(path)?;
    let mut contents = Vec::new();
    input.read_to_end(&mut contents)?;
    Ok(contents)
}

fn copy_executable(source: &Path, destination: &Path) -> Result<()> {
    remove_regular_file_if_exists(destination)?;
    let mut input =
        File::open(source).with_context(|| format!("failed to open {}", source.display()))?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o755)
        .open(destination)
        .with_context(|| format!("failed to create {}", destination.display()))?;
    std::io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    fs::set_permissions(destination, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

fn sibling(path: &Path, suffix: &str) -> Result<PathBuf> {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .context("managed filename is not valid UTF-8")?;
    Ok(path.with_file_name(format!(".{name}.{suffix}")))
}

fn same_file(left: &Path, right: &Path) -> Result<bool> {
    if !right.exists() {
        return Ok(false);
    }
    Ok(left.canonicalize()? == right.canonicalize()?)
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_activation_and_rollback_are_atomic_at_the_file_boundary() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let destination = directory.path().join("farhelm-agent");
        let previous = directory.path().join("farhelm-agent.previous");
        write_atomic(&source, b"new", 0o755).unwrap();
        write_atomic(&destination, b"old", 0o755).unwrap();

        assert!(install_binary(&source, &destination, &previous).unwrap());
        assert_eq!(fs::read(&destination).unwrap(), b"new");
        assert_eq!(fs::read(&previous).unwrap(), b"old");

        swap_with_previous(&destination, &previous).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"old");
        assert_eq!(fs::read(&previous).unwrap(), b"new");
    }

    #[test]
    fn atomic_write_replaces_a_regular_file_and_sets_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        write_atomic(&path, b"first", 0o600).unwrap();
        write_atomic(&path, b"second", 0o640).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }
}
