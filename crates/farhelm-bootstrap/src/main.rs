use std::{
    env,
    io::{self, IsTerminal, Write},
    path::PathBuf,
    process::Command,
};

use anyhow::{Context, Result, bail, ensure};
use farhelm_core::PRODUCT_VERSION;
use farhelm_updater::{Role, Updater, VerifiedBundle};

const EMBEDDED_ARCHIVE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/bundle.tar.gz"));
const EMBEDDED_ROLE: &str = env!("FARHELM_BOOTSTRAP_ROLE");
const DISTRIBUTABLE: bool = env!("FARHELM_BOOTSTRAP_DISTRIBUTABLE").as_bytes()[0] == b'1';

#[derive(Debug, Default, PartialEq, Eq)]
struct Cli {
    verify: bool,
    hub_url: Option<String>,
    agent_id: Option<String>,
    no_service: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let Some(cli) = parse_cli(env::args().skip(1))? else {
        return Ok(());
    };
    ensure!(
        DISTRIBUTABLE,
        "this development bootstrap has no embedded release; use a published FarHelm asset"
    );
    let role = embedded_role()?;
    let bundle = VerifiedBundle::from_embedded(role, PRODUCT_VERSION, EMBEDDED_ARCHIVE)
        .context("embedded FarHelm release validation failed")?;
    if cli.verify {
        println!(
            "Verified embedded {} release {}.",
            role.package_name(),
            bundle.version
        );
        return Ok(());
    }

    let environment = match role {
        Role::Hub => hub_environment(&cli)?,
        Role::Agent => agent_environment(&cli)?,
    };
    Updater::new()?
        .install_with_env(&bundle, None, &environment)
        .await?;
    println!(
        "Single-file {} installation completed. You may delete this downloaded installer.",
        role.package_name()
    );
    Ok(())
}

fn parse_cli(arguments: impl Iterator<Item = String>) -> Result<Option<Cli>> {
    let mut cli = Cli::default();
    let mut arguments = arguments.peekable();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "-h" | "--help" => {
                print_help();
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("{}-installer {PRODUCT_VERSION}", display_role());
                return Ok(None);
            }
            "--verify" => cli.verify = true,
            "--no-service" => cli.no_service = true,
            "--hub-url" => {
                cli.hub_url = Some(
                    arguments
                        .next()
                        .context("--hub-url requires an HTTPS URL")?,
                );
            }
            "--agent-id" => {
                cli.agent_id = Some(arguments.next().context("--agent-id requires a value")?);
            }
            _ => bail!("unknown installer option: {argument}; use --help"),
        }
    }
    if cli.verify {
        ensure!(
            cli.hub_url.is_none() && cli.agent_id.is_none() && !cli.no_service,
            "--verify cannot be combined with installation options"
        );
    }
    Ok(Some(cli))
}

fn print_help() {
    println!("{}-installer {PRODUCT_VERSION}", display_role());
    println!("Install the embedded immutable FarHelm release.");
    println!();
    println!("Options:");
    println!("  --verify            Validate the embedded release without installing");
    println!("  --hub-url <URL>     Agent: public HTTPS Hub URL");
    println!("  --agent-id <ID>     Agent: stable ID (defaults to hostname)");
    println!("  --no-service        Agent: install without a systemd user service");
    println!("  -V, --version       Print installer version");
    println!("  -h, --help          Print help");
}

fn display_role() -> &'static str {
    match EMBEDDED_ROLE {
        "hub" => "farhelm-hub",
        "agent" => "farhelm-agent",
        _ => "farhelm-development",
    }
}

fn embedded_role() -> Result<Role> {
    match EMBEDDED_ROLE {
        "hub" => Ok(Role::Hub),
        "agent" => Ok(Role::Agent),
        value => bail!("invalid embedded FarHelm role: {value}"),
    }
}

fn hub_environment(cli: &Cli) -> Result<Vec<(String, String)>> {
    ensure!(
        cli.hub_url.is_none() && cli.agent_id.is_none() && !cli.no_service,
        "Agent-only options cannot be used with the Hub installer"
    );
    Ok(Vec::new())
}

