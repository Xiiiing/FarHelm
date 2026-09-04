use std::{
    collections::BTreeMap,
    fs,
    io::IsTerminal,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use base64::Engine as _;
use farhelm_hub::HubConfig;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use web_push_native::p256::{
    SecretKey,
    elliptic_curve::{rand_core::OsRng, sec1::ToEncodedPoint},
};

pub const DEFAULT_CONFIG_PATH: &str = "/etc/farhelm/hub.toml";
pub const DEFAULT_DATABASE_PATH: &str = "/var/lib/farhelm/farhelm.db";
pub const LEGACY_CONFIG_PATH: &str = "/etc/farhelm/hub.env";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubFileConfig {
    pub hub: HubSection,
    pub admin: AdminSection,
    pub agents: AgentAuthSection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<PushSection>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub totp_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recovery_code_hashes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentAuthSection {
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub tokens: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushSection {
    pub private_key: String,
    pub public_key: String,
    pub contact: String,
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
        let password_hash = hash_secret(&password)?;
        let mut secret_bytes = [0_u8; 20];
        rand::rng().fill_bytes(&mut secret_bytes);
        let totp_secret = match totp_rs::Secret::Raw(secret_bytes.to_vec()).to_encoded() {
            totp_rs::Secret::Encoded(secret) => secret,
            totp_rs::Secret::Raw(_) => unreachable!(),
        };
        eprintln!("FarHelm TOTP secret (add it to your authenticator): {totp_secret}");
        let mut recovery_codes = Vec::new();
        let mut recovery_code_hashes = Vec::new();
        for _ in 0..8 {
            let mut bytes = [0_u8; 10];
            rand::rng().fill_bytes(&mut bytes);
            let code = bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            recovery_code_hashes.push(hash_secret(&code)?);
            recovery_codes.push(code);
        }
        eprintln!(
            "FarHelm one-time recovery codes (store offline): {}",
            recovery_codes.join(" ")
        );
        let config = Self {
            hub: HubSection {
                bind,
                database,
                console_dir: None,
            },
            admin: AdminSection {
                user,
                password: None,
                password_hash: Some(password_hash),
                totp_secret: Some(totp_secret),
                recovery_code_hashes,
            },
            agents: AgentAuthSection {
                token,
                tokens: BTreeMap::new(),
            },
            push: Some(generate_push_section()?),
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

    pub fn ensure_modern_auth(&mut self) -> Result<bool> {
        let mut changed = false;
        if self.admin.password_hash.is_none() {
            let password = self
                .admin
                .password
                .as_deref()
                .context("admin password is missing")?;
            self.admin.password_hash = Some(hash_secret(password)?);
            self.admin.password = None;
            changed = true;
        }
        if self.admin.totp_secret.is_none() {
            let mut bytes = [0_u8; 20];
            rand::rng().fill_bytes(&mut bytes);
            self.admin.totp_secret =
                Some(match totp_rs::Secret::Raw(bytes.to_vec()).to_encoded() {
                    totp_rs::Secret::Encoded(value) => value,
                    totp_rs::Secret::Raw(_) => unreachable!(),
                });
            eprintln!(
                "FarHelm TOTP secret (add it to your authenticator): {}",
                self.admin.totp_secret.as_deref().unwrap_or_default()
            );
            changed = true;
        }
        if self.admin.recovery_code_hashes.is_empty() {
            let mut recovery_codes = Vec::new();
            for _ in 0..8 {
                let mut bytes = [0_u8; 10];
                rand::rng().fill_bytes(&mut bytes);
                let code = bytes
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>();
                self.admin.recovery_code_hashes.push(hash_secret(&code)?);
                recovery_codes.push(code);
            }
            eprintln!(
                "FarHelm one-time recovery codes (store offline): {}",
                recovery_codes.join(" ")
            );
            changed = true;
        }
        if self.push.is_none() {
            self.push = Some(generate_push_section()?);
            changed = true;
        }
        Ok(changed)
    }

    pub fn runtime(&self) -> HubConfig {
        HubConfig {
            admin_user: self.admin.user.clone(),
            admin_password: self
                .admin
                .password_hash
                .clone()
                .or_else(|| self.admin.password.clone())
                .unwrap_or_default(),
            admin_totp_secret: self.admin.totp_secret.clone(),
            recovery_code_hashes: self.admin.recovery_code_hashes.clone(),
            agent_token: self.agents.token.clone(),
            agent_tokens: self.agents.tokens.clone(),
            push: self.push.as_ref().map(|push| farhelm_hub::PushConfig {
                private_key: push.private_key.clone(),
                public_key: push.public_key.clone(),
                contact: push.contact.clone(),
            }),
            console_dir: self.hub.console_dir.clone(),
            database_path: self.hub.database.clone(),
        }
    }

    pub fn encode(&self) -> Result<String> {
        toml::to_string_pretty(self).context("failed to encode Hub config")
    }
}

fn generate_push_section() -> Result<PushSection> {
    let secret = SecretKey::random(&mut OsRng);
    let private = secret.to_bytes();
    let public = secret.public_key().to_encoded_point(false);
    let section = PushSection {
        private_key: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(private),
        public_key: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(public.as_bytes()),
        contact: std::env::var("FARHELM_VAPID_CONTACT")
            .unwrap_or_else(|_| "mailto:farhelm@localhost".to_owned()),
    };
    eprintln!("FarHelm Web Push public key: {}", section.public_key);
    Ok(section)
}

fn hash_secret(secret: &str) -> Result<String> {
    let mut salt = [0_u8; 16];
    rand::rng().fill_bytes(&mut salt);
    let salt = SaltString::encode_b64(&salt)
        .map_err(|error| anyhow::anyhow!("failed to encode password salt: {error}"))?;
    Ok(Argon2::default()
        .hash_password(secret.as_bytes(), &salt)
        .map_err(|error| anyhow::anyhow!("failed to hash secret: {error}"))?
        .to_string())
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
                password: Some("correct-horse".to_owned()),
                password_hash: None,
                totp_secret: None,
                recovery_code_hashes: Vec::new(),
            },
            agents: AgentAuthSection {
                token: "agent-token-with-at-least-32-characters".to_owned(),
                tokens: BTreeMap::new(),
            },
            push: None,
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
