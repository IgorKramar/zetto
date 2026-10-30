use crate::app::{App, Catalog, Outcome, Record, SearchMode, StageFilter};
use crate::error::{Error, Result};
use crate::index::{Cancellation, FreshnessMode};
use notify::{EventKind, RecursiveMode, Watcher};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU8, Ordering},
    mpsc::{self, Receiver, SyncSender, TrySendError},
};
use std::thread::{self, JoinHandle};

const CAPACITY: usize = 8;
#[derive(Clone, Copy, Debug)]
pub(super) enum Lane {
    Catalog,
    Preview,
    Search,
    Fuzzy,
}
impl Lane {
    fn index(self) -> usize {
        self as usize
    }
}
#[derive(Debug)]
pub(super) enum Read {
    Catalog(FreshnessMode),
    Preview(String),
    Search(String),
    Fuzzy(String),
}
impl Read {
    fn lane(&self) -> Lane {
        match self {
            Self::Catalog(_) => Lane::Catalog,
            Self::Preview(_) => Lane::Preview,
            Self::Search(_) => Lane::Search,
            Self::Fuzzy(_) => Lane::Fuzzy,
        }
    }
}
#[derive(Debug)]
pub(super) enum Write {
    Capture {
        title: String,
        body: String,
    },
    Link {
        source: String,
        target: String,
    },
    Publish {
        source: String,
        target: Option<String>,
    },
    Edit {
        source: String,
    },
}
#[derive(Debug)]
enum Job {
    Read(Read),
    Write(Write),
}
struct Request {
    id: u64,
    token: Cancellation,
    job: Job,
}
pub(super) enum Data {
    Catalog(Catalog),
    Preview(Record),
    Outcome(Outcome),
}
pub(super) struct Reply {
    pub id: u64,
    pub lane: Option<Lane>,
    pub result: Result<Data>,
}

