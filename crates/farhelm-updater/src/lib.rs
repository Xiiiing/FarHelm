//! Verified update discovery and executable download for FarHelm releases.

use std::{fmt, os::unix::fs::PermissionsExt, path::PathBuf, process::Command, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use reqwest::{Client, redirect::Policy};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::{fs, io::AsyncWriteExt};

const RELEASES_API: &str = "https://api.github.com/repos/Xiiiing/FarHelm/releases?per_page=100";
const DOWNLOAD_PREFIX: &str = "https://github.com/Xiiiing/FarHelm/releases/download/";
const API_VERSION: &str = "2026-03-10";
const MAX_REDIRECTS: usize = 8;
const MAX_RUNTIME_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Hub,
    Agent,
}

impl Role {
    #[must_use]
    pub const fn package_name(self) -> &'static str {
        match self {
            Self::Hub => "farhelm-hub",
            Self::Agent => "farhelm-agent",
        }
    }

    #[must_use]
    pub fn versioned_binary_name(self, version: &Version) -> String {
        format!("{}-{version}-linux-x86_64", self.package_name())
    }

    #[must_use]
    pub const fn stable_binary_name(self) -> &'static str {
        match self {
            Self::Hub => "farhelm-hub-linux-x86_64",
            Self::Agent => "farhelm-agent-linux-x86_64",
        }
    }

    const fn max_binary_bytes(self) -> u64 {
        match self {
            Self::Hub => 64 * 1024 * 1024,
            Self::Agent => 256 * 1024 * 1024,
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.package_name())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateCandidate {
    pub version: Version,
    pub tag: String,
    asset: ReleaseAsset,
    digest: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeCandidate {
    pub version: Version,
    pub tag: String,
    asset: ReleaseAsset,
    digest: [u8; 32],
}

impl RuntimeCandidate {
    #[must_use]
    pub fn asset_name(&self) -> &str {
        &self.asset.name
    }
}

impl UpdateCandidate {
    #[must_use]
    pub fn asset_name(&self) -> &str {
        &self.asset.name
    }
}

#[derive(Debug)]
pub struct VerifiedExecutable {
    _temporary: TempDir,
    pub path: PathBuf,
    pub version: Version,
}

#[derive(Debug)]
pub struct VerifiedAsset {
    _temporary: TempDir,
    pub path: PathBuf,
    pub version: Version,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    #[serde(default)]
    immutable: bool,
    assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    digest: Option<String>,
}

pub struct Updater {
    client: Client,
}

impl Updater {
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(600))
            .user_agent("farhelm-updater")
            .redirect(Policy::custom(|attempt| {
                if attempt.previous().len() >= MAX_REDIRECTS {
                    return attempt.error("too many update download redirects");
                }
                if attempt.url().scheme() != "https" {
                    return attempt.error("update download redirected away from HTTPS");
                }
                attempt.follow()
            }))
            .build()
            .context("failed to build update HTTP client")?;
        Ok(Self { client })
    }

    pub async fn check(
        &self,
        role: Role,
        current: &str,
        requested: Option<&str>,
        allow_major: bool,
    ) -> Result<Option<UpdateCandidate>> {
        let releases = self.releases().await?;
        select_candidate(&releases, role, current, requested, allow_major)
    }

    pub async fn runtime(&self, version: &str) -> Result<RuntimeCandidate> {
        let version = Version::parse(version).context("Worker runtime version is invalid")?;
        ensure!(
            version.pre.is_empty() && version.build.is_empty(),
            "Worker runtime requires a formal release version"
        );
        select_runtime_candidate(&self.releases().await?, &version)
    }

    async fn releases(&self) -> Result<Vec<GithubRelease>> {
        let response = self
            .client
            .get(RELEASES_API)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .send()
            .await
            .context("failed to query FarHelm releases")?
            .error_for_status()
            .context("GitHub rejected the FarHelm release query")?;
        response
            .json()
            .await
            .context("GitHub returned invalid FarHelm release metadata")
    }

    pub async fn download(
        &self,
        role: Role,
        candidate: &UpdateCandidate,
    ) -> Result<VerifiedExecutable> {
        let (temporary, binary_path) = self
            .download_asset(
                &candidate.asset,
                candidate.digest,
                role.max_binary_bytes(),
                role.package_name(),
            )
            .await?;
        std::fs::set_permissions(&binary_path, std::fs::Permissions::from_mode(0o755))?;
        verify_executable(role, candidate.version.clone(), &binary_path)?;

        Ok(VerifiedExecutable {
            _temporary: temporary,
            path: binary_path,
            version: candidate.version.clone(),
        })
    }

    pub async fn download_runtime(&self, candidate: &RuntimeCandidate) -> Result<VerifiedAsset> {
        let (temporary, path) = self
            .download_asset(
                &candidate.asset,
                candidate.digest,
                MAX_RUNTIME_BYTES,
                "codex-runtime.tar.gz",
            )
            .await?;
        Ok(VerifiedAsset {
            _temporary: temporary,
            path,
            version: candidate.version.clone(),
        })
    }

    async fn download_asset(
        &self,
        asset: &ReleaseAsset,
        expected_digest: [u8; 32],
        max_bytes: u64,
        staged_name: &str,
    ) -> Result<(TempDir, PathBuf)> {
        let temporary = tempfile::tempdir().context("failed to create update staging directory")?;
        let path = temporary.path().join(staged_name);
        let mut response = self
            .client
            .get(&asset.browser_download_url)
            .send()
            .await
            .context("failed to download FarHelm release")?
            .error_for_status()
            .context("GitHub rejected the FarHelm release download")?;
        if let Some(length) = response.content_length() {
            ensure!(
                length == asset.size,
                "release Content-Length does not match immutable asset metadata"
            );
        }

        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
            .context("failed to create staged release asset")?;
        let mut hasher = Sha256::new();
        let mut downloaded = 0_u64;
        while let Some(chunk) = response
            .chunk()
            .await
            .context("FarHelm release download was interrupted")?
        {
            downloaded = downloaded
                .checked_add(u64::try_from(chunk.len())?)
                .context("release download size overflowed")?;
            ensure!(
                downloaded <= max_bytes,
                "release asset exceeds its size limit"
            );
            ensure!(
                downloaded <= asset.size,
                "release asset exceeds immutable asset metadata"
            );
            hasher.update(&chunk);
            output
                .write_all(&chunk)
                .await
                .context("failed to write staged release asset")?;
        }
        output
            .sync_all()
            .await
            .context("failed to sync staged release asset")?;
        drop(output);
        let actual: [u8; 32] = hasher.finalize().into();
        validate_asset(asset, expected_digest, downloaded, actual)?;
        Ok((temporary, path))
    }
}

