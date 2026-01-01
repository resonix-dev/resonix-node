use anyhow::{anyhow, bail, Context, Result};
use reqwest::Client;
use serde::Deserialize;
use std::{
    ffi::OsString,
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
};
use tar::Archive;
use tokio::{fs, io::AsyncWriteExt};
use tracing::info;
use xz2::read::XzDecoder;

const RELEASE_URL: &str = "https://api.github.com/repos/BtbN/FFmpeg-Builds/releases/latest";

#[derive(Clone, Copy)]
enum ArchiveKind {
    Zip,
    TarXz,
}

#[derive(Clone, Copy)]
struct PlatformSpec {
    slug: &'static str,
    archive: ArchiveKind,
    binary_name: &'static str,
    asset_extension: &'static str,
}

#[derive(Clone, Deserialize)]
struct GithubRelease {
    assets: Vec<GithubAsset>,
}

#[derive(Clone, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

pub fn default_ffmpeg_binary_path() -> Result<PathBuf> {
    let install_dir = default_install_dir()?;
    let spec = platform_spec()?;
    Ok(install_dir.join(spec.binary_name))
}

pub async fn download_latest_ffmpeg() -> Result<PathBuf> {
    let spec = platform_spec()?;
    let install_dir = default_install_dir()?;
    fs::create_dir_all(&install_dir).await.context("create ffmpeg install directory")?;

    let client = Client::builder()
        .user_agent(format!("Resonix/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .context("build http client")?;

    let release = fetch_latest_release(&client).await?;
    let asset = select_asset(&release.assets, spec)
        .ok_or_else(|| anyhow!("No ffmpeg build available for this platform"))?;

    info!(asset = %asset.name, "Downloading ffmpeg build");

    let temp_dir = tempfile::tempdir().context("create ffmpeg temp dir")?;
    let download_path = temp_dir.path().join(&asset.name);

    let mut response = client
        .get(&asset.browser_download_url)
        .send()
        .await
        .context("request ffmpeg archive")?
        .error_for_status()
        .context("ffmpeg download returned error status")?;

    let mut file = fs::File::create(&download_path).await.context("create temp archive file")?;
    while let Some(chunk) = response.chunk().await.context("stream ffmpeg archive")? {
        file.write_all(&chunk).await.context("write ffmpeg archive chunk")?;
    }
    file.flush().await.context("flush ffmpeg archive")?;
    drop(file);

    let install_dir_clone = install_dir.clone();
    let download_path_clone = download_path.clone();
    let extracted_path =
        tokio::task::spawn_blocking(move || extract_archive(&download_path_clone, &install_dir_clone, spec))
            .await
            .context("extract ffmpeg archive join error")??;

    Ok(extracted_path)
}

async fn fetch_latest_release(client: &Client) -> Result<GithubRelease> {
    let response = client
        .get(RELEASE_URL)
        .send()
        .await
        .context("request ffmpeg release metadata")?
        .error_for_status()
        .context("ffmpeg release metadata returned error status")?;

    let bytes = response.bytes().await.context("read release metadata bytes")?;
    let release: GithubRelease = serde_json::from_slice(&bytes).context("parse release metadata")?;
    Ok(release)
}

fn extract_archive(archive_path: &Path, install_dir: &Path, spec: PlatformSpec) -> Result<PathBuf> {
    match spec.archive {
        ArchiveKind::Zip => extract_zip(archive_path, install_dir, spec),
        ArchiveKind::TarXz => extract_tar_xz(archive_path, install_dir, spec),
    }
}

fn extract_zip(archive_path: &Path, install_dir: &Path, spec: PlatformSpec) -> Result<PathBuf> {
    let file = File::open(archive_path).with_context(|| format!("open {}", archive_path.display()))?;
    let mut archive = zip::ZipArchive::new(file).context("read ffmpeg zip archive")?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).context("read ffmpeg zip entry")?;
        if entry.is_dir() {
            continue;
        }
        if entry_matches(entry.name(), spec.binary_name) {
            let target_path = install_dir.join(spec.binary_name);
            write_entry_to_path(&mut entry, &target_path)?;
            set_exec_perms(&target_path)?;
            return Ok(target_path);
        }
    }

    bail!("ffmpeg binary not found in zip archive")
}

