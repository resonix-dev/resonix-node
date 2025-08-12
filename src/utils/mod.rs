pub fn format_ram_mb(ram_mb: u64) -> String {
    if ram_mb < 1024 { format!("{} MB", ram_mb) } else { format!("{:.1} GB", ram_mb as f64 / 1024.0) }
}