fn verify_executable(role: Role, version: Version, path: &std::path::Path) -> Result<()> {
    let output = Command::new(path)
        .arg("--version")
        .output()
        .context("failed to execute staged FarHelm release")?;
    ensure!(
        output.status.success(),
        "staged FarHelm release rejected --version"
    );
    ensure!(
        String::from_utf8_lossy(&output.stdout).trim()
            == format!("{} {version}", role.package_name()),
        "staged FarHelm executable role or version does not match immutable metadata"
    );
    Ok(())
}

fn select_candidate(
    releases: &[GithubRelease],
    role: Role,
    current: &str,
    requested: Option<&str>,
    allow_major: bool,
) -> Result<Option<UpdateCandidate>> {
    let current = Version::parse(current).context("current FarHelm version is invalid")?;
    let requested = requested.map(parse_requested_version).transpose()?;
    let mut candidates = Vec::new();

    for release in releases {
        if release.draft || release.prerelease || !release.immutable {
            continue;
        }
        let Some(version_text) = release.tag_name.strip_prefix('V') else {
            continue;
        };
        let Ok(version) = Version::parse(version_text) else {
            continue;
        };
        if release.tag_name != format!("V{version}") || !version.pre.is_empty() {
            continue;
        }
        if requested.as_ref().is_some_and(|value| value != &version) || version <= current {
            continue;
        }
        let name = role.versioned_binary_name(&version);
        let Some(asset) = release.assets.iter().find(|asset| asset.name == name) else {
            continue;
        };
        let digest = parse_digest(asset.digest.as_deref())?;
        ensure!(asset.size > 0, "release asset is empty");
        ensure!(
            asset.size <= role.max_binary_bytes(),
            "release asset exceeds the role size limit"
        );
        let expected_url = format!("{DOWNLOAD_PREFIX}{}/{name}", release.tag_name);
        ensure!(
            asset.browser_download_url == expected_url,
            "release asset URL is outside the official FarHelm release path"
        );
        candidates.push(UpdateCandidate {
            version,
            tag: release.tag_name.clone(),
            asset: asset.clone(),
            digest,
        });
    }

    candidates.sort_by(|left, right| right.version.cmp(&left.version));
    let has_blocked_major = candidates
        .iter()
        .any(|candidate| candidate.version.major != current.major);
    let candidate = candidates
        .into_iter()
        .find(|candidate| allow_major || candidate.version.major == current.major);
    if candidate.is_none() && has_blocked_major {
        bail!("major-version upgrade requires explicit --allow-major");
    }
    if requested.is_some() && candidate.is_none() {
        bail!("requested version is not an immutable official FarHelm update");
    }
    Ok(candidate)
}

