//! Motor de audio. Corre en su propio hilo: la interfaz nunca lo bloquea.
//! La salida de audio se abre en la primera reproducción (arranque instantáneo).
use rodio::{
    cpal::BufferSize, source::SeekError, ChannelCount, Decoder, OutputStream, OutputStreamBuilder, SampleRate,
    Sink, Source,
};
use std::{
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed},
        mpsc::{self, RecvTimeoutError, TryRecvError, TrySendError},
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
    /// Veces que el audio se quedó sin datos (cortes por falta de decodificación a tiempo).
    pub underruns: u64,
}

enum Cmd {
    Play(PathBuf),
    Pause,
    Resume,
    Stop,
    Volume(f32),
    SetBuffer(u32),
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
    /// Tamaño del búfer de salida (tramas). Se aplica desde la siguiente canción.
    pub fn set_buffer(&self, frames: u32) {
        let _ = self.tx.send(Cmd::SetBuffer(frames));
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

// ---------------------------------------------------------------------------
// Decodificación anticipada
//
// Un hilo aparte decodifica el archivo y llena una cola de ~2 s. El hilo urgente
// de la tarjeta de sonido solo copia de esa cola: un tirón de CPU (antivirus,
// ratón, memoria justa) queda absorbido por el colchón y no se oye.
// ---------------------------------------------------------------------------

const CHUNK: usize = 8192; // muestras por bloque
const BUFFER_SECS: usize = 4;
const PREFILL_CHUNKS: usize = 6;
/// Búfer de salida por defecto (tramas). A 48 kHz son ~170 ms.
pub const DEFAULT_BUFFER_FRAMES: u32 = 8192;

struct Chunk {
    gen: u64,
    data: Vec<f32>,
}

enum DecCmd {
    Seek(Duration),
}

struct BufferedSource {
    rx: mpsc::Receiver<Chunk>,
    cmd: mpsc::Sender<DecCmd>,
    gen: Arc<AtomicU64>,
    queued: Arc<AtomicUsize>,
    underruns: Arc<AtomicU64>,
    buf: Vec<f32>,
    pos: usize,
    channels: ChannelCount,
    rate: SampleRate,
    total: Option<Duration>,
    starving: bool,
}

impl Iterator for BufferedSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        loop {
            if self.pos < self.buf.len() {
                let s = self.buf[self.pos];
                self.pos += 1;
                self.starving = false;
                return Some(s);
            }
            match self.rx.try_recv() {
                Ok(c) => {
                    self.queued.fetch_sub(1, Relaxed);
                    if c.gen == self.gen.load(Relaxed) {
                        self.buf = c.data;
                        self.pos = 0;
                    }
                }
                Err(TryRecvError::Empty) => {
                    // El decodificador no llegó a tiempo: silencio breve, sin bloquear.
                    if !self.starving {
                        self.starving = true;
                        self.underruns.fetch_add(1, Relaxed);
                    }
                    return Some(0.0);
                }
                Err(TryRecvError::Disconnected) => return None,
            }
        }
    }
}

impl Source for BufferedSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> ChannelCount {
        self.channels
    }
    fn sample_rate(&self) -> SampleRate {
        self.rate
    }
    fn total_duration(&self) -> Option<Duration> {
        self.total
    }
    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        self.gen.fetch_add(1, Relaxed); // invalida lo ya decodificado
        self.buf.clear();
        self.pos = 0;
        self.cmd.send(DecCmd::Seek(pos)).map_err(|_| SeekError::NotSupported {
            underlying_source: "hilo de decodificación",
        })
    }
}

/// Sube un poco la prioridad del hilo que decodifica: bajo carga del sistema,
/// el audio es lo último que debe quedarse sin CPU.
#[cfg(windows)]
fn boost_current_thread() {
    extern "system" {
        fn GetCurrentThread() -> isize;
        fn SetThreadPriority(thread: isize, priority: i32) -> i32;
    }
    // THREAD_PRIORITY_ABOVE_NORMAL = 1
    unsafe {
        SetThreadPriority(GetCurrentThread(), 1);
    }
}
#[cfg(not(windows))]
fn boost_current_thread() {}

fn decode_loop(
    mut dec: Decoder<BufReader<File>>,
    tx: mpsc::SyncSender<Chunk>,
    cmd: mpsc::Receiver<DecCmd>,
    gen: Arc<AtomicU64>,
    queued: Arc<AtomicUsize>,
    finished: Arc<AtomicBool>,
) {
    boost_current_thread();
    let mut my_gen = gen.load(Relaxed);
    'outer: loop {
        loop {
            match cmd.try_recv() {
                Ok(DecCmd::Seek(p)) => {
                    let _ = dec.try_seek(p);
                    my_gen = gen.load(Relaxed);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break 'outer,
            }
        }
        let data: Vec<f32> = dec.by_ref().take(CHUNK).collect();
        if data.is_empty() {
            break;
        }
        let mut pending = Some(Chunk { gen: my_gen, data });
        while let Some(ch) = pending.take() {
            queued.fetch_add(1, Relaxed);
            match tx.try_send(ch) {
                Ok(()) => {}
                Err(TrySendError::Disconnected(_)) => break 'outer,
                Err(TrySendError::Full(ch)) => {
                    queued.fetch_sub(1, Relaxed);
                    // Cola llena: esperar un hueco, atendiendo saltos de posición.
                    match cmd.recv_timeout(Duration::from_millis(10)) {
                        Ok(DecCmd::Seek(p)) => {
                            let _ = dec.try_seek(p);
                            my_gen = gen.load(Relaxed);
                        }
                        Err(RecvTimeoutError::Timeout) => pending = Some(ch),
                        Err(RecvTimeoutError::Disconnected) => break 'outer,
                    }
                }
            }
        }
    }
    finished.store(true, Relaxed);
}

