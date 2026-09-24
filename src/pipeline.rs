// pipeline.rs
//
// Zpracovatelská pipeline: čte zprávy (přijaté BMP soubory) z kanálu
// naplňovaného FTP vláknem a předává je rodičovskému procesu přes lokální
// meziprocesovou komunikaci (IPC = inter-process communication).
//
// Na Unixu (Linux) se pro IPC používá Unix domain socket (`UnixListener`) -
// rychlý mechanismus pro komunikaci mezi procesy na stejném stroji,
// adresovaný cestou v souborovém systému místo IP adresy a portu.
// Na Windows se místo toho použije obyčejný TCP socket na localhost.
// Přepínání mezi variantami řeší atributy `#[cfg(unix)]` / `#[cfg(windows)]`
// - do výsledné binárky se zkompiluje jen ta větev kódu, která odpovídá
// cílové platformě; ta druhá jako by v souboru vůbec nebyla.
//
// ROBUSTNOST: hlavním cílem úprav v tomto souboru je, aby jedna vadná
// zpráva nebo dočasný výpadek rodičovského procesu NEUKONČIL celou službu.
// Proto se chyby u jednotlivé zprávy jen loguji (eprintln!) a smyčka
// pokračuje dál, místo aby se chyba propagovala přes `?` až do `main`.

use crate::config::Config;
use std::error::Error;
#[cfg(unix)]
use std::fs;
use std::io::Write;
#[cfg(windows)]
use std::net::{Shutdown, SocketAddr, TcpStream};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc::Receiver;
#[cfg(unix)]
use std::time::Instant;
use std::time::Duration;

/// Typ zprávy posílané z FTP vlákna do pipeline.
/// `enum` s pojmenovanými poli (`Bmp { data, name }`) je tu zatím jediná
/// varianta, ale do budoucna jde snadno přidat další druh vstupu (např.
/// jiný formát souboru), aniž by se měnilo rozhraní kanálu - jen by
/// přibyla další "varianta" tohoto výčtu a nový `match` případ v pipeline.
pub enum Input {
    /// BMP data přijatá přes FTP.
    Bmp { data: Vec<u8>, name: String },
}

/// Jak dlouho čekat na připojení rodičovského procesu, než zprávu
/// vzdáme jako nedoručenou. Bez tohoto limitu by aplikace mohla na
/// `accept()`/`connect()` čekat navěky, kdyby parent proces spadl nebo
/// se opozdil se startem.
const PARENT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

#[cfg(unix)]
pub fn run_pipeline(config: Config, receiver: Receiver<Input>) -> Result<(), Box<dyn Error>> {
    // Pokud ze SOUBOROVÉHO SYSTÉMU zbyl socket soubor z minulého (třeba
    // nekorektně ukončeného) běhu, `bind` by na existující cestu selhal -
    // proto ho nejdřív smažeme, pokud existuje.
    if config.parent_socket.exists() {
        fs::remove_file(&config.parent_socket)?;
    }
    // Tohle je jediná operace, jejíž selhání smí ukončit celou pipeline:
    // bez naslouchacího socketu nemá smysl pokračovat vůbec.
    let parent_listener = UnixListener::bind(&config.parent_socket)?;

    // Nekonečná smyčka: `receiver.recv()` zablokuje aktuální vlákno,
    // dokud nepřijde další zpráva, NEBO dokud se kanál neuzavře (to se
    // stane automaticky ve chvíli, kdy FTP vlákno skončí a jeho `Sender`
    // se zahodí - Rust to hlídá na úrovni vlastnictví/ownershipu).
    loop {
        match receiver.recv() {
            Ok(Input::Bmp { data, name }) => {
                // Validace vstupu - pokud selže, zprávu jen přeskočíme.
                // `continue` skočí zpět na začátek `loop`, tedy na další
                // `receiver.recv()`, aniž by proces skončil.
                if let Err(e) = validate_bmp(&data) {
                    eprintln!("Pipeline: neplatný BMP '{name}', přeskakuji: {e}");
                    continue;
                }
                println!("FTP vstup: přijat BMP {name} ({} B)", data.len());

                // `match` na `Result` místo `?` - chybu tu chceme ZPRACOVAT
                // (zalogovat a jet dál), ne ji propagovat ven z funkce.
                match send_to_parent(&parent_listener, &data, &name) {
                    Ok(()) => println!(
                        "Pipeline: předáno parent procesu (BMP {} B, název {name})",
                        data.len()
                    ),
                    Err(e) => {
                        // Zpráva se pro tuto chvíli ztrácí. Pokud by to
                        // bylo nepřípustné (každý snímek MUSÍ dorazit),
                        // je tohle místo, kam doplnit náhradní zápis na
                        // disk / retry frontu pro pozdější doručení.
                        eprintln!("Pipeline: chyba při odesílání '{name}' parentovi: {e}");
                    }
                }
            }
            // `receiver.recv()` vrátí `Err`, jen když je kanál trvale
            // uzavřený (všichni odesílatelé zanikli) - to typicky
            // znamená, že FTP vlákno spadlo nebo skončilo. Další čekání
            // na zprávy by nemělo smysl, takže tady pipeline korektně
            // (s `Ok(())`) skončí.
            Err(_) => {
                eprintln!("Pipeline: FTP vlákno ukončeno, kanál uzavřen, končím.");
                return Ok(());
            }
        }
    }
}

