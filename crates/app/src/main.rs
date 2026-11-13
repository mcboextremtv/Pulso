#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

slint::include_modules!();

use pulso_core::{
    library::{Library, Track},
    lyrics, paths,
    player::{PlayState, Player},
};
use slint::{ComponentHandle, ModelRc, VecModel, Weak};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

#[derive(Default)]
struct State {
    view: Vec<Track>,
    query: String,
    current: Option<i64>,
    current_dur: Option<Duration>,
    synced: Vec<(Duration, String)>,
    /// -2 = hay que redibujar, -1 = antes de la primera línea, n = línea activa
    last_lyric: i64,
}

type Shared = Arc<Mutex<State>>;
type PlayerRef = Arc<Player>;

fn fmt_time(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

fn model<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(v))
}

fn lyric_window(lines: &[(Duration, String)], cur: i64) -> Vec<LyricLine> {
    let (from, to) = if cur < 0 {
        (0, lines.len().min(9))
    } else {
        let c = cur as usize;
        (c.saturating_sub(3), (c + 6).min(lines.len()))
    };
    (from..to)
        .map(|i| LyricLine {
            text: lines[i].1.clone().into(),
            current: i as i64 == cur,
        })
        .collect()
}

/// Recarga la lista visible desde SQLite en segundo plano.
fn refresh_view(ui: Weak<MainWindow>, st: Shared) {
    thread::spawn(move || {
        let query = st.lock().unwrap().query.clone();
        let tracks = Library::open(&paths::db_path())
            .and_then(|lib| lib.search(&query, 100_000))
            .unwrap_or_default();
        let st2 = st.clone();
        let _ = ui.upgrade_in_event_loop(move |ui| {
            let items: Vec<TrackItem> = tracks
                .iter()
                .map(|t| TrackItem {
                    id: t.id as i32,
                    title: t.title.clone().into(),
                    artist: t.artist.clone().into(),
                    album: t.album.clone().into(),
                    duration: fmt_time(Duration::from_millis(t.duration_ms)).into(),
                })
                .collect();
            let n = items.len();
            st2.lock().unwrap().view = tracks;
            ui.set_tracks(model(items));
            if n == 0 && ui.get_status_text().is_empty() {
                ui.set_status_text("Biblioteca vacía: usa «Agregar carpeta».".into());
            }
        });
    });
}

fn scan_folder(ui: Weak<MainWindow>, st: Shared, root: PathBuf) {
    thread::spawn(move || {
        let w = ui.clone();
        let result = Library::open(&paths::db_path()).and_then(|mut lib| {
            lib.scan(&root, |n| {
                if n % 25 == 0 {
                    let _ = w.upgrade_in_event_loop(move |ui| {
                        ui.set_status_text(format!("Escaneando… {n} archivos").into())
                    });
                }
            })
        });
        let msg = match result {
            Ok(s) => format!(
                "Listo: {} nuevas o actualizadas, {} sin cambios, {} eliminadas, {} con error",
                s.added_or_updated, s.unchanged, s.removed, s.failed
            ),
            Err(e) => format!("Error al escanear: {e}"),
        };
        let _ = ui.upgrade_in_event_loop(move |ui| ui.set_status_text(msg.into()));
        refresh_view(ui, st);
    });
}

struct LyricsResult {
    synced: Vec<(Duration, String)>,
    plain: Vec<String>,
    status: String,
}

fn load_lyrics(t: &Track) -> LyricsResult {
    let path = PathBuf::from(&t.path);
    if let Some(raw) = lyrics::load_sidecar(&path) {
        let synced = lyrics::parse_lrc(&raw);
        if !synced.is_empty() {
            return LyricsResult { synced, plain: vec![], status: String::new() };
        }
    }
    match lyrics::fetch(&t.artist, &t.title, &t.album, Some(t.duration_ms / 1000)) {
        Ok(Some(l)) if l.instrumental => LyricsResult { synced: vec![], plain: vec![], status: "Instrumental".into() },
        Ok(Some(l)) => {
            if let Some(raw) = &l.synced_raw {
                let _ = lyrics::save_sidecar(&path, raw);
                return LyricsResult { synced: lyrics::parse_lrc(raw), plain: vec![], status: String::new() };
            }
            match l.plain {
                Some(p) => LyricsResult {
                    synced: vec![],
                    plain: p.lines().map(str::to_owned).collect(),
                    status: "Letra sin sincronizar".into(),
                },
                None => LyricsResult { synced: vec![], plain: vec![], status: "Sin letra disponible".into() },
            }
        }
        Ok(None) => LyricsResult { synced: vec![], plain: vec![], status: "Sin letra disponible".into() },
        Err(_) => LyricsResult { synced: vec![], plain: vec![], status: "No se pudo buscar la letra (¿sin conexión?)".into() },
    }
}

fn play_at(ui: &MainWindow, st: &Shared, player: &PlayerRef, idx: usize) {
    let track = {
        let g = st.lock().unwrap();
        match g.view.get(idx) {
            Some(t) => t.clone(),
            None => return,
        }
    };
    player.play(&track.path);
    {
        let mut g = st.lock().unwrap();
        g.current = Some(track.id);
        g.current_dur = Some(Duration::from_millis(track.duration_ms));
        g.synced.clear();
        g.last_lyric = -2;
    }
    ui.set_current_id(track.id as i32);
    ui.set_now_title(track.title.clone().into());
    ui.set_now_artist(track.artist.clone().into());
    ui.set_duration_text(fmt_time(Duration::from_millis(track.duration_ms)).into());
    ui.set_position_text("0:00".into());
    ui.set_progress(0.0);
    ui.set_playing(true);
    ui.set_status_text("".into());
    ui.set_lyrics(model(vec![]));
    ui.set_lyrics_status("Buscando letra…".into());

    let st = st.clone();
    let weak = ui.as_weak();
    thread::spawn(move || {
        let res = load_lyrics(&track);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let mut g = st.lock().unwrap();
            if g.current != Some(track.id) {
                return; // el usuario ya cambió de canción
            }
            ui.set_lyrics_status(res.status.into());
            if res.synced.is_empty() {
                ui.set_lyrics(model(
                    res.plain.into_iter().map(|t| LyricLine { text: t.into(), current: false }).collect(),
                ));
            } else {
                g.synced = res.synced;
                g.last_lyric = -2; // el temporizador la dibuja en el siguiente ciclo
            }
        });
    });
}

