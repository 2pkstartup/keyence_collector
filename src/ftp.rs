// ftp.rs
//
// Minimalistický FTP server "na míru" - implementuje jen tolik z FTP
// protokolu (RFC 959), kolik potřebuje typická průmyslová kamera
// (Keyence) k tomu, aby se přihlásila a nahrála (`STOR`) soubor.
//
// Server je záměrně JEDNOVLÁKNOVÝ - obsluhuje vždy jedno control spojení
// najednou. To je v pořádku, protože: (a) data chodí jen z jednoho
// zdroje, (b) frekvence je nízká (~1 zpráva / 3 s), takže by druhé
// souběžné spojení stejně nemělo co dělat. Pokud by se to v budoucnu
// změnilo (víc zdrojů, vyšší frekvence), je tu prostor spustit
// `handle_session` na vlastním vlákně přes `thread::spawn`, podobně jako
// se dnes spouští celý FTP server z `main.rs`.
//
// ROBUSTNOST: hlavní změny oproti "naivní" verzi jsou:
//   - chyba u jednoho spojení/přenosu už neukončí celý server (viz `run`),
//   - síťové operace mají timeouty, aby zaseknutý klient neuvěznil server
//     navěky,
//   - velikost nahrávaného souboru je omezená, aby anomálie (např. chybně
//     nakonfigurované zařízení) nemohla vyčerpat paměť procesu.

use crate::config::Config;
use crate::pipeline::Input;
use std::error::Error;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::Sender;
use std::time::Duration;

/// Maximální povolená velikost jednoho nahrávaného souboru. Slouží jako
/// pojistka proti vyčerpání paměti při neočekávaně velkém/nekonečném
/// přenosu - běžný BMP snímek z průmyslové kamery bývá řádově jednotky
/// až desítky MB, 100 MB je tedy velkorysá rezerva.
const MAX_UPLOAD_BYTES: u64 = 100 * 1024 * 1024;

/// Jak dlouho může síťová operace (čtení/zápis) na control i data
/// spojení čekat, než se považuje za "zaseklou" a operace selže.
const IO_TIMEOUT: Duration = Duration::from_secs(30);

/// Spustí FTP server: naslouchá na nakonfigurovaném portu a pro každé
/// příchozí spojení zpracuje jednu "session" (přihlášení, případně
/// jeden nebo víc uploadů, odhlášení).
pub fn run(config: Config, sender: Sender<Input>) -> Result<(), Box<dyn Error>> {
    // Selhání `bind` (např. port už je obsazený) je neopravitelné -
    // bez naslouchacího socketu server nemá smysl, proto tady `?`.
    let listener = TcpListener::bind((config.ftp_bind.as_str(), config.ftp_port))?;

    // `listener.incoming()` je nekonečný iterátor - vrací `Result<TcpStream, io::Error>`
    // pro každé nové spojení, popř. `Err`, pokud přijetí spojení selže
    // (např. dočasný nedostatek systémových zdrojů).
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                // Chyba na úrovni "přijmi spojení" (ne uvnitř session)
                // by dřív ukončila celý server přes `?`. Teď ji jen
                // zalogujeme a zkusíme přijmout DALŠÍ spojení - jedna
                // přechodná chyba OS by neměla shodit celou službu.
                eprintln!("FTP: chyba při přijímání spojení, pokračuji: {error}");
                continue;
            }
        };

        println!("FTP: nové control spojení od {:?}", stream.peer_addr());

        // Selhání JEDNÉ session (např. klient se odpojí uprostřed
        // přenosu) je normální provozní situace, ne důvod k ukončení
        // serveru - `handle_session` proto vrací `Result`, který se tu
        // jen vyhodnotí a zaloguje, místo aby se propagoval přes `?`.
        if let Err(error) = handle_session(stream, &config, &sender) {
            if !is_client_disconnect(error.as_ref()) {
                eprintln!("FTP session failed: {error}");
            }
            // Běžné odpojení klienta (`is_client_disconnect` == true)
            // se ani nehlásí jako chyba - je to očekávaný konec session.
        }
    }
    Ok(())
}

