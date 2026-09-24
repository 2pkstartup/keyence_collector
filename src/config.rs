// config.rs
//
// Načítání a validace konfigurace ve zjednodušeném formátu `klíč=hodnota`
// (jeden pár na řádek, `#` uvozuje komentář). Modul poskytuje typ `Config`
// a dvě veřejné metody: `from_file` (naparsuje soubor) a `validate`
// (zkontroluje, že načtené hodnoty dávají smysl, než se s nimi začne
// pracovat jinde v aplikaci).

use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::fs;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::str::FromStr;

// `#[derive(Clone, Debug)]` automaticky vygeneruje implementaci traitů
// `Clone` (umožní `.clone()`, potřebujeme to v `main.rs` pro sdílení
// configu mezi hlavním vláknem a FTP vláknem) a `Debug` (umožní vypsat
// hodnotu přes `{:?}`, užitečné např. při ladění).
#[derive(Clone, Debug)]
pub struct Config {
    pub ftp_bind: String,
    pub ftp_advertise: String,
    pub ftp_port: u16,
    pub ftp_user: String,
    pub ftp_password: String,
    // Cesta k Unix socketu na Unixu; textová TCP adresa (např.
    // "127.0.0.1:9100") na Windows. Typ je v obou případech `PathBuf`,
    // protože si stejné pole ukládáme jako "cestu" - na Windows se prostě
    // s obsahem zachází jako s textem a parsuje se až ve `validate`/`pipeline`.
    pub parent_socket: PathBuf,
    pub tmp_dir: PathBuf,
}

// Vlastní chybový typ pro chyby konfigurace. Místo obecné textové chyby
// (`"...".into()`) by šlo časem přidat i strukturovaná pole (např. který
// klíč selhal), ale pro tuto velikost projektu stačí jednoduchý wrapper
// kolem `String`, který jen implementuje potřebné traity, aby ho šlo
// používat jako `Box<dyn Error>`.
#[derive(Debug)]
struct ConfigError(String);

// `Display` určuje, jak se hodnota vypíše přes `{}` (na rozdíl od `{:?}`,
// což řeší `Debug`). Bez `Display` by `ConfigError` nešlo použít jako
// `dyn Error`, protože trait `Error` vyžaduje `Display` jako svoji
// "nadstavbu" (supertrait).
impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

// Prázdná implementace `Error` - stačí to, protože `Error` trait má
// všechny metody (`source()`, ...) s výchozí implementací. Díky tomu
// jde `ConfigError` použít všude, kde se čeká `Box<dyn Error>` (stačí
// `.into()`).
impl Error for ConfigError {}

impl Config {
    /// Načte jednoduchý formát `klíč=hodnota` použitý kolektorem.
    pub fn from_file(path: &str) -> Result<Self, Box<dyn Error>> {
        let contents = fs::read_to_string(path).map_err(|error| {
            ConfigError(format!(
                "nelze načíst konfigurační soubor '{path}': {error}"
            ))
        })?;

        let mut values = HashMap::new();
        // `.lines().enumerate()` dá ke každému řádku i jeho pořadové
        // číslo (od 0) - používáme ho v chybové hlášce, aby uživatel
        // hned věděl, na kterém řádku souboru je problém (+1, protože
        // lidé počítají řádky od 1, ne od 0).
        for (line_number, line) in contents.lines().enumerate() {
            // `split('#').next()` vezme jen část řádku PŘED prvním `#`,
            // čímž efektivně odstraní komentář (pokud na řádku je).
            // `.unwrap_or("")` je tu jen formální pojistka - `split`
            // vždy vrátí aspoň jeden prvek, takže `None` fakticky
            // nikdy nenastane, ale kompilátor to nedokáže sám odvodit.
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            // `split_once('=')` rozdělí řetězec na (levá_část, pravá_část)
            // podle PRVNÍHO výskytu `=` a vrátí `None`, pokud tam `=`
            // vůbec není - to nám umožní snadno detekovat a nahlásit
            // špatně formátovaný řádek.
            let (key, value) = line.split_once('=').ok_or_else(|| {
                ConfigError(format!("{path}:{}: očekáváno key=value", line_number + 1))
            })?;
            values.insert(key.trim().to_string(), value.trim().to_string());
        }

        // `ftp_bind` se v konfiguraci může objevit jen jednou, ale
        // potřebujeme jeho hodnotu na dvou místech: jako `ftp_bind` a
        // jako výchozí hodnotu pro `ftp_advertise`, pokud není zadaná
        // zvlášť. Proto ji tady načteme JEDNOU do proměnné a odtud
        // použijeme na obou místech - vyhneme se tak druhému parsování
        // stejné hodnoty i nutnosti `.unwrap()` v `unwrap_or_else`.
        let ftp_bind = required(&values, "ftp_bind")?;
        let ftp_advertise =
            optional(&values, "ftp_advertise").unwrap_or_else(|| ftp_bind.clone());

        Ok(Self {
            ftp_bind,
            ftp_advertise,
            ftp_port: parse(&values, "ftp_port")?,
            ftp_user: required(&values, "ftp_user")?,
            ftp_password: required(&values, "ftp_password")?,
            parent_socket: PathBuf::from(required(&values, "parent_socket")?),
            tmp_dir: PathBuf::from(required(&values, "tmp_dir")?),
        })
    }

