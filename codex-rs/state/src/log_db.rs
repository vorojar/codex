//! Tracing log export into the local SQLite log database.
//!
//! This module provides a `tracing_subscriber::Layer` that captures events,
//! formats each one into a `LogEntry`, and sends entries to a bounded background
//! queue. The background task inserts into the dedicated `logs` SQLite database
//! in batches to keep logging overhead low.
//!
//! ## Usage
//!
//! ```no_run
//! use codex_state::log_db;
//! use tracing_subscriber::prelude::*;
//!
//! # async fn example(state_db: std::sync::Arc<codex_state::StateRuntime>) {
//! let layer = log_db::start(state_db);
//! let _ = tracing_subscriber::registry()
//!     .with(layer)
//!     .try_init();
//! # }
//! ```

use std::future::Future;
use std::sync::OnceLock;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tracing::Event;
use tracing::field::Field;
use tracing::field::Visit;
use tracing::span::Attributes;
use tracing::span::Id;
use tracing::span::Record;
use tracing_subscriber::Layer;
use tracing_subscriber::field::RecordFields;
use tracing_subscriber::fmt::FormatFields;
use tracing_subscriber::fmt::FormattedFields;
use tracing_subscriber::fmt::format::DefaultFields;
use tracing_subscriber::registry::LookupSpan;
use uuid::Uuid;

use crate::LogEntry;
use crate::StateRuntime;

const LOG_QUEUE_CAPACITY: usize = 512;
const LOG_BATCH_SIZE: usize = 128;
const LOG_FLUSH_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LogSinkQueueConfig {
    pub queue_capacity: usize,
    pub batch_size: usize,
    pub flush_interval: Duration,
}

impl Default for LogSinkQueueConfig {
    fn default() -> Self {
        Self {
            queue_capacity: LOG_QUEUE_CAPACITY,
            batch_size: LOG_BATCH_SIZE,
            flush_interval: LOG_FLUSH_INTERVAL,
        }
    }
}

impl LogSinkQueueConfig {
    fn normalized(self) -> Self {
        Self {
            queue_capacity: self.queue_capacity.max(1),
            batch_size: self.batch_size.max(1),
            flush_interval: if self.flush_interval.is_zero() {
                LOG_FLUSH_INTERVAL
            } else {
                self.flush_interval
            },
        }
    }
}

/// A tracing log writer that can flush entries accepted by its queue.
///
/// Implementations should keep `Layer::on_event` non-blocking for ordinary log
/// events. `flush` should wait for entries accepted before the flush command to
/// be processed by the writer.
pub trait LogWriter<S>: Layer<S>
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn flush(&self) -> impl Future<Output = ()> + Send + '_;
}

pub struct LogDbLayer {
    sender: mpsc::Sender<LogDbCommand>,
    formatter: LogEntryFormatter,
}

pub fn start(state_db: std::sync::Arc<StateRuntime>) -> LogDbLayer {
    LogDbLayer::start(state_db)
}

impl Clone for LogDbLayer {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            formatter: self.formatter.clone(),
        }
    }
}

impl LogDbLayer {
    pub fn start(state_db: std::sync::Arc<StateRuntime>) -> Self {
        Self::start_with_config(state_db, LogSinkQueueConfig::default())
    }

    pub fn start_with_config(
        state_db: std::sync::Arc<StateRuntime>,
        config: LogSinkQueueConfig,
    ) -> Self {
        let config = config.normalized();
        let (sender, receiver) = mpsc::channel(config.queue_capacity);
        tokio::spawn(run_inserter(state_db, receiver, config));
        Self {
            sender,
            formatter: LogEntryFormatter::new(current_process_log_uuid()),
        }
    }

    pub async fn flush(&self) {
        let (tx, rx) = oneshot::channel();
        if self.sender.send(LogDbCommand::Flush(tx)).await.is_ok() {
            let _ = rx.await;
        }
    }

    fn try_send(&self, entry: LogEntry) {
        let _ = self.sender.try_send(LogDbCommand::Entry(Box::new(entry)));
    }
}

