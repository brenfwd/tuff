//! A server's timings controller: an actor that starts and stops the
//! process-wide recorder for that server, builds and saves the reports
//! and serves past ones. Commands and the admin panel go through
//! [`Timings`].

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use tokio::sync::{mpsc, oneshot};

use super::profile::{Labels, Profile};
use super::recorder::{self, RecorderError};
use super::report::Report;

/// Where reports go in the server file area.
pub const DIRECTORY: &str = "timings";
/// Largest report read back from the file area.
pub const MAX_REPORT_BYTES: usize = 256 << 20;
const KEPT_IN_MEMORY: usize = 4;
const QUEUE: usize = 16;

/// The server file a report is saved as.
pub fn report_file(id: &str) -> String {
    format!("{DIRECTORY}/{id}.json")
}

#[derive(Clone, Debug, Default)]
pub struct StartOptions {
    /// Stops and saves on its own after this long.
    pub duration: Option<Duration>,
    /// Who started it, for status lines.
    pub by: String,
}

/// The recording in progress.
#[derive(Clone, Debug, PartialEq)]
pub struct Recording {
    pub started_unix_ms: u64,
    pub elapsed: Duration,
    pub threads: usize,
    pub stops_after: Option<Duration>,
    pub by: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    /// This server's recording, if one runs.
    pub recording: Option<Recording>,
    /// Another server in this process is recording.
    pub busy_elsewhere: bool,
    /// The last report this server made.
    pub last: Option<Summary>,
}

/// What a stopped recording produced.
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub id: String,
    /// The server file it was saved as, or why it could not be.
    pub file: Result<String, String>,
    pub bytes: usize,
    pub duration: Duration,
    pub ticks: u64,
    pub tps: Option<f64>,
    pub mspt_mean: Option<f64>,
    pub mspt_p99: Option<f64>,
    pub mspt_max: Option<f64>,
    /// The spans with the most self time: (label, self ns).
    pub top: Vec<(String, u64)>,
    /// Feature hook time: (feature id, total ns), most first.
    pub features: Vec<(String, u64)>,
    pub threads: usize,
    pub dropped: u64,
}

/// A saved or remembered report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    pub id: String,
    pub file: String,
    pub started_unix_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TimingsError {
    Recorder(RecorderError),
    /// Another server in this process owns the running recording.
    ElsewhereRecording,
    /// The controller is not running (a detached handle or shutdown).
    Unavailable,
    InvalidId(String),
    NotFound(String),
    Io(String),
    Corrupt(String),
}

impl std::fmt::Display for TimingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Recorder(err) => err.fmt(f),
            Self::ElsewhereRecording => {
                f.write_str("another server in this process is recording timings")
            }
            Self::Unavailable => f.write_str("timings are not available on this server"),
            Self::InvalidId(id) => write!(f, "invalid timings report name {id:?}"),
            Self::NotFound(id) => write!(f, "no timings report named {id:?}"),
            Self::Io(err) => write!(f, "timings report storage failed: {err}"),
            Self::Corrupt(err) => write!(f, "unreadable timings report: {err}"),
        }
    }
}

impl std::error::Error for TimingsError {}

/// Export formats for [`Timings::export`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Export {
    Json,
    Folded,
    Speedscope,
}

impl Export {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "json" => Some(Self::Json),
            "folded" => Some(Self::Folded),
            "speedscope" => Some(Self::Speedscope),
            _ => None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Folded => "folded",
            Self::Speedscope => "speedscope.json",
        }
    }
}

enum Request {
    #[cfg(test)]
    Panic,
    Start(
        StartOptions,
        oneshot::Sender<Result<Recording, TimingsError>>,
    ),
    Stop(oneshot::Sender<Result<Summary, TimingsError>>),
    Status(oneshot::Sender<Status>),
    List(oneshot::Sender<Result<Vec<Info>, TimingsError>>),
    Fetch(String, oneshot::Sender<Result<Bytes, TimingsError>>),
    Save(String, Bytes, oneshot::Sender<Result<String, TimingsError>>),
}

/// A handle to a server's timings controller. Requests are bounded and
/// awaited, so a burst of panel calls waits rather than piling up.
#[derive(Clone, Debug)]
pub struct Timings {
    requests: mpsc::Sender<Request>,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Request")
    }
}

impl Timings {
    /// Starts the controller; `files` is the server file area reports are
    /// saved in (`None` keeps them in memory only).
    pub fn spawn(
        files: Option<Arc<dyn ReportFiles>>,
        features: Vec<&'static str>,
        labels: Labels,
    ) -> Self {
        Self::spawn_with(files, features, labels, |mut runner| {
            tokio::spawn(async move { runner.run().await });
        })
    }

