use std::path::Path;
use std::path::PathBuf;

use codex_journal::JournalEntry;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::io::BufWriter;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::ThreadStoreError;
use crate::ThreadStoreResult;

const DEFAULT_QUEUE_CAPACITY: usize = 1024;

/// Async append-only JSONL journal for [`JournalEntry`] items.
///
/// Callers update the in-memory journal first, then enqueue the same keyed entries here for durable
/// persistence. Writes happen on a dedicated worker task; use [`Self::flush`] or
/// [`Self::shutdown`] when durability matters.
pub struct JournalWriter {
    path: PathBuf,
    tx: mpsc::Sender<Command>,
    worker: Option<JoinHandle<ThreadStoreResult<()>>>,
}

impl JournalWriter {
    /// Opens or creates a journal at `path`.
    pub async fn open(path: impl Into<PathBuf>) -> ThreadStoreResult<Self> {
        Self::open_with_capacity(path, DEFAULT_QUEUE_CAPACITY).await
    }

    /// Opens or creates a journal at `path` with a custom queue capacity.
    pub async fn open_with_capacity(
        path: impl Into<PathBuf>,
        queue_capacity: usize,
    ) -> ThreadStoreResult<Self> {
        if queue_capacity == 0 {
            return Err(ThreadStoreError::InvalidRequest {
                message: "journal queue capacity must be greater than zero".to_string(),
            });
        }

        let path = path.into();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(io_error)?;
        }

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .map_err(io_error)?;
        let writer = BufWriter::new(file);
        let (tx, rx) = mpsc::channel(queue_capacity);
        let worker_path = path.clone();
        let worker = tokio::spawn(async move { run_worker(worker_path, writer, rx).await });

        Ok(Self {
            path,
            tx,
            worker: Some(worker),
        })
    }

    /// Returns the backing journal path.
    pub fn path(&self) -> &Path {
        self.path.as_path()
    }

    /// Enqueues one entry for async persistence.
    pub async fn enqueue(&self, entry: JournalEntry) -> ThreadStoreResult<()> {
        self.tx
            .send(Command::Append(vec![entry]))
            .await
            .map_err(send_error)
    }

    /// Enqueues several entries for async persistence.
    pub async fn enqueue_all<I>(&self, entries: I) -> ThreadStoreResult<()>
    where
        I: IntoIterator<Item = JournalEntry>,
    {
        let entries = entries.into_iter().collect::<Vec<_>>();
        if entries.is_empty() {
            return Ok(());
        }

        self.tx
            .send(Command::Append(entries))
            .await
            .map_err(send_error)
    }

    /// Waits until all previously enqueued entries are durable.
    pub async fn flush(&self) -> ThreadStoreResult<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(Command::Flush { reply: reply_tx })
            .await
            .map_err(send_error)?;
        reply_rx.await.map_err(recv_error)?
    }

    /// Flushes pending entries and stops the background worker.
    pub async fn shutdown(mut self) -> ThreadStoreResult<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(Command::Shutdown { reply: reply_tx })
            .await
            .map_err(send_error)?;
        let flush_result = reply_rx.await.map_err(recv_error)?;

        let Some(worker) = self.worker.take() else {
            return flush_result;
        };
        let worker_result = worker.await.map_err(join_error)?;

        flush_result?;
        worker_result
    }
}

enum Command {
    Append(Vec<JournalEntry>),
    Flush {
        reply: oneshot::Sender<ThreadStoreResult<()>>,
    },
    Shutdown {
        reply: oneshot::Sender<ThreadStoreResult<()>>,
    },
}

async fn run_worker(
    path: PathBuf,
    mut writer: BufWriter<tokio::fs::File>,
    mut rx: mpsc::Receiver<Command>,
) -> ThreadStoreResult<()> {
    let mut failure: Option<ThreadStoreError> = None;

    while let Some(command) = rx.recv().await {
        match command {
            Command::Append(entries) => {
                if failure.is_none()
                    && let Err(err) = append_entries(&mut writer, entries.as_slice()).await
                {
                    failure = Some(err);
                }
            }
            Command::Flush { reply } => {
                let result = flush_writer(&mut writer, failure.clone(), path.as_path()).await;
                if let Err(err) = &result {
                    failure = Some(err.clone());
                }
                let _ = reply.send(result);
            }
            Command::Shutdown { reply } => {
                let result = flush_writer(&mut writer, failure.clone(), path.as_path()).await;
                let worker_result = result.clone();
                let _ = reply.send(result);
                return worker_result;
            }
        }
    }

    flush_writer(&mut writer, failure, path.as_path()).await
}

