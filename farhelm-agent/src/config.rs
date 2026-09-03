use std::{
    collections::BTreeMap,
    fs,
    io::IsTerminal,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct AgentPaths {
    pub binary: PathBuf,
    pub previous: PathBuf,
    pub config: PathBuf,
    pub data: PathBuf,
    pub database: PathBuf,
    pub worker: PathBuf,
    pub unit: PathBuf,
    pub legacy_root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentFileConfig {
    pub agent: AgentSection,
    #[serde(default)]
    pub worker: WorkerSection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSection {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    pub hub_url: String,
    pub token: String,
    #[serde(default = "default_heartbeat")]
    pub heartbeat_seconds: u64,
    #[serde(default = "default_command_poll")]
    pub command_poll_seconds: u64,
    pub database: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerSection {
    #[serde(default = "default_python")]
    pub python: String,
}

impl Default for WorkerSection {
    fn default() -> Self {
        Self {
            python: default_python(),
        }
    }
}

impl AgentPaths {
    pub fn discover() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME is missing")?;
        ensure!(home.is_absolute(), "HOME must be an absolute path");
        let config_home = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"));
        let bin_home = std::env::var_os("XDG_BIN_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/bin"));
        for path in [&config_home, &data_home, &bin_home] {
            ensure!(path.is_absolute(), "XDG paths must be absolute");
        }
        let data = data_home.join("farhelm");
        let legacy_root = if std::env::var("FARHELM_UPGRADE").as_deref() == Ok("1") {
            std::env::var_os("FARHELM_INSTALL_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|| data_home.join("farhelm-agent"))
        } else {
            data_home.join("farhelm-agent")
        };
        ensure!(
            legacy_root.is_absolute(),
            "legacy Agent root must be absolute"
        );
        Ok(Self {
            binary: bin_home.join("farhelm-agent"),
            previous: bin_home.join("farhelm-agent.previous"),
            config: config_home.join("farhelm/agent.toml"),
            database: data.join("state/agent.db"),
            worker: data.join(format!(
                "runtime/codex-worker/{}",
                farhelm_core::PRODUCT_VERSION
            )),
            unit: config_home.join("systemd/user/farhelm-agent.service"),
            legacy_root,
            data,
        })
    }
}

impl AgentFileConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read Agent config {}", path.display()))?;
        let config: Self = toml::from_str(&contents)
            .with_context(|| format!("invalid Agent config {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn from_install_input(paths: &AgentPaths) -> Result<Self> {
        let legacy = read_legacy_env(&paths.legacy_root.join("config/agent.env"))?;
        let hub_url = value_or_prompt("FARHELM_HUB_URL", legacy.get("FARHELM_HUB_URL"), false)?;
        let token = value_or_prompt(
            "FARHELM_AGENT_TOKEN",
            legacy.get("FARHELM_AGENT_TOKEN"),
            true,
        )?;
        let id = value_or_prompt("FARHELM_AGENT_ID", legacy.get("FARHELM_AGENT_ID"), false)?;
        let hostname = std::env::var("FARHELM_AGENT_HOSTNAME")
            .ok()
            .or_else(|| legacy.get("FARHELM_AGENT_HOSTNAME").cloned())
            .filter(|value| !value.is_empty());
        let database = legacy
            .get("FARHELM_AGENT_DATABASE")
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .unwrap_or_else(|| paths.database.clone());
        let config = Self {
            agent: AgentSection {
                id,
                hostname,
                hub_url,
                token,
                heartbeat_seconds: legacy_u64(
                    &legacy,
                    "FARHELM_HEARTBEAT_INTERVAL",
                    default_heartbeat(),
                )?,
                command_poll_seconds: legacy_u64(
                    &legacy,
                    "FARHELM_COMMAND_POLL_INTERVAL",
                    default_command_poll(),
                )?,
                database,
            },
            worker: WorkerSection::default(),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.agent.id.is_empty()
                && self.agent.id.len() <= 64
                && self.agent.id.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                }),
            "agent.id is invalid"
        );
        ensure!(
            self.agent.token.len() >= 32,
            "agent.token must contain at least 32 characters"
        );
        ensure!(
            self.agent.heartbeat_seconds >= 5,
            "agent.heartbeat_seconds must be at least 5"
        );
        ensure!(
            self.agent.command_poll_seconds >= 1,
            "agent.command_poll_seconds must be at least 1"
        );
        ensure!(
            !self.agent.database.as_os_str().is_empty(),
            "agent.database is empty"
        );
        ensure!(!self.worker.python.is_empty(), "worker.python is empty");
        let url =
            reqwest::Url::parse(&self.agent.hub_url).context("agent.hub_url is not a valid URL")?;
        let local_http = url.scheme() == "http"
            && url
                .host_str()
                .is_some_and(|host| matches!(host, "127.0.0.1" | "::1" | "localhost"));
        ensure!(
            url.scheme() == "https" || local_http,
            "agent.hub_url must use HTTPS"
        );
        Ok(())
    }

    pub fn encode(&self) -> Result<String> {
        toml::to_string_pretty(self).context("failed to encode Agent config")
    }
}

fn default_heartbeat() -> u64 {
    15
}

fn default_command_poll() -> u64 {
    2
}

fn default_python() -> String {
    "python3".to_owned()
}

fn value_or_prompt(name: &str, legacy: Option<&String>, hidden: bool) -> Result<String> {
    if let Ok(value) = std::env::var(name)
        && !value.is_empty()
    {
        return Ok(value);
    }
    if let Some(value) = legacy
        && !value.is_empty()
    {
        return Ok(value.clone());
    }
    ensure!(
        std::io::stdin().is_terminal(),
        "required install setting {name} is missing"
    );
    let prompt = format!("{name}: ");
    let value = if hidden {
        rpassword::prompt_password(prompt)?
    } else {
        eprint!("{prompt}");
        let mut value = String::new();
        std::io::stdin().read_line(&mut value)?;
        value.trim().to_owned()
    };
    ensure!(
        !value.is_empty(),
        "required install setting {name} is empty"
    );
    Ok(value)
}

fn read_legacy_env(path: &Path) -> Result<BTreeMap<String, String>> {
    if !path.is_file() {
        return Ok(BTreeMap::new());
    }
    let mut values = BTreeMap::new();
    for line in fs::read_to_string(path)?.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (name, value) = line
            .split_once('=')
            .context("legacy Agent config contains an invalid line")?;
        ensure!(
            name.bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'),
            "legacy Agent config contains an invalid setting name"
        );
        values.insert(name.to_owned(), value.to_owned());
    }
    Ok(values)
}