fn buffer_size(frames: u32) -> BufferSize {
    // PULSO_AUDIO_BUFFER=<tramas> tiene prioridad (pruebas); 0 = el de Windows por defecto.
    let n = std::env::var("PULSO_AUDIO_BUFFER")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(frames);
    if n == 0 {
        BufferSize::Default
    } else {
        BufferSize::Fixed(n)
    }
}

struct Open {
    stream: OutputStream,
    frames: u32,
}

type Out = Option<Open>;

fn open_output(frames: u32) -> Result<Open, String> {
    let builder = OutputStreamBuilder::from_default_device()
        .map_err(|e| format!("Sin dispositivo de audio: {e}"))?
        .with_buffer_size(buffer_size(frames));
    let mut stream = builder
        .open_stream_or_fallback()
        .map_err(|e| format!("No se pudo abrir el audio: {e}"))?;
    stream.log_on_drop(false);
    Ok(Open { stream, frames })
}

struct Started {
    sink: Sink,
    duration: Option<Duration>,
    underruns: Arc<AtomicU64>,
}

fn start(path: &Path, out: &mut Out, volume: f32, frames: u32) -> Result<Started, String> {
    if out.as_ref().map(|o| o.frames) != Some(frames) {
        *out = None; // cierra el dispositivo anterior antes de abrir con otro búfer
        *out = Some(open_output(frames)?);
    }
    let file = File::open(path).map_err(|e| format!("No se pudo abrir el archivo: {e}"))?;
    let dec = Decoder::try_from(file).map_err(|e| format!("Formato no compatible: {e}"))?;
    let (channels, rate, total) = (dec.channels(), dec.sample_rate(), dec.total_duration());

    let per_sec = (rate as usize * channels as usize).max(1);
    let capacity = (per_sec * BUFFER_SECS / CHUNK).max(PREFILL_CHUNKS * 2);
    let (tx, rx) = mpsc::sync_channel(capacity);
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let gen = Arc::new(AtomicU64::new(0));
    let queued = Arc::new(AtomicUsize::new(0));
    let finished = Arc::new(AtomicBool::new(false));
    let underruns = Arc::new(AtomicU64::new(0));

    {
        let (gen, queued, finished) = (gen.clone(), queued.clone(), finished.clone());
        thread::Builder::new()
            .name("pulso-decode".into())
            .spawn(move || decode_loop(dec, tx, cmd_rx, gen, queued, finished))
            .map_err(|e| format!("No se pudo crear el hilo de decodificación: {e}"))?;
    }

    // Llenado previo: unos 0,4 s antes de que empiece a sonar (tarda pocos ms).
    let t0 = std::time::Instant::now();
    while queued.load(Relaxed) < PREFILL_CHUNKS
        && !finished.load(Relaxed)
        && t0.elapsed() < Duration::from_millis(500)
    {
        thread::sleep(Duration::from_millis(2));
    }

    let source = BufferedSource {
        rx,
        cmd: cmd_tx,
        gen,
        queued,
        underruns: underruns.clone(),
        buf: Vec::new(),
        pos: 0,
        channels,
        rate,
        total,
        starving: false,
    };
    let sink = Sink::connect_new(out.as_ref().unwrap().stream.mixer());
    sink.set_volume(volume);
    sink.append(source);
    Ok(Started { sink, duration: total, underruns })
}

fn audio_thread(rx: mpsc::Receiver<Cmd>, st: Arc<Mutex<Status>>) {
    let mut out: Out = None;
    let mut sink: Option<Sink> = None;
    let mut underruns: Option<Arc<AtomicU64>> = None;
    let mut volume = 1.0_f32;
    let mut frames = DEFAULT_BUFFER_FRAMES;

    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Cmd::Quit) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(Cmd::Play(path)) => {
                sink = None; // detiene la anterior
                underruns = None;
                let result = start(&path, &mut out, volume, frames);
                let mut g = st.lock().unwrap();
                match result {
                    Ok(s) => {
                        sink = Some(s.sink);
                        underruns = Some(s.underruns);
                        *g = Status {
                            state: PlayState::Playing,
                            duration: s.duration,
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
                underruns = None;
                *st.lock().unwrap() = Status::default();
            }
            Ok(Cmd::SetBuffer(n)) => frames = n,
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
            if let Some(u) = &underruns {
                g.underruns = u.load(Relaxed);
            }
            if g.state == PlayState::Playing && s.empty() {
                g.state = PlayState::Stopped;
                g.finished = true;
            }
        }
    }
}