#[cfg(not(unix))]
pub fn run_pipeline(config: Config, receiver: Receiver<Input>) -> Result<(), Box<dyn Error>> {
    // Na Windows nemáme Unix socket, takže "adresa parenta" je textová
    // reprezentace host:port pro TCP. Převedeme ji na `String` (vlastněná
    // kopie dat) rovnou tady, abychom si nemuseli pamatovat vypůjčenou
    // (borrowed) hodnotu `&str` po celou dobu běhu smyčky.
    let parent_address = config
        .parent_socket
        .to_str()
        .ok_or("neplatná parent_socket adresa")?
        .to_string();

    loop {
        match receiver.recv() {
            Ok(Input::Bmp { data, name }) => {
                if let Err(e) = validate_bmp(&data) {
                    eprintln!("Pipeline: neplatný BMP '{name}', přeskakuji: {e}");
                    continue;
                }
                println!("FTP vstup: přijat BMP {name} ({} B)", data.len());
                match send_to_parent(&parent_address, &data, &name) {
                    Ok(()) => println!(
                        "Pipeline: předáno parent procesu (BMP {} B, název {name})",
                        data.len()
                    ),
                    Err(e) => {
                        eprintln!("Pipeline: chyba při odesílání '{name}' parentovi: {e}");
                    }
                }
            }
            Err(_) => {
                eprintln!("Pipeline: FTP vlákno ukončeno, kanál uzavřen, končím.");
                return Ok(());
            }
        }
    }
}

/// Zkontroluje, že data začínají BMP "magic bytes" (`BM`).
/// Jde jen o rychlou kontrolu signatury, ne o plnou validaci formátu
/// (nekontroluje se např. deklarovaná velikost v hlavičce souboru).
fn validate_bmp(data: &[u8]) -> Result<(), Box<dyn Error>> {
    if data.len() < 2 || &data[..2] != b"BM" {
        return Err("přijatá data nejsou BMP".into());
    }
    Ok(())
}

// --- Unix: čekání na spojení od rodičovského procesu ---

/// `UnixListener::accept()` v blokujícím režimu čeká NAVĚKY, dokud se
/// někdo nepřipojí - žádný vestavěný timeout k dispozici není (na rozdíl
/// třeba od `TcpStream::connect_timeout` na druhé straně). Abychom se
/// nezasekli, když parent proces zrovna není připravený, přepneme
/// listener dočasně do neblokujícího (`non-blocking`) režimu a čekání si
/// "odsimulujeme" krátkým opakovaným pokusem (polling) s vlastním
/// časovým limitem.
#[cfg(unix)]
fn accept_with_timeout(
    listener: &UnixListener,
    timeout: Duration,
) -> Result<UnixStream, Box<dyn Error>> {
    listener.set_nonblocking(true)?;
    let start = Instant::now();

    loop {
        match listener.accept() {
            // Spojení dorazilo - přepneme ho zpět do blokujícího režimu,
            // ať se v `write_frame` chová standardně (čeká, dokud se data
            // nezapíšou, místo aby okamžitě vracela `WouldBlock`).
            Ok((stream, _peer_address)) => {
                stream.set_nonblocking(false)?;
                return Ok(stream);
            }
            // `WouldBlock` v neblokujícím režimu znamená "zatím nikdo
            // nepřipojen, zkus to znovu později" - to NENÍ chyba.
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if start.elapsed() >= timeout {
                    return Err(
                        "časový limit vypršel: parent proces se nepřipojil k socketu".into(),
                    );
                }
                // Krátká pauza, ať polling nezatěžuje CPU zbytečně (busy
                // loop bez spánku by běžel na 100 % jednoho jádra).
                std::thread::sleep(Duration::from_millis(100));
            }
            // Jakákoli jiná chyba (např. socket byl mezitím zavřen) se
            // propaguje dál pomocí `?` v místě volání.
            Err(e) => return Err(e.into()),
        }
    }
}