/// Obslouží jedno FTP control spojení od začátku (pozdrav) do konce
/// (QUIT nebo odpojení klienta). Uvnitř session může proběhnout
/// libovolný počet příkazů, včetně opakovaných `STOR` (nahrání souboru).
fn handle_session(
    mut control: TcpStream,
    config: &Config,
    sender: &Sender<Input>,
) -> Result<(), Box<dyn Error>> {
    // Timeouty na control spojení: pokud klient přestane komunikovat
    // (zavěsí se, ztratí spojení bez korektního odpojení), `read_line`
    // níže po `IO_TIMEOUT` selže s chybou, místo aby čekal navěky a
    // "zamrzl" jednovláknový server pro všechny další klienty.
    control.set_read_timeout(Some(IO_TIMEOUT))?;
    control.set_write_timeout(Some(IO_TIMEOUT))?;

    reply(&mut control, "220 keyence-collector FTP ready")?;

    // `try_clone()` vytvoří nový file descriptor/handle mířící na
    // STEJNÉ síťové spojení - potřebujeme to, protože `BufReader` si
    // bere vlastnictví svého vnitřního streamu, ale zároveň chceme do
    // stejného spojení i zapisovat (přes původní `control`) pro odpovědi.
    let mut reader = BufReader::new(control.try_clone()?);

    // Stav session žije jen jako lokální proměnné této funkce - to je
    // idiomatické řešení v Rustu: místo tříd s mutovatelnými poli se
    // stav často drží jednoduše jako proměnné v těle funkce/smyčky.
    let mut authenticated = false;
    let mut data_listener = None;
    let mut active_data_address = None;

    loop {
        let mut command = String::new();
        // `read_line` čte ze streamu, dokud nenarazí na `\n` (nebo EOF).
        // Vrací počet přečtených bajtů - `0` znamená, že klient spojení
        // korektně ukončil (EOF) bez příkazu `QUIT`, což bereme jako
        // normální konec session, ne jako chybu.
        if reader.read_line(&mut command)? == 0 {
            return Ok(());
        }

        // FTP příkaz má tvar "PŘÍKAZ argument" (např. "USER anonymous").
        // `trim_end()` odstraní koncové `\r\n`, `splitn(2, ' ')` rozdělí
        // řetězec na NEJVÝŠE dvě části podle prvního mezerníku - díky
        // tomu argument (např. jméno souboru) může sám obsahovat mezery.
        let mut parts = command.trim_end().splitn(2, ' ');
        let name = parts.next().unwrap_or("").to_ascii_uppercase();
        let argument = parts.next().unwrap_or("");
        println!("FTP command: {name} {argument}");

        // `match` nad jménem příkazu (`&str`) - jednotlivé "ruce" (arms)
        // mohou mít i dodatečnou podmínku přes `if` (tzv. "match guard"),
        // např. `"USER" if argument == config.ftp_user` se použije,
        // jen když se JMÉNO PŘÍKAZU rovná "USER" A ZÁROVEŇ argument
        // sedí s nakonfigurovaným uživatelem.
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
                // Pasivní režim: SERVER otevře dočasný ("efemérní") port
                // a KLIENT se na něj sám připojí pro přenos dat. Port 0
                // v `bind` znamená "OS, vyber mi jakýkoli volný port".
                let listener = TcpListener::bind((config.ftp_bind.as_str(), 0))?;
                let port = listener.local_addr()?.port();
                let address = config.ftp_advertise.parse::<std::net::Ipv4Addr>()?;
                let octets = address.octets();
                // Formát odpovědi 227 je pevně daný FTP protokolem:
                // "227 Entering Passive Mode (h1,h2,h3,h4,p1,p2)", kde
                // výsledný port = p1*256 + p2 (protože FTP posílá port
                // po bajtech, ne jako jedno číslo).
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
                // Aktivní režim: KLIENT řekne serveru, na jakou adresu a
                // port se má server sám připojit pro přenos dat.
                //
                // POZOR (bezpečnost/robustnost): server se tu připojuje
                // na LIBOVOLNOU adresu, kterou pošle přihlášený klient -
                // to je klasický "FTP bounce" vektor (server jako
                // nechtěná proxy k připojení na jinou adresu/port, ke
                // které by se útočník jinak nedostal). Protože zdrojová
                // kamera pasivní/aktivní režim pravděpodobně nekombinuje
                // libovolně, zvaž, jestli `PORT` vůbec potřebuješ - pokud
                // ne, nejjednodušší a nejbezpečnější je tuto větev úplně
                // odstranit (nebo omezit cílovou adresu jen na IP adresu
                // klienta, který se připojil na control kanál).
                active_data_address = Some(parse_port_argument(argument)?);
                data_listener = None;
                reply(&mut control, "200 PORT command successful")?;
            }

            "STOR" if authenticated => {
                reply(&mut control, "150 Opening data connection")?;

                // Podle toho, jestli klient dřív poslal PASV nebo PORT,
                // buď čekáme na jeho připojení (`accept`), nebo se sami
                // připojujeme na adresu, kterou nám poslal (`connect`).
                let mut data = if let Some(listener) = data_listener.take() {
                    listener.accept()?.0
                } else if let Some(address) = active_data_address.take() {
                    TcpStream::connect(address)?
                } else {
                    return Err("před STOR nebyl nastaven PASV ani PORT".into());
                };

                // Timeout i na DATOVÉM spojení - bez něj by mohlo čtení
                // souboru od zaseklého klienta viset navěky.
                data.set_read_timeout(Some(IO_TIMEOUT))?;

                // `Read::take(limit)` obalí `data` tak, že z něj jde
                // přečíst NEJVÝŠE `limit` bajtů - jakmile se limit
                // vyčerpá, další čtení hlásí konec dat (0 bajtů), i
                // kdyby klient chtěl poslat víc. `read_to_end` tak
                // přestane růst, jakmile narazí na limit, místo aby
                // alokoval neomezené množství paměti.
                let mut contents = Vec::new();
                data.take(MAX_UPLOAD_BYTES).read_to_end(&mut contents)?;

                // Pokud jsme přečetli přesně `MAX_UPLOAD_BYTES`, je
                // vysoce pravděpodobné, že soubor byl ve skutečnosti
                // větší a data jsme jen usekli na limitu - takový
                // přenos raději odmítneme jako chybný, než abychom
                // tiše zpracovali neúplný/poškozený soubor.
                if contents.len() as u64 >= MAX_UPLOAD_BYTES {
                    return Err(format!(
                        "soubor '{argument}' překročil maximální povolenou velikost ({MAX_UPLOAD_BYTES} B)"
                    )
                    .into());
                }

                println!("FTP STOR: přijat soubor {argument} ({} B)", contents.len());

                // Odesíláme zprávu do pipeline přes MPSC kanál.
                // `sender.send(...)` vrátí `Err`, jen pokud PŘÍJEMCE
                // (druhá strana kanálu, tedy hlavní vlákno s pipeline)
                // už neexistuje - to by znamenalo, že pipeline spadla a
                // nemá smysl v přenosu pokračovat, proto tu necháváme
                // `?`, který takovou chybu vrátí až do `run_session`
                // volajícího (a odtud se zaloguje v `run`).
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

            // Neautentizovaný klient dostane na cokoliv jiného "530",
            // dokud se úspěšně nepřihlásí přes USER/PASS.
            _ if !authenticated => reply(&mut control, "530 Not logged in")?,

            // Cokoliv autentizovaného klienta, co jsme výše nezachytili,
            // je z pohledu tohoto minimalistického serveru "neimplementováno".
            _ => reply(&mut control, "502 Command not implemented")?,
        }
    }
}

