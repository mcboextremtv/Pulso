#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

slint::include_modules!();

use pulso_core::{
    library::{Library, Track},
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

#[derive(Default)]
struct State {
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

/// Recarga la lista (búsqueda, carpeta, orden y agrupación) en segundo plano.
fn refresh_view(ui: Weak<MainWindow>, st: Shared) {
    thread::spawn(move || {
        let (query, folder, col, desc, group) = {
            let g = st.lock().unwrap();
            (g.query.clone(), g.folder.clone(), g.sort_col.clone(), g.sort_desc, g.group)
        };
        let prefix = if folder.is_empty() { String::new() } else { format!("{folder}{}", std::path::MAIN_SEPARATOR) };
        let tracks = Library::open(&paths::db_path())
            .and_then(|lib| lib.search_sorted(&query, &prefix, &col, desc, group, 100_000))
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

    let st: Shared = Arc::new(Mutex::new(State {
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
    refresh_view(ui.as_weak(), st.clone());

    if bench {
        slint::Timer::single_shot(Duration::from_millis(0), move || {
            eprintln!("[bench] ventana lista en {:?} (dibujo: {renderer})", t0.elapsed());
        });
    }
    ui.run().expect("error en la interfaz");
}
