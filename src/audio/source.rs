use crate::utils::enc::{encrypt_file_in_place, is_encrypted_file, read_decrypted_file};
use anyhow::{anyhow, Context, Result};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use url::Url;

pub async fn prepare_local_source(uri: &str) -> Result<PathBuf> {
    if let Ok(u) = Url::parse(uri) {
        match u.scheme() {
            "file" => {
                let p = u.to_file_path().map_err(|_| anyhow!("invalid file:// path"))?;
                if !p.exists() {
                    anyhow::bail!("file not found: {}", p.display());
                }
                return Ok(p);
            }
            "http" | "https" => {
                let resp = reqwest::Client::new()
                    .get(uri)
                    .send()
                    .await
                    .context("http get")?
                    .error_for_status()
                    .context("bad status")?;
                let body = resp.bytes().await.context("read body")?;
                let mut tmp = tempfile::Builder::new().prefix("resonix_").tempfile()?;
                tmp.as_file_mut().write_all(&body)?;
                let path = tmp.into_temp_path().keep()?;
                encrypt_file_in_place(&path).context("encrypt http temp file")?;
                return Ok(path);
            }
            _ => {}
        }
    }

    let p = PathBuf::from(uri);
    if !p.exists() {
        anyhow::bail!("source not found: {}", p.display());
    }
    Ok(p)
}

pub async fn transcode_to_mp3(input: &Path) -> Result<PathBuf> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("resonix_").suffix(".mp3");
    let tmp = builder.tempfile()?;
    let out_path = tmp.path().to_path_buf();
    drop(tmp);

    let mut input_arg_path: PathBuf = input.to_path_buf();
    let mut plaintext_tmp: Option<PathBuf> = None;
    if is_encrypted_file(input) {
        let data = read_decrypted_file(input)?;
        let mut t = tempfile::Builder::new().prefix("resonix_ffin_").tempfile()?;
        t.as_file_mut().write_all(&data)?;
        let p = t.into_temp_path().keep()?;
        input_arg_path = p.clone();
        plaintext_tmp = Some(p);
    }

    let ffmpeg_bin = std::env::var("RESONIX_FFMPEG_BIN").unwrap_or_else(|_| "ffmpeg".into());
    let status = tokio::process::Command::new(ffmpeg_bin)
        .arg("-y")
        .arg("-i")
        .arg(&input_arg_path)
        .arg("-vn")
        .arg("-acodec")
        .arg("libmp3lame")
        .arg("-b:a")
        .arg("192k")
        .arg(&out_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .context("run ffmpeg to transcode to mp3")?;

    if let Some(p) = plaintext_tmp.take() {
        let _ = tokio::fs::remove_file(p).await;
    }

    if !status.success() {
        anyhow::bail!("ffmpeg failed with status {status}");
    }

    let meta = tokio::fs::metadata(&out_path).await.context("stat transcoded mp3")?;
    if meta.len() == 0 {
        anyhow::bail!("ffmpeg produced empty file");
    }
    encrypt_file_in_place(&out_path).context("encrypt transcoded mp3")?;
    Ok(out_path)
}

pub fn is_resonix_temp_file(path: &Path) -> bool {
    let tmp_dir = std::env::temp_dir();
    if let Ok(p) = path.canonicalize() {
        if let Ok(t) = tmp_dir.canonicalize() {
            if p.starts_with(t) {
                if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                    return name.starts_with("resonix_");
                }
            }
        }
    }
    false
}

pub fn cleanup_resonix_temp_files() {
    let tmp_dir = std::env::temp_dir();
    if let Ok(read_dir) = fs::read_dir(&tmp_dir) {
        for entry in read_dir.flatten() {
            let p = entry.path();
            if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                if name.starts_with("resonix_") {
                    let _ = fs::remove_file(&p);
                }
            }
        }
    }
}
