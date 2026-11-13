//! Prueba rápida sin interfaz:  cargo run -p pulso-core --example play -- cancion.mp3
use pulso_core::{library, lyrics, player::{PlayState, Player}};
use std::{path::PathBuf, time::Duration};

fn main() {
    let Some(arg) = std::env::args().nth(1) else {
        eprintln!("uso: play <archivo de audio>");
        return;
    };
    let path = PathBuf::from(arg);
    let tags = library::read_tags(&path).ok();
    if let Some(t) = &tags {
        println!("{} — {} [{}]", t.artist, t.title, t.album);
    }

    let player = Player::new();
    player.play(&path);

    let synced = tags
        .as_ref()
        .and_then(|t| {
            lyrics::load_sidecar(&path).or_else(|| {
                let l = lyrics::fetch(&t.artist, &t.title, &t.album, Some(t.duration_ms / 1000)).ok()??;
                let raw = l.synced_raw?;
                let _ = lyrics::save_sidecar(&path, &raw);
                Some(raw)
            })
        })
        .map(|raw| lyrics::parse_lrc(&raw))
        .unwrap_or_default();
    println!("letra sincronizada: {} líneas", synced.len());

    let mut last = None;
    loop {
        std::thread::sleep(Duration::from_millis(200));
        let s = player.status();
        if let Some(e) = &s.error {
            eprintln!("{e}");
            return;
        }
        let cur = lyrics::current_line(&synced, s.position);
        if cur != last {
            if let Some(i) = cur {
                println!("[{:>5.1}s] {}", s.position.as_secs_f32(), synced[i].1);
            }
            last = cur;
        }
        if s.state == PlayState::Stopped {
            return;
        }
    }
}