/// Rozparsuje argument příkazu `PORT` ve tvaru "h1,h2,h3,h4,p1,p2" na
/// textovou adresu "h1.h2.h3.h4:port" použitelnou pro `TcpStream::connect`.
fn parse_port_argument(argument: &str) -> Result<String, Box<dyn Error>> {
    // `argument.split(',')` rozdělí text podle čárek na kusy typu `&str`,
    // `.map(str::parse)` na každý kus zavolá `parse::<u16>()` (typ `u16`
    // se odvodí z anotace `Vec<u16>` níže) a `.collect::<Result<_, _>>()`
    // z iterátoru `Result`ů vytvoří buď `Ok(Vec<u16>)` (pokud VŠECHNY
    // parsování uspěla), nebo první `Err`, na kterou parser narazil.
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

/// Pošle jednořádkovou textovou odpověď klientovi podle FTP konvence.
fn reply(stream: &mut TcpStream, message: &str) -> Result<(), Box<dyn Error>> {
    // POZNÁMKA: FTP protokol (RFC 959) předepisuje ukončení řádků
    // sekvencí `\r\n`. `writeln!` v Rustu ale přidává jen `\n`. Řada
    // klientů to toleruje, ale pro plnou korektnost by šlo použít
    // `write!(stream, "{message}\r\n")?` místo `writeln!`.
    writeln!(stream, "{message}")?;
    stream.flush()?;
    Ok(())
}

/// Pozná, jestli daná chyba jen znamená "klient se odpojil" (běžná,
/// očekávaná událost) - v takovém případě ji `run()` nebude hlásit jako
/// skutečnou chybu serveru.
fn is_client_disconnect(error: &(dyn Error + 'static)) -> bool {
    // Chyby v Rustu mohou tvořit řetězec "příčin" (`source()`) - např.
    // chyba na vysoké úrovni může být zabalená kolem nižší I/O chyby.
    // Tahle smyčka prochází celý řetězec a hledá, jestli je NĚKDE v něm
    // schovaná `std::io::Error` odpovídající odpojení klienta.
    let mut current = Some(error);
    while let Some(error) = current {
        if let Some(io_error) = error.downcast_ref::<std::io::Error>() {
            return matches!(
                io_error.kind(),
                std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::ConnectionReset
            ) || matches!(io_error.raw_os_error(), Some(10053 | 10054 | 10058)); // Windows socket chyby
        }
        current = error.source();
    }
    false
}