// Každá zpráva je uvozena 8bajtovou (u64) délkou ve formátu big-endian.
// Příjemce si tak nejdřív přečte délku a pak přesně tolik bajtů dat -
// tomu se říká "length-prefixed framing" a řeší to problém, kdy by se
// TCP/Unix socket stream mohl jinak "slepit" do jednoho proudu bajtů bez
// jasné hranice mezi jednotlivými zprávami.
#[cfg(unix)]
fn send_to_parent(listener: &UnixListener, bmp: &[u8], name: &str) -> Result<(), Box<dyn Error>> {
    let mut socket = accept_with_timeout(listener, PARENT_CONNECT_TIMEOUT)?;

    // I po úspěšném přijetí spojení nastavíme timeout na ZÁPIS - kdyby
    // parent proces po připojení "zamrzl" a data nevyčítal, `write_all`
    // by se jinak mohl zablokovat navěky.
    socket.set_write_timeout(Some(Duration::from_secs(10)))?;

    write_frame(&mut socket, bmp)?;
    write_frame(&mut socket, name.as_bytes())?;

    // `shutdown(Write)` signalizuje druhé straně "už nic dalšího
    // nepošlu" (odpovídá uzavření zápisové části socketu, TCP/Unix
    // ekvivalent EOF), aniž bychom museli spojení úplně zavírat.
    socket.shutdown(std::net::Shutdown::Write)?;
    Ok(())
}

// --- Windows: čekání na spojení k rodičovskému procesu ---

#[cfg(windows)]
fn send_to_parent(address: &str, bmp: &[u8], name: &str) -> Result<(), Box<dyn Error>> {
    // Na Windows máme k dispozici přímo `connect_timeout`, takže tu
    // není potřeba vlastní polling smyčka jako na Unixu - `SocketAddr`
    // navíc umí `str::parse()` rozparsovat rovnou z textu "host:port".
    let socket_address: SocketAddr = address.parse()?;
    let mut socket = TcpStream::connect_timeout(&socket_address, PARENT_CONNECT_TIMEOUT)?;

    socket.set_write_timeout(Some(Duration::from_secs(10)))?;

    write_frame(&mut socket, bmp)?;
    write_frame(&mut socket, name.as_bytes())?;
    socket.shutdown(Shutdown::Write)?;
    Ok(())
}

/// Zapíše jeden "rámec" (frame): 8bajtová délka v big-endian + samotná
/// data. Funkce je GENERICKÁ přes `W: Write` (generický parametr typu
/// ohraničený traitem `Write`) - díky tomu stejná implementace funguje
/// jak pro `UnixStream`, tak pro `TcpStream` (a teoreticky pro cokoliv
/// jiného, co umí zapisovat bajty), aniž bychom museli mít dvě téměř
/// identické kopie kódu pro Unix a Windows.
fn write_frame<W: Write>(socket: &mut W, data: &[u8]) -> Result<(), Box<dyn Error>> {
    // `to_be_bytes()` převede číslo na pole bajtů ve "big-endian" pořadí
    // (nejvýznamnější bajt první) - je potřeba, aby čtecí strana věděla
    // přesně, jak délku dekódovat, bez ohledu na to, na jaké architektuře
    // (little/big-endian) běží.
    socket.write_all(&(data.len() as u64).to_be_bytes())?;
    socket.write_all(data)?;
    Ok(())
}

// `#[cfg(test)]` říká kompilátoru: tento modul zkompiluj JEN při `cargo
// test`, do běžné (produkční) binárky se vůbec nedostane.
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
