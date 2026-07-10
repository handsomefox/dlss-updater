use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tracing_subscriber::{EnvFilter, fmt::MakeWriter};

const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;
const MAX_LOG_DAYS: usize = 7;

pub fn init() {
    let Some(directory) = log_directory() else {
        return;
    };
    if fs::create_dir_all(&directory).is_err() {
        return;
    }
    prune_old_logs(&directory);
    let writer = CappedDailyWriter::new(directory);
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_target(true)
        .with_writer(writer)
        .try_init();
}

fn log_directory() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .map(|base| base.join("DLSS Updater/logs"))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
            })
            .map(|base| base.join("dlss-updater/logs"))
    }
}

fn today() -> String {
    jiff::Zoned::now().strftime("%Y-%m-%d").to_string()
}

fn prune_old_logs(directory: &std::path::Path) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut logs: Vec<_> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("dlss-updater-") && name.ends_with(".log"))
        })
        .collect();
    logs.sort();
    let remove = logs.len().saturating_sub(MAX_LOG_DAYS);
    for path in logs.into_iter().take(remove) {
        let _ = fs::remove_file(path);
    }
}

#[derive(Clone)]
struct CappedDailyWriter {
    state: Arc<Mutex<WriterState>>,
}

struct WriterState {
    directory: PathBuf,
    date: String,
    file: Option<File>,
    bytes: u64,
}

impl CappedDailyWriter {
    fn new(directory: PathBuf) -> Self {
        Self {
            state: Arc::new(Mutex::new(WriterState {
                directory,
                date: String::new(),
                file: None,
                bytes: 0,
            })),
        }
    }
}

struct LogWriter(Arc<Mutex<WriterState>>);

impl<'a> MakeWriter<'a> for CappedDailyWriter {
    type Writer = LogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        LogWriter(self.state.clone())
    }
}

impl Write for LogWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| io::Error::other("log lock poisoned"))?;
        let date = today();
        if state.date != date {
            let path = state.directory.join(format!("dlss-updater-{date}.log"));
            let file = OpenOptions::new().create(true).append(true).open(path)?;
            state.bytes = file.metadata()?.len();
            state.file = Some(file);
            state.date = date;
            prune_old_logs(&state.directory);
        }
        if state.bytes >= MAX_LOG_BYTES {
            return Ok(buffer.len());
        }
        let remaining = (MAX_LOG_BYTES - state.bytes) as usize;
        let written = state
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("log file unavailable"))?
            .write(&buffer[..buffer.len().min(remaining)])?;
        state.bytes += written as u64;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| io::Error::other("log lock poisoned"))?;
        state.file.as_mut().map_or(Ok(()), Write::flush)
    }
}
