//! Letras: LRCLIB (https://lrclib.net, API abierta con letras sincronizadas),
//! archivos locales editables por el usuario y búsqueda en el navegador.
use crate::Result;
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

const UA: &str = "Pulso/0.1 (reproductor de musica libre)";
const GET_URL: &str = "https://lrclib.net/api/get";
const SEARCH_URL: &str = "https://lrclib.net/api/search";

#[derive(Debug, Default, Clone)]
pub struct Lyrics {
    pub synced_raw: Option<String>,
    pub plain: Option<String>,
    pub instrumental: bool,
}

impl Lyrics {
    pub fn synced_lines(&self) -> Vec<(Duration, String)> {
        self.synced_raw.as_deref().map(parse_lrc).unwrap_or_default()
    }
    fn has_content(&self) -> bool {
        self.synced_raw.is_some() || self.plain.is_some() || self.instrumental
    }
    /// Texto para guardar o editar: sincronizada si existe, si no la plana.
    pub fn best_text(&self) -> Option<String> {
        self.synced_raw.clone().or_else(|| self.plain.clone())
    }
}

fn from_json(v: &serde_json::Value) -> Lyrics {
    let s = |k: &str| v[k].as_str().map(str::to_owned).filter(|t| !t.trim().is_empty());
    Lyrics {
        synced_raw: s("syncedLyrics"),
        plain: s("plainLyrics"),
        instrumental: v["instrumental"].as_bool().unwrap_or(false),
    }
}

/// De una lista de resultados, prefiere la que trae letra sincronizada.
fn best(v: &serde_json::Value) -> Option<Lyrics> {
    let items: Vec<Lyrics> = v.as_array()?.iter().map(from_json).filter(Lyrics::has_content).collect();
    items
        .iter()
        .find(|l| l.synced_raw.is_some())
        .or_else(|| items.iter().find(|l| l.plain.is_some()))
        .or_else(|| items.first())
        .cloned()
}

/// GET con parámetros (los vacíos se omiten). `Ok(None)` = sin datos (400/404);
/// `Err` = problema de red o del servidor, con un mensaje legible.
fn get_json(agent: &ureq::Agent, url: &str, params: &[(&str, &str)]) -> Result<Option<serde_json::Value>> {
    let mut req = agent.get(url);
    for (k, v) in params {
        if !v.is_empty() {
            req = req.query(k, v);
        }
    }
    match req.call() {
        Ok(r) => Ok(Some(serde_json::from_str(&r.into_string()?)?)),
        Err(ureq::Error::Status(400 | 404, _)) => Ok(None),
        Err(ureq::Error::Status(code, _)) => {
            Err(format!("el servicio de letras respondió con el error {code}").into())
        }
        Err(ureq::Error::Transport(t)) => {
            Err(format!("no se pudo conectar con el servicio de letras ({t})").into())
        }
    }
}

/// Busca la letra en LRCLIB. `Ok(None)` = no la tiene. Solo hace red cuando se llama.
/// Orden: coincidencia exacta → búsqueda por campos → búsqueda libre.
pub fn fetch(artist: &str, title: &str, album: &str, duration_secs: Option<u64>) -> Result<Option<Lyrics>> {
    let (artist, title) = (artist.trim(), title.trim());
    if title.is_empty() {
        return Ok(None);
    }
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(8))
        .user_agent(UA)
        .build();

    if !artist.is_empty() {
        let dur = duration_secs.filter(|d| *d > 0).map(|d| d.to_string()).unwrap_or_default();
        let params = [("track_name", title), ("artist_name", artist), ("album_name", album), ("duration", dur.as_str())];
        if let Some(v) = get_json(&agent, GET_URL, &params)? {
            let l = from_json(&v);
            if l.has_content() {
                return Ok(Some(l));
            }
        }
    }
    if let Some(v) = get_json(&agent, SEARCH_URL, &[("track_name", title), ("artist_name", artist)])? {
        if let Some(l) = best(&v) {
            return Ok(Some(l));
        }
    }
    let q = format!("{artist} {title}");
    if let Some(v) = get_json(&agent, SEARCH_URL, &[("q", q.trim())])? {
        if let Some(l) = best(&v) {
            return Ok(Some(l));
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// Letras locales (el usuario manda: puede pegar, corregir o borrar)
//   cancion.lrc          → sincronizada
//   cancion.lyrics.txt   → sin sincronizar
// ---------------------------------------------------------------------------

pub fn sidecar_path(track: &Path) -> PathBuf {
    track.with_extension("lrc")
}

fn plain_path(track: &Path) -> PathBuf {
    let mut name = track.file_stem().map(|s| s.to_os_string()).unwrap_or_default();
    name.push(".lyrics.txt");
    track.with_file_name(name)
}

fn read_nonempty(p: &Path) -> Option<String> {
    fs::read_to_string(p).ok().filter(|s| !s.trim().is_empty())
}

pub fn load_sidecar(track: &Path) -> Option<String> {
    read_nonempty(&sidecar_path(track))
}

pub fn save_sidecar(track: &Path, synced: &str) -> io::Result<()> {
    fs::write(sidecar_path(track), synced)
}

/// Letra guardada junto a la canción (.lrc primero, luego .lyrics.txt).
pub fn load_local(track: &Path) -> Option<String> {
    read_nonempty(&sidecar_path(track)).or_else(|| read_nonempty(&plain_path(track)))
}

/// Guarda el texto: como .lrc si trae marcas de tiempo, si no como .lyrics.txt.
/// Texto vacío = elimina la letra.
pub fn save_local(track: &Path, text: &str) -> io::Result<()> {
    let (lrc, txt) = (sidecar_path(track), plain_path(track));
    let t = text.trim();
    let remove = |p: &Path| match fs::remove_file(p) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    };
    if t.is_empty() {
        remove(&lrc)?;
        return remove(&txt);
    }
    if parse_lrc(t).is_empty() {
        fs::write(&txt, t)?;
        remove(&lrc)
    } else {
        fs::write(&lrc, t)?;
        remove(&txt)
    }
}

/// Para archivos sin etiquetas: deduce (artista, título) del nombre.
/// "017 - Michael Jackson - Blame it" → ("Michael Jackson", "Blame it").
/// Si no está claro, devuelve artista vacío y el nombre limpio como título.
pub fn guess_artist_title(stem: &str) -> (String, String) {
    let s = stem.trim();
    let digits = s.chars().take_while(|c| c.is_ascii_digit()).count();
    let s = if digits > 0 && s[digits..].starts_with([' ', '.', '-', '_', ')']) {
        s[digits..].trim_start_matches([' ', '.', '-', '_', ')'])
    } else {
        s
    };
    let parts: Vec<&str> = s.split(" - ").collect();
    if parts.len() == 2 {
        (parts[0].trim().to_string(), parts[1].trim().to_string())
    } else {
        (String::new(), s.trim().to_string())
    }
}

pub fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Dirección de búsqueda web para que el usuario copie la letra a mano.
pub fn web_search_url(artist: &str, title: &str) -> String {
    let q = format!("{artist} {title} letra");
    format!("https://www.google.com/search?q={}", url_encode(q.trim()))
}

// ---------------------------------------------------------------------------
// Formato LRC
// ---------------------------------------------------------------------------

/// Convierte texto .lrc en líneas ordenadas por tiempo. Admite varias marcas por línea.
pub fn parse_lrc(text: &str) -> Vec<(Duration, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut rest = line.trim();
        let mut times = Vec::new();
        while let Some(stripped) = rest.strip_prefix('[') {
            let Some(end) = stripped.find(']') else { break };
            let tag = &stripped[..end];
            rest = stripped[end + 1..].trim_start();
            if let Some(t) = parse_timestamp(tag) {
                times.push(t);
            } else if times.is_empty() {
                break; // etiqueta de metadatos ([ar:...], [ti:...])
            }
        }
        for t in times {
            out.push((t, rest.to_string()));
        }
    }
    out.sort_by_key(|(t, _)| *t);
    out
}

