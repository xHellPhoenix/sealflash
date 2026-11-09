//! Windows-Installationsstick erstellen: GPT + FAT32 + Dateien aus der ISO.
//!
//! Warum nicht einfach das ISO 1:1 schreiben wie bei Linux? Windows-ISOs sind keine „Hybrid-ISOs“ –
//! von einem 1:1 kopierten Stick startet ein PC nicht. Stattdessen braucht es (wie bei Rufus im
//! Standardmodus für UEFI):
//!
//! 1. GPT-Partitionstabelle mit einer FAT32-Partition (UEFI-Firmware startet von FAT32),
//! 2. alle Dateien der ISO darauf kopiert,
//! 3. `sources/install.wim` aufgeteilt, wenn sie größer als 4 GiB ist – FAT32 kann keine
//!    größeren Dateien speichern. Windows-Setup kann aufgeteilte `.swm`-Dateien lesen.
//!
//! Prüfung: Beim Kopieren wird jede Datei gehasht. Danach wird der Cache verworfen und vom Stick
//! zurückgelesen: Bootsektor, FATs und Verzeichnisse Byte für Byte, jede Datei per Hash.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use sha2::{Digest, Sha256};

use super::blockcache::{BlockCache, Partition};
use super::write::{Device, FlashError, FlashEvent, FlashReport, FlashStage};
use super::{fat32, gpt};

/// Größte Datei, die FAT32 speichern kann (4 GiB − 1 Byte).
pub const FAT32_MAX_FILE: u64 = 4 * 1024 * 1024 * 1024 - 1;
const COPY_CHUNK: usize = 4 * 1024 * 1024;

pub struct MediaOptions {
    /// Ab dieser Größe wird `install.wim` aufgeteilt (im Betrieb [`FAT32_MAX_FILE`]; Tests kleiner).
    pub split_above: u64,
    /// Größe der Teile in MiB (`wimlib-imagex split … <MiB>`).
    pub split_part_mib: u32,
    /// Arbeitsverzeichnis für die aufgeteilten Teile (braucht so viel Platz wie install.wim).
    pub temp_dir: PathBuf,
    /// Pfad zu `wimlib-imagex` (nur nötig, wenn aufgeteilt werden muss).
    pub wimlib: Option<PathBuf>,
    /// Volume-Name des Sticks (max. 11 Zeichen).
    pub label: String,
    /// Zusätzliche Dateien im Hauptverzeichnis, die nicht aus der ISO kommen (z. B.
    /// `autounattend.xml`). Ersetzt eine gleichnamige Datei der ISO.
    pub extra_files: Vec<(String, Vec<u8>)>,
}

impl MediaOptions {
    pub fn new(temp_dir: PathBuf) -> Self {
        Self {
            split_above: FAT32_MAX_FILE,
            split_part_mib: 3800,
            temp_dir,
            wimlib: find_wimlib(),
            label: "SEALFLASH".into(),
            extra_files: Vec::new(),
        }
    }
}

/// Sucht `wimlib-imagex` neben dem Programm und im PATH.
pub fn find_wimlib() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "wimlib-imagex.exe"
    } else {
        "wimlib-imagex"
    };
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let path_dirs = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();
    exe_dir
        .into_iter()
        .chain(path_dirs)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// Woher eine Datei kommt: aus der eingebundenen ISO bzw. dem Arbeitsordner, oder aus dem Speicher.
#[derive(Debug, Clone)]
enum Src {
    File(PathBuf),
    Memory(std::sync::Arc<Vec<u8>>),
}

impl Src {
    fn open(&self) -> io::Result<Box<dyn Read + '_>> {
        Ok(match self {
            Src::File(p) => Box::new(File::open(p)?),
            Src::Memory(b) => Box::new(io::Cursor::new(b.as_slice())),
        })
    }
}

/// Eine zu kopierende Datei: Pfad im Ziel (mit `/`), Quelle, Größe.
#[derive(Debug, Clone)]
struct Item {
    dest: String,
    src: Src,
    size: u64,
}

