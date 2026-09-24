use crate::config::Config;
use std::error::Error;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::TcpListener;
#[cfg(windows)]
use std::net::{Shutdown, TcpStream};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc::Receiver;

pub enum Input {
    Bmp(Vec<u8>),
    Tcp(Vec<u8>),
}

pub fn run_tcp_listener(
    config: Config,
    sender: std::sync::mpsc::Sender<Input>,
) -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind((config.tcp_bind.as_str(), config.tcp_port))?;
    for stream in listener.incoming() {
        let mut stream = stream?;
        let mut data = Vec::new();
        stream.read_to_end(&mut data)?;
        if !data.is_empty() {
            sender.send(Input::Tcp(data))?;
        }
    }
    Ok(())
}

#[cfg(unix)]
pub fn run_pipeline(config: Config, receiver: Receiver<Input>) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(&config.tmp_dir)?;
    if config.parent_socket.exists() {
        fs::remove_file(&config.parent_socket)?;
    }
    let parent_listener = UnixListener::bind(&config.parent_socket)?;
    let bmp_path = config.tmp_dir.join("incoming.bmp");
    let tcp_path = config.tmp_dir.join("incoming.tcp");
    let mut bmp = None;
    let mut tcp = None;

    for input in receiver {
        match input {
            Input::Bmp(data) => {
                validate_bmp(&data)?;
                write_tmp(&bmp_path, &data)?;
                bmp = Some(data);
            }
            Input::Tcp(data) => {
                write_tmp(&tcp_path, &data)?;
                tcp = Some(data);
            }
        }
        if let (Some(bmp_data), Some(tcp_data)) = (bmp.take(), tcp.take()) {
            send_to_parent(&parent_listener, &bmp_data, &tcp_data)?;
            let _ = fs::remove_file(&bmp_path);
            let _ = fs::remove_file(&tcp_path);
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn run_pipeline(config: Config, receiver: Receiver<Input>) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(&config.tmp_dir)?;
    let parent_address = config
        .parent_socket
        .to_str()
        .ok_or("neplatná parent_socket adresa")?;
    let bmp_path = config.tmp_dir.join("incoming.bmp");
    let tcp_path = config.tmp_dir.join("incoming.tcp");
    let mut bmp = None;
    let mut tcp = None;

    for input in receiver {
        match input {
            Input::Bmp(data) => {
                validate_bmp(&data)?;
                write_tmp(&bmp_path, &data)?;
                bmp = Some(data);
            }
            Input::Tcp(data) => {
                write_tmp(&tcp_path, &data)?;
                tcp = Some(data);
            }
        }
        if let (Some(bmp_data), Some(tcp_data)) = (bmp.take(), tcp.take()) {
            send_to_parent(parent_address, &bmp_data, &tcp_data)?;
            let _ = fs::remove_file(&bmp_path);
            let _ = fs::remove_file(&tcp_path);
        }
    }
    Ok(())
}

fn write_tmp(path: &std::path::Path, data: &[u8]) -> Result<(), Box<dyn Error>> {
    let mut file = File::create(path)?;
    file.write_all(data)?;
    file.sync_all()?;
    Ok(())
}

fn validate_bmp(data: &[u8]) -> Result<(), Box<dyn Error>> {
    if data.len() < 2 || &data[..2] != b"BM" {
        return Err("přijatá data nejsou BMP".into());
    }
    Ok(())
}

#[cfg(unix)]
fn send_to_parent(listener: &UnixListener, bmp: &[u8], tcp: &[u8]) -> Result<(), Box<dyn Error>> {
    let (mut socket, _) = listener.accept()?;
    write_frame(&mut socket, bmp)?;
    write_frame(&mut socket, tcp)?;
    socket.shutdown(std::net::Shutdown::Write)?;
    Ok(())
}

#[cfg(windows)]
fn send_to_parent(address: &str, bmp: &[u8], tcp: &[u8]) -> Result<(), Box<dyn Error>> {
    let mut socket = TcpStream::connect(address)?;
    write_tcp_frame(&mut socket, bmp)?;
    write_tcp_frame(&mut socket, tcp)?;
    socket.shutdown(Shutdown::Write)?;
    Ok(())
}

#[cfg(unix)]
fn write_frame(socket: &mut UnixStream, data: &[u8]) -> Result<(), Box<dyn Error>> {
    socket.write_all(&(data.len() as u64).to_be_bytes())?;
    socket.write_all(data)?;
    Ok(())
}

#[cfg(windows)]
fn write_tcp_frame(socket: &mut TcpStream, data: &[u8]) -> Result<(), Box<dyn Error>> {
    socket.write_all(&(data.len() as u64).to_be_bytes())?;
    socket.write_all(data)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_bmp;

    #[test]
    fn accepts_bmp_signature() {
        assert!(validate_bmp(b"BM\x00\x00").is_ok());
    }

    #[test]
    fn rejects_non_bmp_data() {
        assert!(validate_bmp(b"not an image").is_err());
    }
}