// Атомарный флаг объединяет события, сохраняя требование полного обхода.
#[derive(Clone, Default)]
pub(super) struct WatchFlags(Arc<AtomicU8>);
impl WatchFlags {
    fn event(&self, event: notify::Result<notify::Event>) {
        let level = match event {
            Err(_) => 2,
            Ok(event) if event.need_rescan() => 2,
            Ok(event) if matches!(event.kind, EventKind::Access(_)) => 0,
            Ok(event)
                if matches!(
                    event.kind,
                    EventKind::Create(_)
                        | EventKind::Modify(_)
                        | EventKind::Remove(_)
                        | EventKind::Any
                        | EventKind::Other
                ) =>
            {
                1
            }
            _ => 0,
        };
        self.0.fetch_max(level, Ordering::Relaxed);
    }
    pub fn take(&self) -> Option<FreshnessMode> {
        match self.0.swap(0, Ordering::Relaxed) {
            0 => None,
            1 => Some(FreshnessMode::Stat),
            _ => Some(FreshnessMode::Full),
        }
    }
}
pub(super) struct Worker {
    tx: Option<SyncSender<Request>>,
    pub rx: Receiver<Reply>,
    next: u64,
    latest: [u64; 4],
    tokens: [Option<Cancellation>; 4],
    pending: [Option<Request>; 4],
    pub watch: WatchFlags,
    warning: Arc<Mutex<Option<String>>>,
    join: Option<JoinHandle<()>>,
}
impl Worker {
    pub fn start(app: App) -> Self {
        let (tx, requests) = mpsc::sync_channel(CAPACITY);
        let (responses, rx) = mpsc::sync_channel(CAPACITY);
        let watch = WatchFlags::default();
        let flags = watch.clone();
        let warning = Arc::new(Mutex::new(None));
        let watch_warning = warning.clone();
        let join = thread::spawn(move || {
            let root = app.vault.root().to_owned();
            let event_root = root.clone();
            let watcher =
                notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                    if let Ok(event) = &event {
                        // Recovery, блокировки и git не входят в каталог.
                        if !event.need_rescan()
                            && !event.paths.is_empty()
                            && event.paths.iter().all(|path| {
                                path.strip_prefix(&event_root)
                                    .ok()
                                    .and_then(|p| p.components().next())
                                    .is_some_and(|c| {
                                        c.as_os_str() == ".zetto" || c.as_os_str() == ".git"
                                    })
                            })
                        {
                            return;
                        }
                    }
                    flags.event(event);
                })
                .and_then(|mut watcher| {
                    watcher.watch(&root, RecursiveMode::Recursive)?;
                    Ok(watcher)
                });
            if let Err(error) = &watcher {
                *watch_warning.lock().unwrap() = Some(format!(
                    "! Наблюдение недоступно: {error}; периодическая сверка включена"
                ));
            }
            // Рекурсивный watch переживает замену каталога inbox.
            let _watcher = watcher;
            for request in requests {
                let Request { id, token, job } = request;
                let lane = match &job {
                    Job::Read(read) => Some(read.lane()),
                    Job::Write(_) => None,
                };
                let result = execute(&app, job, &token);
                if responses.send(Reply { id, lane, result }).is_err() {
                    break;
                }
            }
        });
        Self {
            tx: Some(tx),
            rx,
            next: 0,
            latest: [0; 4],
            tokens: Default::default(),
            pending: Default::default(),
            watch,
            warning,
            join: Some(join),
        }
    }
    fn id(&mut self) -> u64 {
        self.next += 1;
        self.next
    }
    pub fn read(&mut self, read: Read) -> u64 {
        let lane = read.lane().index();
        if let Some(token) = self.tokens[lane].take() {
            token.cancel();
        }
        let token = Cancellation::default();
        let id = self.id();
        self.latest[lane] = id;
        self.tokens[lane] = Some(token.clone());
        self.pending[lane] = Some(Request {
            id,
            token,
            job: Job::Read(read),
        });
        self.flush();
        id
    }
    pub fn flush(&mut self) {
        for slot in &mut self.pending {
            if let Some(request) = slot.take() {
                match self.tx.as_ref().unwrap().try_send(request) {
                    Ok(()) => {}
                    Err(TrySendError::Full(request)) => *slot = Some(request),
                    Err(TrySendError::Disconnected(request)) => {
                        // Только последний запрос; UI обнаружит разрыв канала rx.
                        *slot = Some(request);
                    }
                }
            }
        }
    }
    pub fn current(&self, lane: Lane, id: u64) -> bool {
        self.latest[lane.index()] == id
    }
    pub fn cancel(&mut self, lane: Lane) {
        let i = lane.index();
        if let Some(token) = self.tokens[i].take() {
            token.cancel();
        }
        self.pending[i] = None;
        self.latest[i] = 0;
    }
    pub fn cancel_reads(&mut self) {
        for lane in [Lane::Catalog, Lane::Preview, Lane::Search, Lane::Fuzzy] {
            self.cancel(lane);
        }
    }
    pub fn write(&mut self, write: Write) -> Result<u64> {
        self.cancel_reads();
        let id = self.id();
        let request = Request {
            id,
            token: Cancellation::local_only(),
            job: Job::Write(write),
        };
        self.tx
            .as_ref()
            .unwrap()
            .try_send(request)
            .map_err(|error| match error {
                TrySendError::Full(_) => Error::new(
                    "queue_full",
                    "Очередь занята; ввод сохранён в форме, повторите запись",
                ),
                TrySendError::Disconnected(_) => {
                    Error::new("io", "Рабочий поток недоступен; запись не принята")
                }
            })?;
        Ok(id)
    }
    pub fn warning(&self) -> Option<String> {
        self.warning.lock().unwrap().take()
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel_reads();
        self.tx.take();
        // Принятые записи завершаются при ошибке терминала и раскрутке panic.
        while self.rx.recv().is_ok() {}
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}
fn execute(app: &App, job: Job, token: &Cancellation) -> Result<Data> {
    match job {
        Job::Read(read) => {
            token.check()?;
            let cached = app.clone().with_freshness(FreshnessMode::Cached);
            match read {
                Read::Catalog(mode) => app
                    .clone()
                    .with_freshness(mode)
                    .catalog(token)
                    .map(Data::Catalog),
                Read::Preview(path) => app.preview(&path, token).map(Data::Preview),
                Read::Search(query) => cached
                    .search(&query, SearchMode::Default, 1000, 0, token)
                    .map(Data::Outcome),
                Read::Fuzzy(query) => cached
                    .fuzzy_titles(&query, StageFilter::Published, 50, token)
                    .map(Data::Outcome),
            }
        }
        Job::Write(write) => match write {
            Write::Capture { title, body } => app.capture(&title, &body),
            Write::Link { source, target } => app.link(&source, &target, None),
            Write::Publish { source, target } => app.publish(&source, target.as_deref()),
            Write::Edit { source } => {
                crate::editor::command().and_then(|argv| app.edit(&source, &argv))
            }
        }
        .map(Data::Outcome),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn queue(capacity: usize) -> (Worker, Receiver<Request>) {
        let (tx, requests) = mpsc::sync_channel(capacity);
        let (_, rx) = mpsc::sync_channel(CAPACITY);
        (
            Worker {
                tx: Some(tx),
                rx,
                next: 0,
                latest: [0; 4],
                tokens: Default::default(),
                pending: Default::default(),
                watch: Default::default(),
                warning: Default::default(),
                join: None,
            },
            requests,
        )
    }
    #[test]
    fn bounded_queue_keeps_only_latest_read_and_cancels_running_query() {
        let (mut worker, requests) = queue(1);
        let old = worker.read(Read::Search("old".into()));
        let running = requests.recv().unwrap();
        let token = running.token.clone();
        let (done, finished) = mpsc::sync_channel(1);
        let query = thread::spawn(move || {
            while token.check().is_ok() {
                thread::yield_now();
            }
            done.send(()).unwrap();
        });
        worker.read(Read::Preview("preview".into())); // Занимаем ограниченный канал.
        for i in 0..1000 {
            worker.read(Read::Search(i.to_string()));
        }
        assert_eq!(worker.pending.iter().flatten().count(), 1);
        assert!(!worker.current(Lane::Search, old));
        finished
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        query.join().unwrap();
        assert!(running.token.is_cancelled());
        requests.recv().unwrap();
        worker.flush();
        let latest = requests.recv().unwrap();
        assert!(worker.current(Lane::Search, latest.id));
        assert!(matches!(latest.job, Job::Read(Read::Search(ref query)) if query == "999"));
        assert!(!latest.token.is_cancelled());
    }
    #[test]
    fn accepted_mutation_survives_read_cancellation_and_full_queue_rejects_new_write() {
        let (mut worker, requests) = queue(1);
        let id = worker
            .write(Write::Capture {
                title: String::new(),
                body: "accepted".into(),
            })
            .unwrap();
        worker.read(Read::Search("query".into()));
        worker.cancel_reads();
        assert_eq!(
            worker
                .write(Write::Capture {
                    title: String::new(),
                    body: "rejected".into()
                })
                .unwrap_err()
                .code,
            "queue_full"
        );
        let accepted = requests.recv().unwrap();
        assert_eq!(accepted.id, id);
        assert!(!accepted.token.is_cancelled());
        assert!(
            matches!(accepted.job, Job::Write(Write::Capture { body, .. }) if body == "accepted")
        );
        assert!(requests.try_recv().is_err());
    }
    #[test]
    fn watch_loss_requires_full_and_access_is_ignored() {
        let flags = WatchFlags::default();
        flags.event(Ok(notify::Event::new(notify::EventKind::Access(
            notify::event::AccessKind::Any,
        ))));
        assert_eq!(flags.take(), None);
        flags.event(Err(notify::Error::generic("overflow")));
        assert_eq!(flags.take(), Some(FreshnessMode::Full));
        let mut event = notify::Event::new(EventKind::Other);
        event.attrs.set_flag(notify::event::Flag::Rescan);
        flags.event(Ok(event));
        for _ in 0..1000 {
            flags.event(Ok(notify::Event::new(EventKind::Create(
                notify::event::CreateKind::File,
            ))));
        }
        assert_eq!(flags.take(), Some(FreshnessMode::Full));
        assert_eq!(flags.take(), None);
    }
}
