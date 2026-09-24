mod config;
mod ftp;
mod pipeline;

use std::env;
use std::error::Error;
use std::sync::mpsc;
use std::thread;

use config::Config;
use pipeline::run_pipeline;

fn main() -> Result<(), Box<dyn Error>> {
    let config_path = env::args()
        .nth(1)
        .unwrap_or_else(|| "keyence-collector.conf".to_string());
    let config = Config::from_file(&config_path)?;
    config.validate()?;

    let (sender, receiver) = mpsc::channel();
    let ftp_config = config.clone();
    let ftp_sender = sender.clone();
    thread::spawn(move || {
        if let Err(error) = ftp::run(ftp_config, ftp_sender) {
            eprintln!("FTP server stopped: {error}");
        }
    });

    let tcp_config = config.clone();
    thread::spawn(move || {
        if let Err(error) = pipeline::run_tcp_listener(tcp_config, sender) {
            eprintln!("TCP listener stopped: {error}");
        }
    });

    println!("keyence-collector poslouchá na FTP a TCP vstupech");
    run_pipeline(config, receiver)
}
