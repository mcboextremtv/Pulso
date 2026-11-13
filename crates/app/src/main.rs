#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

slint::include_modules!();

use pulso_core::{
    library::{Library, Track},
    lyrics, paths,
    player::{PlayState, Player},
};
use slint::{ComponentHandle, ModelRc, VecModel, Weak};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

#[derive(Default)]
struct State {
    view: Vec<Track>,
    query: String,
    /// Carpeta seleccionada (vacía = toda la biblioteca), sin separador final.
    folder: String,
    current: Option<Track>,
    /// Pista cuya letra se está editando (puede no ser la que suena).
    edit_track: Option<Track>,
    synced: Vec<(Duration, String)>,
    /// -2 = hay que redibujar, -1 = antes de la primera línea, n = línea activa
    last_lyric: i64,
    last_underruns: u64,
}

type Shared = Arc<Mutex<State>>;
type PlayerRef = Arc<Player>;

fn cur_id(g: &State) -> Option<i64> {
    g.current.as_ref().map(|t| t.id)
}

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
        .map(|i| LyricLine { text: lines[i].1.clone().into(), current: i as i64 == cur })
        .collect()
}

fn plain_lines(text: &str) -> Vec<LyricLine> {
    text.lines().map(|l| LyricLine { text: l.into(), current: false }).collect()
}

/// Recarga la lista de canciones (según búsqueda y carpeta) en segundo plano.
fn refresh_view(ui: Weak<MainWindow>, st: Shared) {
    thread::spawn(move || {
        let (query, folder) = {
            let g = st.lock().unwrap();
            (g.query.clone(), g.folder.clone())
        };
        let prefix = if folder.is_empty() { String::new() } else { format!("{folder}{}", std::path::MAIN_SEPARATOR) };
        let tracks = Library::open(&paths::db_path())
            .and_then(|lib| lib.search_in(&query, &prefix, 100_000))
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
                ui.set_status_text("No hay canciones aquí: usa «Agregar carpeta».".into());
            }
        });
    });
}

/// Reconstruye el árbol de carpetas del panel izquierdo.
fn refresh_folders(ui: Weak<MainWindow>) {
    thread::spawn(move || {
        let (total, nodes) = Library::open(&paths::db_path())
            .and_then(|lib| Ok((lib.count()?, lib.folder_tree()?)))
            .unwrap_or((0, vec![]));
        let _ = ui.upgrade_in_event_loop(move |ui| {
            let mut items = vec![FolderItem {
                path: "".into(),
                name: "Todas las canciones".into(),
                depth: 0,
                count: total as i32,
            }];
            items.extend(nodes.into_iter().map(|n| FolderItem {
                path: n.path.into(),
                name: n.name.into(),
                depth: n.depth as i32,
                count: n.count as i32,
            }));
            ui.set_folders(model(items));
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
        refresh_folders(ui.clone());
        refresh_view(ui, st);
    });
}

// ---------------------------------------------------------------------------
// Letras
// ---------------------------------------------------------------------------

/// Artista y título para buscar: las etiquetas si existen; si no, se deduce del nombre.
fn names_for(t: &Track) -> (String, String) {
    if !t.artist.is_empty() {
        (t.artist.clone(), t.title.clone())
    } else {
        lyrics::guess_artist_title(&t.title)
    }
}

/// Muestra `text` como letra de la pista actual (sincronizada o plana) y devuelve el aviso.
fn apply_lyrics_text(ui: &MainWindow, st: &Shared, text: &str) {
    let synced = lyrics::parse_lrc(text);
    let mut g = st.lock().unwrap();
    if !synced.is_empty() {
        g.synced = synced;
        g.last_lyric = -2; // el temporizador la dibuja en el siguiente ciclo
        ui.set_lyrics_status("".into());
    } else {
        g.synced.clear();
        if text.trim().is_empty() {
            ui.set_lyrics(model(vec![]));
            ui.set_lyrics_status("Sin letra. Pulsa «Editar» para buscarla o pegarla.".into());
        } else {
            ui.set_lyrics(model(plain_lines(text)));
            ui.set_lyrics_status("Letra sin sincronizar".into());
        }
    }
}

/// Letra local (editable por el usuario) o, si no hay, intento automático con LRCLIB.
/// Devuelve el texto a mostrar o un aviso.
fn load_lyrics(t: &Track) -> Result<String, String> {
    let path = PathBuf::from(&t.path);
    if let Some(text) = lyrics::load_local(&path) {
        return Ok(text);
    }
    let (artist, title) = names_for(t);
    if artist.is_empty() {
        return Err("Sin letra. Pulsa «Editar» para buscarla o pegarla.".into());
    }
    match lyrics::fetch(&artist, &title, &t.album, Some(t.duration_ms / 1000)) {
        Ok(Some(l)) if l.instrumental && l.best_text().is_none() => Err("Instrumental".into()),
        Ok(Some(l)) => match l.best_text() {
            Some(text) => {
                let _ = lyrics::save_local(&path, &text); // se guarda para no volver a pedirla
                Ok(text)
            }
            None => Err("Sin letra disponible. Pulsa «Editar» para pegarla.".into()),
        },
        Ok(None) => Err("No se encontró la letra. Pulsa «Editar» para buscarla o pegarla.".into()),
        Err(e) => Err(format!("No se pudo buscar la letra: {e}")),
    }
}

fn open_url(url: &str) {
    use std::process::Command;
    #[cfg(windows)]
    let _ = Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn();
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = Command::new("xdg-open").arg(url).spawn();
}

// ---------------------------------------------------------------------------
// Reproducción
// ---------------------------------------------------------------------------

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
        g.current = Some(track.clone());
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
            if cur_id(&st.lock().unwrap()) != Some(track.id) {
                return; // el usuario ya cambió de canción
            }
            match res {
                Ok(text) => apply_lyrics_text(&ui, &st, &text),
                Err(msg) => {
                    ui.set_lyrics(model(vec![]));
                    ui.set_lyrics_status(msg.into());
                }
            }
        });
    });
}

