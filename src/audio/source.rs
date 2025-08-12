use anyhow::{anyhow, Context, Result};
use std::{
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

    let status = tokio::process::Command::new("ffmpeg")
        .arg("-y")
        .arg("-i")
        .arg(input)
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

    if !status.success() {
        anyhow::bail!("ffmpeg failed with status {status}");
    }

    let meta = tokio::fs::metadata(&out_path).await.context("stat transcoded mp3")?;
    if meta.len() == 0 {
        anyhow::bail!("ffmpeg produced empty file");
    }
    Ok(out_path)
}