fn legacy_u64(values: &BTreeMap<String, String>, name: &str, default: u64) -> Result<u64> {
    values.get(name).map_or(Ok(default), |value| {
        value
            .parse()
            .with_context(|| format!("legacy {name} is invalid"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdg_paths_and_toml_are_stable() {
        let paths = AgentPaths {
            binary: PathBuf::from("/tmp/bin/farhelm-agent"),
            previous: PathBuf::from("/tmp/bin/farhelm-agent.previous"),
            config: PathBuf::from("/tmp/config/farhelm/agent.toml"),
            data: PathBuf::from("/tmp/data/farhelm"),
            database: PathBuf::from("/tmp/data/farhelm/state/agent.db"),
            worker: PathBuf::from("/tmp/data/farhelm/runtime/codex-worker/0.3.0"),
            unit: PathBuf::from("/tmp/config/systemd/user/farhelm-agent.service"),
            legacy_root: PathBuf::from("/tmp/data/farhelm-agent"),
        };
        let config = AgentFileConfig {
            agent: AgentSection {
                id: "gpu-titan".to_owned(),
                hostname: None,
                hub_url: "https://farhelm.example.test".to_owned(),
                token: "agent-token-with-at-least-32-characters".to_owned(),
                heartbeat_seconds: 15,
                command_poll_seconds: 2,
                database: paths.database.clone(),
            },
            worker: WorkerSection::default(),
        };
        let decoded: AgentFileConfig = toml::from_str(&config.encode().unwrap()).unwrap();
        decoded.validate().unwrap();
        assert_eq!(decoded.agent.id, "gpu-titan");
        assert_eq!(decoded.agent.database, paths.database);
    }
}