/// Alle Dateien und Verzeichnisse unterhalb von `root` (Verzeichnisse zuerst, sortiert).
fn walk(root: &Path) -> io::Result<(Vec<String>, Vec<Item>)> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&dir)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            let dest = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            let meta = e.metadata()?;
            if meta.is_dir() {
                dirs.push(dest.clone());
                stack.push((e.path(), dest));
            } else if meta.is_file() {
                files.push(Item {
                    dest,
                    src: Src::File(e.path()),
                    size: meta.len(),
                });
            }
            // Symlinks u. Ä. gibt es auf Windows-ISOs nicht → ignorieren.
        }
    }
    dirs.sort();
    Ok((dirs, files))
}

fn io_err(e: io::Error) -> FlashError {
    FlashError::Io(e.to_string())
}

/// `install.wim` aufteilen. Liefert die neuen Einträge (`sources/install.swm`, `install2.swm`, …).
fn split_wim(item: &Item, opts: &MediaOptions) -> Result<Vec<Item>, FlashError> {
    let wimlib = opts.wimlib.as_ref().ok_or_else(|| {
        FlashError::Unsupported(
            "install.wim ist größer als 4 GB und muss aufgeteilt werden – dafür fehlt das Programm \
             wimlib-imagex (Linux: Paket „wimtools“; Windows: liegt der App bei)"
                .into(),
        )
    })?;
    let out_dir = opts.temp_dir.join("sealflash-swm");
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).map_err(io_err)?;
    let Src::File(wim) = &item.src else {
        return Err(FlashError::Io(
            "install.wim liegt nicht als Datei vor".into(),
        ));
    };
    let out = Command::new(wimlib)
        .arg("split")
        .arg(wim)
        .arg(out_dir.join("install.swm"))
        .arg(opts.split_part_mib.to_string())
        .output()
        .map_err(|e| FlashError::Io(format!("wimlib-imagex lässt sich nicht starten: {e}")))?;
    if !out.status.success() {
        return Err(FlashError::Io(format!(
            "wimlib-imagex split fehlgeschlagen: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let parent = item.dest.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
    let (_, mut parts) = walk(&out_dir).map_err(io_err)?;
    parts.sort_by(|a, b| a.dest.len().cmp(&b.dest.len()).then(a.dest.cmp(&b.dest)));
    for p in &mut parts {
        p.dest = if parent.is_empty() {
            p.dest.clone()
        } else {
            format!("{parent}/{}", p.dest)
        };
        if p.size > FAT32_MAX_FILE {
            return Err(FlashError::Io(format!(
                "Teil {} ist immer noch zu groß",
                p.dest
            )));
        }
    }
    Ok(parts)
}

struct Progress<'a> {
    stage: FlashStage,
    total: u64,
    done: u64,
    started: Instant,
    last: Instant,
    on_event: &'a mut dyn FnMut(FlashEvent),
}

impl Progress<'_> {
    fn add(&mut self, n: u64) {
        self.done += n;
        if self.last.elapsed().as_millis() >= 200 {
            self.last = Instant::now();
            let secs = self.started.elapsed().as_secs_f64().max(0.001);
            (self.on_event)(FlashEvent::Progress {
                stage: self.stage,
                done: self.done,
                total: Some(self.total),
                bytes_per_sec: self.done as f64 / secs,
            });
        }
    }
}

fn hex(d: impl AsRef<[u8]>) -> String {
    d.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Erstellt den Windows-Stick. `source` ist das Wurzelverzeichnis der eingebundenen ISO.
pub fn write_windows_media<D: Device>(
    dev: D,
    disk_size: u64,
    source: &Path,
    opts: &MediaOptions,
    cancel: &AtomicBool,
    on_event: &mut dyn FnMut(FlashEvent),
) -> Result<FlashReport, FlashError> {
    let check = || {
        if cancel.load(Ordering::Relaxed) {
            Err(FlashError::Cancelled)
        } else {
            Ok(())
        }
    };

    // 1) Inhalt erfassen, zu große install.wim aufteilen.
    let (dirs, mut files) = walk(source).map_err(io_err)?;
    if !files.iter().any(|f| f.dest.eq_ignore_ascii_case("bootmgr")) {
        return Err(FlashError::Unsupported(
            "das ist kein Windows-Installationsmedium (kein bootmgr)".into(),
        ));
    }
    let mut split_parts = Vec::new();
    if let Some(i) = files.iter().position(|f| f.size > opts.split_above) {
        let big = files[i].clone();
        let name = big
            .dest
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if name != "install.wim" {
            return Err(FlashError::Unsupported(format!(
                "{} ist größer als 4 GB und kann nicht auf FAT32 gespeichert werden",
                big.dest
            )));
        }
        on_event(FlashEvent::Stage {
            stage: FlashStage::Split,
        });
        split_parts = split_wim(&big, opts)?;
        files.remove(i);
        files.extend(split_parts.iter().cloned());
    }
    // Zusatzdateien (z. B. autounattend.xml) – ersetzen eine gleichnamige Datei der ISO.
    for (name, data) in &opts.extra_files {
        if name.contains('/') || name.contains('\\') || name.is_empty() {
            return Err(FlashError::Io(format!("ungültiger Dateiname {name}")));
        }
        files.retain(|f| !f.dest.eq_ignore_ascii_case(name));
        files.push(Item {
            dest: name.clone(),
            size: data.len() as u64,
            src: Src::Memory(std::sync::Arc::new(data.clone())),
        });
    }
    if let Some(f) = files.iter().find(|f| f.size > FAT32_MAX_FILE) {
        return Err(FlashError::Unsupported(format!(
            "{} ist größer als 4 GB",
            f.dest
        )));
    }
    let total: u64 = files.iter().map(|f| f.size).sum();
    check()?;

    let mut cache = BlockCache::new(dev, disk_size, 16);
    let result = (|| -> Result<FlashReport, FlashError> {
        // 2) Partitionstabelle
        on_event(FlashEvent::Stage {
            stage: FlashStage::Partition,
        });
        let layout = gpt::layout(
            disk_size,
            &opts.label,
            gpt::random_guid(),
            gpt::random_guid(),
        )
        .map_err(io_err)?;
        if total + total / 20 + 64 * 1024 * 1024 > layout.partition.len {
            return Err(FlashError::TooSmall {
                need: total,
                have: layout.partition.len,
            });
        }
        layout.write(&mut cache).map_err(io_err)?;

        // 3) FAT32 planen (alle Metadaten vorab berechnet, Dateien zusammenhängend)
        on_event(FlashEvent::Stage {
            stage: FlashStage::Format,
        });
        let dests: Vec<(String, u64)> = files.iter().map(|f| (f.dest.clone(), f.size)).collect();
        let plan = fat32::plan(
            layout.partition.len,
            layout.partition.start / 512,
            &opts.label,
            &dirs,
            &dests,
        )
        .map_err(io_err)?;
        {
            let mut part = Partition::new(&mut cache, layout.partition.start, layout.partition.len);
            for r in &plan.metadata {
                part.seek(SeekFrom::Start(r.offset)).map_err(io_err)?;
                part.write_all(&r.data).map_err(io_err)?;
            }
        }

        // 4) Dateien kopieren (mit Hash je Datei)
        on_event(FlashEvent::Stage {
            stage: FlashStage::Copy,
        });
        let mut hashes = Vec::with_capacity(files.len());
        {
            let mut part = Partition::new(&mut cache, layout.partition.start, layout.partition.len);
            let mut p = Progress {
                stage: FlashStage::Copy,
                total,
                done: 0,
                started: Instant::now(),
                last: Instant::now(),
                on_event: &mut *on_event,
            };
            let mut buf = vec![0u8; COPY_CHUNK];
            // Den Rest des letzten Clusters jeder Datei mit Nullen füllen. Die Dateien liegen
            // direkt hintereinander, so entsteht ein einziger fortlaufender Datenstrom: Der
            // Block-Cache schreibt dann ganze Blöcke am Stück und muss nie einen angeschnittenen
            // Sektor vom Stick nachlesen. Ohne das wurde jede kleine Datei einzeln geschrieben
            // (plus ein Lesezugriff) – bei Windows-ISOs mit Hunderten .mui/.dll-Dateien sehr langsam.
            let bpc = plan.bytes_per_cluster;
            debug_assert!((COPY_CHUNK as u64).is_multiple_of(bpc));
            for (item, place) in files.iter().zip(&plan.files) {
                check()?;
                let mut src = item.src.open().map_err(io_err)?;
                part.seek(SeekFrom::Start(place.offset)).map_err(io_err)?;
                let mut h = Sha256::new();
                let mut copied = 0u64;
                loop {
                    check()?;
                    // Puffer ganz füllen (`read` liefert manchmal weniger), damit jeder
                    // Schreibzugriff ein Vielfaches der Clustergröße ist.
                    let mut n = 0;
                    while n < buf.len() {
                        let r = src.read(&mut buf[n..]).map_err(io_err)?;
                        if r == 0 {
                            break;
                        }
                        n += r;
                    }
                    if n == 0 {
                        break;
                    }
                    h.update(&buf[..n]);
                    copied += n as u64;
                    p.add(n as u64);
                    // Letztes Stück: Rest des Clusters mit Nullen auffüllen, im selben Zugriff.
                    let padded = (n as u64).div_ceil(bpc) * bpc;
                    buf[n..padded as usize].fill(0);
                    part.write_all(&buf[..padded as usize]).map_err(io_err)?;
                    if n < buf.len() {
                        break;
                    }
                }
                if copied != item.size {
                    return Err(FlashError::Io(format!(
                        "{} hat sich beim Lesen in der Größe geändert",
                        item.dest
                    )));
                }
                hashes.push(hex(h.finalize()));
            }
        }
        // Alles auf den Stick und den Cache vergessen – die Prüfung muss vom Stick lesen.
        cache.drop_cache().map_err(io_err)?;

        // 5) Zurücklesen: Metadaten Byte für Byte, Dateien per Hash
        on_event(FlashEvent::Stage {
            stage: FlashStage::Readback,
        });
        {
            let mut part = Partition::new(&mut cache, layout.partition.start, layout.partition.len);
            for r in &plan.metadata {
                let mut back = vec![0u8; r.data.len()];
                part.seek(SeekFrom::Start(r.offset)).map_err(io_err)?;
                part.read_exact(&mut back).map_err(io_err)?;
                if back != r.data {
                    return Err(FlashError::ReadbackMismatch {
                        offset: layout.partition.start + r.offset,
                    });
                }
            }
            let mut p = Progress {
                stage: FlashStage::Readback,
                total,
                done: 0,
                started: Instant::now(),
                last: Instant::now(),
                on_event: &mut *on_event,
            };
            let mut buf = vec![0u8; COPY_CHUNK];
            for ((item, place), expected) in files.iter().zip(&plan.files).zip(&hashes) {
                check()?;
                part.seek(SeekFrom::Start(place.offset)).map_err(io_err)?;
                let mut h = Sha256::new();
                let mut left = place.size;
                while left > 0 {
                    let n = left.min(COPY_CHUNK as u64) as usize;
                    part.read_exact(&mut buf[..n]).map_err(io_err)?;
                    h.update(&buf[..n]);
                    left -= n as u64;
                    p.add(n as u64);
                }
                if &hex(h.finalize()) != expected {
                    return Err(FlashError::ReadbackMismatch {
                        offset: layout.partition.start + place.offset,
                    });
                }
                let _ = item;
            }
        }

        // Zusammenfassung: ein Hash über „Pfad + Hash“ aller Dateien.
        let mut manifest = Sha256::new();
        for (item, h) in files.iter().zip(&hashes) {
            manifest.update(format!("{}\t{h}\n", item.dest));
        }
        Ok(FlashReport {
            written: total,
            sha256: hex(manifest.finalize()),
        })
    })();

    if result.is_err() {
        // Halbfertigen Stick unbrauchbar machen (Partitionstabelle weg).
        let zeros = vec![0u8; 1024 * 1024];
        let _ = io::Seek::seek(&mut cache, io::SeekFrom::Start(0))
            .and_then(|_| cache.write_all(&zeros));
    }
    let flush = cache.flush_all();
    if !split_parts.is_empty() {
        let _ = std::fs::remove_dir_all(opts.temp_dir.join("sealflash-swm"));
    }
    let report = result?;
    flush.map_err(io_err)?;
    Ok(report)
}