fn select_runtime_candidate(
    releases: &[GithubRelease],
    version: &Version,
) -> Result<RuntimeCandidate> {
    let tag = format!("V{version}");
    let release = releases
        .iter()
        .find(|release| {
            release.tag_name == tag && !release.draft && !release.prerelease && release.immutable
        })
        .context("matching immutable FarHelm release is unavailable")?;
    let name = format!("farhelm-codex-runtime-{version}-linux-x86_64.tar.gz");
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name == name)
        .context("immutable release omits the Codex runtime asset")?;
    ensure!(asset.size > 0, "Codex runtime asset is empty");
    ensure!(
        asset.size <= MAX_RUNTIME_BYTES,
        "Codex runtime asset exceeds its size limit"
    );
    ensure!(
        asset.browser_download_url == format!("{DOWNLOAD_PREFIX}{tag}/{name}"),
        "Codex runtime URL is outside the official FarHelm release path"
    );
    Ok(RuntimeCandidate {
        version: version.clone(),
        tag,
        asset: asset.clone(),
        digest: parse_digest(asset.digest.as_deref())?,
    })
}

fn parse_requested_version(value: &str) -> Result<Version> {
    let value = value.strip_prefix('V').unwrap_or(value);
    ensure!(
        !value.starts_with('v'),
        "formal release tags use uppercase V"
    );
    let version = Version::parse(value).context("requested FarHelm version is invalid")?;
    ensure!(
        version.pre.is_empty() && version.build.is_empty(),
        "prerelease and build versions are not formal update targets"
    );
    Ok(version)
}

fn parse_digest(value: Option<&str>) -> Result<[u8; 32]> {
    let value = value.context("immutable release asset has no digest")?;
    let value = value
        .strip_prefix("sha256:")
        .context("release asset digest is not SHA-256")?;
    ensure!(value.len() == 64, "release asset SHA-256 length is invalid");
    let mut digest = [0_u8; 32];
    for (index, pair) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let text = std::str::from_utf8(pair)?;
        digest[index] = u8::from_str_radix(text, 16).context("release asset digest is invalid")?;
    }
    Ok(digest)
}

#[cfg(test)]
fn validate_download(
    candidate: &UpdateCandidate,
    downloaded: u64,
    actual_digest: [u8; 32],
) -> Result<()> {
    validate_asset(
        &candidate.asset,
        candidate.digest,
        downloaded,
        actual_digest,
    )
}