fn step(ui: &MainWindow, st: &Shared, player: &PlayerRef, delta: i64) {
    let target = {
        let g = st.lock().unwrap();
        g.view
            .iter()
            .position(|t| Some(t.id) == cur_id(&g))
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

/// PULSO_RENDERER=femtovg (GPU, por defecto) o software. Si la ventana no se puede crear
/// con la GPU, se relanza solo en modo software.
fn main() {
    let t0 = Instant::now();
    let forced = std::env::var("PULSO_RENDERER").ok();
    let renderer = forced.clone().unwrap_or_else(|| "femtovg".into());
    let _ = slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name(renderer.clone())
        .select();
    let ui = match MainWindow::new() {
        Ok(ui) => ui,
        Err(e) => {
            if forced.is_none() {
                if let Ok(exe) = std::env::current_exe() {
                    let _ = std::process::Command::new(exe).env("PULSO_RENDERER", "software").spawn();
                    return;
                }
            }
            panic!("no se pudo crear la ventana: {e}");
        }
    };
    let bench = std::env::var_os("PULSO_BENCH").is_some();
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
    ui.on_folder_selected({
        let (w, st) = (ui.as_weak(), st.clone());
        move |path| {
            if let Some(ui) = w.upgrade() {
                ui.set_selected_folder(path.clone());
            }
            st.lock().unwrap().folder = path.to_string();
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
                        g.view.iter().position(|t| Some(t.id) == cur_id(&g))
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
            let cur = st.lock().unwrap().current.as_ref().map(|t| Duration::from_millis(t.duration_ms));
            if let Some(d) = p.status().duration.or(cur) {
                p.seek(d.mul_f32(f.clamp(0.0, 1.0)));
            }
        }
    });
    ui.on_volume_changed({
        let p = player.clone();
        move |v| p.set_volume(v)
    });

    // ---- Editor de letras ----
    ui.on_edit_lyrics({
        let (w, st) = (ui.as_weak(), st.clone());
        move || {
            let Some(ui) = w.upgrade() else { return };
            let Some(tr) = st.lock().unwrap().current.clone() else { return };
            let (artist, title) = names_for(&tr);
            ui.set_edit_name(format!("Pista: {}", tr.title).into());
            ui.set_edit_artist(artist.into());
            ui.set_edit_title(title.into());
            ui.set_edit_text(lyrics::load_local(Path::new(&tr.path)).unwrap_or_default().into());
            ui.set_edit_status("Escribe o pega la letra. Si tiene marcas [mm:ss.xx] se sincroniza.".into());
            ui.set_editing(true);
            st.lock().unwrap().edit_track = Some(tr);
        }
    });
    ui.on_cancel_edit({
        let w = ui.as_weak();
        move || {
            if let Some(ui) = w.upgrade() {
                ui.set_editing(false);
            }
        }
    });
    ui.on_search_lyrics({
        let (w, st) = (ui.as_weak(), st.clone());
        move |artist, title| {
            let Some(tr) = st.lock().unwrap().edit_track.clone() else { return };
            if let Some(ui) = w.upgrade() {
                ui.set_edit_status("Buscando…".into());
            }
            let (artist, title, weak) = (artist.to_string(), title.to_string(), w.clone());
            thread::spawn(move || {
                let r = lyrics::fetch(&artist, &title, &tr.album, Some(tr.duration_ms / 1000));
                let _ = weak.upgrade_in_event_loop(move |ui| match r {
                    Ok(Some(l)) => match l.best_text() {
                        Some(text) => {
                            ui.set_edit_text(text.into());
                            ui.set_edit_status("Letra encontrada: revísala y pulsa Guardar.".into());
                        }
                        None => ui.set_edit_status("Esa canción figura como instrumental.".into()),
                    },
                    Ok(None) => ui.set_edit_status(
                        "No se encontró. Cambia artista o título, o usa «Buscar en la web» y pega la letra.".into(),
                    ),
                    Err(e) => ui.set_edit_status(format!("Error: {e}").into()),
                });
            });
        }
    });
    ui.on_web_lyrics(|artist, title| open_url(&lyrics::web_search_url(&artist, &title)));
    ui.on_save_lyrics({
        let (w, st) = (ui.as_weak(), st.clone());
        move |text| {
            let Some(ui) = w.upgrade() else { return };
            let Some(tr) = st.lock().unwrap().edit_track.clone() else { return };
            if let Err(e) = lyrics::save_local(Path::new(&tr.path), &text) {
                ui.set_edit_status(format!("No se pudo guardar: {e}").into());
                return;
            }
            ui.set_editing(false);
            if cur_id(&st.lock().unwrap()) == Some(tr.id) {
                apply_lyrics_text(&ui, &st, &text);
            }
        }
    });
    ui.on_delete_lyrics({
        let (w, st) = (ui.as_weak(), st.clone());
        move || {
            let Some(ui) = w.upgrade() else { return };
            let Some(tr) = st.lock().unwrap().current.clone() else { return };
            match lyrics::save_local(Path::new(&tr.path), "") {
                Ok(()) => {
                    apply_lyrics_text(&ui, &st, "");
                    ui.set_lyrics_status("Letra eliminada.".into());
                }
                Err(e) => ui.set_lyrics_status(format!("No se pudo eliminar: {e}").into()),
            }
        }
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
                if bench && s.underruns != g.last_underruns {
                    g.last_underruns = s.underruns;
                    eprintln!("[bench] cortes de audio acumulados: {}", s.underruns);
                }
                if let Some(cur) = g.current.clone() {
                    ui.set_playing(s.state == PlayState::Playing);
                    let dur = s.duration.unwrap_or(Duration::from_millis(cur.duration_ms));
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

    refresh_folders(ui.as_weak());
    refresh_view(ui.as_weak(), st.clone());

    if bench {
        slint::Timer::single_shot(Duration::from_millis(0), move || {
            eprintln!("[bench] ventana lista en {:?} (dibujo: {renderer})", t0.elapsed());
        });
    }
    ui.run().expect("error en la interfaz");
}
