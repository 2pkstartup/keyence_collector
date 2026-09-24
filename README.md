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

## Poznámky k provozu

FTP server v této aplikaci je záměrně jednoduchý a není určen k vystavení přímo do internetu.
Pro produkční provoz omezte bind adresy a firewall pravidla na důvěryhodnou síť.