impl<S> Layer<S> for LogDbLayer
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &Attributes<'_>,
        id: &Id,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        self.formatter.on_new_span(attrs, id, ctx);
    }

    fn on_record(
        &self,
        id: &Id,
        values: &Record<'_>,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        self.formatter.on_record(id, values, ctx);
    }

    fn on_event(&self, event: &Event<'_>, ctx: tracing_subscriber::layer::Context<'_, S>) {
        let entry = self.formatter.format_event(event, ctx);
        self.try_send(entry);
    }
}

/// Formats tracing spans and events into feedback log entries.
///
/// Log writers should call `on_new_span`, `on_record`, and `format_event` from
/// their corresponding `tracing_subscriber::Layer` hooks. Only one
/// `LogEntryFormatter` should be installed in a subscriber. This keeps the
/// feedback-log body, message extraction, and thread-id propagation consistent
/// across local and remote log sinks.
#[derive(Clone, Debug)]
pub struct LogEntryFormatter {
    process_uuid: String,
}

impl LogEntryFormatter {
    /// Create a formatter that stamps generated log entries with `process_uuid`.
    pub fn new(process_uuid: impl Into<String>) -> Self {
        Self {
            process_uuid: process_uuid.into(),
        }
    }

    /// Record initial span fields for later event formatting.
    pub fn on_new_span<S>(
        &self,
        attrs: &Attributes<'_>,
        id: &Id,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) where
        S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    {
        let mut visitor = SpanFieldVisitor::default();
        attrs.record(&mut visitor);

        if let Some(span) = ctx.span(id) {
            let mut extensions = span.extensions_mut();
            debug_assert!(
                extensions.get_mut::<SpanLogContext>().is_none(),
                "only one LogEntryFormatter may be installed in a tracing subscriber"
            );
            extensions.insert(SpanLogContext {
                name: span.metadata().name().to_string(),
                formatted_fields: format_fields(attrs),
                thread_id: visitor.thread_id,
            });
        }
    }

    /// Record updated span fields for later event formatting.
    pub fn on_record<S>(
        &self,
        id: &Id,
        values: &Record<'_>,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) where
        S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    {
        let mut visitor = SpanFieldVisitor::default();
        values.record(&mut visitor);

        if let Some(span) = ctx.span(id) {
            let mut extensions = span.extensions_mut();
            if let Some(log_context) = extensions.get_mut::<SpanLogContext>() {
                if let Some(thread_id) = visitor.thread_id {
                    log_context.thread_id = Some(thread_id);
                }
                append_fields(&mut log_context.formatted_fields, values);
            } else {
                extensions.insert(SpanLogContext {
                    name: span.metadata().name().to_string(),
                    formatted_fields: format_fields(values),
                    thread_id: visitor.thread_id,
                });
            }
        }
    }

    /// Format a tracing event and the current span scope into a `LogEntry`.
    pub fn format_event<S>(
        &self,
        event: &Event<'_>,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) -> LogEntry
    where
        S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    {
        let metadata = event.metadata();
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let thread_id = visitor
            .thread_id
            .clone()
            .or_else(|| self.event_thread_id(event, &ctx));
        let feedback_log_body = self.format_feedback_log_body(event, &ctx);

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_else(|_| Duration::from_secs(0));
        LogEntry {
            ts: now.as_secs() as i64,
            ts_nanos: now.subsec_nanos() as i64,
            level: metadata.level().as_str().to_string(),
            target: metadata.target().to_string(),
            message: visitor.message,
            feedback_log_body: Some(feedback_log_body),
            thread_id,
            process_uuid: Some(self.process_uuid.clone()),
            module_path: metadata.module_path().map(ToString::to_string),
            file: metadata.file().map(ToString::to_string),
            line: metadata.line().map(|line| line as i64),
        }
    }

    fn event_thread_id<S>(
        &self,
        event: &Event<'_>,
        ctx: &tracing_subscriber::layer::Context<'_, S>,
    ) -> Option<String>
    where
        S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    {
        let mut thread_id = None;
        if let Some(scope) = ctx.event_scope(event) {
            for span in scope.from_root() {
                let extensions = span.extensions();
                if let Some(log_context) = extensions.get::<SpanLogContext>()
                    && log_context.thread_id.is_some()
                {
                    thread_id = log_context.thread_id.clone();
                }
            }
        }
        thread_id
    }

    fn format_feedback_log_body<S>(
        &self,
        event: &Event<'_>,
        ctx: &tracing_subscriber::layer::Context<'_, S>,
    ) -> String
    where
        S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    {
        let mut feedback_log_body = String::new();
        if let Some(scope) = ctx.event_scope(event) {
            for span in scope.from_root() {
                let extensions = span.extensions();
                if let Some(log_context) = extensions.get::<SpanLogContext>() {
                    feedback_log_body.push_str(&log_context.name);
                    if !log_context.formatted_fields.is_empty() {
                        feedback_log_body.push('{');
                        feedback_log_body.push_str(&log_context.formatted_fields);
                        feedback_log_body.push('}');
                    }
                } else {
                    feedback_log_body.push_str(span.metadata().name());
                }
                feedback_log_body.push(':');
            }
            if !feedback_log_body.is_empty() {
                feedback_log_body.push(' ');
            }
        }
        feedback_log_body.push_str(&format_fields(event));
        feedback_log_body
    }
}

impl<S> LogWriter<S> for LogDbLayer
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn flush(&self) -> impl Future<Output = ()> + Send + '_ {
        LogDbLayer::flush(self)
    }
}

