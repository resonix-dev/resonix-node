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