    /// [`Timings::spawn`] running the controller through `spawn`, so the
    /// host can supervise it.
    pub fn spawn_with(
        files: Option<Arc<dyn ReportFiles>>,
        features: Vec<&'static str>,
        labels: Labels,
        spawn: impl FnOnce(Runner),
    ) -> Self {
        let (requests, inbox) = mpsc::channel(QUEUE);
        let actor = Controller {
            files,
            features,
            labels,
            inbox,
            current: None,
            recent: VecDeque::new(),
            last: None,
        };
        spawn(Runner(actor));
        Self { requests }
    }

    #[cfg(test)]
    pub(crate) async fn panic_the_controller(&self) {
        self.requests.send(Request::Panic).await.unwrap();
    }

    /// A handle whose every request fails with [`TimingsError::Unavailable`].
    pub fn detached() -> Self {
        Self {
            requests: mpsc::channel(1).0,
        }
    }

    async fn ask<T>(
        &self,
        request: impl FnOnce(oneshot::Sender<T>) -> Request,
    ) -> Result<T, TimingsError> {
        let (reply, answer) = oneshot::channel();
        self.requests
            .send(request(reply))
            .await
            .map_err(|_| TimingsError::Unavailable)?;
        answer.await.map_err(|_| TimingsError::Unavailable)
    }

    /// Starts recording timings.
    pub async fn start(&self, options: StartOptions) -> Result<Recording, TimingsError> {
        self.ask(|reply| Request::Start(options, reply)).await?
    }

    /// Stops this server's recording, saves the report and summarizes it.
    pub async fn stop(&self) -> Result<Summary, TimingsError> {
        self.ask(Request::Stop).await?
    }

    pub async fn status(&self) -> Result<Status, TimingsError> {
        self.ask(Request::Status).await
    }

    /// Saved and remembered reports, newest first.
    pub async fn list(&self) -> Result<Vec<Info>, TimingsError> {
        self.ask(Request::List).await?
    }

    /// A report's JSON (see [`super::report`]).
    pub async fn fetch(&self, id: &str) -> Result<Bytes, TimingsError> {
        self.ask(|reply| Request::Fetch(id.to_owned(), reply))
            .await?
    }

    /// Converts a report and saves it beside the report as
    /// `timings/<id>.<extension>`, returning that server file.
    pub async fn save_export(&self, id: &str, format: Export) -> Result<String, TimingsError> {
        let bytes = self.export(id, format).await?;
        let file = format!("{DIRECTORY}/{id}.{}", format.extension());
        self.ask(|reply| Request::Save(file, bytes, reply)).await?
    }

    /// A report converted to `format`.
    pub async fn export(&self, id: &str, format: Export) -> Result<Bytes, TimingsError> {
        let json = self.fetch(id).await?;
        if format == Export::Json {
            return Ok(json);
        }
        tokio::task::spawn_blocking(move || {
            let report = Report::from_json(&json).map_err(TimingsError::Corrupt)?;
            Ok(Bytes::from(match format {
                Export::Folded => report.folded().into_bytes(),
                _ => serde_json::to_vec(&report.speedscope()).expect("json serializes"),
            }))
        })
        .await
        .map_err(|_| TimingsError::Unavailable)?
    }
}

struct Current {
    started: SystemTime,
    deadline: Option<tokio::time::Instant>,
    stops_after: Option<Duration>,
    by: String,
}

struct Controller {
    files: Option<Arc<dyn ReportFiles>>,
    features: Vec<&'static str>,
    labels: Labels,
    inbox: mpsc::Receiver<Request>,
    current: Option<Current>,
    recent: VecDeque<(String, u64, Bytes)>,
    last: Option<Summary>,
}

fn unix_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.starts_with('.')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// The controller's state; see [`Timings::spawn_with`]. Running it again
/// after a panic resumes with its recording and reports.
pub struct Runner(Controller);

impl Runner {
    pub async fn run(&mut self) {
        self.0.run().await;
    }
}

impl Controller {
    async fn run(&mut self) {
        loop {
            let deadline = self.current.as_ref().and_then(|c| c.deadline);
            tokio::select! {
                request = self.inbox.recv() => match request {
                    Some(request) => self.handle(request).await,
                    None => break,
                },
                _ = async { tokio::time::sleep_until(deadline.expect("guarded")).await },
                    if deadline.is_some() =>
                {
                    match self.finish().await {
                        Ok(summary) => log::info!(
                            "timings recording stopped after {:.1} s: {}",
                            summary.duration.as_secs_f64(),
                            summary.file.as_deref().unwrap_or("not saved")
                        ),
                        Err(err) => log::warn!("timings recording could not stop: {err}"),
                    }
                }
            }
        }
        if self.current.is_some()
            && let Err(err) = self.finish().await
        {
            log::warn!("timings recording could not stop: {err}");
        }
    }