enum LogDbCommand {
    Entry(Box<LogEntry>),
    Flush(oneshot::Sender<()>),
}

#[derive(Debug)]
struct SpanLogContext {
    name: String,
    formatted_fields: String,
    thread_id: Option<String>,
}

#[derive(Default)]
struct SpanFieldVisitor {
    thread_id: Option<String>,
}

impl SpanFieldVisitor {
    fn record_field(&mut self, field: &Field, value: String) {
        if field.name() == "thread_id" && self.thread_id.is_none() {
            self.thread_id = Some(value);
        }
    }
}

impl Visit for SpanFieldVisitor {
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.record_field(field, value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.record_field(field, value.to_string());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.record_field(field, value.to_string());
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.record_field(field, value.to_string());
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.record_field(field, value.to_string());
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.record_field(field, value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.record_field(field, format!("{value:?}"));
    }
}

fn format_fields<R>(fields: R) -> String
where
    R: RecordFields,
{
    let formatter = DefaultFields::default();
    let mut formatted = FormattedFields::<DefaultFields>::new(String::new());
    let _ = formatter.format_fields(formatted.as_writer(), fields);
    formatted.fields
}

fn append_fields(fields: &mut String, values: &Record<'_>) {
    let formatter = DefaultFields::default();
    let mut formatted = FormattedFields::<DefaultFields>::new(std::mem::take(fields));
    let _ = formatter.add_fields(&mut formatted, values);
    *fields = formatted.fields;
}

fn current_process_log_uuid() -> &'static str {
    static PROCESS_LOG_UUID: OnceLock<String> = OnceLock::new();
    PROCESS_LOG_UUID.get_or_init(|| {
        let pid = std::process::id();
        let process_uuid = Uuid::new_v4();
        format!("pid:{pid}:{process_uuid}")
    })
}

async fn run_inserter(
    state_db: std::sync::Arc<StateRuntime>,
    mut receiver: mpsc::Receiver<LogDbCommand>,
    config: LogSinkQueueConfig,
) {
    let mut buffer = Vec::with_capacity(config.batch_size);
    let mut ticker = tokio::time::interval(config.flush_interval);
    // Consume the immediate startup tick so entries flush after the interval.
    ticker.tick().await;
    loop {
        tokio::select! {
            maybe_command = receiver.recv() => {
                match maybe_command {
                    Some(LogDbCommand::Entry(entry)) => {
                        buffer.push(*entry);
                        if buffer.len() >= config.batch_size {
                            flush(&state_db, &mut buffer).await;
                        }
                    }
                    Some(LogDbCommand::Flush(reply)) => {
                        flush(&state_db, &mut buffer).await;
                        let _ = reply.send(());
                    }
                    None => {
                        flush(&state_db, &mut buffer).await;
                        break;
                    }
                }
            }
            _ = ticker.tick() => {
                flush(&state_db, &mut buffer).await;
            }
        }
    }
}

