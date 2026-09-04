use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command, time::Duration};

use anyhow::{Context, Result, ensure};
use farhelm_core::PRODUCT_VERSION;
use farhelm_lifecycle::{
    ServiceScope, install_binary, is_root, remove_regular_file_if_exists, service_is_active,
    swap_with_previous, systemctl_checked, write_atomic,
};
use farhelm_protocol::{FARHELM_PROTOCOL, HealthResponse, HealthStatus};
use farhelm_updater::{Role, Updater};

use crate::config::{DEFAULT_CONFIG_PATH, HubFileConfig, LEGACY_CONFIG_PATH, hash_secret};

pub const BINARY_PATH: &str = "/usr/local/bin/farhelm-hub";
pub const PREVIOUS_PATH: &str = "/usr/local/bin/farhelm-hub.previous";
pub const UNIT_PATH: &str = "/etc/systemd/system/farhelm-hub.service";
pub const UNIT_NAME: &str = "farhelm-hub.service";
const LEGACY_ROOT: &str = "/opt/farhelm-hub";
const LEGACY_CTL: &str = "/usr/local/bin/farhelmctl";
const LEGACY_DATA_ROOT: &str = "/var/lib/farhelm-hub";
const LEGACY_CADDY_EXAMPLE: &str = "/etc/farhelm/Caddyfile.example";
const DATA_ROOT: &str = "/var/lib/farhelm";

const UNIT: &str = r#"[Unit]
Description=FarHelm Control Hub
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=farhelm-hub
Group=farhelm-hub
ExecStart=/usr/local/bin/farhelm-hub serve --config /etc/farhelm/hub.toml
Restart=on-failure
RestartSec=3
NoNewPrivileges=true
PrivateTmp=true
ProtectHome=true
ProtectSystem=strict
ReadWritePaths=/var/lib/farhelm
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
CapabilityBoundingSet=
LockPersonality=true
MemoryDenyWriteExecute=true
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX

[Install]
WantedBy=multi-user.target
"#;

pub async fn install(no_start: bool) -> Result<()> {
    require_root()?;
    ensure!(
        farhelm_hub::has_embedded_console(),
        "this development build has no embedded Console; install a formal FarHelm release"
    );
    let config_path = Path::new(DEFAULT_CONFIG_PATH);
    let old_unit = fs::read(UNIT_PATH).ok();
    let old_config = fs::read(config_path).ok();
    let had_data = Path::new(DATA_ROOT).is_dir();
    let had_identity = service_identity_exists()?;
    let mut config = if config_path.is_file() {
        HubFileConfig::load(config_path)?
    } else {
        HubFileConfig::from_install_input()?
    };
    config.ensure_modern_auth()?;

    let _ = farhelm_lifecycle::systemctl(ServiceScope::System, &["stop", UNIT_NAME]);
    ensure_service_identity()?;
    fs::create_dir_all(DATA_ROOT).context("failed to create Hub data directory")?;
    set_mode(Path::new(DATA_ROOT), 0o750)?;
    chown("farhelm-hub:farhelm-hub", Path::new(DATA_ROOT))?;
    migrate_legacy_database(&mut config)?;

    let current = std::env::current_exe().context("failed to locate current Hub executable")?;
    let replaced_binary =
        install_binary(&current, Path::new(BINARY_PATH), Path::new(PREVIOUS_PATH))?;
    fs::create_dir_all("/etc/farhelm").context("failed to create Hub config directory")?;
    set_mode(Path::new("/etc/farhelm"), 0o750)?;
    chown("root:farhelm-hub", Path::new("/etc/farhelm"))?;
    write_atomic(config_path, config.encode()?.as_bytes(), 0o640)?;
    set_mode(config_path, 0o640)?;
    chown("root:farhelm-hub", config_path)?;
    write_atomic(Path::new(UNIT_PATH), UNIT.as_bytes(), 0o644)?;
    systemctl_checked(ServiceScope::System, &["daemon-reload"])?;

    if no_start {
        println!("FarHelm Hub {PRODUCT_VERSION} installed without starting the service.");
        println!("Start it with: sudo farhelm-hub start");
        return Ok(());
    }
    let activation = async {
        systemctl_checked(ServiceScope::System, &["enable", "--now", UNIT_NAME])?;
        wait_for_health(&config, Duration::from_secs(15)).await
    }
    .await;
    if let Err(error) = activation {
        restore_failed_install(
            old_unit.as_deref(),
            old_config.as_deref(),
            replaced_binary,
            had_data,
            had_identity,
        )?;
        return Err(error).context("Hub installation failed; previous service restored");
    }
    cleanup_legacy()?;
    println!("FarHelm Hub {PRODUCT_VERSION} installed and healthy.");
    println!("Configuration: {DEFAULT_CONFIG_PATH}");
    Ok(())
}