    async fn handle(&mut self, request: Request) {
        match request {
            #[cfg(test)]
            Request::Panic => panic!("test panic"),
            Request::Start(options, reply) => {
                let _ = reply.send(self.start(options));
            }
            Request::Stop(reply) => {
                let result = if self.current.is_some() {
                    self.finish().await
                } else if recorder::is_recording() {
                    Err(TimingsError::ElsewhereRecording)
                } else {
                    Err(TimingsError::Recorder(RecorderError::NotRecording))
                };
                let _ = reply.send(result);
            }
            Request::Status(reply) => {
                let _ = reply.send(self.status());
            }
            Request::List(reply) => {
                let _ = reply.send(self.list().await);
            }
            Request::Fetch(id, reply) => {
                let _ = reply.send(self.fetch(id).await);
            }
            Request::Save(file, bytes, reply) => {
                let _ = reply.send(self.save(file, bytes).await);
            }
        }
    }

    fn start(&mut self, options: StartOptions) -> Result<Recording, TimingsError> {
        if self.current.is_some() {
            return Err(TimingsError::Recorder(RecorderError::AlreadyRecording));
        }
        recorder::start().map_err(|err| match err {
            RecorderError::AlreadyRecording => TimingsError::ElsewhereRecording,
            other => TimingsError::Recorder(other),
        })?;
        self.current = Some(Current {
            started: SystemTime::now(),
            deadline: options
                .duration
                .map(|after| tokio::time::Instant::now() + after),
            stops_after: options.duration,
            by: options.by,
        });
        Ok(self.recording().expect("just started"))
    }

    fn recording(&self) -> Option<Recording> {
        let current = self.current.as_ref()?;
        let (elapsed, threads) =
            recorder::running().map_or((Duration::ZERO, 0), |(_, e, t)| (e, t));
        Some(Recording {
            started_unix_ms: unix_ms(current.started),
            elapsed,
            threads,
            stops_after: current.stops_after,
            by: current.by.clone(),
        })
    }

    fn status(&self) -> Status {
        let recording = self.recording();
        Status {
            busy_elsewhere: recording.is_none() && recorder::is_recording(),
            recording,
            last: self.last.clone(),
        }
    }

    fn unique_id(&self, started: SystemTime, saved: &[String]) -> String {
        let base = format!("timings-{}", file_stamp(unix_ms(started) as i64 / 1000));
        let taken = |id: &str| {
            self.recent.iter().any(|(r, _, _)| r == id)
                || saved.iter().any(|f| *f == report_file(id))
        };
        let mut id = base.clone();
        let mut n = 2;
        while taken(&id) {
            id = format!("{base}-{n}");
            n += 1;
        }
        id
    }

    async fn finish(&mut self) -> Result<Summary, TimingsError> {
        self.current = None;
        let snapshot = recorder::stop().map_err(TimingsError::Recorder)?;
        let saved = self.saved_files().await.unwrap_or_default();
        let id = self.unique_id(snapshot.started, &saved);
        let features = self.features.clone();
        let labels = self.labels.clone();
        let files = self.files.clone();
        let built = {
            let id = id.clone();
            tokio::task::spawn_blocking(move || {
                let profile = Profile::from_snapshot(snapshot);
                let report = Report::new(&id, &profile, &features, &labels);
                let json = Bytes::from(report.to_json());
                let file = match &files {
                    Some(files) => files
                        .write_file(&report_file(&id), &json)
                        .map(|()| report_file(&id))
                        .map_err(|err| err.to_string()),
                    None => Err("no server file area".to_owned()),
                };
                let summary = summarize(&id, &profile, &report, &labels, file, json.len());
                (summary, json, report.started_unix_ms)
            })
            .await
        };
        let (summary, json, started) = built.map_err(|_| TimingsError::Unavailable)?;
        if let Err(err) = &summary.file {
            log::warn!("timings report {id} kept in memory only: {err}");
        }
        self.recent.push_back((id, started, json));
        while self.recent.len() > KEPT_IN_MEMORY {
            self.recent.pop_front();
        }
        self.last = Some(summary.clone());
        Ok(summary)
    }

