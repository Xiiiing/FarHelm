use std::{
    collections::BTreeMap,
    fs,
    io::IsTerminal,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use farhelm_hub::HubConfig;
use serde::{Deserialize, Serialize};

pub const DEFAULT_CONFIG_PATH: &str = "/etc/farhelm/hub.toml";
pub const DEFAULT_DATABASE_PATH: &str = "/var/lib/farhelm/farhelm.db";
pub const LEGACY_CONFIG_PATH: &str = "/etc/farhelm/hub.env";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubFileConfig {
    pub hub: HubSection,
    pub admin: AdminSection,
    pub agents: AgentAuthSection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubSection {
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default = "default_database")]
    pub database: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub console_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminSection {
    pub user: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentAuthSection {
    pub token: String,
}

impl HubFileConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read Hub config {}", path.display()))?;
        let config: Self = toml::from_str(&contents)
            .with_context(|| format!("invalid Hub config {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn from_install_input() -> Result<Self> {
        let legacy = read_legacy_env(Path::new(LEGACY_CONFIG_PATH)).unwrap_or_default();
        let user = value_or_prompt(
            "FARHELM_ADMIN_USER",
            legacy.get("FARHELM_ADMIN_USER"),
            false,
        )?;
        let password = value_or_prompt(
            "FARHELM_ADMIN_PASSWORD",
            legacy.get("FARHELM_ADMIN_PASSWORD"),
            true,
        )?;
        let token = value_or_prompt(
            "FARHELM_AGENT_TOKEN",
            legacy.get("FARHELM_AGENT_TOKEN"),
            true,
        )?;
        let bind = std::env::var("FARHELM_HUB_BIND")
            .ok()
            .or_else(|| legacy.get("FARHELM_HUB_BIND").cloned())
            .unwrap_or_else(default_bind);
        let database = std::env::var_os("FARHELM_HUB_DATABASE")
            .map(PathBuf::from)
            .or_else(|| legacy.get("FARHELM_HUB_DATABASE").map(PathBuf::from))
            .unwrap_or_else(default_database);
        let config = Self {
            hub: HubSection {
                bind,
                database,
                console_dir: None,
            },
            admin: AdminSection { user, password },
            agents: AgentAuthSection { token },
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        let runtime = self.runtime();
        runtime.validate()?;
        let address: std::net::SocketAddr = self
            .hub
            .bind
            .parse()
            .context("hub.bind is not a valid socket address")?;
        ensure!(
            address.ip().is_loopback(),
            "hub.bind must use a loopback address; terminate public TLS with a reverse proxy"
        );
        Ok(())
    }

    pub fn runtime(&self) -> HubConfig {
        HubConfig {
            admin_user: self.admin.user.clone(),
            admin_password: self.admin.password.clone(),
            agent_token: self.agents.token.clone(),
            console_dir: self.hub.console_dir.clone(),
            database_path: self.hub.database.clone(),
        }
    }

    pub fn encode(&self) -> Result<String> {
        toml::to_string_pretty(self).context("failed to encode Hub config")
    }
}

fn default_bind() -> String {
    "127.0.0.1:8787".to_owned()
}

fn default_database() -> PathBuf {
    PathBuf::from(DEFAULT_DATABASE_PATH)
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
            .context("legacy Hub config contains an invalid line")?;
        ensure!(
            name.bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'),
            "legacy Hub config contains an invalid setting name"
        );
        values.insert(name.to_owned(), value.to_owned());
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_round_trip_preserves_runtime_values() {
        let config = HubFileConfig {
            hub: HubSection {
                bind: default_bind(),
                database: PathBuf::from("/tmp/farhelm.db"),
                console_dir: Some(PathBuf::from("/tmp/console")),
            },
            admin: AdminSection {
                user: "admin".to_owned(),
                password: "correct-horse".to_owned(),
            },
            agents: AgentAuthSection {
                token: "agent-token-with-at-least-32-characters".to_owned(),
            },
        };
        let decoded: HubFileConfig = toml::from_str(&config.encode().unwrap()).unwrap();
        assert_eq!(decoded.hub.bind, "127.0.0.1:8787");
        assert_eq!(decoded.agents.token, config.agents.token);
    }

    #[test]
    fn rejects_non_loopback_bind() {
        let mut config: HubFileConfig = toml::from_str(
            r#"
[hub]
bind = "0.0.0.0:8787"
database = "/tmp/farhelm.db"
[admin]
user = "admin"
password = "correct-horse"
[agents]
token = "agent-token-with-at-least-32-characters"
"#,
        )
        .unwrap();
        config.hub.console_dir = Some(PathBuf::from("missing"));
        assert!(config.validate().is_err());
    }
}