fn restore_failed_install(
    old_unit: Option<&[u8]>,
    old_config: Option<&[u8]>,
    replaced_binary: bool,
    had_data: bool,
    had_identity: bool,
) -> Result<()> {
    if let Some(contents) = old_unit {
        write_atomic(Path::new(UNIT_PATH), contents, 0o644)?;
    } else {
        remove_regular_file_if_exists(Path::new(UNIT_PATH))?;
    }
    if replaced_binary && Path::new(PREVIOUS_PATH).is_file() {
        swap_with_previous(Path::new(BINARY_PATH), Path::new(PREVIOUS_PATH))?;
    } else {
        remove_regular_file_if_exists(Path::new(BINARY_PATH))?;
    }
    if let Some(contents) = old_config {
        write_atomic(Path::new(DEFAULT_CONFIG_PATH), contents, 0o640)?;
        chown("root:farhelm-hub", Path::new(DEFAULT_CONFIG_PATH))?;
    } else {
        remove_regular_file_if_exists(Path::new(DEFAULT_CONFIG_PATH))?;
    }
    if !had_data {
        remove_managed_directory(Path::new(DATA_ROOT), "/var/lib", "farhelm")?;
    }
    let _ = farhelm_lifecycle::systemctl(ServiceScope::System, &["daemon-reload"]);
    if old_unit.is_some() {
        let _ = farhelm_lifecycle::systemctl(ServiceScope::System, &["restart", UNIT_NAME]);
    }
    if !had_identity {
        let _ = Command::new("userdel").arg("farhelm-hub").status();
        let _ = Command::new("groupdel").arg("farhelm-hub").status();
    }
    Ok(())
}

pub fn service_action(action: &str) -> Result<()> {
    require_root()?;
    systemctl_checked(ServiceScope::System, &[action, UNIT_NAME])?;
    println!("FarHelm Hub service {action} completed.");
    Ok(())
}

pub async fn restart() -> Result<()> {
    require_root()?;
    let config = HubFileConfig::load(Path::new(DEFAULT_CONFIG_PATH))?;
    restart_and_check(&config).await?;
    println!("FarHelm Hub service restart completed and is healthy.");
    Ok(())
}

pub async fn reset_password() -> Result<()> {
    require_root()?;
    let path = Path::new(DEFAULT_CONFIG_PATH);
    let mut config = HubFileConfig::load(path)?;
    let password = rpassword::prompt_password("New FarHelm administrator password: ")?;
    let confirmation = rpassword::prompt_password("Repeat the new password: ")?;
    ensure!(password == confirmation, "passwords do not match");
    ensure!(
        password.len() >= 12,
        "password must contain at least 12 characters"
    );
    config.admin.password = None;
    config.admin.password_hash = Some(hash_secret(&password)?);
    write_atomic(path, config.encode()?.as_bytes(), 0o640)?;
    set_mode(path, 0o640)?;
    chown("root:farhelm-hub", path)?;
    systemctl_checked(ServiceScope::System, &["stop", UNIT_NAME])?;
    let revoke_result = (|| {
        let state = farhelm_hub::AppState::new(config.runtime())?;
        state.revoke_browser_sessions()
    })();
    if let Err(error) = revoke_result {
        let _ = systemctl_checked(ServiceScope::System, &["start", UNIT_NAME]);
        return Err(error).context("failed to revoke browser sessions");
    }
    for database_file in [
        config.hub.database.clone(),
        Path::new(&format!("{}-wal", config.hub.database.display())).to_owned(),
        Path::new(&format!("{}-shm", config.hub.database.display())).to_owned(),
    ] {
        if database_file.exists() {
            chown("farhelm-hub:farhelm-hub", &database_file)?;
        }
    }
    restart_and_check(&config).await?;
    println!("FarHelm administrator password changed; all browser sessions were revoked.");
    Ok(())
}

pub fn status() -> Result<()> {
    require_root()?;
    if service_is_active(ServiceScope::System, UNIT_NAME)? {
        println!("FarHelm Hub {PRODUCT_VERSION} is active.");
        Ok(())
    } else {
        anyhow::bail!("FarHelm Hub service is not active")
    }
}