    async fn saved_files(&self) -> Result<Vec<String>, TimingsError> {
        let Some(files) = self.files.clone() else {
            return Ok(Vec::new());
        };
        tokio::task::spawn_blocking(move || files.list_files(DIRECTORY))
            .await
            .map_err(|_| TimingsError::Unavailable)?
            .or_else(|err| {
                if err.kind() == std::io::ErrorKind::Unsupported {
                    Ok(Vec::new())
                } else {
                    Err(TimingsError::Io(err.to_string()))
                }
            })
    }

    async fn list(&self) -> Result<Vec<Info>, TimingsError> {
        let mut out: Vec<Info> = self
            .recent
            .iter()
            .map(|(id, started, _)| Info {
                id: id.clone(),
                file: report_file(id),
                started_unix_ms: Some(*started),
            })
            .collect();
        for file in self.saved_files().await? {
            let Some(id) = file
                .strip_prefix(DIRECTORY)
                .and_then(|f| f.strip_prefix('/'))
                .and_then(|f| f.strip_suffix(".json"))
                .filter(|id| !id.contains('.'))
            else {
                continue;
            };
            if valid_id(id) && !out.iter().any(|info| info.id == id) {
                out.push(Info {
                    id: id.to_owned(),
                    file,
                    started_unix_ms: None,
                });
            }
        }
        out.sort_by(|a, b| b.id.cmp(&a.id));
        Ok(out)
    }

    async fn save(&self, file: String, bytes: Bytes) -> Result<String, TimingsError> {
        let Some(files) = self.files.clone() else {
            return Err(TimingsError::Io("no server file area".to_owned()));
        };
        tokio::task::spawn_blocking(move || files.write_file(&file, &bytes).map(|()| file))
            .await
            .map_err(|_| TimingsError::Unavailable)?
            .map_err(|err| TimingsError::Io(err.to_string()))
    }

    async fn fetch(&self, id: String) -> Result<Bytes, TimingsError> {
        if !valid_id(&id) {
            return Err(TimingsError::InvalidId(id));
        }
        if let Some((_, _, json)) = self.recent.iter().find(|(r, _, _)| *r == id) {
            return Ok(json.clone());
        }
        let Some(files) = self.files.clone() else {
            return Err(TimingsError::NotFound(id));
        };
        let path = report_file(&id);
        let read = tokio::task::spawn_blocking(move || files.read_file(&path, MAX_REPORT_BYTES))
            .await
            .map_err(|_| TimingsError::Unavailable)?;
        match read {
            Ok(Some(bytes)) => Ok(Bytes::from(bytes)),
            Ok(None) => Err(TimingsError::NotFound(id)),
            Err(err) if err.kind() == std::io::ErrorKind::Unsupported => {
                Err(TimingsError::NotFound(id))
            }
            Err(err) => Err(TimingsError::Io(err.to_string())),
        }
    }
}

fn summarize(
    id: &str,
    profile: &Profile,
    report: &Report,
    labels: &Labels,
    file: Result<String, String>,
    bytes: usize,
) -> Summary {
    let ticks = report.ticks.as_ref();
    Summary {
        id: id.to_owned(),
        file,
        bytes,
        duration: profile.duration,
        ticks: ticks.map_or(0, |t| t.count),
        tps: ticks.map(|t| t.tps),
        mspt_mean: ticks.map(|t| t.mspt.mean),
        mspt_p99: ticks.map(|t| t.mspt.p99),
        mspt_max: ticks.map(|t| t.mspt.max),
        top: profile
            .top_self(5)
            .into_iter()
            .map(|(name, tag, ns)| {
                let label = match labels.tag(tag) {
                    Some(tag) => format!("{name} [{tag}]"),
                    None => name.to_owned(),
                };
                (label, ns)
            })
            .collect(),
        features: report
            .features
            .iter()
            .filter(|p| p.calls > 0)
            .map(|p| (p.id.clone(), p.total_ns))
            .collect(),
        threads: profile.threads.len(),
        dropped: profile.dropped(),
    }
}

/// The server file area timing reports are saved in.
pub trait ReportFiles: Send + Sync + 'static {
    fn list_files(&self, prefix: &str) -> std::io::Result<Vec<String>>;
    /// A file, refusing one over `max` bytes; `None` if missing.
    fn read_file(&self, path: &str, max: usize) -> std::io::Result<Option<Vec<u8>>>;
    fn write_file(&self, path: &str, contents: &[u8]) -> std::io::Result<()>;
}

/// `YYYYMMDD-HHMMSS` in UTC.
pub fn file_stamp(unix_seconds: i64) -> String {
    let days = unix_seconds.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let secs = unix_seconds.rem_euclid(86_400);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}
