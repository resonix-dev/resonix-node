use std::{io::Write, path::PathBuf};
use anyhow::{anyhow, Context, Result};
use url::Url;

pub async fn prepare_local_source(uri: &str) -> Result<PathBuf> {
    if let Ok(u) = Url::parse(uri) {
        match u.scheme() {
            "file" => {
                let p = u.to_file_path().map_err(|_| anyhow!("invalid file:// path"))?;
                if !p.exists() { anyhow::bail!("file not found: {}", p.display()); }
                return Ok(p);
            }
            "http" | "https" => {
                let resp = reqwest::Client::new().get(uri).send().await.context("http get")?.error_for_status().context("bad status")?;
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
    if !p.exists() { anyhow::bail!("source not found: {}", p.display()); }
    Ok(p)
}
