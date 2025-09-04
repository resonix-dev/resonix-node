use anyhow::{anyhow, Context, Result};
use chacha20poly1305::{aead::Aead, ChaCha20Poly1305, KeyInit, Nonce};
use rand::RngCore;
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::OnceLock,
};

const MAGIC: &[u8; 6] = b"RXENC1";
static KEY: OnceLock<[u8; 32]> = OnceLock::new();

pub fn key() -> &'static [u8; 32] {
    KEY.get_or_init(|| {
        if let Ok(b64) = std::env::var("RESONIX_SECRET_B64") {
            use base64::Engine;
            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64) {
                if bytes.len() == 32 {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&bytes);
                    return arr;
                }
            }
        }
        let mut k = [0u8; 32];
        let mut rng = rand::rng();
        rng.fill_bytes(&mut k);
        k
    })
}

pub fn is_encrypted_file(path: &Path) -> bool {
    if let Ok(mut f) = fs::File::open(path) {
        let mut hdr = [0u8; 6];
        if f.read_exact(&mut hdr).is_ok() {
            return &hdr == MAGIC;
        }
    }
    false
}

pub fn encrypt_bytes(plain: &[u8]) -> Result<Vec<u8>> {
    let key = key();
    let cipher = ChaCha20Poly1305::new(key.into());
    let mut nonce_bytes = [0u8; 12];
    let mut rng = rand::rng();
    rng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let mut out = Vec::with_capacity(MAGIC.len() + nonce_bytes.len() + plain.len() + 16);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&nonce_bytes);
    let ct = cipher.encrypt(nonce, plain).map_err(|_| anyhow!("encrypt bytes"))?;
    out.extend_from_slice(&ct);
    Ok(out)
}

pub fn decrypt_bytes(enc: &[u8]) -> Result<Vec<u8>> {
    if enc.len() < MAGIC.len() + 12 + 16 {
        anyhow::bail!("encrypted blob too small");
    }
    if &enc[..MAGIC.len()] != MAGIC {
        anyhow::bail!("missing magic header");
    }
    let nonce_start = MAGIC.len();
    let nonce_end = nonce_start + 12;
    let nonce = Nonce::from_slice(&enc[nonce_start..nonce_end]);
    let ct = &enc[nonce_end..];
    let key = key();
    let cipher = ChaCha20Poly1305::new(key.into());
    let pt = cipher.decrypt(nonce, ct).map_err(|_| anyhow!("decrypt bytes"))?;
    Ok(pt)
}

pub fn encrypt_file_in_place(path: &Path) -> Result<()> {
    if is_encrypted_file(path) {
        return Ok(());
    }
    let data = fs::read(path).with_context(|| format!("read plaintext file: {}", path.display()))?;
    let enc = encrypt_bytes(&data)?;
    let tmp_path = tmp_swap_path(path, ".encswap");
    {
        let mut f = fs::File::create(&tmp_path)
            .with_context(|| format!("create temp enc: {}", tmp_path.display()))?;
        f.write_all(&enc).context("write encrypted")?;
        f.flush().ok();
    }
    fs::rename(&tmp_path, path).with_context(|| format!("replace with encrypted: {}", path.display()))?;
    Ok(())
}

pub fn read_decrypted_file(path: &Path) -> Result<Vec<u8>> {
    let data = fs::read(path).with_context(|| format!("read file: {}", path.display()))?;
    if data.starts_with(MAGIC) {
        decrypt_bytes(&data)
    } else {
        Ok(data)
    }
}

fn tmp_swap_path(path: &Path, ext: &str) -> PathBuf {
    let mut p = path.to_path_buf();
    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("swap");
    let tmp = format!("{}.{}{}", file_name, std::process::id(), ext.trim_start_matches('.'));
    p.set_file_name(tmp);
    p
}
