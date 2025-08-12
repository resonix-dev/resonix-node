fn main() {
    #[cfg(target_os = "windows")]
    {
        println!("cargo:rerun-if-changed=assets/binary/avatar.ico");
        println!("cargo:rerun-if-changed=assets/binary/icon.ico");

        let mut res = winres::WindowsResource::new();

        let icon_candidates = ["assets/binary/icon.ico", "assets/binary/avatar.ico"];
        for path in icon_candidates {
            if std::path::Path::new(path).exists() {
                res.set_icon(path);
                let _ = res.compile();
                return;
            }
        }
        // No icon found; do nothing.
    }
}