pub fn doctor(config_path: &Path) -> Result<()> {
    let config = HubFileConfig::load(config_path)?;
    ensure!(
        farhelm_hub::has_embedded_console() || config.hub.console_dir.is_some(),
        "no Console is available"
    );
    ensure!(
        config.hub.database.parent().is_some(),
        "hub.database has no parent directory"
    );
    if config_path == Path::new(DEFAULT_CONFIG_PATH) {
        let mode = fs::metadata(config_path)?.permissions().mode() & 0o777;
        ensure!(
            mode & 0o007 == 0,
            "Hub config must not be readable by other users"
        );
    }
    println!("FarHelm Hub {PRODUCT_VERSION} configuration is valid.");
    Ok(())
}

pub async fn update(check: bool, requested: Option<&str>, allow_major: bool) -> Result<()> {
    require_root()?;
    let updater = Updater::new()?;
    let Some(candidate) = updater
        .check(Role::Hub, PRODUCT_VERSION, requested, allow_major)
        .await?
    else {
        println!("FarHelm Hub {PRODUCT_VERSION} is up to date.");
        return Ok(());
    };
    if check {
        println!(
            "FarHelm Hub {} is available (current {}).",
            candidate.version, PRODUCT_VERSION
        );
        return Ok(());
    }
    ensure!(
        Path::new(BINARY_PATH).is_file(),
        "FarHelm Hub is not installed"
    );
    println!("Downloading verified {}...", candidate.asset_name());
    let executable = updater.download(Role::Hub, &candidate).await?;
    install_binary(
        &executable.path,
        Path::new(BINARY_PATH),
        Path::new(PREVIOUS_PATH),
    )?;
    let config = HubFileConfig::load(Path::new(DEFAULT_CONFIG_PATH))?;
    if let Err(error) = restart_and_check(&config).await {
        swap_with_previous(Path::new(BINARY_PATH), Path::new(PREVIOUS_PATH))?;
        systemctl_checked(ServiceScope::System, &["restart", UNIT_NAME])?;
        return Err(error).context("Hub update failed; previous binary restored");
    }
    println!("FarHelm Hub updated to {}.", executable.version);
    Ok(())
}

pub async fn rollback() -> Result<()> {
    require_root()?;
    swap_with_previous(Path::new(BINARY_PATH), Path::new(PREVIOUS_PATH))?;
    let config = HubFileConfig::load(Path::new(DEFAULT_CONFIG_PATH))?;
    if let Err(error) = restart_and_check(&config).await {
        swap_with_previous(Path::new(BINARY_PATH), Path::new(PREVIOUS_PATH))?;
        systemctl_checked(ServiceScope::System, &["restart", UNIT_NAME])?;
        return Err(error).context("Hub rollback failed; original binary restored");
    }
    println!("FarHelm Hub rolled back successfully.");
    Ok(())
}

pub fn uninstall(keep_data: bool) -> Result<()> {
    require_root()?;
    let _ = farhelm_lifecycle::systemctl(ServiceScope::System, &["disable", "--now", UNIT_NAME]);
    remove_regular_file_if_exists(Path::new(UNIT_PATH))?;
    remove_regular_file_if_exists(Path::new(BINARY_PATH))?;
    remove_regular_file_if_exists(Path::new(PREVIOUS_PATH))?;
    if !keep_data {
        remove_regular_file_if_exists(Path::new(DEFAULT_CONFIG_PATH))?;
        remove_managed_directory(Path::new(DATA_ROOT), "/var/lib", "farhelm")?;
        remove_empty_directory(Path::new("/etc/farhelm"))?;
    }
    let _ = farhelm_lifecycle::systemctl(ServiceScope::System, &["daemon-reload"]);
    if !keep_data {
        let _ = Command::new("userdel").arg("farhelm-hub").status();
        let _ = Command::new("groupdel").arg("farhelm-hub").status();
    }
    println!("FarHelm Hub uninstalled.");
    if keep_data {
        println!("Configuration, data, and their service identity were kept.");
    }
    println!("If Caddy was configured for FarHelm, remove that site block separately.");
    Ok(())
}