fn extract_tar_xz(archive_path: &Path, install_dir: &Path, spec: PlatformSpec) -> Result<PathBuf> {
    let file = File::open(archive_path).with_context(|| format!("open {}", archive_path.display()))?;
    let decoder = XzDecoder::new(file);
    let mut archive = Archive::new(decoder);

    for entry in archive.entries().context("iterate tar entries")? {
        let mut entry = entry.context("read tar entry")?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path().context("read tar entry path")?;
        if entry_matches(&path.to_string_lossy(), spec.binary_name) {
            let target_path = install_dir.join(spec.binary_name);
            write_entry_to_path(&mut entry, &target_path)?;
            set_exec_perms(&target_path)?;
            return Ok(target_path);
        }
    }

    bail!("ffmpeg binary not found in tar archive")
}

fn write_entry_to_path<R: Read>(reader: &mut R, target_path: &Path) -> Result<()> {
    if target_path.exists() {
        std::fs::remove_file(target_path).ok();
    }
    let mut output =
        File::create(target_path).with_context(|| format!("create {}", target_path.display()))?;
    io::copy(reader, &mut output).context("write ffmpeg binary")?;
    Ok(())
}

fn entry_matches(entry_path: &str, binary_name: &str) -> bool {
    let normalized = entry_path.replace('\\', "/");
    normalized.contains("/bin/") && normalized.ends_with(binary_name)
}

fn default_install_dir() -> Result<PathBuf> {
    Ok(home_dir()?.join(".resonix").join("bin"))
}

fn home_dir() -> Result<PathBuf> {
    if let Some(home) = std::env::var_os("HOME") {
        return Ok(PathBuf::from(home));
    }

    if cfg!(windows) {
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            return Ok(PathBuf::from(profile));
        }
        let drive = std::env::var_os("HOMEDRIVE");
        let path = std::env::var_os("HOMEPATH");
        if let (Some(drive), Some(path)) = (drive, path) {
            let mut combined = OsString::from(drive);
            combined.push(path);
            return Ok(PathBuf::from(combined));
        }
    }

    Err(anyhow!("Unable to determine the current user's home directory"))
}

fn select_asset<'a>(assets: &'a [GithubAsset], spec: PlatformSpec) -> Option<GithubAsset> {
    assets.iter().find(|asset| match_asset(asset, spec)).cloned()
}

fn match_asset(asset: &GithubAsset, spec: PlatformSpec) -> bool {
    asset.name.contains(spec.slug)
        && asset.name.contains("gpl")
        && !asset.name.contains("shared")
        && asset.name.ends_with(spec.asset_extension)
}

fn platform_spec() -> Result<PlatformSpec> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Ok(PlatformSpec {
            slug: "win64",
            archive: ArchiveKind::Zip,
            binary_name: "ffmpeg.exe",
            asset_extension: ".zip",
        }),
        ("linux", "x86_64") => Ok(PlatformSpec {
            slug: "linux64",
            archive: ArchiveKind::TarXz,
            binary_name: "ffmpeg",
            asset_extension: ".tar.xz",
        }),
        ("linux", "aarch64") => Ok(PlatformSpec {
            slug: "linuxarm64",
            archive: ArchiveKind::TarXz,
            binary_name: "ffmpeg",
            asset_extension: ".tar.xz",
        }),
        ("linux", "arm") => Ok(PlatformSpec {
            slug: "linuxarmhf",
            archive: ArchiveKind::TarXz,
            binary_name: "ffmpeg",
            asset_extension: ".tar.xz",
        }),
        _ => bail!(
            "Automatic ffmpeg download is not supported on this platform. Please install ffmpeg and set FFMPEG_PATH."
        ),
    }
}

fn set_exec_perms(_path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms =
            std::fs::metadata(_path).with_context(|| format!("stat {}", _path.display()))?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(_path, perms).with_context(|| format!("set perms for {}", _path.display()))?;
    }
    Ok(())
}
