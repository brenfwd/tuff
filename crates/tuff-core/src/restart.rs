//! Process-wide restart request: `/tuff restart` records it and stops the
//! server; the binary then either re-executes itself or exits with
//! [`EXIT_CODE`] for a supervisor, per [`Mode`].

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::oneshot;

/// The status a restarting server exits with (`EX_TEMPFAIL`).
pub const EXIT_CODE: i32 = 75;

static REQUESTED: AtomicBool = AtomicBool::new(false);
static PENDING: Mutex<Option<oneshot::Sender<()>>> = Mutex::new(None);

pub fn requested() -> bool {
    REQUESTED.load(Ordering::SeqCst)
}

pub const MODE_KEY: &str = "tuff.restart.mode";

/// What the process does after the clean shutdown of a restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Re-execute the current binary with the same arguments.
    SelfExec,
    /// Exit with [`EXIT_CODE`] for a supervisor to start it again.
    Exit,
}

impl Mode {
    pub fn parse(text: &str) -> Option<Mode> {
        match text {
            "self" => Some(Mode::SelfExec),
            "exit" => Some(Mode::Exit),
            _ => None,
        }
    }
}

static MODE: Mutex<Mode> = Mutex::new(Mode::SelfExec);

pub fn mark(mode: Mode) {
    *MODE.lock().unwrap_or_else(|e| e.into_inner()) = mode;
    REQUESTED.store(true, Ordering::SeqCst);
}

pub const BROADCAST_KEY: &str = "tuff.restart.broadcast_countdown";
pub const DISCONNECT_MESSAGE_KEY: &str = "tuff.restart.disconnect_message";
pub const DISCONNECT_TEXT_KEY: &str = "tuff.restart.disconnect_text";
pub const DEFAULT_DISCONNECT_TEXT: &str = "Server is restarting, please reconnect shortly";

static DISCONNECT_TEXT: Mutex<Option<String>> = Mutex::new(None);

/// Sets the text players are disconnected with for this restart; `None`
/// keeps the generic shutdown text.
pub fn set_disconnect_text(text: Option<String>) {
    *DISCONNECT_TEXT.lock().unwrap_or_else(|e| e.into_inner()) = text;
}

/// What a player is disconnected with while the server stops.
pub fn shutdown_reason() -> tuff_item::text::TextComponent {
    let custom = requested()
        .then(|| {
            DISCONNECT_TEXT
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        })
        .flatten();
    match custom {
        Some(text) => crate::world::commands::text(text),
        None => {
            tuff_item::text::TextComponent::translate("multiplayer.disconnect.server_shutdown", [])
        }
    }
}

pub fn clear() {
    REQUESTED.store(false, Ordering::SeqCst);
    set_disconnect_text(None);
    cancel();
}

/// The executable, arguments and (inherited) environment a restart re-runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relaunch {
    pub exe: PathBuf,
    pub args: Vec<OsString>,
}

impl Relaunch {
    pub fn current() -> std::io::Result<Relaunch> {
        Ok(Relaunch {
            exe: std::env::current_exe()?,
            args: std::env::args_os().skip(1).collect(),
        })
    }

    /// Replaces the process (Unix, keeping PID and terminal) or starts a
    /// new one and exits (elsewhere). Returns only on failure.
    pub fn exec(&self) -> std::io::Error {
        let mut command = std::process::Command::new(&self.exe);
        command.args(&self.args);
        #[cfg(unix)]
        {
            std::os::unix::process::CommandExt::exec(&mut command)
        }
        #[cfg(not(unix))]
        match command.spawn() {
            Ok(_) => std::process::exit(0),
            Err(err) => err,
        }
    }
}

/// What the binary does once the server has stopped.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Nothing,
    Exit(i32),
    Relaunch(Relaunch),
}

/// The action for the current restart state; `relaunch` supplies the
/// command line so tests need not touch the real process.
pub fn action(relaunch: impl FnOnce() -> std::io::Result<Relaunch>) -> Action {
    if !requested() {
        return Action::Nothing;
    }
    let mode = *MODE.lock().unwrap_or_else(|e| e.into_inner());
    match mode {
        Mode::SelfExec => relaunch().map_or(Action::Exit(EXIT_CODE), Action::Relaunch),
        Mode::Exit => Action::Exit(EXIT_CODE),
    }
}

/// Registers a countdown; `None` when one is already running.
pub fn begin_countdown() -> Option<oneshot::Receiver<()>> {
    let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    if pending.as_ref().is_some_and(|tx| !tx.is_closed()) {
        return None;
    }
    let (tx, rx) = oneshot::channel();
    *pending = Some(tx);
    Some(rx)
}

/// Cancels the running countdown; false if none was running.
pub fn cancel() -> bool {
    let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    pending.take().is_some_and(|tx| tx.send(()).is_ok())
}

pub fn finish_countdown() {
    PENDING.lock().unwrap_or_else(|e| e.into_inner()).take();
}
