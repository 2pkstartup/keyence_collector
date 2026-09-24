#[path = "../config.rs"]
mod config;

use config::Config;
use std::env;
use std::error::Error;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

fn main() -> Result<(), Box<dyn Error>> {
    let config_path = find_config_path()?;
    let config = Config::from_file(config_path.to_string_lossy().as_ref())?;
    config.validate()?;
    let ftp_address = format!(
        "{}:{}",
        ftp_connect_host(&config.ftp_bind, &config.ftp_advertise),
        config.ftp_port
    );
    let parent_address = config.parent_socket.to_string_lossy().to_string();

    let parent_port = parent_address.parse::<SocketAddr>()?.port();
    let parent_listener = TcpListener::bind(SocketAddr::new(
        IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
        parent_port,
    ))?;
    parent_listener.set_nonblocking(false)?;
    println!("Validator čeká na parent spojení na {parent_address}");

    send_bmp_via_ftp(&ftp_address, &config.ftp_user, &config.ftp_password)?;

    let (mut parent, _) = parent_listener.accept()?;
    parent.set_read_timeout(Some(Duration::from_secs(10)))?;
    let bmp = read_frame(&mut parent)?;
    let name = String::from_utf8(read_frame(&mut parent)?)?;

    if bmp.len() < 2 || &bmp[..2] != b"BM" {
        return Err("Collector předal neplatný BMP payload".into());
    }
    println!("OK: collector přijal BMP ({} B), název {}", bmp.len(), name);
    Ok(())
}

fn ftp_connect_host<'a>(bind: &'a str, advertise: &'a str) -> &'a str {
    if bind == "0.0.0.0" || bind == "::" {
        advertise
    } else {
        bind
    }
}

fn find_config_path() -> Result<std::path::PathBuf, Box<dyn Error>> {
    let explicit = env::args().nth(1);
    let mut candidates = Vec::new();
    if let Some(path) = explicit {
        candidates.push(std::path::PathBuf::from(path));
    } else {
        if let Ok(current_dir) = env::current_dir() {
            candidates.push(current_dir.join("keyence-collector.conf"));
        }
        if let Ok(executable) = env::current_exe() {
            if let Some(directory) = executable.parent() {
                let path = directory.join("keyence-collector.conf");
                if !candidates.contains(&path) {
                    candidates.push(path);
                }
            }
        }
    }

    candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| {
            "Konfigurační soubor keyence-collector.conf nebyl nalezen. Předejte jeho cestu jako první argument.".into()
        })
}

fn send_bmp_via_ftp(address: &str, user: &str, password: &str) -> Result<(), Box<dyn Error>> {
    let mut control = TcpStream::connect(address)?;
    control.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(control.try_clone()?);
    expect_reply(&mut reader, "220")?;
    send_command(&mut control, &format!("USER {user}"))?;
    expect_reply(&mut reader, "331")?;
    send_command(&mut control, &format!("PASS {password}"))?;
    expect_reply(&mut reader, "230")?;
    send_command(&mut control, "PASV")?;
    let passive_reply = read_reply(&mut reader)?;
    let data_address = parse_pasv_address(&passive_reply)?;
    let mut data = TcpStream::connect(data_address)?;
    send_command(&mut control, "STOR validator.bmp")?;
    expect_reply(&mut reader, "150")?;
    data.write_all(&test_bmp())?;
    data.shutdown(std::net::Shutdown::Write)?;
    drop(data);
    expect_reply(&mut reader, "226")?;
    send_command(&mut control, "QUIT")?;
    Ok(())
}

fn test_bmp() -> Vec<u8> {
    let mut bmp = vec![0_u8; 54];
    bmp[0] = b'B';
    bmp[1] = b'M';
    bmp
}

fn send_command(stream: &mut TcpStream, command: &str) -> Result<(), Box<dyn Error>> {
    writeln!(stream, "{command}")?;
    stream.flush()?;
    Ok(())
}

fn read_reply(reader: &mut BufReader<TcpStream>) -> Result<String, Box<dyn Error>> {
    let mut reply = String::new();
    reader.read_line(&mut reply)?;
    if reply.is_empty() {
        return Err("FTP server ukončil spojení bez odpovědi".into());
    }
    Ok(reply)
}

fn expect_reply(reader: &mut BufReader<TcpStream>, expected: &str) -> Result<(), Box<dyn Error>> {
    let reply = read_reply(reader)?;
    if !reply.starts_with(expected) {
        return Err(format!("FTP odpověď '{reply}', očekáváno {expected}").into());
    }
    Ok(())
}

fn parse_pasv_address(reply: &str) -> Result<String, Box<dyn Error>> {
    let start = reply.find('(').ok_or("FTP PASV odpověď nemá adresu")? + 1;
    let end = reply[start..]
        .find(')')
        .map(|index| start + index)
        .ok_or("FTP PASV odpověď nemá uzavírací závorku")?;
    let values: Vec<u16> = reply[start..end]
        .split(',')
        .map(str::parse)
        .collect::<Result<_, _>>()?;
    if values.len() != 6 || values[..4].iter().any(|value| *value > 255) {
        return Err("FTP PASV odpověď má neplatný formát".into());
    }
    let port = values[4] * 256 + values[5];
    Ok(format!(
        "{}.{}.{}.{}:{port}",
        values[0], values[1], values[2], values[3]
    ))
}

fn read_frame(stream: &mut TcpStream) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut length = [0_u8; 8];
    stream.read_exact(&mut length)?;
    let length = u64::from_be_bytes(length);
    if length > 16 * 1024 * 1024 {
        return Err("Collector poslal příliš velký payload".into());
    }
    let mut data = vec![0_u8; length as usize];
    stream.read_exact(&mut data)?;
    Ok(data)
}
