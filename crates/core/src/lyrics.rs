//! Letras desde LRCLIB (https://lrclib.net): API abierta, con letras sincronizadas (.lrc).
use crate::Result;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

const UA: &str = "Pulso/0.1 (reproductor de musica libre)";

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
}

fn from_json(v: &serde_json::Value) -> Lyrics {
    let s = |k: &str| v[k].as_str().map(str::to_owned).filter(|t| !t.trim().is_empty());
    Lyrics {
        synced_raw: s("syncedLyrics"),
        plain: s("plainLyrics"),
        instrumental: v["instrumental"].as_bool().unwrap_or(false),
    }
}

/// Busca la letra. `Ok(None)` = LRCLIB no la tiene. Solo hace red cuando se llama.
pub fn fetch(artist: &str, title: &str, album: &str, duration_secs: Option<u64>) -> Result<Option<Lyrics>> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(8))
        .user_agent(UA)
        .build();

    let mut req = agent
        .get("https://lrclib.net/api/get")
        .query("track_name", title)
        .query("artist_name", artist);
    if !album.is_empty() {
        req = req.query("album_name", album);
    }
    if let Some(d) = duration_secs.filter(|d| *d > 0) {
        req = req.query("duration", &d.to_string());
    }
    match req.call() {
        Ok(r) => return Ok(Some(from_json(&serde_json::from_str(&r.into_string()?)?))),
        Err(ureq::Error::Status(404, _)) => {}
        Err(e) => return Err(e.into()),
    }

    // Respaldo: búsqueda aproximada.
    let r = agent
        .get("https://lrclib.net/api/search")
        .query("track_name", title)
        .query("artist_name", artist)
        .call();
    match r {
        Ok(r) => {
            let v: serde_json::Value = serde_json::from_str(&r.into_string()?)?;
            let best = v
                .as_array()
                .and_then(|a| a.iter().find(|x| x["syncedLyrics"].is_string()).or_else(|| a.first()));
            Ok(best.map(from_json))
        }
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

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

pub fn sidecar_path(track: &Path) -> PathBuf {
    track.with_extension("lrc")
}

pub fn load_sidecar(track: &Path) -> Option<String> {
    fs::read_to_string(sidecar_path(track)).ok().filter(|s| !s.trim().is_empty())
}

pub fn save_sidecar(track: &Path, synced: &str) -> std::io::Result<()> {
    fs::write(sidecar_path(track), synced)
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
}