fn parse_timestamp(tag: &str) -> Option<Duration> {
    let (m, s) = tag.split_once(':')?;
    let m: u64 = m.trim().parse().ok()?;
    let s: f64 = s.trim().parse().ok()?;
    if !s.is_finite() || s < 0.0 {
        return None;
    }
    Some(Duration::from_secs(m * 60) + Duration::from_secs_f64(s))
}

/// Índice de la línea activa en `pos` (None si aún no empieza la primera).
pub fn current_line(lines: &[(Duration, String)], pos: Duration) -> Option<usize> {
    lines.partition_point(|(t, _)| *t <= pos).checked_sub(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lrc() {
        let l = parse_lrc("[ar:Alguien]\n[00:12.50] Hola\n[00:05.00][01:00.25] Coro\n\n[00:20.00]");
        assert_eq!(l.len(), 4);
        assert_eq!(l[0], (Duration::from_secs(5), "Coro".into()));
        assert_eq!(l[1], (Duration::from_millis(12_500), "Hola".into()));
        assert_eq!(l[3].0, Duration::from_millis(60_250));
    }

    #[test]
    fn finds_current_line() {
        let l = parse_lrc("[00:05.00] a\n[00:10.00] b");
        assert_eq!(current_line(&l, Duration::from_secs(1)), None);
        assert_eq!(current_line(&l, Duration::from_secs(5)), Some(0));
        assert_eq!(current_line(&l, Duration::from_secs(99)), Some(1));
    }

    #[test]
    fn guesses_names() {
        let g = guess_artist_title;
        assert_eq!(g("017 - Michael Jackson - Blame it on the boogie"), ("Michael Jackson".into(), "Blame it on the boogie".into()));
        assert_eq!(g("02- Como Quisiera"), (String::new(), "Como Quisiera".into()));
        assert_eq!(g("02 Celia"), (String::new(), "Celia".into()));
        assert_eq!(g("2Pac - Changes"), ("2Pac".into(), "Changes".into()));
        assert_eq!(g("003 - LO POCO - GALERIA - 2000").0, "");
    }

    #[test]
    fn encodes_urls() {
        assert_eq!(url_encode("Celia Cruz & año"), "Celia+Cruz+%26+a%C3%B1o");
        assert!(web_search_url("A", "B c").ends_with("q=A+B+c+letra"));
    }

    #[test]
    fn local_lyrics_roundtrip() {
        let dir = std::env::temp_dir().join(format!("pulso-lyr-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let track = dir.join("tema.mp3");

        assert!(load_local(&track).is_none());
        save_local(&track, "Línea uno\nLínea dos").unwrap();
        assert!(!sidecar_path(&track).exists() && plain_path(&track).exists());
        assert_eq!(load_local(&track).unwrap(), "Línea uno\nLínea dos");

        save_local(&track, "[00:01.00] Hola").unwrap(); // al sincronizar, reemplaza la plana
        assert!(sidecar_path(&track).exists() && !plain_path(&track).exists());

        save_local(&track, "  ").unwrap(); // vacío = eliminar
        assert!(load_local(&track).is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
