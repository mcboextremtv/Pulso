//! Motor de audio. Corre en su propio hilo: la interfaz nunca lo bloquea.
//! La salida de audio se abre en la primera reproducción (arranque instantáneo).
use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink, Source};
use std::{
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
    sync::{
        mpsc::{self, RecvTimeoutError},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PlayState {
    #[default]
    Stopped,
    Playing,
    Paused,
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub state: PlayState,
    pub position: Duration,
    pub duration: Option<Duration>,
    pub finished: bool,
    pub error: Option<String>,
}

enum Cmd {
    Play(PathBuf),
    Pause,
    Resume,
    Stop,
    Volume(f32),
    Seek(Duration),
    Quit,
}

pub struct Player {
    tx: mpsc::Sender<Cmd>,
    status: Arc<Mutex<Status>>,
}

impl Default for Player {
    fn default() -> Self {
        Self::new()
    }
}

impl Player {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let status = Arc::new(Mutex::new(Status::default()));
        let st = status.clone();
        thread::Builder::new()
            .name("pulso-audio".into())
            .spawn(move || audio_thread(rx, st))
            .expect("no se pudo crear el hilo de audio");
        Self { tx, status }
    }

    pub fn play(&self, path: impl Into<PathBuf>) {
        let _ = self.tx.send(Cmd::Play(path.into()));
    }
    pub fn pause(&self) {
        let _ = self.tx.send(Cmd::Pause);
    }
    pub fn resume(&self) {
        let _ = self.tx.send(Cmd::Resume);
    }
    pub fn stop(&self) {
        let _ = self.tx.send(Cmd::Stop);
    }
    pub fn set_volume(&self, v: f32) {
        let _ = self.tx.send(Cmd::Volume(v.clamp(0.0, 1.0)));
    }
    pub fn seek(&self, pos: Duration) {
        let _ = self.tx.send(Cmd::Seek(pos));
    }
    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }
    /// Devuelve true una sola vez cuando una pista termina sola.
    pub fn take_finished(&self) -> bool {
        std::mem::take(&mut self.status.lock().unwrap().finished)
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Quit);
    }
}

type Out = Option<(OutputStream, OutputStreamHandle)>;

fn start(path: &Path, out: &mut Out, volume: f32) -> Result<(Sink, Option<Duration>), String> {
    if out.is_none() {
        *out = Some(
            OutputStream::try_default().map_err(|e| format!("Sin dispositivo de audio: {e}"))?,
        );
    }
    let handle = &out.as_ref().unwrap().1;
    let file = File::open(path).map_err(|e| format!("No se pudo abrir el archivo: {e}"))?;
    let decoder = Decoder::new(BufReader::new(file))
        .map_err(|e| format!("Formato no compatible: {e}"))?;
    let duration = decoder.total_duration();
    let sink = Sink::try_new(handle).map_err(|e| format!("Error de audio: {e}"))?;
    sink.set_volume(volume);
    sink.append(decoder);
    Ok((sink, duration))
}

fn audio_thread(rx: mpsc::Receiver<Cmd>, st: Arc<Mutex<Status>>) {
    let mut out: Out = None;
    let mut sink: Option<Sink> = None;
    let mut volume = 1.0_f32;

    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Cmd::Quit) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(Cmd::Play(path)) => {
                sink = None; // detiene la anterior
                let mut g = st.lock().unwrap();
                match start(&path, &mut out, volume) {
                    Ok((s, dur)) => {
                        sink = Some(s);
                        *g = Status {
                            state: PlayState::Playing,
                            duration: dur,
                            ..Status::default()
                        };
                    }
                    Err(e) => {
                        *g = Status {
                            error: Some(e),
                            ..Status::default()
                        }
                    }
                }
            }
            Ok(Cmd::Pause) => {
                if let Some(s) = &sink {
                    s.pause();
                    let mut g = st.lock().unwrap();
                    if g.state == PlayState::Playing {
                        g.state = PlayState::Paused;
                    }
                }
            }
            Ok(Cmd::Resume) => {
                if let Some(s) = &sink {
                    s.play();
                    let mut g = st.lock().unwrap();
                    if g.state == PlayState::Paused {
                        g.state = PlayState::Playing;
                    }
                }
            }
            Ok(Cmd::Stop) => {
                sink = None;
                *st.lock().unwrap() = Status::default();
            }
            Ok(Cmd::Volume(v)) => {
                volume = v;
                if let Some(s) = &sink {
                    s.set_volume(v);
                }
            }
            Ok(Cmd::Seek(p)) => {
                if let Some(s) = &sink {
                    let _ = s.try_seek(p);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
        }

        if let Some(s) = &sink {
            let mut g = st.lock().unwrap();
            g.position = s.get_pos();
            if g.state == PlayState::Playing && s.empty() {
                g.state = PlayState::Stopped;
                g.finished = true;
            }
        }
    }
}
