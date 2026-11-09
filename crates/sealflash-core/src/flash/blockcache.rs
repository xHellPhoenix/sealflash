//! Block-Cache zwischen kleinen Zugriffen (FAT32-Bibliothek) und direktem Gerätezugriff.
//!
//! Direkter Zugriff am Betriebssystem-Cache vorbei (`O_DIRECT` / `FILE_FLAG_NO_BUFFERING`) verlangt
//! sektor-ausgerichtete Puffer und Längen. `fatfs` schreibt aber beliebige Häppchen
//! (32-Byte-Verzeichniseinträge, einzelne FAT-Einträge, Cluster). Dieser Cache übersetzt:
//!
//! - Das Gerät wird in Blöcke zu 4 MiB geteilt; die zuletzt benutzten liegen im RAM (LRU).
//!   Große Blöcke = große Schreibzugriffe: USB-Sticks sind bei vielen kleinen Zugriffen
//!   ein Vielfaches langsamer als bei wenigen großen.
//! - Je 512-Byte-Sektor merkt er sich „gültig“ (vom Gerät gelesen oder ganz beschrieben) und
//!   „geändert“. Ein Sektor wird nur dann vom Gerät gelesen, wenn er teilweise beschrieben oder
//!   gelesen wird. Fortlaufendes Schreiben (Dateiinhalte) kommt so ganz ohne Lesen aus.
//! - Beim Verdrängen und bei `flush` werden nur geänderte Sektorbereiche geschrieben.

use std::io::{self, Read, Seek, SeekFrom, Write};

use super::write::{AlignedBuf, Device};

pub const SECTOR: usize = 512;
const BLOCK: usize = 4 * 1024 * 1024;
const SECTORS_PER_BLOCK: usize = BLOCK / SECTOR;

struct Slot {
    index: u64,
    buf: AlignedBuf,
    /// Länge dieses Blocks (am Geräteende kürzer).
    len: usize,
    valid: Vec<bool>,
    dirty: Vec<bool>,
    last_use: u64,
}

pub struct BlockCache<D: Device> {
    dev: D,
    size: u64,
    capacity: usize,
    slots: Vec<Slot>,
    pos: u64,
    tick: u64,
}

