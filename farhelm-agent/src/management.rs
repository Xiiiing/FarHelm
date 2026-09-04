use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command, time::Duration};

use anyhow::{Context, Result, ensure};
use farhelm_core::PRODUCT_VERSION;
use farhelm_lifecycle::{
    ServiceScope, install_binary, is_root, remove_regular_file_if_exists, service_is_active,
    swap_with_previous, systemctl_checked, write_atomic,
};
use farhelm_updater::{Role, Updater};

use crate::{
    config::{AgentFileConfig, AgentPaths},
    resources::ensure_worker_environment,
};

pub const UNIT_NAME: &str = "farhelm-agent.service";

pub async fn install(no_service: bool) -> Result<()> {
    require_user()?;
    let paths = AgentPaths::discover()?;
    let no_service = no_service || legacy_foreground_mode(&paths)?;
    let old_unit = fs::read(&paths.unit).ok();
    let had_config = paths.config.is_file();
    let had_data = paths.data.is_dir();
    let mut config = if paths.config.is_file() {
        AgentFileConfig::load(&paths.config)?
    } else {
        AgentFileConfig::from_install_input(&paths)?
    };
    let _ = farhelm_lifecycle::systemctl(ServiceScope::User, &["stop", UNIT_NAME]);
    migrate_legacy_database(&paths, &mut config)?;

    fs::create_dir_all(&paths.data)
        .with_context(|| format!("failed to create {}", paths.data.display()))?;
    set_mode(&paths.data, 0o700)?;
    if let Some(parent) = paths.database.parent() {
        fs::create_dir_all(parent)?;
        set_mode(parent, 0o700)?;
    }
    config.worker.python = ensure_worker_environment(&paths.worker, &config.worker.python)
        .await?
        .to_string_lossy()
        .into_owned();
    let current = std::env::current_exe().context("failed to locate current Agent executable")?;
    let replaced_binary = install_binary(&current, &paths.binary, &paths.previous)?;
    write_atomic(&paths.config, config.encode()?.as_bytes(), 0o600)?;
    set_mode(&paths.config, 0o600)?;

    if no_service {
        let _ = farhelm_lifecycle::systemctl(ServiceScope::User, &["disable", "--now", UNIT_NAME]);
        remove_regular_file_if_exists(&paths.unit)?;
        let _ = farhelm_lifecycle::systemctl(ServiceScope::User, &["daemon-reload"]);
        cleanup_legacy(&paths)?;
        println!("FarHelm Agent {PRODUCT_VERSION} installed without a systemd service.");
        println!(
            "Run it with: {} run --config {}",
            paths.binary.display(),
            paths.config.display()
        );
        print_path_hint(&paths.binary);
        return Ok(());
    }

    let unit = unit_contents(&paths)?;
    write_atomic(&paths.unit, unit.as_bytes(), 0o600)?;
    let activation = async {
        systemctl_checked(ServiceScope::User, &["daemon-reload"])?;
        systemctl_checked(ServiceScope::User, &["enable", "--now", UNIT_NAME])?;
        wait_until_active(Duration::from_secs(15)).await
    }
    .await;
    if let Err(error) = activation {
        restore_failed_install(
            &paths,
            old_unit.as_deref(),
            replaced_binary,
            had_config,
            had_data,
        )?;
        return Err(error).context("Agent installation failed; previous service restored");
    }
    cleanup_legacy(&paths)?;
    warn_if_linger_disabled();
    println!("FarHelm Agent {PRODUCT_VERSION} installed and active.");
    println!("Program: {}", paths.binary.display());
    println!("Configuration: {}", paths.config.display());
    print_path_hint(&paths.binary);
    Ok(())
}

fn restore_failed_install(
    paths: &AgentPaths,
    old_unit: Option<&[u8]>,
    replaced_binary: bool,
    had_config: bool,
    had_data: bool,
) -> Result<()> {
    if let Some(contents) = old_unit {
        write_atomic(&paths.unit, contents, 0o600)?;
    } else {
        remove_regular_file_if_exists(&paths.unit)?;
    }
    if replaced_binary && paths.previous.is_file() {
        swap_with_previous(&paths.binary, &paths.previous)?;
    } else {
        remove_regular_file_if_exists(&paths.binary)?;
    }
    if !had_config {
        remove_regular_file_if_exists(&paths.config)?;
    }
    if !had_data {
        remove_managed_data(&paths.data, "farhelm")?;
    }
    let _ = farhelm_lifecycle::systemctl(ServiceScope::User, &["daemon-reload"]);
    if old_unit.is_some() {
        let _ = farhelm_lifecycle::systemctl(ServiceScope::User, &["restart", UNIT_NAME]);
    }
    Ok(())
}

pub fn service_action(action: &str) -> Result<()> {
    require_user()?;
    systemctl_checked(ServiceScope::User, &[action, UNIT_NAME])?;
    println!("FarHelm Agent service {action} completed.");
    Ok(())
}