    /// Ověří, že načtené hodnoty dávají smysl (formát IP adres, prázdné
    /// povinné hodnoty, platnost cest apod.), než se s configem začne
    /// pracovat jinde v aplikaci. Cílem je selhat hned na startu se
    /// srozumitelnou chybou, místo aby špatná konfigurace způsobila
    /// nejasnou chybu až za běhu (např. hluboko v `ftp.rs` při prvním
    /// příkazu PASV).
    pub fn validate(&self) -> Result<(), Box<dyn Error>> {
        // `ftp_bind` může být obecná IP adresa (Rust umí bindnout
        // listener na IPv4 i IPv6), takže tady zůstává obecný `IpAddr`.
        std::net::IpAddr::from_str(&self.ftp_bind)
            .map_err(|_| ConfigError("ftp_bind musí být IP adresa".to_string()))?;

        // OPRAVA: `ftp_advertise` se v `ftp.rs` používá výhradně jako
        // IPv4 adresa pro sestavení odpovědi na příkaz PASV:
        //   let address = config.ftp_advertise.parse::<Ipv4Addr>()?;
        //   let octets = address.octets();  // PASV odpověď potřebuje přesně 4 bajty
        // FTP protokol ve své klasické podobě (příkaz PASV) IPv6 adresy
        // vůbec neumí vyjádřit - proto tu validace musí trvat konkrétně
        // na `Ipv4Addr`, ne na obecném `IpAddr`. Kdyby se tu původně
        // povolila i IPv6 adresa, `validate()` by prošla v pořádku, ale
        // server by spadl na chybě až při prvním příkazu PASV od
        // klienta - tedy zbytečně pozdě a na nečekaném místě.
        Ipv4Addr::from_str(&self.ftp_advertise)
            .map_err(|_| ConfigError("ftp_advertise musí být IPv4 adresa".to_string()))?;

        if self.ftp_user.is_empty() || self.ftp_password.is_empty() {
            return Err(
                ConfigError("ftp_user a ftp_password nesmí být prázdné".to_string()).into(),
            );
        }
        if !self.tmp_dir.is_absolute() {
            return Err(ConfigError("tmp_dir musí být absolutní cesta".to_string()).into());
        }

        // `cfg!(windows)` je RUNTIME kontrola (na rozdíl od atributu
        // `#[cfg(windows)]`, který kód na jiné platformě vůbec
        // nezkompiluje) - výsledný `if` blok se ale při kompilaci pro
        // Unix vyhodnotí jako mrtvý kód a kompilátor/optimalizátor ho
        // odstraní, takže na běhu programu to nemá žádný dopad navíc.
        // Používá se tu proto, že celá metoda `validate` musí existovat
        // na obou platformách se stejnou signaturou, ale jen jedna
        // konkrétní podmínka uvnitř se liší.
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

/// Přečte povinnou hodnotu z mapy načtených klíč=hodnota párů.
/// Vrátí chybu, pokud klíč chybí NEBO je jeho hodnota prázdný řetězec
/// (prázdná hodnota se v praxi chová stejně nešikovně jako chybějící
/// klíč, proto se ošetřuje stejně).
fn required(values: &HashMap<String, String>, key: &str) -> Result<String, Box<dyn Error>> {
    values
        .get(key)
        .filter(|value| !value.is_empty())
        .cloned()
        .ok_or_else(|| ConfigError(format!("chybí konfigurace {key}")).into())
}

/// Stejné jako `required`, ale chybějící/prázdnou hodnotu jen vrátí
/// jako `None` místo chyby - pro volitelné konfigurační klíče.
fn optional(values: &HashMap<String, String>, key: &str) -> Option<String> {
    values.get(key).filter(|value| !value.is_empty()).cloned()
}

/// Přečte povinnou hodnotu a rovnou ji naparsuje na požadovaný typ `T`
/// (např. `u16` pro `ftp_port`). Generický parametr `T: FromStr` říká
/// "T je jakýkoli typ, který umí vzniknout z textu" (tak jak to dělají
/// třeba `u16`, `f64`, `bool`...). Podmínka `where T::Err: fmt::Display`
/// navíc vyžaduje, aby chybu z parsování šlo vypsat přes `{}` - to
/// potřebujeme pro sestavení srozumitelné chybové hlášky.
fn parse<T: FromStr>(values: &HashMap<String, String>, key: &str) -> Result<T, Box<dyn Error>>
where
    T::Err: fmt::Display,
{
    required(values, key)?
        .parse()
        .map_err(|error: T::Err| ConfigError(format!("neplatná hodnota {key}: {error}")).into())
}
