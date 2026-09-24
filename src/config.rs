use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::fs;
use std::net::IpAddr;
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Clone, Debug)]
pub struct Config {
    pub ftp_bind: String,
    pub ftp_port: u16,
    pub ftp_user: String,
    pub ftp_password: String,
    pub tcp_bind: String,
    pub tcp_port: u16,
    pub parent_socket: PathBuf,
    pub tmp_dir: PathBuf,
}

#[derive(Debug)]
struct ConfigError(String);

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for ConfigError {}

impl Config {
    pub fn from_file(path: &str) -> Result<Self, Box<dyn Error>> {
        let contents = fs::read_to_string(path)?;
        let mut values = HashMap::new();
        for (line_number, line) in contents.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let (key, value) = line.split_once('=').ok_or_else(|| {
                ConfigError(format!("{path}:{}: očekáváno key=value", line_number + 1))
            })?;
            values.insert(key.trim().to_string(), value.trim().to_string());
        }

        Ok(Self {
            ftp_bind: required(&values, "ftp_bind")?,
            ftp_port: parse(&values, "ftp_port")?,
            ftp_user: required(&values, "ftp_user")?,
            ftp_password: required(&values, "ftp_password")?,
            tcp_bind: required(&values, "tcp_bind")?,
            tcp_port: parse(&values, "tcp_port")?,
            parent_socket: PathBuf::from(required(&values, "parent_socket")?),
            tmp_dir: PathBuf::from(required(&values, "tmp_dir")?),
        })
    }

    pub fn validate(&self) -> Result<(), Box<dyn Error>> {
        IpAddr::from_str(&self.ftp_bind)
            .map_err(|_| ConfigError("ftp_bind musí být IP adresa".to_string()))?;
        IpAddr::from_str(&self.tcp_bind)
            .map_err(|_| ConfigError("tcp_bind musí být IP adresa".to_string()))?;
        if self.ftp_user.is_empty() || self.ftp_password.is_empty() {
            return Err(
                ConfigError("ftp_user a ftp_password nesmí být prázdné".to_string()).into(),
            );
        }
        if !self.tmp_dir.is_absolute() {
            return Err(ConfigError("tmp_dir musí být absolutní cesta".to_string()).into());
        }
        if cfg!(windows) {
            self.parent_socket
                .to_str()
                .ok_or_else(|| ConfigError("parent_socket musí být platná TCP adresa".to_string()))?
                .parse::<std::net::SocketAddr>()
                .map_err(|_| {
                    ConfigError("parent_socket musí být TCP adresa ve tvaru IP:port".to_string())
                })?;
        }
        Ok(())
    }
}

fn required(values: &HashMap<String, String>, key: &str) -> Result<String, Box<dyn Error>> {
    values
        .get(key)
        .filter(|value| !value.is_empty())
        .cloned()
        .ok_or_else(|| ConfigError(format!("chybí konfigurace {key}")).into())
}

fn parse<T: FromStr>(values: &HashMap<String, String>, key: &str) -> Result<T, Box<dyn Error>>
where
    T::Err: fmt::Display,
{
    required(values, key)?
        .parse()
        .map_err(|error: T::Err| ConfigError(format!("neplatná hodnota {key}: {error}")).into())
}
