# Architektur & Entscheidungen

Diese Datei hält fest, **warum** SealFlash so gebaut ist. Neue Entscheidungen werden unten ergänzt.

## Überblick

```text
┌──────────────── GUI (Svelte, läuft im WebView) ────────────────┐
│  Distro wählen → Stick wählen → Flash! → Fortschritt            │
└───────────────┬─────────────────────────────────▲──────────────┘
                │ invoke("get_catalog")            │ Events (Fortschritt, ab Phase 3)
┌───────────────▼─────────────────────────────────┴──────────────┐
│  src-tauri  –  Tauri-Commands, übersetzt Fehler für die GUI     │
└───────────────┬────────────────────────────────────────────────┘
                │ normale Rust-Funktionsaufrufe
┌───────────────▼────────────────────────────────────────────────┐
│  sealflash-core – Katalog, Versionsermittlung, Download,        │
│  Prüfsummen/GPG, USB-Erkennung, Flashen (keine GUI-Abhängigkeit)│
└────────────────────────────────────────────────────────────────┘
           (Linux, ab Phase 5: kleiner Flash-Helper via pkexec)
```

## Entscheidungen

### E1 – Cargo-Workspace mit eigenem `sealflash-core`
**Warum:** Die gefährlichen Teile (Laufwerke erkennen, Rohdaten schreiben) sollen mit
`cargo test` prüfbar sein, ohne ein Fenster zu öffnen. Außerdem soll der spätere Linux-Helper,
der mit Root-Rechten läuft, *dieselben* Sicherheitsprüfungen nutzen, aber möglichst wenig Code
enthalten. Ein Root-Prozess mit kompletter GUI wäre eine unnötig große Angriffsfläche.

### E2 – Svelte 5 statt Vanilla-TS oder React
**Warum:** Die GUI hat viel Zustand (Auswahl, Fortschritt, Fehler), der sich gegenseitig beeinflusst.
Mit Vanilla-TS müsste man das DOM selbst nachführen, was schnell unübersichtlich wird. Svelte
übersetzt die Komponenten beim Build in schlankes JavaScript (~50 KB) ohne Laufzeit-Framework,
und die Syntax liegt sehr nah an HTML/CSS/TS. Kein SvelteKit, weil wir weder Routing noch
Server-Rendering brauchen.

### E3 – Das Frontend darf fast nichts
`capabilities/default.json` erlaubt nur `core:default`. Das Frontend hat **keinen** direkten
Zugriff auf Dateisystem, Shell oder HTTP. Alles läuft über eigene Rust-Commands, die genau eine
Sache tun. Selbst wenn über manipulierte Katalogtexte HTML/JS ins WebView gelangen sollte, könnte
es keine Laufwerke beschreiben. Zusätzlich gilt eine strenge Content-Security-Policy (CSP).

### E4 – Katalogformat in Rust definiert, Schema generiert
Das Format steht nur an einer Stelle (`model.rs`). `schemars` erzeugt daraus das JSON-Schema,
und ein Test schlägt fehl, wenn die eingecheckte Schema-Datei veraltet ist. So können Code und
Doku nicht auseinanderlaufen.

### E5 – Zweistufige Katalogprüfung
1. **Form** (`serde`, `deny_unknown_fields`): Felder und Typen stimmen.
2. **Inhalt** (`validate.rs`): nur HTTPS, gültige Regexe, eindeutige IDs, Fingerprints im richtigen
   Format, Prüfsumme vorhanden.

Ein Katalog mit Fehlern wird **komplett** verworfen, nicht teilweise benutzt. Dann greifen der Cache
bzw. der eingebaute Katalog.

### E6 – Versionsermittlung über Verzeichnislistings statt eigenem Server
Die meisten Distros veröffentlichen ihre ISOs in Verzeichnissen mit fester Namenslogik. Mit
Regex-Schritten im Katalog lässt sich das ohne Code-Änderung beschreiben. Neue Distros brauchen
also nur einen Katalogeintrag und keine neue App-Version.

Beim Erstellen des Beispielkatalogs (01.11.2026) gegen die echten Server geprüft. Dabei fiel auf:
- Fedora-Mirror (`download.fedoraproject.org`) zeigt bereits ein leeres `45/`. Deshalb nutzen wir
  `dl.fedoraproject.org` fürs Listing und probieren bei Sackgassen die nächste Version.
- Einige Server liefern Links als `./datei` → Links werden vor dem Regex-Vergleich normalisiert.
- `download.truenas.com` liefert eine flache Liste aller Pfade → ein einziger `match` über den
  vollen Pfad reicht, und Beta/RC-Versionen fallen durch die reine Zahlen-Regex heraus.
- openSUSE Leap: Unter `distribution/leap/` liegt noch das alte `42.3/`, das numerisch „größer“
  als `16.1` ist → die Regex lässt nur 16+ zu.
- Batocera ist eine komprimierte `.img.gz` (kein ISO) mit nur MD5 → `compression: gzip`, Warnung.

### E7 – GPG-Fingerprints im Katalog fest hinterlegt („Pinning“)
Die Fingerprints wurden aus den aktuellen Signaturen gelesen **und** gegen die Schlüsselquellen der
Hersteller abgeglichen (Ubuntu, Mint, Debian, Arch über `keyserver.ubuntu.com`; Fedora über
`fedoraproject.org/fedora.gpg`; openSUSE über das Repo-Key-File; Proxmox über das
Archive-Keyring). Vor dem Release (Phase 3) noch einmal gegen die Webseiten der Hersteller prüfen.

