use std::env;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_else(|_| String::from("unknown"));
    if target_os == "windows" {
        println!("cargo:rerun-if-changed=assets/app/avatar.ico");
        println!("cargo:rerun-if-changed=assets/app/icon.ico");
        let mut res = winres::WindowsResource::new();

        for path in ["assets/app/icon.ico", "assets/app/avatar.ico"] {
            if Path::new(path).exists() {
                res.set_icon(path);
                break;
            }
        }

        let company = env::var("RESONIX_COMPANY").unwrap_or_else(|_| "Resonix OSS Team".into());
        let product = env::var("RESONIX_PRODUCT").unwrap_or_else(|_| "Resonix".into());
        let copyright = env::var("RESONIX_COPYRIGHT").unwrap_or_else(|_| "© 2025 Resonix OSS".into());

        let version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
        res.set("CompanyName", &company);
        res.set("FileDescription", "High-performance relay-based audio node");
        res.set("ProductName", &product);
        res.set("ProductVersion", &version);
        res.set("FileVersion", &version);
        res.set("OriginalFilename", "resonix-node.exe");
        res.set("InternalName", "resonix-node");
        res.set("LegalCopyright", &copyright);

        if let Err(e) = res.compile() {
            eprintln!("Failed to embed Windows resources: {e}");
        }
    }
    // If not Windows or no icons, nothing else to do.
}
