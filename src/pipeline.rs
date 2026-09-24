use crate::config::Config;
use std::error::Error;
#[cfg(unix)]
use std::fs;
use std::io::Write;
#[cfg(windows)]
use std::net::{Shutdown, TcpStream};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc::Receiver;

pub enum Input {
    /// BMP data received through FTP.
    Bmp { data: Vec<u8>, name: String },
}

#[cfg(unix)]
pub fn run_pipeline(config: Config, receiver: Receiver<Input>) -> Result<(), Box<dyn Error>> {
    if config.parent_socket.exists() {
        fs::remove_file(&config.parent_socket)?;
    }
    let parent_listener = UnixListener::bind(&config.parent_socket)?;

    for input in receiver {
        match input {
            Input::Bmp { data, name } => {
                validate_bmp(&data)?;
                println!("FTP vstup: přijat BMP {name} ({} B)", data.len());
                send_to_parent(&parent_listener, &data, &name)?;
                println!(
                    "Pipeline: předáno parent procesu (BMP {} B, název {name})",
                    data.len()
                );
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn run_pipeline(config: Config, receiver: Receiver<Input>) -> Result<(), Box<dyn Error>> {
    let parent_address = config
        .parent_socket
        .to_str()
        .ok_or("neplatná parent_socket adresa")?;

    for input in receiver {
        match input {
            Input::Bmp { data, name } => {
                validate_bmp(&data)?;
                println!("FTP vstup: přijat BMP {name} ({} B)", data.len());
                send_to_parent(parent_address, &data, &name)?;
                println!(
                    "Pipeline: předáno parent procesu (BMP {} B, název {name})",
                    data.len()
                );
            }
        }
    }
    Ok(())
}

fn validate_bmp(data: &[u8]) -> Result<(), Box<dyn Error>> {
    if data.len() < 2 || &data[..2] != b"BM" {
        return Err("přijatá data nejsou BMP".into());
    }
    Ok(())
}

// Each payload is prefixed with an unsigned 64-bit big-endian length.
#[cfg(unix)]
fn send_to_parent(listener: &UnixListener, bmp: &[u8], name: &str) -> Result<(), Box<dyn Error>> {
    let (mut socket, _) = listener.accept()?;
    write_frame(&mut socket, bmp)?;
    write_frame(&mut socket, name.as_bytes())?;
    socket.shutdown(std::net::Shutdown::Write)?;
    Ok(())
}

#[cfg(windows)]
fn send_to_parent(address: &str, bmp: &[u8], name: &str) -> Result<(), Box<dyn Error>> {
    let mut socket = TcpStream::connect(address)?;
    write_tcp_frame(&mut socket, bmp)?;
    write_tcp_frame(&mut socket, name.as_bytes())?;
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