fn agent_environment(cli: &Cli) -> Result<Vec<(String, String)>> {
    let mut environment = Vec::new();
    if cli.no_service {
        environment.push(("FARHELM_NO_SERVICE".to_owned(), "1".to_owned()));
    }
    if agent_config_path().is_some_and(|path| path.is_file()) {
        if cli.hub_url.is_some() || cli.agent_id.is_some() {
            eprintln!("Existing Agent connection settings will be preserved.");
        }
        return Ok(environment);
    }

    let hub_url = cli
        .hub_url
        .clone()
        .or_else(|| env::var("FARHELM_HUB_URL").ok())
        .map(Ok)
        .unwrap_or_else(|| prompt_line("Hub HTTPS URL", None))?;
    let agent_id = cli
        .agent_id
        .clone()
        .or_else(|| env::var("FARHELM_AGENT_ID").ok())
        .map(Ok)
        .unwrap_or_else(|| prompt_line("Agent ID", hostname().as_deref()))?;
    let token = match env::var("FARHELM_AGENT_TOKEN") {
        Ok(token) => token,
        Err(_) => {
            ensure!(
                io::stdin().is_terminal(),
                "FARHELM_AGENT_TOKEN is required when no interactive terminal is available"
            );
            rpassword::prompt_password("Agent token (hidden): ")
                .context("failed to read Agent token")?
        }
    };
    ensure!(!hub_url.is_empty(), "Hub URL cannot be empty");
    ensure!(!agent_id.is_empty(), "Agent ID cannot be empty");
    ensure!(!token.is_empty(), "Agent token cannot be empty");
    environment.extend([
        ("FARHELM_HUB_URL".to_owned(), hub_url),
        ("FARHELM_AGENT_ID".to_owned(), agent_id),
        ("FARHELM_AGENT_TOKEN".to_owned(), token),
    ]);
    Ok(environment)
}

fn prompt_line(label: &str, default: Option<&str>) -> Result<String> {
    ensure!(
        io::stdin().is_terminal(),
        "{label} is required when no interactive terminal is available"
    );
    match default {
        Some(value) => eprint!("{label} [{value}]: "),
        None => eprint!("{label}: "),
    }
    io::stderr().flush().context("failed to display prompt")?;
    let mut value = String::new();
    io::stdin()
        .read_line(&mut value)
        .context("failed to read installer input")?;
    let value = value.trim();
    if value.is_empty() {
        default
            .map(str::to_owned)
            .context(format!("{label} cannot be empty"))
    } else {
        Ok(value.to_owned())
    }
}

fn hostname() -> Option<String> {
    let output = Command::new("hostname").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let hostname = String::from_utf8(output.stdout).ok()?;
    let hostname = hostname.trim();
    (!hostname.is_empty()).then(|| hostname.to_owned())
}

fn agent_config_path() -> Option<PathBuf> {
    if let Some(root) = env::var_os("FARHELM_INSTALL_ROOT") {
        return Some(PathBuf::from(root).join("config/agent.env"));
    }
    let data_home = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))?;
    Some(data_home.join("farhelm-agent/config/agent.env"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_non_secret_agent_options() {
        let cli = parse_cli(
            [
                "--hub-url",
                "https://hub.example.test",
                "--agent-id",
                "gpu-a",
                "--no-service",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .unwrap()
        .unwrap();
        assert_eq!(cli.hub_url.as_deref(), Some("https://hub.example.test"));
        assert_eq!(cli.agent_id.as_deref(), Some("gpu-a"));
        assert!(cli.no_service);
    }

    #[test]
    fn rejects_secret_or_unknown_cli_options() {
        assert!(parse_cli(["--token", "secret"].into_iter().map(str::to_owned)).is_err());
        assert!(parse_cli(["--verify", "--no-service"].into_iter().map(str::to_owned)).is_err());
    }
}