async fn flush(state_db: &StateRuntime, buffer: &mut Vec<LogEntry>) {
    if buffer.is_empty() {
        return;
    }
    let entries = buffer.split_off(0);
    let _ = state_db.insert_logs(entries.as_slice()).await;
}

#[derive(Default)]
struct MessageVisitor {
    message: Option<String>,
    thread_id: Option<String>,
}

impl MessageVisitor {
    fn record_field(&mut self, field: &Field, value: String) {
        if field.name() == "message" && self.message.is_none() {
            self.message = Some(value.clone());
        }
        if field.name() == "thread_id" && self.thread_id.is_none() {
            self.thread_id = Some(value);
        }
    }
}

impl Visit for MessageVisitor {
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.record_field(field, value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.record_field(field, value.to_string());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.record_field(field, value.to_string());
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.record_field(field, value.to_string());
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.record_field(field, value.to_string());
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.record_field(field, value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.record_field(field, format!("{value:?}"));
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::Arc;
    use std::sync::Mutex;

    use pretty_assertions::assert_eq;
    use tracing_subscriber::filter::Targets;
    use tracing_subscriber::fmt::writer::MakeWriter;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    use super::*;

    fn temp_codex_home() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("codex-state-log-db-{}", Uuid::new_v4()))
    }

    async fn wait_for_log_count(runtime: &StateRuntime, expected: usize) -> Vec<crate::LogRow> {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let rows = runtime
                .query_logs(&crate::LogQuery::default())
                .await
                .expect("query logs");
            if rows.len() == expected {
                return rows;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {expected} logs; saw {}",
                rows.len()
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    fn test_entry(message: &str) -> LogEntry {
        LogEntry {
            ts: 1,
            ts_nanos: 2,
            level: "INFO".to_string(),
            target: "test".to_string(),
            message: Some(message.to_string()),
            feedback_log_body: Some(message.to_string()),
            thread_id: Some("thread-1".to_string()),
            process_uuid: Some("process-1".to_string()),
            module_path: Some("module".to_string()),
            file: Some("file.rs".to_string()),
            line: Some(7),
        }
    }

    #[derive(Clone, Default)]
    struct SharedWriter {
        bytes: Arc<Mutex<Vec<u8>>>,
    }

    impl SharedWriter {
        fn snapshot(&self) -> String {
            String::from_utf8(self.bytes.lock().expect("writer mutex poisoned").clone())
                .expect("valid utf-8")
        }
    }

    struct SharedWriterGuard {
        bytes: Arc<Mutex<Vec<u8>>>,
    }

    impl<'a> MakeWriter<'a> for SharedWriter {
        type Writer = SharedWriterGuard;

        fn make_writer(&'a self) -> Self::Writer {
            SharedWriterGuard {
                bytes: Arc::clone(&self.bytes),
            }
        }
    }

    impl io::Write for SharedWriterGuard {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.bytes
                .lock()
                .expect("writer mutex poisoned")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[derive(Clone)]
    struct RecordingFormatterLayer {
        formatter: LogEntryFormatter,
        entries: Arc<Mutex<Vec<LogEntry>>>,
    }

    impl RecordingFormatterLayer {
        fn new(process_uuid: &str) -> Self {
            Self {
                formatter: LogEntryFormatter::new(process_uuid),
                entries: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn entries(&self) -> Vec<LogEntry> {
            self.entries.lock().expect("entries mutex poisoned").clone()
        }
    }

    impl<S> Layer<S> for RecordingFormatterLayer
    where
        S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    {
        fn on_new_span(
            &self,
            attrs: &Attributes<'_>,
            id: &Id,
            ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            self.formatter.on_new_span(attrs, id, ctx);
        }

        fn on_record(
            &self,
            id: &Id,
            values: &Record<'_>,
            ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            self.formatter.on_record(id, values, ctx);
        }

        fn on_event(&self, event: &Event<'_>, ctx: tracing_subscriber::layer::Context<'_, S>) {
            self.entries
                .lock()
                .expect("entries mutex poisoned")
                .push(self.formatter.format_event(event, ctx));
        }
    }

    #[test]
    fn log_entry_formatter_formats_recorded_span_fields() {
        let layer = RecordingFormatterLayer::new("process-1");
        let subscriber = tracing_subscriber::registry().with(
            layer
                .clone()
                .with_filter(Targets::new().with_default(tracing::Level::TRACE)),
        );
        let dispatch = tracing::Dispatch::new(subscriber);

        tracing::dispatcher::with_default(&dispatch, || {
            let span = tracing::info_span!(
                "feedback-thread",
                thread_id = "thread-1",
                turn = tracing::field::Empty
            );
            span.record("turn", 7);
            span.in_scope(|| tracing::info!("thread-scoped"));
        });

        let entries = layer.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].process_uuid.as_deref(), Some("process-1"));
        assert_eq!(entries[0].thread_id.as_deref(), Some("thread-1"));
        assert_eq!(
            entries[0].feedback_log_body.as_deref(),
            Some("feedback-thread{thread_id=\"thread-1\" turn=7}: thread-scoped")
        );
    }

    #[tokio::test]
    async fn sqlite_feedback_logs_match_feedback_formatter_shape() {
        let codex_home = temp_codex_home();
        let runtime = StateRuntime::init(codex_home.clone(), "test-provider".to_string())
            .await
            .expect("initialize runtime");
        let writer = SharedWriter::default();
        let layer = start(runtime.clone());

        let subscriber = tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(writer.clone())
                    .with_ansi(false)
                    .with_target(false)
                    .with_filter(Targets::new().with_default(tracing::Level::TRACE)),
            )
            .with(
                layer
                    .clone()
                    .with_filter(Targets::new().with_default(tracing::Level::TRACE)),
            );
        let guard = subscriber.set_default();

        tracing::trace!("threadless-before");
        tracing::info_span!("feedback-thread", thread_id = "thread-1", turn = 1).in_scope(|| {
            tracing::info!(foo = 2, "thread-scoped");
        });
        tracing::debug!("threadless-after");

        layer.flush().await;
        drop(guard);

        let feedback_logs = writer.snapshot();
        let without_timestamps = |logs: &str| {
            logs.lines()
                .map(|line| match line.split_once(' ') {
                    Some((_, rest)) => rest,
                    None => line,
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let sqlite_logs = String::from_utf8(
            runtime
                .query_feedback_logs("thread-1")
                .await
                .expect("query feedback logs"),
        )
        .expect("valid utf-8");
        assert_eq!(
            without_timestamps(&sqlite_logs),
            without_timestamps(&feedback_logs)
        );

        let _ = tokio::fs::remove_dir_all(codex_home).await;
    }

    #[tokio::test]
    async fn flush_persists_logs_for_query() {
        let codex_home = temp_codex_home();
        let runtime = StateRuntime::init(codex_home.clone(), "test-provider".to_string())
            .await
            .expect("initialize runtime");
        let layer = start(runtime.clone());

        let guard = tracing_subscriber::registry()
            .with(
                layer
                    .clone()
                    .with_filter(Targets::new().with_default(tracing::Level::TRACE)),
            )
            .set_default();

        tracing::info!("buffered-log");

        layer.flush().await;
        drop(guard);

        let after_flush = runtime
            .query_logs(&crate::LogQuery::default())
            .await
            .expect("query logs after flush");
        assert_eq!(after_flush.len(), 1);
        assert_eq!(after_flush[0].message.as_deref(), Some("buffered-log"));

        let _ = tokio::fs::remove_dir_all(codex_home).await;
    }

    #[tokio::test]
    async fn configured_batch_size_flushes_without_explicit_flush() {
        let codex_home = temp_codex_home();
        let runtime = StateRuntime::init(codex_home.clone(), "test-provider".to_string())
            .await
            .expect("initialize runtime");
        let layer = LogDbLayer::start_with_config(
            runtime.clone(),
            LogSinkQueueConfig {
                queue_capacity: 8,
                batch_size: 2,
                flush_interval: std::time::Duration::from_secs(60),
            },
        );

        let guard = tracing_subscriber::registry()
            .with(
                layer
                    .clone()
                    .with_filter(Targets::new().with_default(tracing::Level::TRACE)),
            )
            .set_default();

        tracing::info!("first-batch-log");
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        assert_eq!(
            runtime
                .query_logs(&crate::LogQuery::default())
                .await
                .expect("query logs before batch fills")
                .len(),
            0
        );

        tracing::info!("second-batch-log");
        let after_batch = wait_for_log_count(&runtime, /*expected*/ 2).await;
        drop(guard);

        assert_eq!(
            after_batch
                .iter()
                .map(|row| row.message.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("first-batch-log"), Some("second-batch-log")]
        );

        let _ = tokio::fs::remove_dir_all(codex_home).await;
    }

    #[tokio::test]
    async fn configured_flush_interval_persists_buffered_logs() {
        let codex_home = temp_codex_home();
        let runtime = StateRuntime::init(codex_home.clone(), "test-provider".to_string())
            .await
            .expect("initialize runtime");
        let layer = LogDbLayer::start_with_config(
            runtime.clone(),
            LogSinkQueueConfig {
                queue_capacity: 8,
                batch_size: 128,
                flush_interval: std::time::Duration::from_millis(10),
            },
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let guard = tracing_subscriber::registry()
            .with(
                layer
                    .clone()
                    .with_filter(Targets::new().with_default(tracing::Level::TRACE)),
            )
            .set_default();

        tracing::info!("interval-log");
        let after_interval = wait_for_log_count(&runtime, /*expected*/ 1).await;
        drop(guard);

        assert_eq!(after_interval[0].message.as_deref(), Some("interval-log"));

        let _ = tokio::fs::remove_dir_all(codex_home).await;
    }

    #[tokio::test]
    async fn event_queue_drops_new_entries_when_full() {
        let (sender, mut receiver) = mpsc::channel(1);
        let layer = LogDbLayer {
            sender,
            formatter: LogEntryFormatter::new("process-1"),
        };

        layer.try_send(test_entry("first-queued-log"));
        layer.try_send(test_entry("dropped-log"));

        match receiver.try_recv().expect("first entry queued") {
            LogDbCommand::Entry(entry) => {
                assert_eq!(entry.message.as_deref(), Some("first-queued-log"));
            }
            LogDbCommand::Flush(_) => panic!("expected queued entry"),
        }
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn flush_waits_for_queue_capacity_and_receiver_processing() {
        let (sender, mut receiver) = mpsc::channel(1);
        let layer = LogDbLayer {
            sender,
            formatter: LogEntryFormatter::new("process-1"),
        };

        layer.try_send(test_entry("queued-before-flush"));
        let mut flush_task = tokio::spawn({
            let layer = layer.clone();
            async move {
                layer.flush().await;
            }
        });

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(!flush_task.is_finished());

        match receiver.recv().await.expect("queued entry") {
            LogDbCommand::Entry(entry) => {
                assert_eq!(entry.message.as_deref(), Some("queued-before-flush"));
            }
            LogDbCommand::Flush(_) => panic!("expected queued entry"),
        }

        match receiver.recv().await.expect("flush command") {
            LogDbCommand::Flush(reply) => {
                assert!(!flush_task.is_finished());
                let _ = reply.send(());
            }
            LogDbCommand::Entry(_) => panic!("expected flush command"),
        }

        tokio::time::timeout(std::time::Duration::from_secs(1), &mut flush_task)
            .await
            .expect("flush task completes")
            .expect("flush task succeeds");
    }
}