async fn append_entries(
    writer: &mut BufWriter<tokio::fs::File>,
    entries: &[JournalEntry],
) -> ThreadStoreResult<()> {
    for entry in entries {
        let mut line = serde_json::to_vec(entry).map_err(|source| ThreadStoreError::Internal {
            message: format!("failed to serialize journal entry: {source}"),
        })?;
        line.push(b'\n');
        writer.write_all(line.as_slice()).await.map_err(io_error)?;
    }
    Ok(())
}

async fn flush_writer(
    writer: &mut BufWriter<tokio::fs::File>,
    failure: Option<ThreadStoreError>,
    path: &Path,
) -> ThreadStoreResult<()> {
    if let Some(err) = failure {
        return Err(err);
    }

    writer.flush().await.map_err(io_error)?;
    writer
        .get_mut()
        .sync_data()
        .await
        .map_err(|err| ThreadStoreError::Internal {
            message: format!("failed to sync journal at {}: {err}", path.display()),
        })?;
    Ok(())
}

fn io_error(err: std::io::Error) -> ThreadStoreError {
    ThreadStoreError::Internal {
        message: err.to_string(),
    }
}

fn send_error(_: mpsc::error::SendError<Command>) -> ThreadStoreError {
    ThreadStoreError::Internal {
        message: "journal worker is not available".to_string(),
    }
}

fn recv_error(_: oneshot::error::RecvError) -> ThreadStoreError {
    ThreadStoreError::Internal {
        message: "journal worker dropped its response channel".to_string(),
    }
}

fn join_error(err: tokio::task::JoinError) -> ThreadStoreError {
    ThreadStoreError::Internal {
        message: format!("journal worker task failed: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use codex_journal::Journal;
    use codex_journal::JournalMetadataItem;
    use codex_journal::JournalTranscriptItem;
    use codex_journal::PromptMessage;
    use codex_protocol::models::ContentItem;
    use codex_protocol::models::ResponseItem;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    use super::JournalWriter;

    fn developer_entry(text: &str) -> codex_journal::JournalEntry {
        codex_journal::JournalEntry::new(
            ["prompt", text],
            JournalMetadataItem::new(PromptMessage::developer_text(text)),
        )
    }

    fn user_entry(text: &str) -> codex_journal::JournalEntry {
        codex_journal::JournalEntry::new(
            ["history", text],
            JournalTranscriptItem {
                id: format!("history-{text}"),
                turn_id: None,
                item: ResponseItem::Message {
                    id: None,
                    role: "user".to_string(),
                    content: vec![ContentItem::InputText {
                        text: text.to_string(),
                    }],
                    phase: None,
                },
            },
        )
    }

    #[tokio::test]
    async fn enqueue_and_flush_persists_entries_in_order() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("journal.jsonl");
        let journal = JournalWriter::open(path.clone())
            .await
            .expect("journal should open");

        journal
            .enqueue(developer_entry("first"))
            .await
            .expect("first entry should enqueue");
        journal
            .enqueue(user_entry("hello"))
            .await
            .expect("second entry should enqueue");
        journal.flush().await.expect("journal should flush");

        let loaded = Journal::load_jsonl(path.as_path()).expect("journal should load");

        assert_eq!(
            loaded.entries(),
            vec![developer_entry("first"), user_entry("hello")]
        );
    }

    #[tokio::test]
    async fn enqueue_all_and_shutdown_persist_entries() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("journal.jsonl");
        let journal = JournalWriter::open(path.clone())
            .await
            .expect("journal should open");

        journal
            .enqueue_all(vec![developer_entry("one"), developer_entry("two")])
            .await
            .expect("entries should enqueue");
        journal.shutdown().await.expect("journal should shut down");

        let loaded = Journal::load_jsonl(path.as_path()).expect("journal should load");

        assert_eq!(
            loaded.entries(),
            vec![developer_entry("one"), developer_entry("two")]
        );
    }

    #[tokio::test]
    async fn reopen_appends_to_existing_journal() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("journal.jsonl");

        let first = JournalWriter::open(path.clone())
            .await
            .expect("first journal should open");
        first
            .enqueue(developer_entry("first"))
            .await
            .expect("entry should enqueue");
        first
            .shutdown()
            .await
            .expect("first journal should shut down");

        let second = JournalWriter::open(path.clone())
            .await
            .expect("second journal should open");
        second
            .enqueue(user_entry("second"))
            .await
            .expect("entry should enqueue");
        second
            .shutdown()
            .await
            .expect("second journal should shut down");

        let loaded = Journal::load_jsonl(path.as_path()).expect("journal should load");

        assert_eq!(
            loaded.entries(),
            vec![developer_entry("first"), user_entry("second")]
        );
    }
}
