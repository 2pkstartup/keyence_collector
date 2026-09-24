# Keyence Collector

Služba pro příjem dat z Keyence zařízení a jejich předání parent procesu.
Collector současně poskytuje:

- FTP server pro příjem BMP obrázků přes `STOR`.
- Předání BMP a názvu souboru v jednom spojení parent procesu.

## Požadavky

- Rust stable a Cargo.
- Na Windows povolené příchozí TCP porty ve Windows Firewallu.
- Parent proces, který umí přijmout datové rámce popsané níže.

## Sestavení

```text
cargo build --release
```

Kontrola a testy:

```text
cargo fmt -- --check
cargo test
cargo check
```

## Konfigurace

Aplikace načítá soubor `keyence-collector.conf`, případně cestu k souboru z prvního argumentu příkazové řádky.
Komentáře začínající znakem `#` a prázdné řádky jsou ignorovány.

Pro první spuštění na Windows vytvořte lokální konfiguraci z přiloženého vzoru:

```powershell
Copy-Item .\keyence-collector.conf.example .\keyence-collector.conf
```

Potom upravte zejména heslo a adresu parent procesu. Soubor `keyence-collector.conf` není určený pro commit.

Příklad pro Windows:

```ini
ftp_bind=0.0.0.0
ftp_advertise=192.168.1.50
ftp_port=2121
ftp_user=keyence
ftp_password=zmenit-heslo
parent_socket=127.0.0.1:9100
tmp_dir=C:\\ProgramData\\KeyenceCollector\\tmp
```

Příklad pro Unix:

```ini
ftp_bind=0.0.0.0
ftp_advertise=192.168.1.50
ftp_port=2121
ftp_user=keyence
ftp_password=zmenit-heslo
parent_socket=/run/keyence-collector/parent.sock
tmp_dir=/dev/shm/keyence-collector
```

`parent_socket` má platformní význam:

- Windows: TCP adresa ve formátu `IP:port`, například `127.0.0.1:9100`.
- Unix: cesta k Unix domain socketu.

`ftp_bind` je lokální bind adresa. Pro čtečku v síti použijte `0.0.0.0` nebo LAN adresu počítače.
`ftp_advertise` je LAN adresa počítače, kterou collector oznámí při `PASV`; nesmí být `127.0.0.1`, pokud se připojuje vzdálená čtečka.
Collector podporuje aktivní FTP režim `PORT` i pasivní režim `PASV`.

`tmp_dir` musí být absolutní cesta. Adresář aplikace při startu vytvoří, pokud neexistuje.
Heslo v konfiguračním souboru chraňte vhodnými oprávněními souboru.

## Spuštění

Windows PowerShell:

```powershell
.\target\release\keyence_collector.exe .\keyence-collector.conf
```

Unix:

```text
./target/release/keyence_collector ./keyence-collector.conf
```

Pokud argument není uveden, použije se `keyence-collector.conf` z aktuálního adresáře.

## FTP vstup

Collector podporuje minimální FTP workflow:

1. Připojit se na `ftp_bind:ftp_port`.
2. Přihlásit se pomocí `ftp_user` a `ftp_password`.
3. Nastavit pasivní režim (`PASV`).
4. Odeslat BMP příkazem `STOR soubor.bmp`.

Data musí začínat signaturou BMP `BM`. Poškozená nebo jiná data jsou odmítnuta.

## Předání BMP

Collector předává BMP okamžitě po FTP uploadu; souřadnice se čtou z názvu BMP.

Pro zapojení Keyence SR-750 použijte:

- PC s collectorem: `192.168.210.1`
- SR-750: `192.168.210.200`
- FTP server collectoru: `192.168.210.1:21`

Na konzoli collectoru se zobrazí:

- `FTP STOR`: přijatý BMP soubor,
- `Pipeline: předáno parent procesu`: kompletní zpracované měření.

Pokud se po čtení nezobrazí žádná z těchto zpráv, čtečka se k FTP portu nepřipojila nebo používá jinou adresu či port.

Collector zpracovává BMP v paměti a nevytváří dočasný `incoming.bmp`.
Konfigurační položka `tmp_dir` zůstává kvůli kompatibilitě konfigurace, ale v tomto toku se nepoužívá.

## Validační aplikace

