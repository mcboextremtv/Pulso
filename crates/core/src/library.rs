//! Biblioteca en SQLite con escaneo incremental (solo relee archivos modificados).
use crate::Result;
use lofty::{prelude::*, probe::Probe};
use rusqlite::{params, Connection};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

pub const EXTENSIONS: &[&str] = &["mp3", "flac", "ogg", "m4a", "aac", "wav", "mp4"];

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub id: i64,
    pub path: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u64,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ScanStats {
    pub added_or_updated: usize,
    pub unchanged: usize,
    pub removed: usize,
    pub failed: usize,
}

/// Lee las etiquetas de un archivo (sin tocar la base de datos).
pub fn read_tags(path: &Path) -> Result<Track> {
    let tagged = Probe::open(path)?.read()?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let get = |v: Option<std::borrow::Cow<'_, str>>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    Ok(Track {
        id: 0,
        path: path.to_string_lossy().into_owned(),
        title: tag.and_then(|t| get(t.title())).unwrap_or(stem),
        artist: tag.and_then(|t| get(t.artist())).unwrap_or_default(),
        album: tag.and_then(|t| get(t.album())).unwrap_or_default(),
        duration_ms: tagged.properties().duration().as_millis() as u64,
    })
}

pub struct Library {
    conn: Connection,
}

impl Library {
    pub fn open(path: &Path) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS tracks (
                 id          INTEGER PRIMARY KEY,
                 path        TEXT UNIQUE NOT NULL,
                 title       TEXT NOT NULL,
                 artist      TEXT NOT NULL,
                 album       TEXT NOT NULL,
                 duration_ms INTEGER NOT NULL,
                 mtime       INTEGER NOT NULL
             );",
        )?;
        Ok(Self { conn })
    }

    /// Escanea `root` recursivamente. `progress` recibe el nº de archivos revisados.
    pub fn scan(&mut self, root: &Path, mut progress: impl FnMut(usize)) -> Result<ScanStats> {
        let files = collect_audio_files(root);
        let mut stats = ScanStats::default();
        let mut seen: HashSet<String> = HashSet::with_capacity(files.len());

        let tx = self.conn.transaction()?;
        {
            let mut get_mtime = tx.prepare("SELECT mtime FROM tracks WHERE path = ?1")?;
            let mut upsert = tx.prepare(
                "INSERT INTO tracks (path, title, artist, album, duration_ms, mtime)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(path) DO UPDATE SET title=?2, artist=?3, album=?4,
                     duration_ms=?5, mtime=?6",
            )?;
            for (i, file) in files.iter().enumerate() {
                let path_s = file.to_string_lossy().into_owned();
                let mtime = fs::metadata(file)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                seen.insert(path_s.clone());

                let known: Option<i64> = get_mtime.query_row([&path_s], |r| r.get(0)).ok();
                if known == Some(mtime) {
                    stats.unchanged += 1;
                } else {
                    match read_tags(file) {
                        Ok(t) => {
                            upsert.execute(params![
                                path_s, t.title, t.artist, t.album, t.duration_ms as i64, mtime
                            ])?;
                            stats.added_or_updated += 1;
                        }
                        Err(_) => stats.failed += 1,
                    }
                }
                progress(i + 1);
            }
        }

        // Elimina lo que ya no existe dentro de esta carpeta.
        let root_s = root.to_string_lossy().into_owned();
        let stale: Vec<String> = {
            let mut q = tx.prepare("SELECT path FROM tracks WHERE path LIKE ?1 || '%'")?;
            let rows = q.query_map([&root_s], |r| r.get::<_, String>(0))?;
            rows.filter_map(|r| r.ok()).filter(|p| !seen.contains(p)).collect()
        };
        for p in &stale {
            tx.execute("DELETE FROM tracks WHERE path = ?1", [p])?;
            stats.removed += 1;
        }
        tx.commit()?;
        Ok(stats)
    }

    /// Busca en título, artista y álbum. Cadena vacía = toda la biblioteca.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Track>> {
        let like = format!("%{}%", query.trim());
        let mut stmt = self.conn.prepare(
            "SELECT id, path, title, artist, album, duration_ms FROM tracks
             WHERE title LIKE ?1 OR artist LIKE ?1 OR album LIKE ?1
             ORDER BY artist COLLATE NOCASE, album COLLATE NOCASE, title COLLATE NOCASE
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![like, limit as i64], |r| {
            Ok(Track {
                id: r.get(0)?,
                path: r.get(1)?,
                title: r.get(2)?,
                artist: r.get(3)?,
                album: r.get(4)?,
                duration_ms: r.get::<_, i64>(5)? as u64,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn count(&self) -> Result<usize> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get::<_, i64>(0))? as usize)
    }
}

fn collect_audio_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            let p = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                stack.push(p);
            } else if p
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
                .unwrap_or(false)
            {
                out.push(p);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WAV mono de 8 kHz, 8 bits, `secs` segundos de silencio.
    fn silent_wav(secs: u32) -> Vec<u8> {
        let data_len = 8000 * secs;
        let mut v = Vec::new();
        v.extend(b"RIFF");
        v.extend((36 + data_len).to_le_bytes());
        v.extend(b"WAVEfmt ");
        v.extend(16u32.to_le_bytes());
        v.extend(1u16.to_le_bytes()); // PCM
        v.extend(1u16.to_le_bytes()); // mono
        v.extend(8000u32.to_le_bytes());
        v.extend(8000u32.to_le_bytes());
        v.extend(1u16.to_le_bytes());
        v.extend(8u16.to_le_bytes());
        v.extend(b"data");
        v.extend(data_len.to_le_bytes());
        v.extend(std::iter::repeat(128u8).take(data_len as usize));
        v
    }

    #[test]
    fn scan_is_incremental() {
        let dir = std::env::temp_dir().join(format!("pulso-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub/tono.wav"), silent_wav(2)).unwrap();
        fs::write(dir.join("notas.txt"), b"no es audio").unwrap();

        let mut lib = Library::open_in_memory().unwrap();
        let s1 = lib.scan(&dir, |_| {}).unwrap();
        assert_eq!((s1.added_or_updated, s1.unchanged, s1.removed), (1, 0, 0));

        let tracks = lib.search("", 100).unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].title, "tono");
        assert!((1900..=2100).contains(&tracks[0].duration_ms), "{}", tracks[0].duration_ms);

        let s2 = lib.scan(&dir, |_| {}).unwrap();
        assert_eq!((s2.added_or_updated, s2.unchanged), (0, 1));
        assert_eq!(lib.search("ton", 10).unwrap().len(), 1);
        assert_eq!(lib.search("zzz", 10).unwrap().len(), 0);

        fs::remove_file(dir.join("sub/tono.wav")).unwrap();
        let s3 = lib.scan(&dir, |_| {}).unwrap();
        assert_eq!(s3.removed, 1);
        assert_eq!(lib.count().unwrap(), 0);
        let _ = fs::remove_dir_all(&dir);
    }
}
