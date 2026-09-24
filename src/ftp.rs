use crate::config::Config;
use crate::pipeline::Input;
use std::error::Error;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::Sender;

pub fn run(config: Config, sender: Sender<Input>) -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind((config.ftp_bind.as_str(), config.ftp_port))?;
    for stream in listener.incoming() {
        let stream = stream?;
        if let Err(error) = handle_session(stream, &config, &sender) {
            eprintln!("FTP session failed: {error}");
        }
    }
    Ok(())
}

fn handle_session(
    mut control: TcpStream,
    config: &Config,
    sender: &Sender<Input>,
) -> Result<(), Box<dyn Error>> {
    reply(&mut control, "220 keyence-collector FTP ready")?;
    let mut reader = BufReader::new(control.try_clone()?);
    let mut authenticated = false;
    let mut data_listener = None;

    loop {
        let mut command = String::new();
        if reader.read_line(&mut command)? == 0 {
            return Ok(());
        }
        let mut parts = command.trim_end().splitn(2, ' ');
        let name = parts.next().unwrap_or("").to_ascii_uppercase();
        let argument = parts.next().unwrap_or("");
        match name.as_str() {
            "USER" if argument == config.ftp_user => reply(&mut control, "331 Password required")?,
            "PASS" if argument == config.ftp_password => {
                authenticated = true;
                reply(&mut control, "230 Logged in")?;
            }
            "SYST" => reply(&mut control, "215 UNIX Type: L8")?,
            "TYPE" => reply(&mut control, "200 Type set")?,
            "PWD" => reply(&mut control, "257 \"/\" is current directory")?,
            "PASV" if authenticated => {
                let listener = TcpListener::bind((config.ftp_bind.as_str(), 0))?;
                let port = listener.local_addr()?.port();
                let address = config.ftp_bind.parse::<std::net::Ipv4Addr>()?;
                let octets = address.octets();
                reply(
                    &mut control,
                    &format!(
                        "227 Entering Passive Mode ({},{},{},{},{},{})",
                        octets[0],
                        octets[1],
                        octets[2],
                        octets[3],
                        port / 256,
                        port % 256
                    ),
                )?;
                data_listener = Some(listener);
            }
            "STOR" if authenticated && argument.to_ascii_lowercase().ends_with(".bmp") => {
                let listener = data_listener.take().ok_or("PASV required before STOR")?;
                reply(&mut control, "150 Opening data connection")?;
                let (mut data, _) = listener.accept()?;
                let mut contents = Vec::new();
                data.read_to_end(&mut contents)?;
                sender.send(Input::Bmp(contents))?;
                reply(&mut control, "226 Transfer complete")?;
            }
            "QUIT" => {
                reply(&mut control, "221 Goodbye")?;
                return Ok(());
            }
            _ if !authenticated => reply(&mut control, "530 Not logged in")?,
            _ => reply(&mut control, "502 Command not implemented")?,
        }
    }
}

fn reply(stream: &mut TcpStream, message: &str) -> Result<(), Box<dyn Error>> {
    writeln!(stream, "{message}")?;
    stream.flush()?;
    Ok(())
}