Projekt obsahuje nástroj `collector_validator`, který ověří celý Windows TCP/FTP průchod collectorem.
Validator načte stejný `keyence-collector.conf` jako collector, takže automaticky použije správný FTP port i přihlašovací údaje.
Nejprve spusťte collector a potom v druhém PowerShellu spusťte:

```powershell
cargo run --bin collector_validator
```

Při spuštění z `target\release` musí být `keyence-collector.conf` ve stejné složce jako EXE. Jinak lze cestu zadat explicitně:

```powershell
cargo run --bin collector_validator -- .\keyence-collector.conf
```

Při úspěchu nástroj vypíše `OK` a velikost přijatého BMP.

## Protokol parent procesu

Collector posílá vždy dva po sobě jdoucí rámce v pořadí:

1. BMP payload.
2. Název BMP souboru z FTP příkazu `STOR`.

Každý rámec má tento tvar:

```text
8 bajtů: délka payloadu jako unsigned 64-bit big-endian
N bajtů: payload
```

Na Unixu parent proces přijímá spojení přes Unix domain socket. Na Windows musí parent proces poslouchat na TCP adrese uvedené v `parent_socket`; collector se k této adrese připojuje pro každé kompletní měření.


# keyence-collector — shrnutí review a diskuze

Tento dokument shrnuje review kódu a doprovodnou diskuzi k projektu
`keyence-collector` (FTP server přijímající BMP soubory od kamery a
předávající je rodičovskému procesu). Zaměření: **robustnost a
spolehlivost**, ne syrový výkon — aplikace zpracovává ~1 zprávu / 3 s
z jediného zdroje.

## Přehled souborů a hlavních změn

### `main.rs`
Vstupní bod aplikace — najde konfigurák, spustí FTP server na
samostatném vlákně, hlavní vlákno běží v `run_pipeline`. Bez
funkčních změn, jen doplněné komentáře vysvětlující ownership,
`move` closures a MPSC kanál.

### `pipeline.rs`
Čte přijaté BMP z kanálu a předává je rodičovskému procesu přes
lokální IPC (Unix socket na Linuxu, TCP na Windows).

Klíčové robustnostní úpravy:
- **Chyba u jedné zprávy už neshodí celý proces.** Validace BMP i
  odeslání parentovi se zpracovávají přes `match`/`continue` místo
  propagace `?` — poškozený soubor nebo dočasný výpadek parenta se
  jen zaloguje a smyčka pokračuje dál.
- **Timeout na čekání na spojení s parentem** (`PARENT_CONNECT_TIMEOUT`,
  30 s). Na Unixu řešeno vlastní polling smyčkou (`accept_with_timeout`),
  protože `UnixListener::accept()` nemá vestavěný timeout. Na Windows
  stačí vestavěný `TcpStream::connect_timeout`.
- **Timeout na zápis** (`set_write_timeout`), aby zaseklý/nedostupný
  parent neuvěznil pipeline navěky.
- Sjednocený `write_frame<W: Write>` — generická funkce nahrazuje dřívější
  duplicitní kód pro Unix/Windows.

### `ftp.rs`
Minimalistický FTP server (USER/PASS, PASV/PORT, STOR, QUIT).
Záměrně **jednovláknový** — obsluhuje jedno spojení najednou, což je
u jediného zdroje s nízkou frekvencí v pořádku.

Klíčové robustnostní úpravy:
- Chyba při přijetí spojení (`listener.incoming()`) už neukončí celý
  server — zaloguje se a smyčka pokračuje na další spojení.
- **Timeouty** (`IO_TIMEOUT`, 30 s) na control i data socketu, aby
  zaseklý klient nezablokoval server navěky.
- **Limit velikosti nahrávaného souboru** (`MAX_UPLOAD_BYTES`, 100 MB)
  přes `Read::take`, jako pojistka proti vyčerpání paměti při
  anomálii na straně zdroje.
- Zdokumentované riziko `PORT` příkazu (tzv. FTP bounce — server se
  připojuje na libovolnou adresu, kterou pošle klient); ponecháno
  funkční, ale okomentováno jako bezpečnostní úvaha k případnému
  budoucímu omezení/odstranění.

### `config.rs`
Načítání a validace konfigurace ve formátu `klíč=hodnota`.

