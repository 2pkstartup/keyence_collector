mod config;
mod ftp;
mod pipeline;

use std::env;
use std::error::Error;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

use config::Config;
use pipeline::run_pipeline;

fn main() -> Result<(), Box<dyn Error>> {
    let config_path = find_config_path()?;
    let config = Config::from_file(config_path.to_string_lossy().as_ref())?;
    config.validate()?;

    // FTP uploads are forwarded to the processing pipeline.
    let (sender, receiver) = mpsc::channel();
    let ftp_config = config.clone();
    let ftp_sender = sender.clone();
    thread::spawn(move || {
        if let Err(error) = ftp::run(ftp_config, ftp_sender) {
            eprintln!("FTP server stopped: {error}");
        }
    });

    println!("keyence-collector poslouchá na FTP vstupu");
    run_pipeline(config, receiver)
}

fn find_config_path() -> Result<PathBuf, Box<dyn Error>> {
    let config_name = "keyence-collector.conf";

    // An explicit path always has priority over automatic lookup.
    if let Some(path) = env::args().nth(1) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!("Konfigurační soubor nebyl nalezen: {}", path.display()).into());
    }

    let mut candidates = Vec::new();
    if let Ok(current_dir) = env::current_dir() {
        candidates.push(current_dir.join(config_name));
    }
    if let Ok(executable) = env::current_exe() {
        if let Some(executable_dir) = executable.parent() {
            let path = executable_dir.join(config_name);
            if !candidates.contains(&path) {
                candidates.push(path);
            }
        }
    }

    if let Some(path) = candidates.iter().find(|path| path.is_file()) {
        return Ok(path.clone());
    }

    let searched = candidates
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join("; ");
    Err(format!(
        "Konfigurační soubor '{config_name}' nebyl nalezen. Hledáno v: {searched}. \
         Použijte například: cargo run -- .\\keyence-collector.conf"
    )
    .into())
}