pub async fn restart() -> Result<()> {
    require_user()?;
    restart_and_check().await?;
    println!("FarHelm Agent service restart completed and is active.");
    Ok(())
}

pub fn status() -> Result<()> {
    require_user()?;
    if service_is_active(ServiceScope::User, UNIT_NAME)? {
        println!("FarHelm Agent {PRODUCT_VERSION} is active.");
        Ok(())
    } else {
        anyhow::bail!("FarHelm Agent service is not active")
    }
}

pub fn doctor(config_path: Option<&Path>) -> Result<(AgentFileConfig, AgentPaths)> {
    require_user()?;
    let paths = AgentPaths::discover()?;
    let path = config_path.unwrap_or(&paths.config);
    let config = AgentFileConfig::load(path)?;
    let mode = fs::metadata(path)?.permissions().mode() & 0o777;
    ensure!(mode & 0o077 == 0, "Agent config must use mode 0600");
    let python_ok = Command::new(&config.worker.python)
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    ensure!(
        python_ok,
        "Worker Python `{}` is unavailable",
        config.worker.python
    );
    println!("FarHelm Agent {PRODUCT_VERSION} configuration is valid.");
    println!("Worker runtime: {}", paths.worker.display());
    Ok((config, paths))
}

pub async fn update(check: bool, requested: Option<&str>, allow_major: bool) -> Result<()> {
    require_user()?;
    let updater = Updater::new()?;
    let Some(candidate) = updater
        .check(Role::Agent, PRODUCT_VERSION, requested, allow_major)
        .await?
    else {
        println!("FarHelm Agent {PRODUCT_VERSION} is up to date.");
        return Ok(());
    };
    if check {
        println!(
            "FarHelm Agent {} is available (current {}).",
            candidate.version, PRODUCT_VERSION
        );
        return Ok(());
    }
    let paths = AgentPaths::discover()?;
    ensure!(paths.binary.is_file(), "FarHelm Agent is not installed");
    println!("Downloading verified {}...", candidate.asset_name());
    let executable = updater.download(Role::Agent, &candidate).await?;
    let mut config = AgentFileConfig::load(&paths.config)?;
    config.worker.python = ensure_worker_environment(&paths.worker, &config.worker.python)
        .await?
        .to_string_lossy()
        .into_owned();
    write_atomic(&paths.config, config.encode()?.as_bytes(), 0o600)?;
    install_binary(&executable.path, &paths.binary, &paths.previous)?;
    if let Err(error) = restart_and_check().await {
        swap_with_previous(&paths.binary, &paths.previous)?;
        systemctl_checked(ServiceScope::User, &["restart", UNIT_NAME])?;
        return Err(error).context("Agent update failed; previous binary restored");
    }
    println!("FarHelm Agent updated to {}.", executable.version);
    Ok(())
}

pub async fn rollback() -> Result<()> {
    require_user()?;
    let paths = AgentPaths::discover()?;
    swap_with_previous(&paths.binary, &paths.previous)?;
    if let Err(error) = restart_and_check().await {
        swap_with_previous(&paths.binary, &paths.previous)?;
        systemctl_checked(ServiceScope::User, &["restart", UNIT_NAME])?;
        return Err(error).context("Agent rollback failed; original binary restored");
    }
    println!("FarHelm Agent rolled back successfully.");
    Ok(())
}

pub fn uninstall(keep_data: bool) -> Result<()> {
    require_user()?;
    let paths = AgentPaths::discover()?;
    let _ = farhelm_lifecycle::systemctl(ServiceScope::User, &["disable", "--now", UNIT_NAME]);
    remove_regular_file_if_exists(&paths.unit)?;
    remove_regular_file_if_exists(&paths.binary)?;
    remove_regular_file_if_exists(&paths.previous)?;
    if !keep_data {
        remove_regular_file_if_exists(&paths.config)?;
        remove_managed_data(&paths.data, "farhelm")?;
        remove_managed_data(&paths.legacy_root, "farhelm-agent")?;
        remove_empty_parent(&paths.config)?;
    }
    let _ = farhelm_lifecycle::systemctl(ServiceScope::User, &["daemon-reload"]);
    println!("FarHelm Agent uninstalled.");
    if keep_data {
        println!("Configuration and data were kept.");
    }
    Ok(())
}

async fn restart_and_check() -> Result<()> {
    systemctl_checked(ServiceScope::User, &["restart", UNIT_NAME])?;
    wait_until_active(Duration::from_secs(15)).await
}