fn validate_asset(
    asset: &ReleaseAsset,
    expected_digest: [u8; 32],
    downloaded: u64,
    actual_digest: [u8; 32],
) -> Result<()> {
    ensure!(
        downloaded == asset.size,
        "release asset length does not match immutable metadata"
    );
    ensure!(
        actual_digest == expected_digest,
        "release asset SHA-256 does not match immutable metadata"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, immutable: bool) -> GithubRelease {
        let version = tag.trim_start_matches(['V', 'v']);
        GithubRelease {
            tag_name: tag.to_owned(),
            draft: false,
            prerelease: false,
            immutable,
            assets: vec![
                ReleaseAsset {
                    name: format!("farhelm-agent-{version}-linux-x86_64"),
                    browser_download_url: format!(
                        "{DOWNLOAD_PREFIX}{tag}/farhelm-agent-{version}-linux-x86_64"
                    ),
                    size: 1024,
                    digest: Some(format!("sha256:{}", "11".repeat(32))),
                },
                ReleaseAsset {
                    name: format!("farhelm-codex-runtime-{version}-linux-x86_64.tar.gz"),
                    browser_download_url: format!(
                        "{DOWNLOAD_PREFIX}{tag}/farhelm-codex-runtime-{version}-linux-x86_64.tar.gz"
                    ),
                    size: 2048,
                    digest: Some(format!("sha256:{}", "22".repeat(32))),
                },
            ],
        }
    }

    #[test]
    fn selects_only_newest_immutable_uppercase_release() {
        let releases = vec![
            release("v9.0.0", true),
            release("V0.2.0", false),
            release("V0.2.1", true),
            release("V0.3.0", true),
        ];
        let selected = select_candidate(&releases, Role::Agent, "0.2.0", None, false)
            .unwrap()
            .unwrap();
        assert_eq!(selected.version, Version::new(0, 3, 0));
        assert_eq!(selected.tag, "V0.3.0");
        assert_eq!(selected.asset_name(), "farhelm-agent-0.3.0-linux-x86_64");
    }

    #[test]
    fn major_upgrade_requires_explicit_permission() {
        let releases = vec![release("V1.0.0", true)];
        assert!(select_candidate(&releases, Role::Agent, "0.9.0", None, false).is_err());
        assert_eq!(
            select_candidate(&releases, Role::Agent, "0.9.0", None, true)
                .unwrap()
                .unwrap()
                .version,
            Version::new(1, 0, 0)
        );
    }

    #[test]
    fn newer_major_does_not_hide_same_major_update() {
        let releases = vec![release("V1.0.0", true), release("V0.9.1", true)];
        assert_eq!(
            select_candidate(&releases, Role::Agent, "0.9.0", None, false)
                .unwrap()
                .unwrap()
                .version,
            Version::new(0, 9, 1)
        );
    }

    #[test]
    fn requested_release_must_be_newer_and_formal() {
        let releases = vec![release("V0.3.0", true)];
        assert!(select_candidate(&releases, Role::Agent, "0.2.0", Some("v0.3.0"), false).is_err());
        assert!(select_candidate(&releases, Role::Agent, "0.3.0", Some("V0.3.0"), false).is_err());
    }

    #[test]
    fn validates_digest_and_download() {
        assert_eq!(
            parse_digest(Some(&format!("sha256:{}", "ab".repeat(32)))).unwrap(),
            [0xab; 32]
        );
        assert!(parse_digest(Some("sha256:abcd")).is_err());
        let candidate = select_candidate(
            &[release("V0.3.0", true)],
            Role::Agent,
            "0.2.0",
            None,
            false,
        )
        .unwrap()
        .unwrap();
        assert!(validate_download(&candidate, 1023, [0x11; 32]).is_err());
        assert!(validate_download(&candidate, 1024, [0x22; 32]).is_err());
        assert!(validate_download(&candidate, 1024, [0x11; 32]).is_ok());
    }

    #[test]
    fn runtime_requires_exact_immutable_release_asset() {
        let candidate =
            select_runtime_candidate(&[release("V0.4.0", true)], &Version::new(0, 4, 0)).unwrap();
        assert_eq!(candidate.tag, "V0.4.0");
        assert_eq!(
            candidate.asset_name(),
            "farhelm-codex-runtime-0.4.0-linux-x86_64.tar.gz"
        );
        assert_eq!(candidate.digest, [0x22; 32]);
        assert!(
            select_runtime_candidate(&[release("V0.4.0", false)], &Version::new(0, 4, 0)).is_err()
        );
    }
}
