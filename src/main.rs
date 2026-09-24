// main.rs
//
// Vstupní bod aplikace `keyence-collector`. Tento soubor:
//   1. najde a načte konfigurační soubor,
//   2. spustí FTP server na samostatném vlákně (přijímá BMP soubory od kamery),
//   3. na hlavním vlákně spustí zpracovatelskou pipeline, která přijatá data
//      předává rodičovskému procesu přes lokální socket.
//
// Komunikace mezi FTP vláknem a pipeline probíhá přes MPSC kanál
// (`std::sync::mpsc` = "multi-producer, single-consumer" fronta). FTP
// vlákno do kanálu zprávy POSÍLÁ (producer), hlavní vlákno je z kanálu
// ČTE (consumer). MPSC kanál je bezpečný pro sdílení dat mezi vlákny, aniž
// bychom museli ručně řešit zamykání (mutexy) - Rust nás navíc na úrovni
// kompilátoru donutí předat vlastnictví dat správně, jinak program
// nepůjde přeložit.

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

// Návratový typ `Result<(), Box<dyn Error>>` u `main` umožňuje používat
// operátor `?` přímo uvnitř `main`. Pokud kdekoli uvnitř dojde k chybě,
// `?` ji okamžitě "vrátí ven" - `main` tím skončí s `Err`, Rust ji vytiskne
// na stderr a proces skončí s nenulovým návratovým kódem.
//
// `Box<dyn Error>` znamená "ukazatel na haldu, který obsahuje NĚJAKÝ typ
// implementující trait `Error`". Používá se, když nás nezajímá přesný typ
// chyby (může to být chyba čtení souboru, síťová chyba, vlastní textová
// chyba...), jen že k ní došlo a umíme ji vypsat pomocí `{error}`.
fn main() -> Result<(), Box<dyn Error>> {
    let config_path = find_config_path()?;
    let config = Config::from_file(config_path.to_string_lossy().as_ref())?;
    config.validate()?;

    // `mpsc::channel()` vytvoří dvojici (odesílatel, příjemce) sdílející
    // jednu frontu. Fronta je NEOMEZENÁ - pokud by producent posílal
    // rychleji, než konzument stíhá číst, fronta by rostla bez limitu.
    // Při frekvenci ~1 zpráva / 3 s to není problém, ale je dobré to mít
    // na paměti, kdyby se frekvence v budoucnu výrazně zvýšila.
    let (sender, receiver) = mpsc::channel();

    // `Config` musí implementovat trait `Clone`, protože ho potřebujeme
    // použít na dvou místech současně (FTP vlákno i hlavní pipeline).
    // Rust nedovoluje sdílet vlastnictví jedné hodnoty mezi dvěma vlákny
    // bez klonování nebo synchronizačního obalu (např. `Arc`) - klonování
    // je tu nejjednodušší řešení, protože `Config` se po startu nemění.
    let ftp_config = config.clone();

    // `thread::spawn` přijímá uzávěru (closure) typu `FnOnce() -> T`.
    // Klíčové slovo `move` říká: "tato closure si bere VLASTNICTVÍ všech
    // proměnných, které používá (zde `ftp_config` a `sender`), a od teď
    // patří jen jí". Bez `move` by Rust closure nedovolil spustit na
    // jiném vlákně, protože by nebylo jasné, jak dlouho proměnné z
    // vnějšího scope "přežijí".
    //
    // `sender` zde NEKLONUJEME (na rozdíl od `config`) - potřebujeme ho
    // jen na jednom místě (FTP vlákno je jediný producent), takže stačí
    // přesunout (move) originál přímo do vlákna. Klonovat `Sender` dává
    // smysl, jen když chceme, aby do stejného kanálu posílalo víc vláken.
    thread::spawn(move || {
        // FTP server běží ve své vlastní nekonečné smyčce. Pokud selže
        // (např. nejde nabindovat port), vrátí `Err`. Closure předaná do
        // `thread::spawn` ale nemůže tuto chybu vrátit ven do `main` -
        // vlákno běží nezávisle a jeho návratová hodnota by šla získat
        // jen přes `JoinHandle::join()`, který tu nepoužíváme. Chybu tedy
        // aspoň zalogujeme, ať víme, že FTP server přestal fungovat.
        if let Err(error) = ftp::run(ftp_config, sender) {
            eprintln!("FTP server stopped: {error}");
        }
    });

    println!("keyence-collector poslouchá na FTP vstupu");

    // Hlavní vlákno teď vstupuje do `run_pipeline` a čte zprávy z
    // `receiver`, dokud FTP vlákno neskončí (čímž se `sender` zahodí a
    // kanál se uzavře) nebo dokud pipeline nenarazí na neopravitelnou
    // chybu (např. nejde nabindovat lokální IPC socket).
    run_pipeline(config, receiver)
}

/// Najde cestu ke konfiguračnímu souboru podle priority:
///   1. cesta zadaná jako argument příkazové řádky,
///   2. `keyence-collector.conf` v aktuálním pracovním adresáři,
///   3. `keyence-collector.conf` ve stejném adresáři jako spustitelný soubor.
fn find_config_path() -> Result<PathBuf, Box<dyn Error>> {
    let config_name = "keyence-collector.conf";

    // `env::args()` vrací iterátor přes argumenty příkazové řádky, včetně
    // jména/cesty programu na indexu 0. `.nth(1)` posune iterátor a vezme
    // prvek na indexu 1, tedy první skutečný argument (pokud existuje).
    if let Some(path) = env::args().nth(1) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        // Uživatel zadal cestu explicitně, ale soubor tam není -
        // je lepší selhat rovnou s jasnou chybou, než tiše zkoušet
        // další místa (to by mohlo vést k nechtěnému použití jiného
        // konfiguráku, než uživatel čekal).
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

    // `.iter().find(predikát)` projde prvky iterátoru a vrátí první,
    // pro který uzávěra vrátí `true` - tady tedy první existující soubor.
    if let Some(path) = candidates.iter().find(|path| path.is_file()) {
        return Ok(path.clone());
    }

    // Žádná z cest nevyšla - poskládáme přehlednou chybovou hlášku se
    // seznamem prohledaných míst, ať je snadné problém odladit.
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
