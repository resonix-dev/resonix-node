use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

fn download_to(url: &str, dest: &Path) -> Result<(), String> {
    println!("cargo:warning=Downloading {} -> {}", url, dest.display());
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let resp = reqwest::blocking::get(url).map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("Request failed: {}", resp.status()));
    }
    let bytes = resp.bytes().map_err(|e| e.to_string())?;
    fs::write(dest, &bytes).map_err(|e| e.to_string())?;
    Ok(())
}

fn ensure_yt_dlp(target_os: &str, out_dir: &Path) -> Result<PathBuf, String> {
    let (url, filename) = match target_os {
        "windows" => ("https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe", "yt-dlp.exe"),
        "macos" => ("https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_macos", "yt-dlp"),
        _ => ("https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp", "yt-dlp"), // linux & others
    };
    let dest = out_dir.join(filename);
    if !dest.exists() {
        download_to(url, &dest)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = fs::metadata(&dest) {
            let mut perm = meta.permissions();
            perm.set_mode(0o755);
            let _ = fs::set_permissions(&dest, perm);
        }
    }
    Ok(dest)
}

fn ensure_ffmpeg(target_os: &str, out_dir: &Path) -> Result<PathBuf, String> {
    if target_os == "macos" {
        return Err("Skipping ffmpeg embed on macOS".into());
    }

    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_else(|_| "x86_64".into());
    let (platform_tag, is_zip) = match (target_os, target_arch.as_str()) {
        ("windows", "aarch64") => ("winarm64", true),
        ("windows", _) => ("win64", true),
        ("linux", "aarch64") => ("linuxarm64", false),
        ("linux", _) => ("linux64", false),
        (other, _) => return Err(format!("Unsupported OS for ffmpeg embedding: {other}")),
    };
    let archive_name = if is_zip { "ffmpeg.zip" } else { "ffmpeg.tar.xz" };
    let archive_url = format!(
        "https://github.com/BtbN/FFmpeg-Builds/releases/latest/download/ffmpeg-master-latest-{platform_tag}-gpl.{}",
        if is_zip { "zip" } else { "tar.xz" }
    );
    let bin_subpath = if target_os == "windows" {
        format!("ffmpeg-master-latest-{platform_tag}-gpl/bin/ffmpeg.exe")
    } else {
        format!("ffmpeg-master-latest-{platform_tag}-gpl/bin/ffmpeg")
    };
    let ffmpeg_bin = out_dir.join(if target_os == "windows" { "ffmpeg.exe" } else { "ffmpeg" });
    if ffmpeg_bin.exists() {
        // Assume prior full extraction already happened.
        return Ok(ffmpeg_bin);
    }

    let archive_path = out_dir.join(archive_name);
    download_to(&archive_url, &archive_path)?;
    if is_zip {
        let file = fs::File::open(&archive_path).map_err(|e| e.to_string())?;
        let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        for i in 0..zip.len() {
            let mut f = zip.by_index(i).map_err(|e| e.to_string())?;
            let name = f.name().to_string();
            if let Some(bin_prefix) = bin_subpath.rsplit_once('/') {
                // (dir, file)
                let bin_dir_prefix = bin_prefix.0.to_string();
                if name.ends_with('/') {
                    continue;
                }
                if name.contains(&bin_dir_prefix) {
                    if let Some(filename) = name.split('/').last() {
                        let out_path = out_dir.join(filename);
                        let mut out_f = fs::File::create(&out_path).map_err(|e| e.to_string())?;
                        io::copy(&mut f, &mut out_f).map_err(|e| e.to_string())?;
                    }
                }
            }
        }
    } else {
        // tar.xz
        let file = fs::File::open(&archive_path).map_err(|e| e.to_string())?;
        let decompressor = xz2::read::XzDecoder::new(file);
        let mut archive = tar::Archive::new(decompressor);
        for entry in archive.entries().map_err(|e| e.to_string())? {
            let mut entry = entry.map_err(|e| e.to_string())?;
            if let Ok(path) = entry.path() {
                if let Some(path_str) = path.to_str() {
                    if let Some((bin_dir_prefix, _file)) = bin_subpath.rsplit_once('/') {
                        if path_str.contains(bin_dir_prefix) && !path_str.ends_with('/') {
                            if let Some(filename) = path.file_name() {
                                let out_path = out_dir.join(filename);
                                entry.unpack(&out_path).map_err(|e| e.to_string())?;
                            }
                        }
                    }
                }
            }
        }
    }
    let _ = fs::remove_file(&archive_path);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = fs::metadata(&ffmpeg_bin) {
            let mut perm = meta.permissions();
            perm.set_mode(0o755);
            let _ = fs::set_permissions(&ffmpeg_bin, perm);
        }
    }
    Ok(ffmpeg_bin)
}

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_else(|_| String::from("unknown"));
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_OS");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-check-cfg=cfg(has_embedded_bins)");

    if target_os == "windows" {
        println!("cargo:rerun-if-changed=assets/app/avatar.ico");
        println!("cargo:rerun-if-changed=assets/app/icon.ico");
        let mut res = winres::WindowsResource::new();
        for path in ["assets/app/icon.ico", "assets/app/avatar.ico"] {
            if Path::new(path).exists() {
                res.set_icon(path);
                let _ = res.compile();
                break;
            }
        }
    }

    let bin_dir = Path::new("assets").join("bin");
    fs::create_dir_all(&bin_dir).expect("create bin dir");

    if let Err(e) = ensure_yt_dlp(&target_os, &bin_dir) {
        println!("cargo:warning=Failed to ensure yt-dlp: {e}");
    }
    if let Err(e) = ensure_ffmpeg(&target_os, &bin_dir) {
        println!("cargo:warning=Failed to ensure ffmpeg: {e}");
    }

    println!("cargo:rerun-if-changed={}", bin_dir.display());

    println!("cargo:rustc-env=RESONIX_EMBED_OS_DIR={}", bin_dir.display());

    let out_dir_fs = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR set"));
    let gen_path = out_dir_fs.join("embedded_bins.rs");
    let mut gen = String::new();
    gen.push_str("// @generated by build.rs\n");
    let yt = match target_os.as_str() {
        "windows" => bin_dir.join("yt-dlp.exe"),
        _ => bin_dir.join("yt-dlp"),
    };
    if yt.exists() {
        let rel = yt.strip_prefix(".").unwrap_or(&yt); // best-effort
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        gen.push_str(&format!("pub const YT_DLP_PATH: &str = r#\"{}\"#;\n", yt.display()));
        gen.push_str(&format!(
            "pub const YT_DLP: &[u8] = include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), r#\"/{}\"#));\n",
            rel_str
        ));
    } else {
        gen.push_str("pub const YT_DLP_PATH: &str = \"\";\npub const YT_DLP: &[u8] = &[];\n");
    }
    let ff = if target_os == "windows" { bin_dir.join("ffmpeg.exe") } else { bin_dir.join("ffmpeg") };
    if ff.exists() {
        let rel = ff.strip_prefix(".").unwrap_or(&ff);
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        gen.push_str(&format!("pub const FFMPEG_PATH: &str = r#\"{}\"#;\n", ff.display()));
        gen.push_str(&format!(
            "pub const FFMPEG: &[u8] = include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), r#\"/{}\"#));\n",
            rel_str
        ));
    } else {
        gen.push_str("pub const FFMPEG_PATH: &str = \"\";\npub const FFMPEG: &[u8] = &[];\n");
    }

    // Generic embedding of every file present in assets/bin for completeness (so ffprobe, dlls, etc. are shipped).
    let mut embedded_list_entries = String::new();
    embedded_list_entries
        .push_str("pub struct EmbeddedFile { pub name: &'static str, pub bytes: &'static [u8] }\n");
    let mut array_items = Vec::new();
    if let Ok(read_dir) = fs::read_dir(&bin_dir) {
        for entry in read_dir.flatten() {
            if let Ok(ft) = entry.file_type() {
                if ft.is_dir() {
                    continue;
                }
            }
            let path = entry.path();
            if let Some(fname) = path.file_name().and_then(|s| s.to_str()) {
                let rel = path.strip_prefix(".").unwrap_or(&path);
                let rel_str = rel.to_string_lossy().replace('\\', "/");
                // Sanitize identifier
                let mut ident = fname
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                    .collect::<String>();
                if !ident.chars().next().map(|c| c.is_ascii_alphabetic() || c == '_').unwrap_or(false) {
                    ident = format!("_{}", ident);
                }
                ident = ident.to_ascii_uppercase();
                embedded_list_entries.push_str(&format!("pub const EMBED_FILE_{ident}: &[u8] = include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), r#\"/{}\"#));\n", rel_str));
                array_items
                    .push(format!("EmbeddedFile {{ name: r#\"{fname}\"#, bytes: EMBED_FILE_{ident} }}"));
            }
        }
    }
    embedded_list_entries.push_str("pub const EMBEDDED_FILES: &[EmbeddedFile] = &[\n");
    for item in &array_items {
        embedded_list_entries.push_str("    ");
        embedded_list_entries.push_str(item);
        embedded_list_entries.push_str(",\n");
    }
    embedded_list_entries.push_str("];");
    gen.push_str(&embedded_list_entries);
    if fs::write(&gen_path, gen).is_err() {
        println!("cargo:warning=Failed to write embedded_bins.rs");
    }
    println!("cargo:rustc-env=RESONIX_EMBED_BINS_RS={}", gen_path.display());
    println!("cargo:rustc-cfg=has_embedded_bins");
}