fn step(ui: &MainWindow, st: &Shared, player: &PlayerRef, delta: i64) {
    let target = {
        let g = st.lock().unwrap();
        g.view
            .iter()
            .position(|t| Some(t.id) == g.current)
            .map(|i| i as i64 + delta)
            .filter(|i| *i >= 0 && (*i as usize) < g.view.len())
    };
    if let Some(i) = target {
        play_at(ui, st, player, i as usize);
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn pick_folder() -> Option<PathBuf> {
    rfd::FileDialog::new().set_title("Elige tu carpeta de música").pick_folder()
}

#[cfg(not(any(windows, target_os = "macos")))]
fn pick_folder() -> Option<PathBuf> {
    std::env::var_os("PULSO_MUSIC_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Music")))
}

fn main() {
    let t0 = Instant::now();
    let ui = MainWindow::new().expect("no se pudo crear la ventana");
    let player: PlayerRef = Arc::new(Player::new());
    let st: Shared = Arc::new(Mutex::new(State { last_lyric: -2, ..Default::default() }));

    player.set_volume(0.8);

    ui.on_open_folder({
        let (w, st) = (ui.as_weak(), st.clone());
        move || {
            if let Some(dir) = pick_folder() {
                scan_folder(w.clone(), st.clone(), dir);
            }
        }
    });
    ui.on_search({
        let (w, st) = (ui.as_weak(), st.clone());
        move |q| {
            st.lock().unwrap().query = q.to_string();
            refresh_view(w.clone(), st.clone());
        }
    });
    ui.on_play_track({
        let (w, st, p) = (ui.as_weak(), st.clone(), player.clone());
        move |i| {
            if let Some(ui) = w.upgrade() {
                play_at(&ui, &st, &p, i as usize);
            }
        }
    });
    ui.on_toggle_play({
        let (w, st, p) = (ui.as_weak(), st.clone(), player.clone());
        move || {
            let Some(ui) = w.upgrade() else { return };
            match p.status().state {
                PlayState::Playing => {
                    p.pause();
                    ui.set_playing(false);
                }
                PlayState::Paused => {
                    p.resume();
                    ui.set_playing(true);
                }
                PlayState::Stopped => {
                    let idx = {
                        let g = st.lock().unwrap();
                        g.view.iter().position(|t| Some(t.id) == g.current)
                    };
                    if let Some(i) = idx {
                        play_at(&ui, &st, &p, i);
                    }
                }
            }
        }
    });
    ui.on_next({
        let (w, st, p) = (ui.as_weak(), st.clone(), player.clone());
        move || {
            if let Some(ui) = w.upgrade() {
                step(&ui, &st, &p, 1);
            }
        }
    });
    ui.on_prev({
        let (w, st, p) = (ui.as_weak(), st.clone(), player.clone());
        move || {
            if let Some(ui) = w.upgrade() {
                step(&ui, &st, &p, -1);
            }
        }
    });
    ui.on_seek({
        let (st, p) = (st.clone(), player.clone());
        move |f| {
            let dur = p.status().duration.or(st.lock().unwrap().current_dur);
            if let Some(d) = dur {
                p.seek(d.mul_f32(f.clamp(0.0, 1.0)));
            }
        }
    });
    ui.on_volume_changed({
        let p = player.clone();
        move |v| p.set_volume(v)
    });

    // Un solo temporizador de 250 ms: posición, barra de progreso, letra y paso a la siguiente pista.
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(250), {
        let (w, st, p) = (ui.as_weak(), st.clone(), player.clone());
        move || {
            let Some(ui) = w.upgrade() else { return };
            let s = p.status();
            {
                let mut g = st.lock().unwrap();
                if let Some(e) = &s.error {
                    ui.set_status_text(e.clone().into());
                }
                if g.current.is_some() {
                    ui.set_playing(s.state == PlayState::Playing);
                    let dur = s.duration.or(g.current_dur).unwrap_or_default();
                    ui.set_position_text(fmt_time(s.position).into());
                    ui.set_progress(if dur.as_secs_f32() > 0.0 {
                        (s.position.as_secs_f32() / dur.as_secs_f32()).min(1.0)
                    } else {
                        0.0
                    });
                    if !g.synced.is_empty() {
                        let idx = lyrics::current_line(&g.synced, s.position).map(|i| i as i64).unwrap_or(-1);
                        if idx != g.last_lyric {
                            g.last_lyric = idx;
                            ui.set_lyrics(model(lyric_window(&g.synced, idx)));
                        }
                    }
                }
            }
            if p.take_finished() {
                step(&ui, &st, &p, 1);
            }
        }
    });

    refresh_view(ui.as_weak(), st.clone());

    if std::env::var_os("PULSO_BENCH").is_some() {
        slint::Timer::single_shot(Duration::from_millis(0), move || {
            eprintln!("[bench] ventana lista en {:?}", t0.elapsed());
        });
    }
    ui.run().expect("error en la interfaz");
}
