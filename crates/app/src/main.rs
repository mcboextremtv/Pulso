#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

slint::include_modules!();

use pulso_core::{
    library::{Library, Track, TrackQuery},
    lyrics, paths,
    player::{PlayState, Player},
    settings::Settings,
};
use slint::{ComponentHandle, Model, ModelRc, VecModel, Weak};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

#[derive(Default, Clone, Copy, PartialEq)]
enum InputKind {
    #[default]
    None,
    New,
    Rename,
}

#[derive(Default)]
struct State {
    /// "folders" o "lists"
    mode: String,
    /// Lista seleccionada (-1 = ninguna / todas las canciones)
    playlist: i64,
    /// Canciones a añadir cuando se elija una lista en el diálogo
    pending_paths: Vec<String>,
    input: InputKind,
    /// Pistas en el orden de la lista (sin las filas de grupo).
    view: Vec<Track>,
    /// Fila de la interfaz que ocupa cada pista de `view` (hay filas extra con grupos).
    ui_rows: Vec<usize>,
    query: String,
    /// Carpeta seleccionada (vacía = toda la biblioteca), sin separador final.
    folder: String,
    sort_col: String,
    sort_desc: bool,
    group: bool,
    current: Option<Track>,
    /// Pista cuya letra se está editando (puede no ser la que suena).
    edit_track: Option<Track>,
    /// Pista cuyas etiquetas se están editando.
    tags_track: Option<Track>,
    synced: Vec<(Duration, String)>,
    /// -2 = hay que redibujar, -1 = ninguna línea activa, n = línea activa
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

fn plain_lines(text: &str) -> Vec<LyricLine> {
    text.lines().map(|l| LyricLine { text: l.into(), current: false }).collect()
}

fn save_settings(ui: &MainWindow) {
    let s = Settings {
        sidebar_w: ui.get_sidebar_w(),
        lyrics_w: ui.get_lyrics_w(),
        title_w: ui.get_title_w(),
        artist_w: ui.get_artist_w(),
        sort_col: ui.get_sort_col().to_string(),
        sort_desc: ui.get_sort_desc(),
        group_artist: ui.get_group_artist(),
        audio_buffer: ui.get_audio_buffer().max(1024) as u32,
        sidebar_mode: ui.get_sidebar_mode().to_string(),
        acoustid_key: ui.get_acoustid_key().to_string(),
    };
    let _ = s.save();
}

// ---------------------------------------------------------------------------
// Lista de canciones
// ---------------------------------------------------------------------------

fn track_item(t: &Track, index: usize) -> TrackItem {
    TrackItem {
        id: t.id as i32,
        index: index as i32,
        header: false,
        title: t.title.clone().into(),
        artist: t.artist.clone().into(),
        album: t.album.clone().into(),
        duration: fmt_time(Duration::from_millis(t.duration_ms)).into(),
    }
}

/// Filas de la interfaz. Con `group`, antes de cada artista va una fila de encabezado.
/// Devuelve también la fila que ocupa cada pista.
fn build_rows(view: &[Track], group: bool) -> (Vec<TrackItem>, Vec<usize>) {
    let mut items = Vec::with_capacity(view.len() + 16);
    let mut rows = Vec::with_capacity(view.len());
    let mut i = 0;
    while i < view.len() {
        if group {
            let key = view[i].artist.to_lowercase();
            let mut j = i;
            while j < view.len() && view[j].artist.to_lowercase() == key {
                j += 1;
            }
            let name = if view[i].artist.is_empty() { "Sin artista".to_string() } else { view[i].artist.clone() };
            items.push(TrackItem {
                id: -1,
                index: -1,
                header: true,
                title: name.into(),
                artist: format!("{} canciones", j - i).into(),
                album: "".into(),
                duration: "".into(),
            });
            for k in i..j {
                rows.push(items.len());
                items.push(track_item(&view[k], k));
            }
            i = j;
        } else {
            rows.push(items.len());
            items.push(track_item(&view[i], i));
            i += 1;
        }
    }
    (items, rows)
}

/// Recarga la lista (búsqueda, carpeta o lista, orden y agrupación) en segundo plano.
fn refresh_view(ui: Weak<MainWindow>, st: Shared) {
    thread::spawn(move || {
        let (query, folder, col, desc, group, playlist) = {
            let g = st.lock().unwrap();
            let in_lists = g.mode == "lists";
            (
                g.query.clone(),
                if in_lists { String::new() } else { g.folder.clone() },
                g.sort_col.clone(),
                g.sort_desc,
                g.group,
                if in_lists && g.playlist >= 0 { Some(g.playlist) } else { None },
            )
        };
        let prefix = if folder.is_empty() { String::new() } else { format!("{folder}{}", std::path::MAIN_SEPARATOR) };
        let tracks = Library::open(&paths::db_path())
            .and_then(|lib| {
                lib.query_tracks(&TrackQuery {
                    text: &query,
                    folder_prefix: &prefix,
                    playlist,
                    sort_col: &col,
                    desc,
                    group_by_artist: group,
                    limit: 100_000,
                })
            })
            .unwrap_or_default();
        let st2 = st.clone();
        let _ = ui.upgrade_in_event_loop(move |ui| {
            let (items, rows) = build_rows(&tracks, group);
            let n = tracks.len();
            {
                let mut g = st2.lock().unwrap();
                g.view = tracks;
                g.ui_rows = rows;
            }
            ui.set_tracks(model(items));
            if n == 0 && (playlist.is_some() || ui.get_status_text().is_empty()) {
                let msg = if playlist.is_some() {
                    "Esta lista está vacía: añade canciones con el botón + de cada fila."
                } else {
                    "No hay canciones aquí: usa «Agregar carpeta»."
                };
                ui.set_status_text(msg.into());
            }
        });
    });
}

/// Recarga las listas de reproducción (barra lateral y selector «añadir a…»).
fn refresh_playlists(ui: Weak<MainWindow>) {
    thread::spawn(move || {
        let (total, lists) = Library::open(&paths::db_path())
            .and_then(|lib| Ok((lib.count()?, lib.playlists()?)))
            .unwrap_or((0, vec![]));
        let _ = ui.upgrade_in_event_loop(move |ui| {
            let real: Vec<PlaylistItem> = lists
                .iter()
                .map(|l| PlaylistItem { id: l.id as i32, name: l.name.clone().into(), count: l.count as i32 })
                .collect();
            let mut all = vec![PlaylistItem { id: -1, name: "Todas las canciones".into(), count: total as i32 }];
            all.extend(real.iter().cloned());
            ui.set_playlists(model(all));
            ui.set_pick_lists(model(real));
        });
    });
}

fn say(ui: Weak<MainWindow>, msg: String) {
    let _ = ui.upgrade_in_event_loop(move |ui| ui.set_status_text(msg.into()));
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
        refresh_playlists(ui.clone());
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

/// Muestra `text` como letra de la pista actual (sincronizada o plana).
fn apply_lyrics_text(ui: &MainWindow, st: &Shared, text: &str) {
    let synced = lyrics::parse_lrc(text);
    let mut g = st.lock().unwrap();
    if !synced.is_empty() {
        g.synced = synced;
        g.last_lyric = -2; // el temporizador reconstruye las líneas en el siguiente ciclo
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

/// Marca la línea activa sin reconstruir el modelo (la vista se desplaza sola hasta ella).
fn update_lyric_highlight(ui: &MainWindow, g: &mut State, idx: i64) {
    let m = ui.get_lyrics();
    if g.last_lyric == -2 || m.row_count() != g.synced.len() {
        ui.set_lyrics(model(
            g.synced.iter().map(|(_, t)| LyricLine { text: t.clone().into(), current: false }).collect(),
        ));
        g.last_lyric = -1; // la línea activa se marca en el siguiente ciclo, ya con las filas creadas
        return;
    }
    let set = |row: i64, current: bool| {
        if row >= 0 {
            if let Some(mut r) = m.row_data(row as usize) {
                r.current = current;
                m.set_row_data(row as usize, r);
            }
        }
    };
    set(g.last_lyric, false);
    set(idx, true);
    g.last_lyric = idx;
}

// ---------------------------------------------------------------------------
// Reproducción
// ---------------------------------------------------------------------------

fn play_at(ui: &MainWindow, st: &Shared, player: &PlayerRef, idx: usize) {
    let (track, ui_row) = {
        let g = st.lock().unwrap();
        match g.view.get(idx) {
            Some(t) => (t.clone(), g.ui_rows.get(idx).copied()),
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
    if let Some(row) = ui_row {
        ui.invoke_ensure_visible(row as i32); // solo se desplaza si la fila no se ve
    }

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

    // Ajustes guardados: tamaños de paneles y columnas, orden, agrupación y búfer de audio.
    let cfg = Settings::load();
    ui.set_sidebar_w(cfg.sidebar_w);
    ui.set_lyrics_w(cfg.lyrics_w);
    ui.set_title_w(cfg.title_w);
    ui.set_artist_w(cfg.artist_w);
    ui.set_sort_col(cfg.sort_col.clone().into());
    ui.set_sort_desc(cfg.sort_desc);
    ui.set_group_artist(cfg.group_artist);
    ui.set_audio_buffer(cfg.audio_buffer as i32);
    player.set_buffer(cfg.audio_buffer);
    player.set_volume(0.8);

    ui.set_sidebar_mode(cfg.sidebar_mode.clone().into());
    ui.set_acoustid_key(cfg.acoustid_key.clone().into());
    let st: Shared = Arc::new(Mutex::new(State {
        mode: cfg.sidebar_mode.clone(),
        playlist: -1,
        last_lyric: -2,
        sort_col: cfg.sort_col,
        sort_desc: cfg.sort_desc,
        group: cfg.group_artist,
        ..Default::default()
    }));

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
    // Clic en una cabecera: ascendente → descendente → orden de carpeta.
    ui.on_header_clicked({
        let (w, st) = (ui.as_weak(), st.clone());
        move |col| {
            let Some(ui) = w.upgrade() else { return };
            {
                let mut g = st.lock().unwrap();
                if g.sort_col != col.as_str() {
                    g.sort_col = col.to_string();
                    g.sort_desc = false;
                } else if !g.sort_desc {
                    g.sort_desc = true;
                } else {
                    g.sort_col.clear();
                    g.sort_desc = false;
                }
                ui.set_sort_col(g.sort_col.clone().into());
                ui.set_sort_desc(g.sort_desc);
            }
            save_settings(&ui);
            refresh_view(w.clone(), st.clone());
        }
    });
    ui.on_toggle_group({
        let (w, st) = (ui.as_weak(), st.clone());
        move || {
            let Some(ui) = w.upgrade() else { return };
            let group = {
                let mut g = st.lock().unwrap();
                g.group = !g.group;
                g.group
            };
            ui.set_group_artist(group);
            save_settings(&ui);
            refresh_view(w.clone(), st.clone());
        }
    });
    ui.on_layout_changed({
        let w = ui.as_weak();
        move || {
            if let Some(ui) = w.upgrade() {
                save_settings(&ui);
            }
        }
    });
    ui.on_set_buffer({
        let (w, p) = (ui.as_weak(), player.clone());
        move |n| {
            let Some(ui) = w.upgrade() else { return };
            ui.set_audio_buffer(n);
            p.set_buffer(n.max(1024) as u32);
            save_settings(&ui);
        }
    });
    ui.on_play_track({
        let (w, st, p) = (ui.as_weak(), st.clone(), player.clone());
        move |i| {
            if let Some(ui) = w.upgrade() {
                play_at(&ui, &st, &p, i.max(0) as usize);
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

    // ---- Listas de reproducción ----
    ui.on_mode_changed({
        let (w, st) = (ui.as_weak(), st.clone());
        move |mode| {
            let Some(ui) = w.upgrade() else { return };
            st.lock().unwrap().mode = mode.to_string();
            ui.set_sidebar_mode(mode);
            ui.set_status_text("".into());
            save_settings(&ui);
            refresh_playlists(w.clone());
            refresh_view(w.clone(), st.clone());
        }
    });
    ui.on_playlist_selected({
        let (w, st) = (ui.as_weak(), st.clone());
        move |id| {
            let Some(ui) = w.upgrade() else { return };
            st.lock().unwrap().playlist = id as i64;
            ui.set_selected_playlist(id);
            ui.set_status_text("".into());
            refresh_view(w.clone(), st.clone());
        }
    });
    ui.on_new_playlist({
        let (w, st) = (ui.as_weak(), st.clone());
        move || {
            let Some(ui) = w.upgrade() else { return };
            st.lock().unwrap().input = InputKind::New;
            ui.set_input_title("Nueva lista".into());
            ui.set_input_text("".into());
            ui.set_input_open(true);
        }
    });
    ui.on_rename_playlist({
        let (w, st) = (ui.as_weak(), st.clone());
        move || {
            let Some(ui) = w.upgrade() else { return };
            let id = ui.get_selected_playlist();
            let name = ui.get_playlists().iter().find(|p| p.id == id).map(|p| p.name).unwrap_or_default();
            st.lock().unwrap().input = InputKind::Rename;
            ui.set_input_title("Renombrar lista".into());
            ui.set_input_text(name);
            ui.set_input_open(true);
        }
    });
    ui.on_input_accepted({
        let (w, st) = (ui.as_weak(), st.clone());
        move |text| {
            let Some(ui) = w.upgrade() else { return };
            let (kind, selected) = (st.lock().unwrap().input, ui.get_selected_playlist() as i64);
            let name = text.trim().to_string();
            if name.is_empty() {
                return;
            }
            ui.set_input_open(false);
            st.lock().unwrap().input = InputKind::None;
            let (weak, st) = (w.clone(), st.clone());
            thread::spawn(move || {
                let res = Library::open(&paths::db_path()).and_then(|lib| match kind {
                    InputKind::New => lib.create_playlist(&name).map(Some),
                    InputKind::Rename => lib.rename_playlist(selected, &name).map(|_| None),
                    InputKind::None => Ok(None),
                });
                match res {
                    Ok(new_id) => {
                        refresh_playlists(weak.clone());
                        if let Some(id) = new_id {
                            st.lock().unwrap().playlist = id;
                            let _ = weak.upgrade_in_event_loop(move |ui| ui.set_selected_playlist(id as i32));
                            refresh_view(weak, st);
                        }
                    }
                    Err(e) => say(weak, format!("No se pudo guardar la lista: {e}")),
                }
            });
        }
    });
    ui.on_delete_playlist({
        let w = ui.as_weak();
        move || {
            let Some(ui) = w.upgrade() else { return };
            let id = ui.get_selected_playlist();
            let name = ui.get_playlists().iter().find(|p| p.id == id).map(|p| p.name).unwrap_or_default();
            ui.set_confirm_text(format!("¿Eliminar la lista «{name}»? Las canciones no se borran de tu disco.").into());
            ui.set_confirm_open(true);
        }
    });
    ui.on_confirm_accepted({
        let (w, st) = (ui.as_weak(), st.clone());
        move || {
            let Some(ui) = w.upgrade() else { return };
            let id = ui.get_selected_playlist() as i64;
            ui.set_confirm_open(false);
            ui.set_selected_playlist(-1);
            ui.set_status_text("".into());
            st.lock().unwrap().playlist = -1;
            let (weak, st) = (w.clone(), st.clone());
            thread::spawn(move || {
                if let Err(e) = Library::open(&paths::db_path()).and_then(|mut lib| lib.delete_playlist(id)) {
                    say(weak.clone(), format!("No se pudo eliminar la lista: {e}"));
                }
                refresh_playlists(weak.clone());
                refresh_view(weak, st);
            });
        }
    });
    ui.on_add_row({
        let (w, st) = (ui.as_weak(), st.clone());
        move |index| {
            let Some(ui) = w.upgrade() else { return };
            let Some(tr) = st.lock().unwrap().view.get(index.max(0) as usize).cloned() else { return };
            ui.set_pick_title(format!("Añadir «{}» a…", tr.title).into());
            ui.set_pick_new_name("".into());
            st.lock().unwrap().pending_paths = vec![tr.path];
            ui.set_pick_open(true);
        }
    });
    ui.on_add_all({
        let (w, st) = (ui.as_weak(), st.clone());
        move || {
            let Some(ui) = w.upgrade() else { return };
            let paths: Vec<String> = st.lock().unwrap().view.iter().map(|t| t.path.clone()).collect();
            ui.set_pick_title(format!("Añadir las {} canciones que ves a…", paths.len()).into());
            ui.set_pick_new_name("".into());
            st.lock().unwrap().pending_paths = paths;
            ui.set_pick_open(true);
        }
    });
    // Añade `pending_paths` a una lista existente o recién creada y avisa del resultado.
    fn add_pending(weak: Weak<MainWindow>, st: Shared, target: Result<i64, String>) {
        let paths = std::mem::take(&mut st.lock().unwrap().pending_paths);
        thread::spawn(move || {
            let msg = match target {
                Err(e) => format!("No se pudo crear la lista: {e}"),
                Ok(id) => Library::open(&paths::db_path())
                    .and_then(|mut lib| {
                        let added = lib.add_to_playlist(id, &paths)?;
                        let name = lib.playlists()?.into_iter().find(|l| l.id == id).map(|l| l.name).unwrap_or_default();
                        Ok((added, name))
                    })
                    .map(|(added, name)| {
                        let skipped = paths.len() - added;
                        match (added, skipped) {
                            (0, _) => format!("Ya estaban todas en «{name}»."),
                            (n, 0) => format!("Añadidas {n} a «{name}»."),
                            (n, k) => format!("Añadidas {n} a «{name}» ({k} ya estaban)."),
                        }
                    })
                    .unwrap_or_else(|e| format!("No se pudo añadir: {e}")),
            };
            say(weak.clone(), msg);
            refresh_playlists(weak);
        });
    }
    ui.on_pick_playlist({
        let (w, st) = (ui.as_weak(), st.clone());
        move |id| {
            if let Some(ui) = w.upgrade() {
                ui.set_pick_open(false);
            }
            add_pending(w.clone(), st.clone(), Ok(id as i64));
        }
    });
    ui.on_pick_new({
        let (w, st) = (ui.as_weak(), st.clone());
        move |name| {
            let name = name.trim().to_string();
            if name.is_empty() {
                return;
            }
            if let Some(ui) = w.upgrade() {
                ui.set_pick_open(false);
            }
            let created = Library::open(&paths::db_path())
                .and_then(|lib| lib.create_playlist(&name))
                .map_err(|e| e.to_string());
            add_pending(w.clone(), st.clone(), created);
        }
    });
    ui.on_remove_row({
        let (w, st) = (ui.as_weak(), st.clone());
        move |index| {
            let (tr, playlist) = {
                let g = st.lock().unwrap();
                (g.view.get(index.max(0) as usize).cloned(), g.playlist)
            };
            let Some(tr) = tr else { return };
            let (weak, st) = (w.clone(), st.clone());
            thread::spawn(move || {
                let _ = Library::open(&paths::db_path()).and_then(|lib| lib.remove_from_playlist(playlist, &tr.path));
                refresh_playlists(weak.clone());
                refresh_view(weak, st);
            });
        }
    });

    // ---- Identificación (AcoustID): solo se guarda la clave del usuario ----
    ui.on_save_acoustid({
        let w = ui.as_weak();
        move |key| {
            let Some(ui) = w.upgrade() else { return };
            let clean: String = key.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
            ui.set_acoustid_key(clean.into());
            save_settings(&ui);
        }
    });
    ui.on_open_acoustid_page(|| open_url("https://acoustid.org/new-application"));

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

    // ---- Editor de etiquetas ----
    ui.on_edit_tags({
        let (w, st) = (ui.as_weak(), st.clone());
        move |index| {
            let Some(ui) = w.upgrade() else { return };
            let Some(tr) = st.lock().unwrap().view.get(index.max(0) as usize).cloned() else { return };
            ui.set_tags_title(tr.title.clone().into());
            ui.set_tags_artist(tr.artist.clone().into());
            ui.set_tags_album(tr.album.clone().into());
            ui.set_tags_path(tr.path.clone().into());
            ui.set_tags_status("Los cambios se escriben dentro del archivo de música.".into());
            ui.set_tags_open(true);
            st.lock().unwrap().tags_track = Some(tr);
        }
    });
    ui.on_save_tags({
        let (w, st, p) = (ui.as_weak(), st.clone(), player.clone());
        move |title, artist, album| {
            let Some(tr) = st.lock().unwrap().tags_track.clone() else { return };
            if let Some(ui) = w.upgrade() {
                ui.set_tags_status("Guardando…".into());
            }
            let (title, artist, album) = (title.to_string(), artist.to_string(), album.to_string());
            let (weak, st, p) = (w.clone(), st.clone(), p.clone());
            thread::spawn(move || {
                // Windows no deja escribir un archivo abierto: si es el que suena, se suelta un
                // instante y se retoma en el mismo punto.
                let is_current = cur_id(&st.lock().unwrap()) == Some(tr.id);
                let before = p.status();
                if is_current {
                    p.stop();
                    thread::sleep(Duration::from_millis(300));
                }
                let result = Library::open(&paths::db_path())
                    .and_then(|lib| lib.apply_tags(tr.id, Path::new(&tr.path), &title, &artist, &album));
                if is_current {
                    p.play(&tr.path);
                    p.seek(before.position);
                    if before.state == PlayState::Paused {
                        p.pause();
                    }
                }
                let _ = weak.upgrade_in_event_loop(move |ui| match result {
                    Ok(()) => {
                        ui.set_tags_open(false);
                        if is_current {
                            let mut g = st.lock().unwrap();
                            if let Some(c) = g.current.as_mut() {
                                c.title = if title.trim().is_empty() { c.title.clone() } else { title.trim().to_string() };
                                c.artist = artist.trim().to_string();
                                c.album = album.trim().to_string();
                                ui.set_now_title(c.title.clone().into());
                                ui.set_now_artist(c.artist.clone().into());
                            }
                        }
                        refresh_view(ui.as_weak(), st.clone());
                    }
                    Err(e) => ui.set_tags_status(format!("No se pudo guardar: {e}").into()),
                });
            });
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
                if s.underruns != g.last_underruns {
                    g.last_underruns = s.underruns;
                    ui.set_underruns(s.underruns as i32);
                    if bench {
                        eprintln!("[bench] cortes de audio acumulados: {}", s.underruns);
                    }
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
                            update_lyric_highlight(&ui, &mut g, idx);
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
    refresh_playlists(ui.as_weak());
    refresh_view(ui.as_weak(), st.clone());

    if bench {
        slint::Timer::single_shot(Duration::from_millis(0), move || {
            eprintln!("[bench] ventana lista en {:?} (dibujo: {renderer})", t0.elapsed());
        });
    }
    ui.run().expect("error en la interfaz");
}
