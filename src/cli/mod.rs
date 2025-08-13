use std::env;

pub enum CliAction {
    PrintVersion,
    InitConfig,
    RunServer,
}

pub fn parse_args() -> CliAction {
    let mut version_flag = false;
    let mut init_config = false;
    for arg in env::args().skip(1) {
        // skip executable name
        match arg.as_str() {
            "--version" | "-V" | "-version" => version_flag = true,
            "--init-config" => init_config = true,
            _ => {}
        }
    }
    if version_flag {
        return CliAction::PrintVersion;
    }
    if init_config {
        return CliAction::InitConfig;
    }
    CliAction::RunServer
}

pub fn print_version() {
    println!("Resonix v{}", env!("CARGO_PKG_VERSION"));
}

pub fn init_config_file() {
    use std::fs;
    use std::path::Path;
    let target = Path::new("Resonix.toml");
    if target.exists() {
        eprintln!("Resonix.toml already exists; aborting --init-config");
        return;
    }
    if let Err(e) = fs::write(target, crate::config::DEFAULT_CONFIG_TEMPLATE) {
        eprintln!("Failed to write Resonix.toml: {e}");
    } else {
        println!("Created Resonix.toml");
    }
}