async fn wait_until_active(timeout: Duration) -> Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if service_is_active(ServiceScope::User, UNIT_NAME)? {
            return Ok(());
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "Agent failed its service health check"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn unit_contents(paths: &AgentPaths) -> Result<String> {
    let binary = systemd_quote(&paths.binary)?;
    let config = systemd_quote(&paths.config)?;
    let data = systemd_quote(&paths.data)?;
    Ok(format!(
        "[Unit]\nDescription=FarHelm Agent\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=simple\nExecStart={binary} run --config {config}\nRestart=on-failure\nRestartSec=3\nEnvironment=RUST_LOG=info\nNoNewPrivileges=true\nPrivateTmp=true\nProtectSystem=strict\nReadWritePaths={data}\nTasksMax=512\n\n[Install]\nWantedBy=default.target\n"
    ))
}

fn systemd_quote(path: &Path) -> Result<String> {
    let value = path.to_str().context("managed path is not valid UTF-8")?;
    ensure!(
        !value.chars().any(char::is_control),
        "managed path contains control characters"
    );
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

fn migrate_legacy_database(paths: &AgentPaths, config: &mut AgentFileConfig) -> Result<()> {
    if !config.agent.database.starts_with(&paths.legacy_root)
        || !config.agent.database.is_file()
        || paths.database.is_file()
    {
        if paths.database.is_file() {
            config.agent.database = paths.database.clone();
        }
        return Ok(());
    }
    if let Some(parent) = paths.database.parent() {
        fs::create_dir_all(parent)?;
    }
    let source = rusqlite::Connection::open(&config.agent.database)?;
    source.execute(
        "VACUUM INTO ?1",
        [paths.database.to_string_lossy().as_ref()],
    )?;
    config.agent.database = paths.database.clone();
    Ok(())
}

fn cleanup_legacy(paths: &AgentPaths) -> Result<()> {
    if paths.legacy_root.exists() {
        remove_managed_data(&paths.legacy_root, "farhelm-agent")?;
    }
    Ok(())
}

fn legacy_foreground_mode(paths: &AgentPaths) -> Result<bool> {
    if std::env::var("FARHELM_UPGRADE").as_deref() != Ok("1") {
        return Ok(false);
    }
    let mode_path = paths.legacy_root.join("INSTALL_MODE");
    if !mode_path.is_file() {
        return Ok(false);
    }
    match fs::read_to_string(&mode_path)?.trim() {
        "service" => Ok(false),
        "foreground" => Ok(true),
        _ => anyhow::bail!("legacy Agent installation mode is invalid"),
    }
}

fn remove_managed_data(path: &Path, expected_name: &str) -> Result<()> {
    ensure!(path.is_absolute(), "managed data path must be absolute");
    ensure!(
        path.file_name().is_some_and(|name| name == expected_name),
        "unsafe managed data path"
    );
    if path.exists() {
        let metadata = fs::symlink_metadata(path)?;
        ensure!(
            metadata.file_type().is_dir(),
            "managed data path is not a directory"
        );
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

fn remove_empty_parent(config: &Path) -> Result<()> {
    if let Some(parent) = config.parent() {
        match fs::remove_dir(parent) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn set_mode(path: &Path, mode: u32) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

fn print_path_hint(binary: &Path) {
    let Some(parent) = binary.parent() else {
        return;
    };
    let on_path = std::env::var_os("PATH")
        .is_some_and(|value| std::env::split_paths(&value).any(|entry| entry == parent));
    if !on_path {
        eprintln!(
            "Warning: {} is not in PATH; use {} until your shell PATH is updated.",
            parent.display(),
            binary.display()
        );
    }
}

fn require_user() -> Result<()> {
    ensure!(
        !is_root(),
        "Agent service management must be run without sudo/root"
    );
    Ok(())
}

fn warn_if_linger_disabled() {
    let Some(user) = std::env::var_os("USER") else {
        return;
    };
    let output = Command::new("loginctl")
        .arg("show-user")
        .arg(user)
        .args(["--property=Linger", "--value"])
        .output();
    if output.is_ok_and(|value| value.status.success() && value.stdout != b"yes\n") {
        eprintln!(
            "Warning: systemd linger is disabled; the Agent may stop after logout or reboot."
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_uses_stable_binary_and_toml_config() {
        let paths = AgentPaths {
            binary: "/tmp/home/.local/bin/farhelm-agent".into(),
            previous: "/tmp/home/.local/bin/farhelm-agent.previous".into(),
            config: "/tmp/home/.config/farhelm/agent.toml".into(),
            data: "/tmp/home/.local/share/farhelm".into(),
            database: "/tmp/home/.local/share/farhelm/state/agent.db".into(),
            worker: "/tmp/home/.local/share/farhelm/runtime/codex-worker/0.4.1".into(),
            unit: "/tmp/home/.config/systemd/user/farhelm-agent.service".into(),
            legacy_root: "/tmp/home/.local/share/farhelm-agent".into(),
        };
        let unit = unit_contents(&paths).unwrap();
        assert!(unit.contains("/tmp/home/.local/bin/farhelm-agent\" run --config"));
        assert!(unit.contains("/tmp/home/.config/farhelm/agent.toml"));
        assert!(!unit.contains("current/run.sh"));
    }
}
