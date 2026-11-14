//! Ajustes del usuario en un archivo de texto sencillo (clave=valor), en la carpeta de datos.
use crate::{paths, player::DEFAULT_BUFFER_FRAMES};
use std::{fs, io};

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub sidebar_w: f32,
    pub lyrics_w: f32,
    pub title_w: f32,
    pub artist_w: f32,
    /// "", "title", "artist", "album" o "duration" ("" = orden de carpeta)
    pub sort_col: String,
    pub sort_desc: bool,
    pub group_artist: bool,
    pub audio_buffer: u32,
    /// "folders" o "lists"
    pub sidebar_mode: String,
    /// Clave de aplicación de AcoustID (la pone cada usuario)
    pub acoustid_key: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            sidebar_w: 220.0,
            lyrics_w: 340.0,
            title_w: 260.0,
            artist_w: 160.0,
            sort_col: String::new(),
            sort_desc: false,
            group_artist: false,
            audio_buffer: DEFAULT_BUFFER_FRAMES,
            sidebar_mode: "folders".into(),
            acoustid_key: String::new(),
        }
    }
}

impl Settings {
    pub fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            let num = || v.parse::<f32>().ok().filter(|n| n.is_finite());
            match k {
                "sidebar_w" => s.sidebar_w = num().unwrap_or(s.sidebar_w).clamp(140.0, 480.0),
                "lyrics_w" => s.lyrics_w = num().unwrap_or(s.lyrics_w).clamp(220.0, 640.0),
                "title_w" => s.title_w = num().unwrap_or(s.title_w).clamp(100.0, 700.0),
                "artist_w" => s.artist_w = num().unwrap_or(s.artist_w).clamp(60.0, 500.0),
                "sort_col" if ["", "title", "artist", "album", "duration"].contains(&v) => {
                    s.sort_col = v.to_string()
                }
                "sort_desc" => s.sort_desc = v == "true",
                "group_artist" => s.group_artist = v == "true",
                "sidebar_mode" if v == "folders" || v == "lists" => s.sidebar_mode = v.to_string(),
                "acoustid_key" if v.len() <= 64 && v.chars().all(|c| c.is_ascii_alphanumeric()) => {
                    s.acoustid_key = v.to_string()
                }
                "audio_buffer" => {
                    s.audio_buffer = v.parse().ok().filter(|n| (1024..=65536).contains(n)).unwrap_or(s.audio_buffer)
                }
                _ => {}
            }
        }
        s
    }

    pub fn to_text(&self) -> String {
        format!(
            "sidebar_w={}\nlyrics_w={}\ntitle_w={}\nartist_w={}\nsort_col={}\nsort_desc={}\ngroup_artist={}\naudio_buffer={}\nsidebar_mode={}\nacoustid_key={}\n",
            self.sidebar_w.round(),
            self.lyrics_w.round(),
            self.title_w.round(),
            self.artist_w.round(),
            self.sort_col,
            self.sort_desc,
            self.group_artist,
            self.audio_buffer,
            self.sidebar_mode,
            self.acoustid_key
        )
    }

    pub fn load() -> Self {
        fs::read_to_string(paths::data_dir().join("settings.txt"))
            .map(|t| Self::parse(&t))
            .unwrap_or_default()
    }

    pub fn save(&self) -> io::Result<()> {
        fs::write(paths::data_dir().join("settings.txt"), self.to_text())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_sanitize() {
        let s = Settings { sidebar_w: 300.0, sidebar_mode: "lists".into(), acoustid_key: "AbC123".into(), sort_col: "artist".into(), sort_desc: true, group_artist: true, audio_buffer: 16384, ..Default::default() };
        assert_eq!(Settings::parse(&s.to_text()), s);
        let bad = Settings::parse("sidebar_w=99999\nsort_col=drop table\naudio_buffer=3\nbasura");
        assert_eq!(bad.sidebar_w, 480.0);
        assert_eq!(bad.sort_col, "");
        assert_eq!(bad.audio_buffer, DEFAULT_BUFFER_FRAMES);
    }
}
