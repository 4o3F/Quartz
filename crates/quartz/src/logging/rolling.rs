//! Bounded records, an idle-blocking writer, and acknowledged panic diagnostics.

use crossbeam_channel::{Receiver, Sender};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// Per-file log budget from architecture 01 §7.
pub(super) const LOG_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Retain the current file and two archives per target (01 §7).
pub(super) const LOG_FILE_COUNT: usize = 3;
/// Formatted record byte budget from architecture 01 §7.
const RECORD_BYTES: usize = 4096;
/// Record count budget from architecture 01 §7.
const QUEUED_RECORDS: usize = 64;
/// Bound orderly log flushing within the core shutdown budget (01 §6).
const LOG_STOP_TIMEOUT: Duration = Duration::from_secs(2);
/// Panic flushing must finish before abort; bound disk stalls to the log shutdown budget (01 §7).
const PANIC_FLUSH_TIMEOUT: Duration = LOG_STOP_TIMEOUT;
/// Stop and panic requests have a reserved control position independent of ordinary records.
const LOG_CONTROL_CAPACITY: usize = 1;
/// A worker publishes one completion result.
const LOG_COMPLETION_CAPACITY: usize = 1;
const LOG_THREAD_NAME: &str = "quartz-log";
/// Panic payloads can contain user content; only this fixed diagnostic is persisted.
const PANIC_RECORD: &[u8] = b"core thread panicked\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogLife {
    Running,
    Stopped,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum RecordLength {
    Within,
    Exceeded,
}
enum LogControl {
    Stop,
    FlushPanic { reply: Sender<io::Result<()>> },
}
enum LogInput {
    Record(Vec<u8>),
    Control(LogControl),
    Disconnected,
}
struct LogInbox {
    records: Receiver<Vec<u8>>,
    controls: Receiver<LogControl>,
    dropped: Arc<AtomicUsize>,
    #[cfg(test)]
    waits: Arc<AtomicUsize>,
}
impl LogInbox {
    fn receive(&self) -> LogInput {
        #[cfg(test)]
        self.waits.fetch_add(1, Ordering::Relaxed);
        crossbeam_channel::select_biased! {
            recv(self.controls) -> result => match result { Ok(control) => LogInput::Control(control), Err(_) => LogInput::Disconnected },
            recv(self.records) -> result => match result { Ok(record) => LogInput::Record(record), Err(_) => LogInput::Disconnected },
        }
    }
}
#[derive(Clone)]
pub(super) struct LogWriter {
    sender: Sender<Vec<u8>>,
    life: Arc<Mutex<LogLife>>,
    dropped: Arc<AtomicUsize>,
    #[cfg(test)]
    waits: Arc<AtomicUsize>,
}
pub(super) struct LogRecord {
    writer: LogWriter,
    bytes: Vec<u8>,
    length: RecordLength,
}
impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for LogWriter {
    type Writer = LogRecord;
    fn make_writer(&'writer self) -> Self::Writer {
        LogRecord {
            writer: self.clone(),
            bytes: Vec::new(),
            length: RecordLength::Within,
        }
    }
}
impl Write for LogRecord {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let available = RECORD_BYTES.saturating_sub(self.bytes.len());
        if bytes.len() > available {
            self.length = RecordLength::Exceeded;
        }
        self.bytes
            .extend_from_slice(&bytes[..available.min(bytes.len())]);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Drop for LogRecord {
    fn drop(&mut self) {
        if self.bytes.is_empty() {
            return;
        }
        let life = self
            .writer
            .life
            .lock()
            .expect("log admission only mutates bounded memory");
        if *life == LogLife::Stopped {
            return;
        }
        if self.length == RecordLength::Exceeded {
            self.bytes = b"log record exceeded the record byte limit\n".to_vec();
        }
        if self
            .writer
            .sender
            .try_send(std::mem::take(&mut self.bytes))
            .is_err()
        {
            self.writer.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}
#[derive(Clone, Debug)]
pub(crate) struct PanicLog {
    controls: Sender<LogControl>,
    path: PathBuf,
}
impl PanicLog {
    pub(crate) fn record(&self) -> io::Result<()> {
        if thread::current().name() == Some(LOG_THREAD_NAME) {
            // A panic in the writer itself cannot wait for its own acknowledgement.
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?;
            file.write_all(PANIC_RECORD)?;
            return file.flush();
        }
        let deadline = Instant::now() + PANIC_FLUSH_TIMEOUT;
        let (reply, received) = crossbeam_channel::bounded(LOG_COMPLETION_CAPACITY);
        self.controls
            .send_deadline(LogControl::FlushPanic { reply }, deadline)
            .map_err(io::Error::other)?;
        received.recv_deadline(deadline).map_err(io::Error::other)?
    }
}
#[derive(Debug)]
pub(crate) struct FileLogGuard {
    life: Arc<Mutex<LogLife>>,
    controls: Sender<LogControl>,
    finished: Receiver<io::Result<()>>,
    worker: Option<JoinHandle<()>>,
    panic: Option<PanicLog>,
}
impl FileLogGuard {
    pub(crate) fn panic_log(&self) -> PanicLog {
        self.panic
            .clone()
            .expect("file log has a panic destination")
    }
    pub(super) fn start(log: RollingLog) -> io::Result<LogWorker> {
        let path = log.path.clone();
        let mut worker = Self::start_with(move |inbox| log.run(inbox))?;
        worker.guard.panic = Some(PanicLog {
            controls: worker.guard.controls.clone(),
            path,
        });
        Ok(worker)
    }
    fn start_with(
        write: impl FnOnce(LogInbox) -> io::Result<()> + Send + 'static,
    ) -> io::Result<LogWorker> {
        let (sender, records) = crossbeam_channel::bounded(QUEUED_RECORDS);
        let (controls, control_input) = crossbeam_channel::bounded(LOG_CONTROL_CAPACITY);
        let (completed, finished) = crossbeam_channel::bounded(LOG_COMPLETION_CAPACITY);
        let life = Arc::new(Mutex::new(LogLife::Running));
        let dropped = Arc::new(AtomicUsize::new(0));
        #[cfg(test)]
        let waits = Arc::new(AtomicUsize::new(0));
        let writer = LogWriter {
            sender,
            life: Arc::clone(&life),
            dropped: Arc::clone(&dropped),
            #[cfg(test)]
            waits: Arc::clone(&waits),
        };
        let worker = thread::Builder::new()
            .name(LOG_THREAD_NAME.into())
            .spawn(move || {
                let outcome = write(LogInbox {
                    records,
                    controls: control_input,
                    dropped,
                    #[cfg(test)]
                    waits,
                });
                if let Err(error) = &outcome {
                    eprintln!("could not write rolling diagnostics: {error}");
                }
                let _ = completed.send(outcome);
            })?;
        Ok(LogWorker {
            writer,
            guard: Self {
                life,
                controls,
                finished,
                worker: Some(worker),
                panic: None,
            },
        })
    }
    fn finish(&mut self) -> io::Result<()> {
        *self.life.lock().expect("log lifecycle lock") = LogLife::Stopped;
        let deadline = Instant::now() + LOG_STOP_TIMEOUT;
        match self.controls.send_deadline(LogControl::Stop, deadline) {
            Ok(()) | Err(crossbeam_channel::SendTimeoutError::Disconnected(_)) => {}
            Err(error) => return Err(io::Error::other(error)),
        }
        let outcome = self
            .finished
            .recv_deadline(deadline)
            .map_err(io::Error::other)?;
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| io::Error::other("log writer panicked"))?;
        }
        outcome
    }
}
impl Drop for FileLogGuard {
    fn drop(&mut self) {
        if let Err(error) = self.finish() {
            eprintln!("could not finish rolling diagnostics: {error}");
        }
    }
}
pub(super) struct LogWorker {
    pub writer: LogWriter,
    pub guard: FileLogGuard,
}
pub(super) struct RollingLog {
    path: PathBuf,
    file: Option<File>,
    bytes: u64,
    limit: u64,
    count: usize,
}
impl RollingLog {
    pub(super) fn new(path: PathBuf, file: File) -> io::Result<Self> {
        let bytes = file.metadata()?.len();
        Ok(Self {
            path,
            file: Some(file),
            bytes,
            limit: LOG_FILE_BYTES,
            count: LOG_FILE_COUNT,
        })
    }
    fn archive(&self, index: usize) -> PathBuf {
        let mut path = self.path.as_os_str().to_os_string();
        path.push(format!(".{index}"));
        PathBuf::from(path)
    }
    fn remove_oldest(&self) -> io::Result<()> {
        match fs::remove_file(self.archive(self.count - 1)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
    fn rotate(&mut self) -> io::Result<()> {
        self.file.take();
        self.remove_oldest()?;
        for index in (1..self.count - 1).rev() {
            match fs::rename(self.archive(index), self.archive(index + 1)) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        fs::rename(&self.path, self.archive(1))?;
        self.file = Some(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?,
        );
        self.bytes = 0;
        Ok(())
    }
    fn append(&mut self, record: &[u8]) -> io::Result<()> {
        let length = u64::try_from(record.len()).map_err(io::Error::other)?;
        if self.bytes > 0 && self.bytes.saturating_add(length) > self.limit {
            self.rotate()?;
        }
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("rolling log is closed"))?
            .write_all(record)?;
        self.bytes += length;
        Ok(())
    }
    fn report_dropped(&mut self, dropped: &AtomicUsize) -> io::Result<()> {
        let count = dropped.swap(0, Ordering::Relaxed);
        if count > 0 {
            self.append(
                format!("dropped {count} diagnostic records because the log queue was full\n")
                    .as_bytes(),
            )?;
        }
        Ok(())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.as_mut().expect("open rolling log").flush()
    }
    fn run(mut self, inbox: LogInbox) -> io::Result<()> {
        loop {
            match inbox.receive() {
                LogInput::Record(record) => {
                    self.append(&record)?;
                    self.report_dropped(&inbox.dropped)?;
                }
                LogInput::Control(LogControl::FlushPanic { reply }) => {
                    match self.append(PANIC_RECORD).and_then(|()| self.flush()) {
                        Ok(()) => {
                            let _ = reply.send(Ok(()));
                        }
                        Err(error) => {
                            let _ = reply.send(Err(io::Error::new(
                                error.kind(),
                                "could not flush panic diagnostic",
                            )));
                            return Err(error);
                        }
                    }
                }
                LogInput::Control(LogControl::Stop) | LogInput::Disconnected => {
                    for record in inbox.records.try_iter() {
                        self.append(&record)?;
                    }
                    self.report_dropped(&inbox.dropped)?;
                    return self.flush();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::fmt::MakeWriter;

    /// Exceeds two intervals of the former 100 ms idle poll.
    const TEST_IDLE_WINDOW: Duration = Duration::from_millis(250);
    fn worker(directory: &tempfile::TempDir) -> LogWorker {
        let path = directory.path().join("core.log");
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .expect("log file");
        FileLogGuard::start(RollingLog::new(path, file).expect("log")).expect("log worker")
    }
    #[test]
    fn exact_record_limit_is_preserved_and_empty_records_are_not_queued() {
        let (release, blocked) = crossbeam_channel::bounded(1);
        let (reported, received) = crossbeam_channel::bounded(1);
        let worker = FileLogGuard::start_with(move |inbox| {
            blocked.recv().expect("release fake writer");
            reported
                .send(inbox.records.try_recv().expect("exact-sized record"))
                .expect("report formatted record");
            Ok(())
        })
        .expect("worker");
        drop(worker.writer.make_writer());
        {
            let mut record = worker.writer.make_writer();
            record
                .write_all(&vec![b'x'; RECORD_BYTES])
                .expect("exact record limit");
        }
        let queued = worker.writer.sender.len();
        release.send(()).expect("release writer");
        let record = received
            .recv_timeout(LOG_STOP_TIMEOUT)
            .expect("formatted record");
        drop(worker.guard);
        assert_eq!(queued, 1, "empty records consume no queue entries");
        assert_eq!(record, vec![b'x'; RECORD_BYTES]);
    }
    #[test]
    fn idle_writer_blocks_without_periodic_wakes_and_stop_drains_records() {
        let directory = tempfile::tempdir().expect("isolated logs");
        let worker = worker(&directory);
        let deadline = Instant::now() + LOG_STOP_TIMEOUT;
        while worker.writer.waits.load(Ordering::Relaxed) == 0 {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        let _ = crossbeam_channel::never::<()>().recv_timeout(TEST_IDLE_WINDOW);
        assert_eq!(
            worker.writer.waits.load(Ordering::Relaxed),
            1,
            "an idle writer remains in one blocking receive"
        );
        {
            let mut record = worker.writer.make_writer();
            record.write_all(b"drain on stop\n").expect("record");
        }
        drop(worker.guard);
        assert_eq!(
            fs::read(directory.path().join("core.log")).expect("flushed records"),
            b"drain on stop\n"
        );
    }
    #[test]
    fn panic_record_is_flushed_before_acknowledgement_even_when_records_are_full() {
        let directory = tempfile::tempdir().expect("isolated logs");
        let path = directory.path().join("core.log");
        let log = RollingLog::new(
            path.clone(),
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .expect("log file"),
        )
        .expect("log");
        let (release, blocked) = crossbeam_channel::bounded(1);
        let worker = FileLogGuard::start_with(move |inbox| {
            blocked.recv().expect("release fake disk");
            log.run(inbox)
        })
        .expect("worker");
        for _ in 0..QUEUED_RECORDS {
            let mut record = worker.writer.make_writer();
            record.write_all(b"queued\n").expect("record");
        }
        let panic = PanicLog {
            controls: worker.guard.controls.clone(),
            path: path.clone(),
        };
        let (done, acknowledged) = crossbeam_channel::bounded(1);
        let panic_thread = thread::spawn(move || {
            done.send(panic.record()).expect("panic acknowledgement");
        });
        release.send(()).expect("disk resumes");
        acknowledged
            .recv_timeout(PANIC_FLUSH_TIMEOUT)
            .expect("bounded acknowledgement")
            .expect("panic persisted");
        assert!(
            fs::read_to_string(&path)
                .expect("panic log before shutdown")
                .contains("core thread panicked")
        );
        panic_thread.join().expect("panic recorder");
        drop(worker.guard);
    }

    #[test]
    fn rotation_caps_files_and_keeps_the_latest_records_across_reopen() {
        let directory = tempfile::tempdir().expect("isolated log directory");
        let path = directory.path().join("core.log");
        let open = || {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .expect("log file")
        };
        let mut log = RollingLog::new(path.clone(), open()).expect("log");
        log.limit = 4;
        for record in [b"1111", b"2222", b"3333", b"4444"] {
            log.append(record).expect("rotate");
        }
        assert_eq!(fs::read(log.archive(1)).expect("newer archive"), b"3333");
        assert_eq!(fs::read(log.archive(2)).expect("oldest archive"), b"2222");
        drop(log);
        let mut reopened = RollingLog::new(path.clone(), open()).expect("reopen");
        reopened.limit = 4;
        reopened.append(b"5555").expect("rotate existing full file");
        assert_eq!(fs::read(&path).expect("current"), b"5555");
        assert_eq!(
            fs::read_dir(directory.path()).expect("logs").count(),
            LOG_FILE_COUNT
        );
    }
    #[test]
    fn blocked_disk_writer_does_not_block_producers_and_queue_memory_is_bounded() {
        let (release, blocked) = crossbeam_channel::bounded(1);
        let worker = FileLogGuard::start_with(move |inbox| {
            blocked.recv().expect("fake disk released");
            assert!(inbox.records.len() <= QUEUED_RECORDS);
            Ok(())
        })
        .expect("log worker");
        let writer = worker.writer.clone();
        let (finished, completion) = crossbeam_channel::bounded(1);
        let producer = thread::spawn(move || {
            for _ in 0..QUEUED_RECORDS * 2 {
                let mut record = writer.make_writer();
                record
                    .write_all(b"bounded diagnostic\n")
                    .expect("format record");
            }
            finished.send(()).expect("producer completed");
        });
        let completed = completion.recv_timeout(LOG_STOP_TIMEOUT);
        let queued = worker.writer.sender.len();
        let dropped = worker.writer.dropped.load(Ordering::Relaxed);
        release
            .send(())
            .expect("release fake disk after producer deadline");
        producer.join().expect("producer released");
        completed.expect("disk stalls cannot block producers");
        assert_eq!(queued, QUEUED_RECORDS);
        assert!(dropped > 0, "overload is counted");
        drop(worker.guard);
    }
    #[test]
    fn oversized_records_emit_a_fixed_diagnostic_and_shutdown_flushes_accepted_records() {
        let directory = tempfile::tempdir().expect("isolated log directory");
        let path = directory.path().join("ui.log");
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .expect("log file");
        let worker =
            FileLogGuard::start(RollingLog::new(path.clone(), file).expect("log")).expect("worker");
        {
            let mut record = worker.writer.make_writer();
            record
                .write_all(&vec![b'x'; RECORD_BYTES + 1])
                .expect("oversized record");
        }
        {
            let mut record = worker.writer.make_writer();
            record
                .write_all(b"accepted diagnostic\n")
                .expect("accepted record");
        }
        drop(worker.guard);
        assert_eq!(
            fs::read_to_string(path).expect("flushed log"),
            "log record exceeded the record byte limit\naccepted diagnostic\n"
        );
    }
}