Oprava bugu:
- `ftp_advertise` se dřív validoval jako obecná `IpAddr` (přijme i
  IPv6), ale v `ftp.rs` se používá výhradně jako `Ipv4Addr` pro
  sestavení PASV odpovědi (FTP protokol IPv6 v PASV nepodporuje).
  Validace teď vyžaduje konkrétně `Ipv4Addr`, takže neplatná
  konfigurace selže hned při startu se srozumitelnou chybou, místo
  aby server spadl až při prvním PASV příkazu.
- Úklid: `ftp_bind` se načítá jednou do proměnné a použije se jak pro
  `ftp_bind`, tak jako fallback pro `ftp_advertise` — odstraněn
  křehký `.unwrap()` v původním kódu.

## Volba mechanismu pro předávání dat mezi procesy

Otázka: jaký je nejrobustnější způsob předávání dat mezi aplikacemi
(vzhledem k plánovanému nasazení na Raspberry Pi se SD kartou, tedy
bez zápisu na disk kvůli opotřebení).

Porovnané možnosti:
- **Message queue / broker** (RabbitMQ, NATS, Redis Streams) —
  nejrobustnější (persistence, potvrzování doručení, retry,
  backpressure), ale pro tento rozsah (1 zdroj, 1 zpráva/3 s) je to
  zbytečně těžké řešení a vyžaduje provoz další služby.
- **Souborová fronta / spool adresář** — velmi robustní (přežije pád
  kterékoli strany), ale zapisuje na disk → zamítnuto kvůli SD kartě.
- **Unix doménový socket** (aktuální řešení) — rychlé, žádný zápis na
  disk (socket "soubor" je jen adresa pro spojení, ne datový obsah).
  Má vestavěný `listen`/`accept()` cyklus, takže opakované připojování
  po výpadku je přirozená součást API. Možné další vylepšení:
  **abstract namespace socket** (Linux) — adresa bez souboru na disku
  vůbec, odpadá i řešení `fs::remove_file` před `bind`.
- **Pojmenovaná roura (FIFO/named pipe)** — také bez zápisu na disk,
  ale bez konceptu `listen`/`accept`; vyžaduje ruční orchestraci
  pořadí otevírání při restartu stran a snáz vede k deadlocku.
  Nezachovává hranice zpráv (stejně jako socket) — potřebovalo by
  stejné length-prefixed framing.
- **Sdílená paměť** — nejrychlejší, ale nejméně robustní (žádná
  fronta, žádné garance doručení, riziko race conditions) — nehodí se
  pro spolehlivé doručení diskrétních zpráv.

**Závěr:** Unix doménový socket zůstává pro tento případ nejlepší
volbou. Případné vylepšení: přechod na abstract namespace socket.

## Práce s Gitem přes webové rozhraní GitHubu

Cíl: nahrát upravené soubory na GitHub bez rizika pro fungující
hlavní verzi (`main`).

Řešení: **větev (branch)** — kopie kódu, kterou lze upravovat
nezávisle na `main`, dokud se změny vědomě nesloučí.

Postup čistě přes web:
1. Rozbalovací tlačítko s názvem větve (vlevo nahoře nad soubory) →
   napsat nový název → **Create branch: … from main**.
2. Přepnout se na novou větev tím samým tlačítkem.
3. Upravit/nahrát soubory (přes tužku "Edit" u existujícího souboru,
   nebo **Add file → Upload files**).
4. Při každém commitu zkontrolovat volbu dole: **"Commit directly to
   the `<název>` branch"** — ne `main`.
5. Volitelně založit **Pull Request** (tlačítko "Compare & pull
   request") pro přehledný diff před sloučením; `main` se nezmění, dokud
   se PR ručně nesloučí (**Merge pull request**).

## Otevřené / navržené další kroky
- Zvážit implementaci abstract namespace socketu na Unixu.
- Zvážit omezení nebo odstranění podpory příkazu `PORT` v `ftp.rs`
  kvůli riziku FTP bounce, pokud aktivní režim není reálně potřeba.
- Zvážit validaci existence/zapisovatelnosti `tmp_dir` v `config.rs`,
  pokud se skutečně používá pro dočasné soubory.
## Poznámky k provozu

FTP server v této aplikaci je záměrně jednoduchý a není určen k vystavení přímo do internetu.
Pro produkční provoz omezte bind adresy a firewall pravidla na důvěryhodnou síť.