### E8 – Fehler als Code + Detail
Fehler gehen als `{ code, detail }` an die GUI. Die GUI übersetzt den `code`, und `detail` ist
aufklappbar für Techniker. So bleibt die Mehrsprachigkeit im Frontend, und das Backend muss keine
Sprache kennen.

### E10 – Katalog in eigenem Repo
Der Katalog liegt in [`xHellPhoenix/sealflash-catalog`](https://github.com/xHellPhoenix/sealflash-catalog),
die App in `xHellPhoenix/sealflash`. **Warum:** Ein neuer Distro-Eintrag oder eine korrigierte Regex
soll sofort bei allen Nutzern ankommen, ohne neue App-Version. Und wer nur den Katalog pflegt, braucht
keinen Zugriff auf den App-Code. Die Kopie in `catalog/` wird beim Bauen als Fallback eingebettet.

### E9 – Eigene kleine i18n statt Bibliothek
`src/lib/i18n.svelte.ts` umfasst ~30 Zeilen: flache JSON-Dateien pro Sprache, Platzhalter `{name}`,
Fallback auf Deutsch. Für zwei Sprachen reicht das. Wird es mehr (Plural-Regeln, Datumsformate),
kann man auf eine Bibliothek wie `svelte-i18n` wechseln.

### E11 – Katalog laden: Online → Cache → eingebaut
Die App startet **immer**, auch offline. Der Online-Katalog wird per ETag abgefragt: Hat er sich
nicht geändert, antwortet GitHub mit „304 Not Modified“ ohne Inhalt. Ein ungültiger Online-Katalog
überschreibt **nie** den guten Cache. Der Cache wird beim Lesen erneut geprüft, denn er könnte
beschädigt sein. Geschrieben wird atomar (temporäre Datei + Umbenennen), damit ein Absturz keine
halbe Datei hinterlässt. Die GUI zeigt an, woher der Katalog stammt, und warum, falls nicht online.

### E12 – `Fetcher`-Trait statt direktem `reqwest`
Der Resolver kennt nur das Trait `Fetcher` (`get`, `content_length`). In der App steckt dahinter
`reqwest`, in den Tests ein `FakeFetcher` mit vorbereiteten Antworten. So prüfen die Tests echte
Server-Eigenheiten (leeres Fedora-`45/`, Leaps `42.3/`, das flache TrueNAS-Listing) ohne Internet,
schnell und reproduzierbar. Dieses Muster heißt *Dependency Injection*.

### E13 – Sicherheit im HTTP-Client
- `https_only(true)`: Auch eine Weiterleitung von HTTPS auf HTTP wird abgelehnt.
- URLs aus Antworten (JSON-APIs) werden zusätzlich auf HTTPS geprüft.
- Textantworten sind auf 32 MB begrenzt (TrueNAS-Listing: ~12 MB) und werden beim Lesen mitgezählt,
  weil `Content-Length` fehlen oder falsch sein kann.
- Prüfsummen aus APIs müssen exakt zum Algorithmus passen (z. B. 64 Hex-Zeichen bei SHA-256).
- Pfade aus Listings mit `..` werden verworfen.
- Ein einziger automatischer Wiederholungsversuch bei Netzwerkfehlern, nicht bei HTTP-Fehlern.
  (Beim Testen gab es einen kurzen Aussetzer bei `mirrors.kernel.org`.)

### E14 – Begrenztes Backtracking
Ist die höchste Version eine Sackgasse (leeres Verzeichnis, 404), probiert der Resolver die
nächste, aber höchstens 3 pro Schritt. Ein kaputter Katalogeintrag kann so keinen Server mit
Anfragen fluten.

### E15 – Rust-Rekursion mit `async` braucht `Box::pin`
Das Durchlaufen der Listing-Schritte ist rekursiv. Ein `async fn`, das sich selbst aufruft, hätte
ein Future, das sich selbst enthält, also unendliche Größe. `Box::pin` legt das innere Future auf
den Heap; dann ist nur ein Zeiger im äußeren Future gespeichert.

### E16 – GPG mit Sequoia und nativem Krypto-Backend
GPG-Prüfung in der App selbst, statt das `gpg`-Programm aufzurufen: Unter Windows ist `gpg` meist
nicht installiert, und die Ausgabe von `gpg` zu parsen ist fehleranfällig. Sequoia ist eine
OpenPGP-Implementierung in Rust. Als Krypto-Backend nehmen wir das jeweils native:
**CNG** unter Windows (in Windows eingebaut), **Nettle** unter Linux (Systembibliothek, über GnuTLS
praktisch überall vorhanden). Das reine Rust-Backend stuft Sequoia selbst als experimentell ein.

### E17 – Vertrauensmodell: nur gepinnte Schlüssel
Der Schlüsselbund wird bei jeder Prüfung frisch über HTTPS geladen, aber **nur Zertifikate mit
einem Fingerprint aus dem Katalog** werden übernommen. Eine Signatur zählt nur, wenn sie
kryptografisch stimmt und von genau so einem Schlüssel stammt. Ein Angreifer, der den Schlüsselbund
oder die Signatur austauscht, scheitert also an den gepinnten Fingerprints.

Bei Clearsign-Dateien (Fedora) wird **nur der Text verwendet, den Sequoia als signiert bestätigt**,
nie der Rohtext. Sonst könnte jemand Zeilen außerhalb des signierten Bereichs einschmuggeln.

### E18 – Ausnahme: v3-Signaturen (openSUSE)
openSUSE signiert noch im alten v3-Format, das Sequoia seit 2021 ablehnt. Die Kryptografie
(RSA-4096 + SHA-512) ist stark, veraltet ist nur das Paketformat (keine geschützten Metadaten,
64-Bit-Schlüssel-ID). Weil wir ohnehin nur den gepinnten Schlüssel zulassen, ist das vertretbar.
Die Ausnahme ist **pro Variante** im Katalog anzuschalten (`allow_v3`) und erzeugt eine Warnung.
Die Alternative wäre, bei openSUSE ganz auf GPG zu verzichten, und das wäre schlechter.

### E19 – Reihenfolge: erst Signatur, dann Download
Prüfsummendatei und Signatur sind ein paar KB groß, das Image mehrere GB. Deshalb zuerst die
Signatur prüfen: Ist sie falsch, wird **kein einziges Byte** des Images geladen (ist per Test
abgesichert). Erst danach wird heruntergeladen. Der Hash läuft während des Downloads mit, sodass
die fertige Datei nicht noch einmal gelesen werden muss. Bei falschem Hash wird die Datei sofort
gelöscht.

### E20 – Fortsetzen mit `Range` + `If-Range`
Abgebrochene Downloads bleiben als `.part` liegen, daneben steht in `.part.json` die URL plus
ETag/Last-Modified. Beim nächsten Versuch wird der vorhandene Teil eingelesen (für den Hash) und
ab dort weitergeladen. `If-Range` stellt sicher, dass der Server nur dann ab der Mitte liefert,
wenn sich die Datei nicht geändert hat. Sonst schickt er sie komplett neu, und wir beginnen von vorn.

### E21 – Binärdaten nie als Text behandeln
Signaturen (`.gpg`, `.sig`) und manche Schlüsselbünde sind binär. Der erste Entwurf hat sie über
`get_text` geladen, was ungültiges UTF-8 ersetzt und die Signatur zerstört. Deshalb gibt es
`get_bytes` für alles, was geprüft wird; `get_text` ist nur für HTML-Listings und JSON da.

### E22 – Dateinamen vom Server misstrauen
Der Dateiname kommt vom Server. Bevor er als Pfad verwendet wird, wird geprüft: kein `/` oder `\`,
kein `..`, kein versteckter Name, keine unter Windows verbotenen Zeichen. Sonst könnte ein
manipulierter Server z. B. `../../.bashrc` überschreiben (Path Traversal).

### E23 – Laufwerke: Rohdaten und Urteil getrennt
Der Plattform-Code (`device/linux.rs`, `device/windows.rs`) sammelt nur **Rohdaten**. Ob ein
Laufwerk beschrieben werden darf, entscheidet allein `device::classify`: eine reine Funktion ohne
Systemzugriff, für Linux und Windows dieselbe, vollständig getestet. Prinzip **„Default deny“**:
Ein Laufwerk ist nur wählbar, wenn *jede* Regel erfüllt ist. Fehlt eine Information, gilt das
Laufwerk als verboten (unter Windows z. B. unbekanntes `IsBoot` → als System behandelt).

Die Regeln in der Reihenfolge ihrer Priorität:
1. **System:** trägt `/`, `/boot`, `/home` … oder Swap (Linux), bzw. `IsBoot`, `IsSystem` oder die
   Auslagerungsdatei (Windows). Gilt **auch für USB**, z. B. wenn Linux gerade vom Stick läuft.
2. **Nur USB** (Bus-Typ).
3. **Zweite, unabhängige Prüfung:** Linux: Gerätename `sdX`. Windows: Das Systemlaufwerk
   (`%SystemDrive%`, meist `C:`) liegt nicht darauf, unabhängig von dem, was Windows bei
   `IsBoot`/`IsSystem` meldet.
4. Nicht schreibgeschützt, nicht von LVM/RAID/LUKS belegt, Medium eingelegt.
5. Höchstens 2 TB, ohne Ausnahme.
6. **Externe Festplatten** (nicht als Wechselmedium gemeldet und über 128 GB) sind ausgeblendet,
   außer man setzt ausdrücklich das Häkchen. Auf solchen Platten liegen oft Backups. Viele moderne
   Sticks melden sich übrigens *nicht* als Wechselmedium, deshalb reicht dieses Merkmal allein
   nicht als Regel.

### E24 – Linux: `/sys` statt `lsblk`
`/sys/block` ist die stabile Kernel-Schnittstelle (die auch `lsblk` liest). So hängen wir nicht von
einem externen Programm und dessen Ausgabeformat ab. Eingebundene Partitionen werden über die
**Gerätenummer** (Major:Minor) mit `/proc/self/mountinfo` abgeglichen, nicht über Pfadnamen, die
auch `/dev/disk/by-uuid/…` heißen können. LVM/LUKS/RAID wird über `holders/` erkannt; trägt so ein
Gerät `/` oder `/home`, gilt das USB-Laufwerk als Systemlaufwerk.

### E25 – Windows: WMI (`MSFT_Disk`)
`MSFT_Disk` liefert `BusType`, `IsBoot` und `IsSystem` direkt; dazu kommen `MSFT_Partition`
(Laufwerksbuchstaben), `Win32_DiskDrive` (Wechselmedium) und `Win32_PageFileUsage`. Die Umrechnung
in Rohdaten (`windows_map.rs`) ist reiner Code und wird auch unter Linux getestet. Eine WMI-Verbindung
darf den Thread nicht wechseln (COM), deshalb läuft die Erkennung über `spawn_blocking`.

Stolperstein: `wmi` erlaubt `windows`/`windows-core` in einem Versionsbereich, und Cargo hat eine
inkompatible Mischung gewählt (0.61 + 0.62). Gelöst im `Cargo.lock` (beide auf dieselbe Version wie
Tauri); das CI baut mit `--locked`, damit das nicht unbemerkt verloren geht.

### E26 – Aktualisieren per Abfrage alle 2 s
Solange der Auswahldialog offen ist, wird die Liste alle 2 Sekunden neu eingelesen. Echte
Hotplug-Benachrichtigungen (udev bzw. `WM_DEVICECHANGE`) wären eleganter, sind aber pro System
aufwendig. Ein Scan dauert nur Millisekunden. Vor dem Schreiben (Phase 5) wird das Laufwerk
ohnehin noch einmal frisch geprüft.

### E27 – Entwickler-Schalter nur in Debug-Builds
`SEALFLASH_DEV_SYSROOT` lässt die App einen nachgebauten `/sys`-Baum lesen, um die GUI ohne echten
Stick zu testen. Der Code dafür steht hinter `cfg(debug_assertions)` und ist in Release-Builds gar
nicht vorhanden, damit niemand einer echten Installation ein falsches Laufwerk unterschieben kann.

### E28 – CI unter Linux und Windows
GitHub Actions baut und testet jeden Push auf `ubuntu-24.04` und `windows-latest`. So wird auch
der Windows-Code (WMI, CNG) wirklich ausgeführt. `.gitattributes` erzwingt LF-Zeilenenden und
schützt die Signatur-Fixtures: Würde Git sie unter Windows nach CRLF umwandeln, wären die echten
Signaturen ungültig.

### E29 – Dreifache Prüfung beim Schreiben
1. **Vor dem Schreiben** wird die Image-Datei noch einmal gehasht. Stimmt der Hash nicht, wird der
   Stick gar nicht angefasst.
2. **Während des Schreibens** wird die Datei erneut gehasht. Wurde sie zwischen Prüfung und Schreiben
   ausgetauscht (TOCTOU: „time of check to time of use“), wird der Anfang des Sticks genullt, damit
   er nicht mit Mischdaten bootet.
3. **Danach wird zurückgelesen** und mit dem Geschriebenen verglichen. Das erkennt defekte Sticks und
   **gefälschte** Sticks, die mehr Speicher vorgeben, als sie haben (sie überschreiben dann den Anfang
   wieder). Der Test `fake_capacity_stick_detected` simuliert genau das.

Schreibzugriffe gehen am Cache vorbei (Linux `O_DIRECT`, Windows `FILE_FLAG_NO_BUFFERING`), sonst
käme das Zurücklesen aus dem RAM und würde nichts beweisen. Dafür müssen Puffer im Speicher auf
4096 Bytes ausgerichtet sein (`AlignedBuf`), und jeder Schreibzugriff muss ein Vielfaches davon
sein. Der letzte Block wird deshalb mit Nullen aufgefüllt.

### E30 – Laufwerk unmittelbar vor dem Schreiben neu prüfen
Zwischen Auswahl und „Flash!“ kann viel passieren. Unter Linux bekommt ein neu eingesteckter Stick
sogar oft denselben Namen `/dev/sdb`. `device::recheck` scannt deshalb neu, wendet alle Regeln erneut
an und vergleicht Größe, Name und Seriennummer. Diese Prüfung läuft **in dem Prozess, der schreibt**
(unter Linux also im Root-Helper), und verlässt sich nicht auf das, was die GUI schickt.

### E31 – Bestätigung durch Abtippen, im Backend erneut geprüft
Ein versehentlicher Klick darf keinen Stick löschen, deshalb muss man den Namen des Sticks abtippen.
Der Command `forge` prüft den eingetippten Namen **noch einmal selbst**. Ein Fehler in der GUI kann die
Bestätigung so nicht aushebeln. Nach einem Fehler gibt es bewusst kein „Nochmal“, nur „Zurück“:
Jeder Schreibversuch braucht eine neue Bestätigung.

### E32 – Linux: kleiner Root-Helper über pkexec
Die GUI läuft als normaler Nutzer. WebKitGTK mit Root-Rechten wäre eine riesige Angriffsfläche.
Geschrieben wird von `sealflash-flash-helper`: kein Netzwerk, keine GUI, eine Aufgabe. Er bekommt die
Anfrage als JSON über stdin und meldet Fortschritt als JSON-Zeilen. Er **misstraut** der Anfrage:
- Das Laufwerk wird neu erkannt und muss alle Regeln erfüllen.
- Das Image muss eine normale Datei sein (kein Symlink) und dem aufrufenden Nutzer gehören
  (`PKEXEC_UID`). Sonst könnte man ihn benutzen, um z. B. `/etc/shadow` auf einen Stick zu kopieren.
  Schon der Hash-Zwang verhindert das (man müsste den Hash der geheimen Datei kennen); die
  Eigentümerprüfung ist eine zweite Sperre.
- Schließt die App stdin (auch bei einem Absturz), bricht der Helper ab. Ein Root-Prozess soll nicht
  verwaist weiterschreiben.

Die polkit-Regel verlangt jedes Mal das Admin-Passwort (`auth_admin`, kein `_keep`). Vor dem
Öffnen werden alle Partitionen ausgehängt, dann wird das Gerät mit `O_EXCL` geöffnet: Benutzt noch
irgendetwas das Gerät, verweigert der Kernel das, eine letzte Absicherung.

### E33 – Windows: Admin-Manifest nur im Release-Build
Laut Anforderung braucht die App unter Windows Administratorrechte (Manifest `requireAdministrator`).
Das gilt nur für Release-Builds, sonst müsste man schon zum Entwickeln jedes Mal ein Admin-Terminal
öffnen. Ohne Adminrechte meldet die App beim Schreiben einen verständlichen Fehler. Vor dem Schreiben
werden alle Volumes des Sticks gesperrt (`FSCTL_LOCK_VOLUME`) und ausgehängt; die Sperren bleiben bis
zum Ende offen, sonst hängt Windows sie sofort wieder ein.

### E34 – Abhängigkeiten im Debug-Build optimieren
`[profile.dev.package."*"] opt-level = 2`: Ohne diese Einstellung hasht ein Debug-Build nur ~15 MB/s,
und ein Test mit einem echten Image dauert Minuten. Der eigene Code bleibt unoptimiert, damit er
schnell kompiliert und gut zu debuggen ist.

### E35 – `rust-version` bestimmt, welche Paketversionen Cargo wählt
Mit `resolver = "3"` wählt Cargo nur Paketversionen, die mit der angegebenen Mindest-Rust-Version
(`rust-version`) funktionieren. Bei `1.85` blieb Tauri deshalb auf 2.11, weil 2.12 Rust 1.90 braucht.
Die npm-Pakete waren aber schon 2.12, und `tauri dev` meldete einen Versions-Mismatch. Jetzt steht
dort `1.90`, und Rust- und npm-Seite von Tauri sind wieder auf derselben Minor-Version.

### E36 – Windows-ISOs lädt der Nutzer selbst herunter, SealFlash prüft sie
Microsoft bietet keinen offenen Download-Link an. Die Download-Seite fordert die Links über eine
Schnittstelle mit Bot-Schutz an. Den zu umgehen wäre technisch möglich, verstößt aber gegen
Microsofts Regeln und kann jederzeit brechen, deshalb machen wir das bewusst **nicht**. Stattdessen:
1. „Download-Seite öffnen“ öffnet Microsofts Seite im Browser des Nutzers.
2. Der Nutzer wählt die heruntergeladene ISO aus.
3. SealFlash lädt dieselbe Seite über HTTPS und liest die **von Microsoft veröffentlichte Tabelle
   der SHA-256-Werte** aus (Katalog: `"checksum": {"type": "page_listing"}`). Steht der Hash der
   Datei nicht darin, wird nicht geschrieben. Ist die Tabelle leer (Seite umgebaut), ist das ein
   Fehler und kein „Durchwinken“.

### E37 – „Eigenes Image“: Prüfsumme ist Pflicht
Für Images, die nicht im Katalog stehen, gibt der Nutzer die Prüfsumme des Herstellers ein (SHA-256
oder SHA-512, MD5 nur mit Warnung). Ohne Prüfsumme lässt sich nichts schreiben. Das ist dieselbe Regel
wie bei Downloads: Beschädigte oder manipulierte Dateien fallen so auf. Der Schreibmodus wird
automatisch erkannt (Windows-Installer oder 1:1). Bei einem ISO ohne Bootsektor warnt die GUI.

### E38 – Windows-Stick: GPT + FAT32 statt 1:1-Abbild
Windows-ISOs sind keine Hybrid-ISOs: 1:1 auf einen Stick geschrieben, startet davon kein PC. SealFlash
baut den Stick deshalb wie Rufus im UEFI-Standardmodus: GPT mit einer FAT32-Partition
(„Microsoft Basic Data“) und alle Dateien der ISO darauf. UEFI-Firmware startet `efi\boot\bootx64.efi`
direkt von FAT32, auch mit Secure Boot, weil die Datei von Microsoft signiert ist.
**FAT32 kann keine Dateien über 4 GB speichern**, `install.wim` ist aber meist größer. Sie wird
mit `wimlib-imagex split` in `install.swm`, `install2.swm` … zerlegt, die das Windows-Setup direkt
lesen kann. Die Alternative NTFS bräuchte einen eigenen NTFS-Treiber für die Firmware
(Rufus' „UEFI:NTFS“) und wäre viel komplexer. Folge: nur UEFI-Boot, kein reines BIOS. Seit Windows 11
ist UEFI ohnehin Pflicht.

### E39 – Eigenes FAT32 statt Bibliothek
Erst wurde die Bibliothek `fatfs` verwendet. `fsck.vfat` meldete danach Fehler: Sie schreibt
Langnamen-Einträge vor „.“ und „..“, und FSInfo bleibt uninitialisiert. Weil die komplette Dateiliste
vorher bekannt ist, ist ein eigener „Planer“ einfach. Er berechnet alle Metadaten (Bootsektor, FATs,
Verzeichnisse) vorab und legt jede Datei zusammenhängend ab. Danach werden nur noch Bytes an
berechnete Stellen geschrieben. Das ist leicht zu prüfen: Beim Zurücklesen werden die Metadaten Byte
für Byte und jede Datei per SHA-256 verglichen. Getestet wird gegen die Standardwerkzeuge
`fsck.vfat`, `sgdisk -v` und `wimlib-imagex join`.

### E40 – Block-Cache für den Direktzugriff
Am Betriebssystem-Cache vorbei (`O_DIRECT` / `FILE_FLAG_NO_BUFFERING`) muss jeder Zugriff
sektor-ausgerichtet sein. Partitionstabelle und FAT-Strukturen sind aber kleine, krumme Stücke.
`BlockCache` sammelt sie in 1-MiB-Blöcken und merkt sich pro 512-Byte-Sektor „gültig“ und „geändert“.
Fortlaufende Dateiinhalte kommen so ganz ohne vorheriges Lesen aus.

### E41 – ISO einbinden: ohne fremde Programme und ohne Austausch-Lücke
- **Linux** (im Root-Helper): Der Helper öffnet die Datei selbst (`O_NOFOLLOW`), prüft Besitzer
  und Fingerabdruck (Gerät, Inode, Größe, Änderungszeit von vor der Hash-Prüfung) und hängt **genau
  diesen Dateigriff** per `LOOP_SET_FD` an ein Loop-Gerät. Mit `mount(2)` wird es dann eingebunden,
  schreibgeschützt und mit `nosuid,nodev,noexec`, in ein Verzeichnis unter `/run`, das nur root
  lesen kann. Wer den Pfad zwischen Prüfung und Einbinden austauscht, fällt auf (`ImageChanged`).
  Das Loop-Gerät löst sich beim Aushängen selbst (`LO_FLAGS_AUTOCLEAR`).
- **Windows**: `Mount-DiskImage -Access ReadOnly` (PowerShell, eingebaut). Der Fingerabdruck ist dort
  Größe + Änderungszeit.
- Nach dem Kopieren wird der Fingerabdruck erneut verglichen. Hat sich die ISO geändert, wird der
  Stick unbrauchbar gemacht.

### E42 – Windows-ISOs am UDF-Dateisystem erkennen
Microsofts ISOs sind „UDF-Bridge“-Images: Alle Dateien stehen nur im UDF-Teil, der ISO9660-Teil
enthält nur eine `README.TXT` („This disc contains a UDF file system …“). SealFlash liest deshalb das
UDF-Hauptverzeichnis selbst (`local::udf`, ca. 150 Zeilen nach ECMA-167) und sucht `sources/` und
`bootmgr`. Die Testdatei `windows-udf-only.iso` hat genau diesen Aufbau (`genisoimage -udf -hide …`).

### E43 – Datei-Dialog und Browser öffnen nur im Backend
`tauri-plugin-dialog` und `tauri-plugin-opener` werden nur aus Rust benutzt. In
`capabilities/default.json` steht weiterhin nur `core:default`, das Frontend kann also weder Dateien
lesen noch beliebige Programme oder Adressen öffnen. „Download-Seite öffnen“ übergibt nur die
Varianten-ID, die Adresse kommt aus dem Katalog (und muss HTTPS sein). „Flash!“ mit eigener Datei
akzeptiert nur Pfade, die vorher im Backend geprüft wurden. Beim Schreiben wird ohnehin noch einmal
gehasht.

### E44 – Privates Arbeitsverzeichnis für Zwischendateien
Die aufgeteilten `.swm`-Dateien entstehen in einem neu angelegten Verzeichnis mit Zufallsnamen und
Modus 0700, unter Linux in `/var/tmp` (liegt auf der Platte, `/tmp` ist oft eine RAM-Disk). Ein fester
Name wie `/tmp/sealflash` wäre gefährlich, weil der Helper als root läuft: Ein anderer Benutzer könnte
das Verzeichnis vorher anlegen und Symlinks hineinlegen, und root würde dann fremde Dateien
überschreiben.

### E45 – Komprimierte Images: gzip, xz, zstd im Fluss
Eigene Images sind oft komprimiert (Raspberry Pi: `.img.xz`). Sie werden beim Schreiben im Fluss
entpackt (`flate2`, `xz2`, `ruzstd`). Die Prüfsumme gilt, wie bei den Herstellern üblich, für die
komprimierte Datei. `xz2` kompiliert liblzma 5.2 mit. Die Versionen 5.6.0/5.6.1 mit der Hintertür von
2024 sind nicht betroffen.

### E46 – Linux: .deb und .rpm, kein AppImage
Ein AppImage ist eine Datei, die sich beim Start per FUSE selbst einbindet. Der Root-Helper müsste
dann aus diesem Einhängepunkt gestartet werden. Den darf root aber standardmäßig gar nicht lesen
(FUSE ohne `allow_other`). Außerdem nennt die polkit-Regel einen festen Pfad. Das .deb/.rpm
installiert den Helper nach `/usr/lib/sealflash/`, die polkit-Regel nach
`/usr/share/polkit-1/actions/` und zieht `wimtools`/`wimlib-utils` und pkexec als Abhängigkeiten
mit. Gebaut wird das .deb auf Ubuntu 22.04, damit es auch auf älteren Systemen installierbar ist:
Die Mindestversionen der Bibliotheken richten sich nach dem System, auf dem gebaut wurde.

### E47 – Release-App startet nur den Helper aus `/usr/lib/sealflash`
Im Debug-Build sucht die App den Helper auch neben sich selbst (bequem beim Entwickeln). Im
Release-Build nur unter `/usr/lib/sealflash/`. Dorthin kann nur root schreiben, und dieser Pfad steht
in der polkit-Regel. Läge die App z. B. in einem vom Benutzer beschreibbaren Ordner, könnte sonst
jemand dort einen veränderten „Helper“ ablegen. Der würde mit Administratorrechten laufen, sobald
der Benutzer das Passwort eingibt.

### E48 – wimlib im Windows-Installer: Hash festgeschrieben
wimlib veröffentlicht keine Prüfsummen oder Signaturen. Der SHA-256 des Windows-Pakets wurde beim
ersten Download festgehalten („Trust on first use“) und steht in `fetch-wimlib.ps1`. Jeder spätere
Download, lokal oder im CI, muss exakt passen. So kann eine später manipulierte Datei auf dem Server
nicht unbemerkt in unseren Installer gelangen. wimlib steht unter der GPLv3+, genau wie SealFlash.
Deshalb liegen die Lizenztexte im Installer, und jedes Release enthält den Quelltext
(`wimlib-1.14.5.tar.gz`). [THIRD-PARTY.md](../THIRD-PARTY.md) listet alle Abhängigkeiten mit
Lizenz und wird mit `scripts/third_party.py` erzeugt.

### E49 – Release als Entwurf, mit SHA256SUMS
Der Release-Workflow veröffentlicht nichts selbst, sondern legt einen Entwurf an. Ein Mensch prüft
und veröffentlicht. `SHA256SUMS.txt` über alle Dateien gibt Nutzern dieselbe Prüfmöglichkeit, die
SealFlash selbst bei Distributionen verlangt. Der Windows-Installer ist (noch) nicht code-signiert:
Ein Zertifikat kostet Geld, SmartScreen warnt deshalb beim ersten Start.

### E50 – Speicherplatz vor dem Download prüfen, Cache sichtbar machen
Sobald die Größe bekannt ist (`Content-Length`), wird geprüft, ob die Datei plus 256 MB Reserve auf
das Laufwerk passt. Ein Abbruch bei 95 % wegen voller Platte wäre ärgerlich. Heruntergeladene Images
bleiben im Cache (zweiter Stick ohne neuen Download). Unten rechts zeigt die App, wie viel Platz das
belegt, und kann ihn leeren. Gelöscht werden nur Dateien direkt im Cache-Ordner, Links werden nicht
verfolgt.

### E51 – Umbenennung BootForge → SealFlash (0.2.0)
„BootForge“ wird bereits von anderen USB-Boot-Tools verwendet (eine Android-App, ein
Windows-Programm mit „™“ und ein Open-Source-Projekt). Um Verwechslungen und Ärger mit Markenrechten
zu vermeiden, heißt das Projekt ab 0.2.0 **SealFlash**: „versiegelt geflasht“ steht für das, was es von
anderen Tools abhebt, nämlich dass nur geprüfte Images auf den Stick kommen. Geändert wurden App-Name,
Kennung (`io.github.xhellphoenix.sealflash`), Crate- und Repo-Namen, Helper-Pfad
(`/usr/lib/sealflash/`), polkit-Regel, Katalog-URL, Umgebungsvariablen (`SEALFLASH_*`) und das
Volume-Label der Windows-Sticks. Weil sich die Kennung ändert, legt die App neue Ordner für Cache und
Einstellungen an. Bereits heruntergeladene Images aus BootForge werden nicht übernommen.

### E52 – Windows-Einrichtung per Antwortdatei, ohne Passwort und ohne Partitionierung
Vor dem Schreiben kann man Fragen der Windows-Installation beantworten: lokales Konto statt
Microsoft-Konto, Computername, Region/Tastatur, Datenschutz-Fragen, Hardware-Prüfungen. SealFlash
erzeugt daraus `autounattend.xml` (`flash::unattend`) und legt sie ins Hauptverzeichnis des Sticks.
Dort sucht Windows-Setup sie automatisch. Die Datei wird wie alle anderen beim Zurücklesen geprüft.
- **Kein Passwort in der Datei:** Windows speichert Passwörter in Antwortdateien nur Base64-kodiert,
  also lesbar für jeden, der den Stick hat. Das Konto wird deshalb ohne Passwort angelegt. Man setzt
  es nach der ersten Anmeldung.
- **Keine automatische Partitionierung:** Eine Antwortdatei könnte auch die Festplatte löschen und
  neu aufteilen. Steckt so ein Stick im falschen PC, wären Daten weg. Die Plattenauswahl bleibt
  deshalb immer eine Entscheidung am Bildschirm, und ein Test stellt sicher, dass die Datei kein
  `DiskConfiguration` enthält.
- **Sprache aus der ISO:** Verlangt die Antwortdatei eine Setup-Sprache, die das Image nicht
  enthält, bricht Windows ab. Die Sprache wird deshalb aus `sources/lang.ini` der eingebundenen ISO
  gelesen. Die Datei ist bei Microsoft UTF-16-kodiert.
- **Eingaben doppelt geprüft:** Erlaubte Zeichen und Längen wie in Windows (Benutzername max. 20,
  Computername max. 15, NetBIOS-Zeichen). Geprüft wird schon während der Eingabe und noch einmal im
  Kern bzw. Root-Helper vor dem Schreiben. Alle Werte werden zusätzlich XML-maskiert.
- **Hardware-Prüfungen umgehen** nutzt die von Microsoft für Tests vorgesehenen
  `LabConfig`-Registry-Werte. Das ist offiziell nicht unterstützt und wird in der App so
  gekennzeichnet.
- **Lokale Konten:** Microsoft macht sie in neueren Windows-Versionen schwerer
  (`BypassNRO`, `HideOnlineAccountScreens`). Über Antwortdateien funktioniert es, weil Firmen das
  brauchen. Dass das in künftigen Versionen so bleibt, ist nicht garantiert.

### E53 – Hersteller ohne Prüfsumme: „Nur Herkunft geprüft“ als bewusste, sichtbare Ausnahme
Microsoft veröffentlicht für die Windows-Server-Testversionen keine Prüfsummen. Nach der
Grundregel („nie ohne passende Prüfsumme flashen“) gäbe es diese Images in SealFlash gar nicht.
Mit dem Projektinhaber wurde entschieden, eine **eng begrenzte, sichtbar gekennzeichnete
Ausnahme** einzuführen:
- Katalog: Quelle `direct` (fester offizieller Link) **nur zusammen mit** `checksum.type =
  "https_origin"` und einer Liste **exakter** Server (`allowed_hosts`, keine Platzhalter). Andere
  Kombinationen lehnt der Validator ab (Test `origin_only_is_tightly_restricted`).
- Download: Nach allen Weiterleitungen (`go.microsoft.com` → `aka.ms` →
  `software-static.download.prss.microsoft.com`) muss die Antwort per HTTPS von einem dieser Server
  kommen. Geprüft wird **bevor ein Byte gespeichert wird**. Ein ähnlich klingender Name wie
  `…microsoft.com.evil.example` zählt nicht.
- Was das absichert: Die Datei kommt nachweislich vom Server des Herstellers und wurde unterwegs
  nicht verändert (TLS). Was es **nicht** absichert: eine auf dem Server selbst ausgetauschte Datei.
  Das kann nur eine veröffentlichte Prüfsumme oder Signatur.
- Kennzeichnung: gelbes Abzeichen „Nur Herkunft geprüft“, Hinweistext bei der Version, Zeile
  „Echtheit“ im Bestätigungsdialog, eigene Stufe `Assurance::Origin` im Ergebnis.
- Cache: Der beim Download berechnete SHA-256 wird neben der Datei festgehalten (`.sha256`). Eine
  später beschädigte Datei fällt so auf und wird neu geladen.
- Nur für Downloads, die SealFlash selbst macht. Bei „Eigenes Image“ ist die Herkunft unbekannt,
  dort bleibt die Prüfsumme Pflicht.

### E54 – Windows-Modus: Dateien als ein fortlaufender Datenstrom schreiben (0.4.1)
- Problem: Beim Kopieren einer Windows-ISO fiel die Geschwindigkeit oft auf 3–5 MB/s. Die ISO
  enthält Hunderte kleine Dateien (`.mui`, `.dll`). Jede endete mitten in einem Sektor, deshalb
  - las der Block-Cache diesen Sektor erst vom Stick (ein Lesezugriff pro Datei) und
  - schrieb jede Datei einzeln, weil zwischen zwei Dateien der ungenutzte Rest des Clusters lag.
  Mit `FILE_FLAG_WRITE_THROUGH` wartet jeder dieser kleinen Zugriffe einzeln auf den Stick.
- Lösung: Der Rest des letzten Clusters jeder Datei wird mit Nullen aufgefüllt und im selben
  Zugriff geschrieben. Da der FAT32-Plan die Dateien lückenlos hintereinander legt, entsteht ein
  einziger fortlaufender Datenstrom. Der Block-Cache (jetzt 4-MiB-Blöcke, 16 Stück = 64 MB RAM)
  schreibt ihn in großen Stücken.
- Ergebnis im Test (400 kleine Dateien, 4-KB-Cluster): vorher 401 Lese- und 375 Schreibzugriffe,
  jetzt 0 Lese- und 4 Schreibzugriffe (`many_small_files_are_written_in_large_sequential_chunks`).
- Die Nullen liegen hinter dem Dateiende im eigenen Cluster der Datei. Sie gehören nicht zum
  Dateiinhalt, an der Prüfung (Hash über die Dateigröße) ändert sich nichts.

## Offene Punkte
- **Kompatibilität des Online-Katalogs:** Wegen `deny_unknown_fields` lehnt eine ältere App einen
  Katalog mit neuen Feldern ab (z. B. `allow_v3`) und fällt auf ihren Cache zurück. Neue Felder darf
  der Online-Katalog deshalb erst bekommen, wenn die passende App-Version veröffentlicht ist.
  Langfristig: `schema_version` erhöhen und pro Version eine eigene Datei anbieten.
- Download-Ordner wählbar; alte Versionen automatisch aufräumen.
- GitHub Action im Katalog-Repo, die `resolve_all` regelmäßig laufen lässt und bei kaputten
  Einträgen ein Issue öffnet.
- TypeScript-Typen werden noch von Hand gepflegt (`src/lib/types.ts`). Später eventuell mit
  `specta`/`ts-rs` generieren.
- Code-Signatur für den Windows-Installer, Auto-Update (Tauri-Updater mit signierten Updates).
- Katalog-Zweig `windows-manual-other` beim Veröffentlichen von 0.1.0 nach `main` mergen (die
  App-Version auf `main` ist die erste, ältere gibt es nicht; siehe docs/RELEASE.md).
- Windows-Sticks für reine BIOS-PCs (MBR + NTFS + Bootsektor) – bewusst nicht geplant, siehe E38.
