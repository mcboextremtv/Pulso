//! Biblioteca en SQLite con escaneo incremental (solo relee archivos modificados).
use crate::Result;
use lofty::{config::WriteOptions, prelude::*, probe::Probe, tag::Tag};
use rusqlite::{params, Connection};
use std::{
    collections::{BTreeMap, HashSet},
    ffi::OsString,
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

/// Escribe las etiquetas básicas en el archivo (crea la etiqueta si no tenía ninguna).
pub fn write_tags(path: &Path, title: &str, artist: &str, album: &str) -> Result<()> {
    let mut tagged = Probe::open(path)?.read()?;
    if tagged.primary_tag().is_none() {
        let tag_type = tagged.primary_tag_type();
        tagged.insert_tag(Tag::new(tag_type));
    }
    let tag = tagged.primary_tag_mut().ok_or("este formato no admite etiquetas")?;
    let (title, artist, album) = (title.trim(), artist.trim(), album.trim());
    if title.is_empty() { tag.remove_title() } else { tag.set_title(title.to_string()) }
    if artist.is_empty() { tag.remove_artist() } else { tag.set_artist(artist.to_string()) }
    if album.is_empty() { tag.remove_album() } else { tag.set_album(album.to_string()) }
    tagged.save_to_path(path, WriteOptions::default())?;
    Ok(())
}

/// Carpeta de la vista en árbol (aplanada, con sangría por `depth`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderNode {
    pub path: String,
    pub name: String,
    pub depth: usize,
    /// Canciones dentro de esta carpeta, incluidas sus subcarpetas.
    pub count: usize,
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
        self.search_in(query, "", limit)
    }

    /// Igual que `search`, limitado a una carpeta (y sus subcarpetas).
    /// `folder_prefix` debe terminar en el separador de rutas; vacío = sin límite.
    pub fn search_in(&self, query: &str, folder_prefix: &str, limit: usize) -> Result<Vec<Track>> {
        self.search_sorted(query, folder_prefix, "", false, false, limit)
    }

    /// Búsqueda con orden. `sort_col`: "title", "artist", "album", "duration" o "" (orden de
    /// carpeta). Con `group_by_artist` las pistas salen agrupadas por artista.
    pub fn search_sorted(
        &self,
        query: &str,
        folder_prefix: &str,
        sort_col: &str,
        desc: bool,
        group_by_artist: bool,
        limit: usize,
    ) -> Result<Vec<Track>> {
        // Lista cerrada: el nombre de columna nunca viene del usuario tal cual.
        let col = match sort_col {
            "title" => "title COLLATE NOCASE",
            "artist" => "artist COLLATE NOCASE",
            "album" => "album COLLATE NOCASE",
            "duration" => "duration_ms",
            _ => "path COLLATE NOCASE",
        };
        let dir = if desc { "DESC" } else { "ASC" };
        let order = if group_by_artist {
            format!("artist COLLATE NOCASE ASC, {col} {dir}, path COLLATE NOCASE")
        } else {
            format!("{col} {dir}, path COLLATE NOCASE")
        };
        let sql = format!(
            "SELECT id, path, title, artist, album, duration_ms FROM tracks
             WHERE (title LIKE ?1 OR artist LIKE ?1 OR album LIKE ?1)
               AND (?3 = '' OR substr(path, 1, length(?3)) = ?3)
             ORDER BY {order}
             LIMIT ?2"
        );
        let like = format!("%{}%", query.trim());
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![like, limit as i64, folder_prefix], |r| {
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

    /// Escribe título, artista y álbum en el archivo y actualiza la biblioteca.
    /// Un campo vacío se borra de la etiqueta (el título vacío vuelve al nombre del archivo).
    pub fn apply_tags(&self, id: i64, path: &Path, title: &str, artist: &str, album: &str) -> Result<()> {
        write_tags(path, title, artist, album)?;
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let title = if title.trim().is_empty() { stem } else { title.trim().to_string() };
        let mtime = fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.conn.execute(
            "UPDATE tracks SET title = ?2, artist = ?3, album = ?4, mtime = ?5 WHERE id = ?1",
            params![id, title, artist.trim(), album.trim(), mtime],
        )?;
        Ok(())
    }

    /// Árbol de carpetas de la biblioteca, aplanado y ordenado. Empieza debajo de la
    /// carpeta común a todas las canciones (no se muestran niveles vacíos de arriba).
    pub fn folder_tree(&self) -> Result<Vec<FolderNode>> {
        let mut stmt = self.conn.prepare("SELECT path FROM tracks")?;
        let paths: Vec<String> = stmt.query_map([], |r| r.get(0))?.filter_map(|r| r.ok()).collect();

        let mut direct: BTreeMap<PathBuf, usize> = BTreeMap::new();
        for p in &paths {
            if let Some(dir) = Path::new(p).parent() {
                *direct.entry(dir.to_path_buf()).or_default() += 1;
            }
        }
        let comps: Vec<Vec<OsString>> = direct
            .keys()
            .map(|d| d.components().map(|c| c.as_os_str().to_owned()).collect())
            .collect();
        let Some(first) = comps.first() else { return Ok(vec![]) };
        let mut common = first.clone();
        for c in &comps[1..] {
            let n = common.iter().zip(c).take_while(|(a, b)| a == b).count();
            common.truncate(n);
        }
        let base = common.len();

        let mut counts: BTreeMap<PathBuf, usize> = BTreeMap::new();
        for (dir, n) in &direct {
            for anc in dir.ancestors() {
                if anc.components().count() <= base {
                    break;
                }
                *counts.entry(anc.to_path_buf()).or_default() += n;
            }
        }
        let mut nodes: Vec<(Vec<String>, FolderNode)> = counts
            .into_iter()
            .map(|(p, count)| {
                let key: Vec<String> =
                    p.components().map(|c| c.as_os_str().to_string_lossy().to_lowercase()).collect();
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.to_string_lossy().into_owned());
                let depth = key.len() - base - 1;
                (key, FolderNode { path: p.to_string_lossy().into_owned(), name, depth, count })
            })
            .collect();
        nodes.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(nodes.into_iter().map(|(_, n)| n).collect())
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

    #[test]
    fn folder_tree_and_filter() {
        let lib = Library::open_in_memory().unwrap();
        // Rutas con el separador propio de cada sistema (en Windows, "\\").
        let native = |s: &str| {
            s.split('/')
                .filter(|x| !x.is_empty())
                .fold(PathBuf::from(std::path::MAIN_SEPARATOR_STR), |acc, x| acc.join(x))
                .to_string_lossy()
                .into_owned()
        };
        for (i, p) in ["/m/rock/a/1.mp3", "/m/rock/b/2.mp3", "/m/pop/3.mp3"].iter().enumerate() {
            lib.conn
                .execute(
                    "INSERT INTO tracks (path,title,artist,album,duration_ms,mtime) VALUES (?1,?2,'','',1000,0)",
                    params![native(p), format!("t{i}")],
                )
                .unwrap();
        }
        let tree = lib.folder_tree().unwrap();
        let view: Vec<(&str, usize, usize)> = tree.iter().map(|n| (n.name.as_str(), n.depth, n.count)).collect();
        assert_eq!(view, vec![("pop", 0, 1), ("rock", 0, 2), ("a", 1, 1), ("b", 1, 1)]);

        let sep = std::path::MAIN_SEPARATOR;
        let rock = tree.iter().find(|n| n.name == "rock").unwrap();
        let under_rock = lib.search_in("", &format!("{}{sep}", rock.path), 100).unwrap();
        assert_eq!(under_rock.len(), 2);
        assert_eq!(lib.search_in("t2", "", 100).unwrap().len(), 1);
    }

    #[test]
    fn sorts_groups_and_edits_tags() {
        let dir = std::env::temp_dir().join(format!("pulso-tags-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for n in ["b.wav", "a.wav", "c.wav"] {
            fs::write(dir.join(n), silent_wav(1)).unwrap();
        }
        let mut lib = Library::open_in_memory().unwrap();
        lib.scan(&dir, |_| {}).unwrap();

        let titles = |v: Vec<Track>| v.into_iter().map(|t| t.title).collect::<Vec<_>>();
        assert_eq!(titles(lib.search_sorted("", "", "title", false, false, 10).unwrap()), ["a", "b", "c"]);
        assert_eq!(titles(lib.search_sorted("", "", "title", true, false, 10).unwrap()), ["c", "b", "a"]);

        let a = lib.search("a", 10).unwrap().into_iter().find(|t| t.title == "a").unwrap();
        lib.apply_tags(a.id, Path::new(&a.path), "Mi canción", "Zeta", "Mi álbum").unwrap();
        let again = lib.search("Mi canción", 10).unwrap();
        assert_eq!((again[0].artist.as_str(), again[0].album.as_str()), ("Zeta", "Mi álbum"));
        let from_file = read_tags(Path::new(&a.path)).unwrap();
        assert_eq!((from_file.title.as_str(), from_file.artist.as_str()), ("Mi canción", "Zeta"));

        // Agrupado por artista: los sin artista primero, "Zeta" al final.
        let g = lib.search_sorted("", "", "", false, true, 10).unwrap();
        assert_eq!(g.last().unwrap().artist, "Zeta");
        // Tras editar, un nuevo escaneo no debe tratarlo como cambiado.
        assert_eq!(lib.scan(&dir, |_| {}).unwrap().unchanged, 3);
        let _ = fs::remove_dir_all(&dir);
    }
}
