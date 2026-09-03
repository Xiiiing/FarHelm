//! Verified update discovery, download, and extraction for FarHelm releases.

use std::{
    fmt,
    fs::File,
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use flate2::read::GzDecoder;
use reqwest::{Client, redirect::Policy};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::{fs, io::AsyncWriteExt, process::Command};

const RELEASES_API: &str = "https://api.github.com/repos/Xiiiing/FarHelm/releases?per_page=100";
const DOWNLOAD_PREFIX: &str = "https://github.com/Xiiiing/FarHelm/releases/download/";
const API_VERSION: &str = "2026-03-10";
const MAX_REDIRECTS: usize = 8;
const MAX_ARCHIVE_ENTRIES: usize = 2_048;
const MAX_UNPACKED_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Hub,
    Agent,
}

impl Role {
    const fn package_name(self) -> &'static str {
        match self {
            Self::Hub => "farhelm-hub",
            Self::Agent => "farhelm-agent",
        }
    }

    const fn max_archive_bytes(self) -> u64 {
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

impl UpdateCandidate {
    #[must_use]
    pub fn archive_name(&self) -> &str {
        &self.asset.name
    }
}

#[derive(Debug)]
pub struct VerifiedBundle {
    _temporary: TempDir,
    pub root: PathBuf,
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
            .timeout(Duration::from_secs(120))
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
        let releases: Vec<GithubRelease> = response
            .json()
            .await
            .context("GitHub returned invalid FarHelm release metadata")?;
        select_candidate(&releases, role, current, requested, allow_major)
    }

    pub async fn download(
        &self,
        role: Role,
        candidate: &UpdateCandidate,
    ) -> Result<VerifiedBundle> {
        let temporary = tempfile::tempdir().context("failed to create update staging directory")?;
        let archive_path = temporary.path().join("release.tar.gz.part");
        let mut response = self
            .client
            .get(&candidate.asset.browser_download_url)
            .send()
            .await
            .context("failed to download FarHelm release")?
            .error_for_status()
            .context("GitHub rejected the FarHelm release download")?;
        if let Some(length) = response.content_length() {
            ensure!(
                length == candidate.asset.size,
                "release Content-Length does not match immutable asset metadata"
            );
        }

        let mut archive = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&archive_path)
            .await
            .context("failed to create staged release archive")?;
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
                downloaded <= role.max_archive_bytes(),
                "release archive exceeds the role size limit"
            );
            ensure!(
                downloaded <= candidate.asset.size,
                "release archive exceeds immutable asset metadata"
            );
            hasher.update(&chunk);
            archive
                .write_all(&chunk)
                .await
                .context("failed to write staged release archive")?;
        }
        archive
            .sync_all()
            .await
            .context("failed to sync staged release archive")?;
        drop(archive);
        let actual: [u8; 32] = hasher.finalize().into();
        validate_download(candidate, downloaded, actual)?;

        let expected_root = format!("{}-{}-linux-x86_64", role.package_name(), candidate.version);
        let unpacked = temporary.path().join("unpacked");
        std::fs::create_dir(&unpacked).context("failed to create archive extraction directory")?;
        unpack_archive(&archive_path, &unpacked, &expected_root)?;
        let root = unpacked.join(expected_root);
        let package_version = std::fs::read_to_string(root.join("VERSION"))
            .context("release package VERSION is missing")?;
        ensure!(
            package_version.trim() == candidate.version.to_string(),
            "release package VERSION does not match immutable metadata"
        );
        ensure!(
            root.join("install.sh").is_file(),
            "release package installer is missing"
        );
        Ok(VerifiedBundle {
            _temporary: temporary,
            root,
            version: candidate.version.clone(),
        })
    }

    pub async fn install(
        &self,
        bundle: &VerifiedBundle,
        existing_root: Option<&Path>,
    ) -> Result<()> {
        let mut command = Command::new("bash");
        command
            .arg(bundle.root.join("install.sh"))
            .current_dir(&bundle.root)
            .env("FARHELM_UPGRADE", "1")
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        if let Some(root) = existing_root {
            command.env("FARHELM_INSTALL_ROOT", root);
        }
        let status = command
            .status()
            .await
            .context("failed to start verified FarHelm installer")?;
        ensure!(status.success(), "verified FarHelm installer failed");
        Ok(())
    }
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
        if requested.as_ref().is_some_and(|value| value != &version) {
            continue;
        }
        if version <= current {
            continue;
        }
        let name = format!("{}-{}-linux-x86_64.tar.gz", role.package_name(), version);
        let Some(asset) = release.assets.iter().find(|asset| asset.name == name) else {
            continue;
        };
        let digest = parse_digest(asset.digest.as_deref())?;
        ensure!(asset.size > 0, "release asset is empty");
        ensure!(
            asset.size <= role.max_archive_bytes(),
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

fn validate_download(
    candidate: &UpdateCandidate,
    downloaded: u64,
    actual_digest: [u8; 32],
) -> Result<()> {
    ensure!(
        downloaded == candidate.asset.size,
        "release archive is truncated"
    );
    ensure!(
        actual_digest == candidate.digest,
        "release archive SHA-256 does not match immutable asset metadata"
    );
    Ok(())
}

fn unpack_archive(archive_path: &Path, destination: &Path, expected_root: &str) -> Result<()> {
    let archive_file = File::open(archive_path).context("failed to open staged release archive")?;
    let decoder = GzDecoder::new(archive_file);
    let mut archive = tar::Archive::new(decoder);
    let mut entries = 0_usize;
    let mut unpacked_bytes = 0_u64;
    for entry in archive.entries().context("release archive is invalid")? {
        let mut entry = entry.context("release archive entry is invalid")?;
        entries += 1;
        ensure!(
            entries <= MAX_ARCHIVE_ENTRIES,
            "release archive has too many entries"
        );
        let path = entry.path().context("release archive path is invalid")?;
        validate_archive_path(&path, expected_root)?;
        let kind = entry.header().entry_type();
        ensure!(
            kind.is_file() || kind.is_dir(),
            "release archive contains a link or special file"
        );
        if kind.is_file() {
            unpacked_bytes = unpacked_bytes
                .checked_add(entry.size())
                .context("release unpacked size overflowed")?;
            ensure!(
                unpacked_bytes <= MAX_UNPACKED_BYTES,
                "release archive exceeds the unpacked size limit"
            );
        }
        ensure!(
            entry
                .unpack_in(destination)
                .context("failed to unpack release archive entry")?,
            "release archive entry escaped the staging directory"
        );
    }
    ensure!(entries > 0, "release archive is empty");
    Ok(())
}

fn validate_archive_path(path: &Path, expected_root: &str) -> Result<()> {
    ensure!(
        !path.is_absolute(),
        "release archive contains an absolute path"
    );
    let mut components = path.components();
    ensure!(
        components.next() == Some(Component::Normal(expected_root.as_ref())),
        "release archive has an unexpected top-level directory"
    );
    ensure!(
        components.all(|component| matches!(component, Component::Normal(_))),
        "release archive contains an unsafe path"
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
            assets: vec![ReleaseAsset {
                name: format!("farhelm-agent-{version}-linux-x86_64.tar.gz"),
                browser_download_url: format!(
                    "{DOWNLOAD_PREFIX}{tag}/farhelm-agent-{version}-linux-x86_64.tar.gz"
                ),
                size: 1024,
                digest: Some(format!("sha256:{}", "11".repeat(32))),
            }],
        }
    }

    #[test]
    fn selects_only_newest_immutable_uppercase_release() {
        let releases = vec![
            release("v9.0.0", true),
            release("V0.2.0", false),
            release("V0.1.1", true),
            release("V0.2.0", true),
        ];
        let selected = select_candidate(&releases, Role::Agent, "0.1.0", None, false)
            .unwrap()
            .unwrap();
        assert_eq!(selected.version, Version::new(0, 2, 0));
        assert_eq!(selected.tag, "V0.2.0");
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
        let releases = vec![release("V0.1.1", true)];
        assert!(select_candidate(&releases, Role::Agent, "0.1.0", Some("v0.1.1"), false).is_err());
        assert!(select_candidate(&releases, Role::Agent, "0.1.1", Some("V0.1.1"), false).is_err());
    }

    #[test]
    fn validates_digest_and_archive_paths() {
        assert_eq!(
            parse_digest(Some(&format!("sha256:{}", "ab".repeat(32)))).unwrap(),
            [0xab; 32]
        );
        assert!(parse_digest(Some("sha256:abcd")).is_err());
        assert!(
            validate_archive_path(Path::new("../escape"), "farhelm-agent-0.1.0-linux-x86_64")
                .is_err()
        );
        assert!(
            validate_archive_path(Path::new("/absolute"), "farhelm-agent-0.1.0-linux-x86_64")
                .is_err()
        );
        assert!(
            validate_archive_path(Path::new("other/file"), "farhelm-agent-0.1.0-linux-x86_64")
                .is_err()
        );
        assert!(
            validate_archive_path(
                Path::new("farhelm-agent-0.1.0-linux-x86_64/bin/agent"),
                "farhelm-agent-0.1.0-linux-x86_64"
            )
            .is_ok()
        );
    }

    #[test]
    fn rejects_truncated_or_corrupted_downloads() {
        let candidate = select_candidate(
            &[release("V0.1.1", true)],
            Role::Agent,
            "0.1.0",
            None,
            false,
        )
        .unwrap()
        .unwrap();
        assert!(validate_download(&candidate, 1023, [0x11; 32]).is_err());
        assert!(validate_download(&candidate, 1024, [0x22; 32]).is_err());
        assert!(validate_download(&candidate, 1024, [0x11; 32]).is_ok());
    }
}