pub async fn health(base: &str) -> Result<()> {
    let endpoint = format!("{}/api/v1/health", base.trim_end_matches('/'));
    let response = reqwest::get(&endpoint)
        .await
        .with_context(|| format!("failed to connect to {endpoint}"))?
        .error_for_status()
        .with_context(|| format!("Hub health request failed at {endpoint}"))?;
    let health: HealthResponse = response
        .json()
        .await
        .context("Hub returned invalid health response")?;
    ensure!(health.status == HealthStatus::Ok, "Hub is not healthy");
    ensure!(
        health.protocol == FARHELM_PROTOCOL,
        "unsupported Hub protocol: {}",
        health.protocol
    );
    println!(
        "{} {} is healthy ({})",
        health.service, health.version, health.protocol
    );
    Ok(())
}

async fn restart_and_check(config: &HubFileConfig) -> Result<()> {
    systemctl_checked(ServiceScope::System, &["restart", UNIT_NAME])?;
    wait_for_health(config, Duration::from_secs(15)).await
}

async fn wait_for_health(config: &HubFileConfig, timeout: Duration) -> Result<()> {
    let endpoint = format!("http://{}/api/v1/health", config.hub.bind);
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if service_is_active(ServiceScope::System, UNIT_NAME)?
            && let Ok(response) = reqwest::get(&endpoint).await
            && response.status().is_success()
        {
            return Ok(());
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "Hub failed its health check"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn require_root() -> Result<()> {
    ensure!(
        is_root(),
        "Hub service management must be run with sudo/root"
    );
    Ok(())
}

fn ensure_service_identity() -> Result<()> {
    if service_identity_exists()? {
        return Ok(());
    }
    let status = Command::new("useradd")
        .args([
            "--system",
            "--home",
            DATA_ROOT,
            "--shell",
            "/usr/sbin/nologin",
            "farhelm-hub",
        ])
        .status()
        .context("failed to create FarHelm Hub service identity")?;
    ensure!(
        status.success(),
        "failed to create FarHelm Hub service identity"
    );
    Ok(())
}

fn service_identity_exists() -> Result<bool> {
    Ok(Command::new("id")
        .args(["-u", "farhelm-hub"])
        .output()?
        .status
        .success())
}

fn chown(owner: &str, path: &Path) -> Result<()> {
    let status = Command::new("chown").arg(owner).arg(path).status()?;
    ensure!(
        status.success(),
        "failed to set ownership on {}",
        path.display()
    );
    Ok(())
}

fn set_mode(path: &Path, mode: u32) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

fn cleanup_legacy() -> Result<()> {
    remove_regular_file_if_exists(Path::new(LEGACY_CTL))?;
    remove_regular_file_if_exists(Path::new(LEGACY_CONFIG_PATH))?;
    remove_regular_file_if_exists(Path::new(LEGACY_CADDY_EXAMPLE))?;
    if Path::new(LEGACY_ROOT).exists() {
        remove_managed_directory(Path::new(LEGACY_ROOT), "/opt", "farhelm-hub")?;
    }
    if Path::new(LEGACY_DATA_ROOT).exists() {
        remove_managed_directory(Path::new(LEGACY_DATA_ROOT), "/var/lib", "farhelm-hub")?;
    }
    Ok(())
}

fn migrate_legacy_database(config: &mut HubFileConfig) -> Result<()> {
    let legacy_root = Path::new(LEGACY_DATA_ROOT);
    if !config.hub.database.starts_with(legacy_root) || !config.hub.database.is_file() {
        return Ok(());
    }
    let destination = Path::new(DATA_ROOT).join("farhelm.db");
    if !destination.is_file() {
        let source = rusqlite::Connection::open(&config.hub.database)?;
        source.execute("VACUUM INTO ?1", [destination.to_string_lossy().as_ref()])?;
    }
    chown("farhelm-hub:farhelm-hub", &destination)?;
    config.hub.database = destination;
    Ok(())
}

fn remove_managed_directory(path: &Path, expected_parent: &str, expected_name: &str) -> Result<()> {
    ensure!(
        path.parent() == Some(Path::new(expected_parent)),
        "unsafe managed directory parent"
    );
    ensure!(
        path.file_name().is_some_and(|name| name == expected_name),
        "unsafe managed directory name"
    );
    if path.exists() {
        let metadata = fs::symlink_metadata(path)?;
        ensure!(
            metadata.file_type().is_dir(),
            "managed directory is not a directory"
        );
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

fn remove_empty_directory(path: &Path) -> Result<()> {
    match fs::remove_dir(path) {
        Ok(()) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}