impl<D: Device> BlockCache<D> {
    /// `size`: Gerätegröße in Bytes (Vielfaches von 512). `capacity`: Blöcke im RAM.
    pub fn new(dev: D, size: u64, capacity: usize) -> Self {
        assert!(
            size.is_multiple_of(SECTOR as u64),
            "Gerätegröße muss ein Vielfaches von 512 sein"
        );
        Self {
            dev,
            size,
            capacity: capacity.max(2),
            slots: Vec::new(),
            pos: 0,
            tick: 0,
        }
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    /// Alles Geänderte schreiben und das Gerät synchronisieren.
    pub fn flush_all(&mut self) -> io::Result<()> {
        for i in 0..self.slots.len() {
            Self::write_back(&mut self.dev, &mut self.slots[i])?;
        }
        self.dev.sync()
    }

    /// Cache leeren (nach `flush_all`): folgende Lesezugriffe kommen wieder vom Gerät.
    pub fn drop_cache(&mut self) -> io::Result<()> {
        self.flush_all()?;
        self.slots.clear();
        Ok(())
    }

    pub fn into_inner(mut self) -> io::Result<D> {
        self.flush_all()?;
        Ok(self.dev)
    }

    fn write_back(dev: &mut D, slot: &mut Slot) -> io::Result<()> {
        let base = slot.index * BLOCK as u64;
        let n = slot.len / SECTOR;
        let mut s = 0;
        while s < n {
            if !slot.dirty[s] {
                s += 1;
                continue;
            }
            let start = s;
            while s < n && slot.dirty[s] {
                s += 1;
            }
            // Zusammenhängender geänderter Bereich → ein ausgerichteter Schreibzugriff.
            dev.seek(SeekFrom::Start(base + (start * SECTOR) as u64))?;
            dev.write_all(&slot.buf[start * SECTOR..s * SECTOR])?;
        }
        slot.dirty.iter_mut().for_each(|d| *d = false);
        Ok(())
    }

    /// Liefert den Slot für Block `index` (lädt nichts – Sektoren werden bei Bedarf nachgelesen).
    fn slot(&mut self, index: u64) -> io::Result<usize> {
        self.tick += 1;
        if let Some(i) = self.slots.iter().position(|s| s.index == index) {
            self.slots[i].last_use = self.tick;
            return Ok(i);
        }
        let len = (self.size - index * BLOCK as u64).min(BLOCK as u64) as usize;
        let slot = Slot {
            index,
            buf: AlignedBuf::new(BLOCK),
            len,
            valid: vec![false; SECTORS_PER_BLOCK],
            dirty: vec![false; SECTORS_PER_BLOCK],
            last_use: self.tick,
        };
        if self.slots.len() < self.capacity {
            self.slots.push(slot);
            return Ok(self.slots.len() - 1);
        }
        // Am längsten unbenutzten Block verdrängen.
        let lru = (0..self.slots.len())
            .min_by_key(|&i| self.slots[i].last_use)
            .expect("nicht leer");
        Self::write_back(&mut self.dev, &mut self.slots[lru])?;
        self.slots[lru] = slot;
        Ok(lru)
    }

    /// Sektoren `[from, to)` eines Slots vom Gerät nachladen, soweit noch nicht gültig.
    fn ensure_valid(&mut self, i: usize, from: usize, to: usize) -> io::Result<()> {
        let mut s = from;
        while s < to {
            if self.slots[i].valid[s] {
                s += 1;
                continue;
            }
            let start = s;
            while s < to && !self.slots[i].valid[s] {
                s += 1;
            }
            let base = self.slots[i].index * BLOCK as u64;
            self.dev
                .seek(SeekFrom::Start(base + (start * SECTOR) as u64))?;
            let slot = &mut self.slots[i];
            self.dev
                .read_exact(&mut slot.buf[start * SECTOR..s * SECTOR])?;
            slot.valid[start..s].iter_mut().for_each(|v| *v = true);
        }
        Ok(())
    }
}

impl<D: Device> Read for BlockCache<D> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.size || out.is_empty() {
            return Ok(0);
        }
        let index = self.pos / BLOCK as u64;
        let off = (self.pos % BLOCK as u64) as usize;
        let i = self.slot(index)?;
        let n = out.len().min(self.slots[i].len - off);
        self.ensure_valid(i, off / SECTOR, (off + n).div_ceil(SECTOR))?;
        out[..n].copy_from_slice(&self.slots[i].buf[off..off + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl<D: Device> Write for BlockCache<D> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.pos >= self.size {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "Ende des Geräts erreicht",
            ));
        }
        let index = self.pos / BLOCK as u64;
        let off = (self.pos % BLOCK as u64) as usize;
        let i = self.slot(index)?;
        let n = data.len().min(self.slots[i].len - off);
        let (first, last) = (off / SECTOR, (off + n).div_ceil(SECTOR));
        // Nur angeschnittene Rand-Sektoren müssen vorher gelesen werden.
        if !off.is_multiple_of(SECTOR) {
            self.ensure_valid(i, first, first + 1)?;
        }
        if !(off + n).is_multiple_of(SECTOR) {
            self.ensure_valid(i, last - 1, last)?;
        }
        let slot = &mut self.slots[i];
        slot.buf[off..off + n].copy_from_slice(&data[..n]);
        slot.valid[first..last].iter_mut().for_each(|v| *v = true);
        slot.dirty[first..last].iter_mut().for_each(|d| *d = true);
        self.pos += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        // `fatfs` ruft flush häufig auf – echtes Schreiben erst über `flush_all`.
        Ok(())
    }
}

impl<D: Device> Seek for BlockCache<D> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new = match pos {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::End(d) => self.size as i128 + d as i128,
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
        };
        if new < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Position vor dem Anfang",
            ));
        }
        self.pos = new as u64;
        Ok(self.pos)
    }
}

/// Ausschnitt eines Geräts (eine Partition) als eigenständiger Datenstrom.
pub struct Partition<'a, S> {
    inner: &'a mut S,
    start: u64,
    len: u64,
    pos: u64,
}

impl<'a, S: Read + Write + Seek> Partition<'a, S> {
    pub fn new(inner: &'a mut S, start: u64, len: u64) -> Self {
        Self {
            inner,
            start,
            len,
            pos: 0,
        }
    }
}

impl<S: Read + Write + Seek> Read for Partition<'_, S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let max = (self.len.saturating_sub(self.pos)).min(buf.len() as u64) as usize;
        if max == 0 {
            return Ok(0);
        }
        self.inner.seek(SeekFrom::Start(self.start + self.pos))?;
        let n = self.inner.read(&mut buf[..max])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl<S: Read + Write + Seek> Write for Partition<'_, S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let max = (self.len.saturating_sub(self.pos)).min(buf.len() as u64) as usize;
        if max == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "Ende der Partition erreicht",
            ));
        }
        self.inner.seek(SeekFrom::Start(self.start + self.pos))?;
        let n = self.inner.write(&buf[..max])?;
        self.pos += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<S: Read + Write + Seek> Seek for Partition<'_, S> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new = match pos {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::End(d) => self.len as i128 + d as i128,
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
        };
        if new < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Position vor dem Anfang",
            ));
        }
        self.pos = new as u64;
        Ok(self.pos)
    }
}
