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
        println!("FTP: nové control spojení od {:?}", stream.peer_addr());
        if let Err(error) = handle_session(stream, &config, &sender) {
            if !is_client_disconnect(error.as_ref()) {
                eprintln!("FTP session failed: {error}");
            }
        }
    }
    Ok(())
}

fn handle_session(
    // Handles one FTP control connection and sends uploaded BMP files to the pipeline.
    mut control: TcpStream,
    config: &Config,
    sender: &Sender<Input>,
) -> Result<(), Box<dyn Error>> {
    reply(&mut control, "220 keyence-collector FTP ready")?;
    let mut reader = BufReader::new(control.try_clone()?);
    let mut authenticated = false;
    let mut data_listener = None;
    let mut active_data_address = None;

    loop {
        let mut command = String::new();
        if reader.read_line(&mut command)? == 0 {
            return Ok(());
        }
        let mut parts = command.trim_end().splitn(2, ' ');
        let name = parts.next().unwrap_or("").to_ascii_uppercase();
        let argument = parts.next().unwrap_or("");
        println!("FTP command: {name} {argument}");
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
                // A separate ephemeral listener is used for every passive data transfer.
                let listener = TcpListener::bind((config.ftp_bind.as_str(), 0))?;
                let port = listener.local_addr()?.port();
                let address = config.ftp_advertise.parse::<std::net::Ipv4Addr>()?;
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
                active_data_address = None;
            }
            "PORT" if authenticated => {
                active_data_address = Some(parse_port_argument(argument)?);
                data_listener = None;
                reply(&mut control, "200 PORT command successful")?;
            }
            "STOR" if authenticated => {
                reply(&mut control, "150 Opening data connection")?;
                let mut data = if let Some(listener) = data_listener.take() {
                    listener.accept()?.0
                } else if let Some(address) = active_data_address.take() {
                    TcpStream::connect(address)?
                } else {
                    return Err("před STOR nebyl nastaven PASV ani PORT".into());
                };
                let mut contents = Vec::new();
                data.read_to_end(&mut contents)?;
                println!("FTP STOR: přijat soubor {argument} ({} B)", contents.len());
                sender.send(Input::Bmp {
                    data: contents,
                    name: argument.to_string(),
                })?;
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

fn parse_port_argument(argument: &str) -> Result<String, Box<dyn Error>> {
    let values: Vec<u16> = argument
        .split(',')
        .map(str::parse)
        .collect::<Result<_, _>>()?;
    if values.len() != 6 || values[..4].iter().any(|value| *value > 255) {
        return Err("neplatný PORT argument".into());
    }
    let port = values[4] * 256 + values[5];
    Ok(format!(
        "{}.{}.{}.{}:{port}",
        values[0], values[1], values[2], values[3]
    ))
}

fn reply(stream: &mut TcpStream, message: &str) -> Result<(), Box<dyn Error>> {
    writeln!(stream, "{message}")?;
    stream.flush()?;
    Ok(())
}

fn is_client_disconnect(error: &(dyn Error + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(error) = current {
        if let Some(io_error) = error.downcast_ref::<std::io::Error>() {
            return matches!(
                io_error.kind(),
                std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::ConnectionReset
            ) || matches!(io_error.raw_os_error(), Some(10053 | 10054 | 10058));
        }
        current = error.source();
    }
    false
}
