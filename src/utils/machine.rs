//! Machine-mode output sink.
//!
//! In machine mode (`--format json|yaml|csv` given explicitly) stdout must hold
//! exactly one document. Commands hand their result to [`report`], which parks
//! it in a process-wide slot; `main` owns the end of the run and prints the
//! parked document — on its own for a success, or attached to the error
//! envelope under a `report` key for a failure. Outside machine mode every
//! entry point here is a no-op so today's behaviour is untouched.

use std::sync::Mutex;

use serde::Serialize;
use serde_json::Value;

use crate::config::Config;
use crate::utils::format::{sanitize_control_chars, OutputFormat};

/// Maximum number of characters retained in a sanitized `detail`/`error` string.
const MAX_DIAGNOSTIC_CHARS: usize = 1024;

/// The single document awaiting emission by `main`.
static PENDING: Mutex<Option<Value>> = Mutex::new(None);

/// True when the command must emit exactly one machine document on stdout:
/// the user passed an explicit `--format` and it resolved to a machine format.
/// `--format auto` (even piped) is deliberately excluded — see the design doc.
pub fn is_machine_mode(config: &Config) -> bool {
    config.format_explicit
        && matches!(
            config.runtime_output_format,
            OutputFormat::Json | OutputFormat::Yaml | OutputFormat::Csv
        )
}

/// Park `report` as the run's single machine document. No-op outside machine
/// mode. Nothing is printed here; `main` renders the document once, in the
/// resolved format, after the command returns.
// Callers land in Task 2+.
#[allow(dead_code)]
pub fn report<T: Serialize>(config: &Config, report: &T) {
    if !is_machine_mode(config) {
        return;
    }
    let value = match serde_json::to_value(report) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!("machine report could not be serialized: {err}");
            return;
        }
    };
    let mut slot = match PENDING.lock() {
        Ok(slot) => slot,
        Err(poisoned) => poisoned.into_inner(),
    };
    if slot.is_some() {
        debug_assert!(
            false,
            "machine::report called twice in one run; stdout must hold one document"
        );
        tracing::warn!("machine report replaced a pending document; this is a bug");
    }
    *slot = Some(value);
}

/// Take the parked document, if any. Used by `main` only.
pub fn take_pending() -> Option<Value> {
    match PENDING.lock() {
        Ok(mut slot) => slot.take(),
        Err(poisoned) => poisoned.into_inner().take(),
    }
}

/// Render a successful machine document in the resolved format.
pub fn render_success(format: OutputFormat, report: &Value) -> String {
    match format {
        OutputFormat::Yaml => serde_yaml::to_string(report).unwrap_or_default(),
        OutputFormat::Csv => render_csv(report),
        _ => serde_json::to_string_pretty(report).unwrap_or_default(),
    }
}

/// Render the error envelope, attaching the parked document under `report`
/// when the command produced one before failing. With no pending report the
/// output is byte-identical to the pre-existing envelope.
pub fn render_failure(format: OutputFormat, envelope: Value, report: Option<Value>) -> String {
    let mut envelope = envelope;
    if let Some(report) = report {
        if let Some(object) = envelope.as_object_mut() {
            object.insert("report".to_string(), report);
        }
    }
    match format {
        OutputFormat::Yaml => serde_yaml::to_string(&envelope).unwrap_or_default(),
        _ => serde_json::to_string(&envelope).unwrap_or_default(),
    }
}

/// CSV is rows-only: it cannot carry an error object, so only the data shape
/// is rendered here. An `ItemReport`-shaped object becomes its item rows, a
/// flat array of objects becomes the union of its keys, and anything else
/// degrades to a single `report` column holding the JSON text.
fn render_csv(report: &Value) -> String {
    if let Some(items) = report.get("items").and_then(Value::as_array) {
        let headers = ["name", "status", "detail", "error"];
        let rows: Vec<Vec<String>> = items
            .iter()
            .map(|item| {
                headers
                    .iter()
                    .map(|key| item.get(*key).map(scalar_to_field).unwrap_or_default())
                    .collect()
            })
            .collect();
        return write_csv(&headers.map(|h| h.to_string()), &rows);
    }

    if let Some(array) = report.as_array() {
        if !array.is_empty() && array.iter().all(|item| item.is_object()) {
            let mut headers: Vec<String> = Vec::new();
            for item in array {
                for key in item.as_object().expect("checked above").keys() {
                    if !headers.iter().any(|existing| existing == key) {
                        headers.push(key.clone());
                    }
                }
            }
            let rows: Vec<Vec<String>> = array
                .iter()
                .map(|item| {
                    headers
                        .iter()
                        .map(|key| item.get(key).map(scalar_to_field).unwrap_or_default())
                        .collect()
                })
                .collect();
            return write_csv(&headers, &rows);
        }
        if array.is_empty() {
            return String::new();
        }
    }

    let json = serde_json::to_string(report).unwrap_or_default();
    write_csv(&["report".to_string()], &[vec![json]])
}

/// Render one scalar cell. Nested values keep their JSON text so no data is
/// silently dropped; `null` renders as an empty cell.
fn scalar_to_field(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(_) | Value::Number(_) => value.to_string(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// RFC4180 rows with the same writer configuration the table formatter uses.
fn write_csv(headers: &[String], rows: &[Vec<String>]) -> String {
    let mut writer = csv::WriterBuilder::new()
        .terminator(csv::Terminator::Any(b'\n'))
        .from_writer(Vec::new());
    if writer.write_record(headers).is_err() {
        return String::new();
    }
    for row in rows {
        if writer.write_record(row).is_err() {
            return String::new();
        }
    }
    let bytes = match writer.into_inner() {
        Ok(bytes) => bytes,
        Err(_) => return String::new(),
    };
    String::from_utf8(bytes).unwrap_or_default()
}

/// Sanitize and bound an untrusted diagnostic string. Never a secret value.
fn sanitize_diagnostic(input: &str) -> String {
    let sanitized = sanitize_control_chars(input);
    if sanitized.chars().count() <= MAX_DIAGNOSTIC_CHARS {
        return sanitized;
    }
    sanitized.chars().take(MAX_DIAGNOSTIC_CHARS).collect()
}

/// Per-item outcome of a batch command (migrate, bulk `set`, `mv`, imports,
/// file batches, `rotate --due`). Names only, never values.
// Callers land in Task 2+.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ItemReport {
    pub summary: ItemSummary,
    pub items: Vec<ItemOutcome>,
}

// Callers land in Task 2+.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct ItemSummary {
    pub total: usize,
    pub succeeded: usize,
    pub skipped: usize,
    pub failed: usize,
}

// Callers land in Task 2+.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ItemOutcome {
    pub name: String,
    pub status: ItemStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// Callers land in Task 2+.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ItemStatus {
    Ok,
    Skipped,
    Failed,
}

// Callers land in Task 2+.
#[allow(dead_code)]
impl ItemReport {
    pub fn new() -> Self {
        Self {
            summary: ItemSummary::default(),
            items: Vec::new(),
        }
    }

    pub fn ok(&mut self, name: &str, detail: Option<&str>) {
        self.summary.total += 1;
        self.summary.succeeded += 1;
        self.push(name, ItemStatus::Ok, detail, None);
    }

    pub fn skipped(&mut self, name: &str, detail: Option<&str>) {
        self.summary.total += 1;
        self.summary.skipped += 1;
        self.push(name, ItemStatus::Skipped, detail, None);
    }

    pub fn failed(&mut self, name: &str, error: &str) {
        self.summary.total += 1;
        self.summary.failed += 1;
        self.push(name, ItemStatus::Failed, None, Some(error));
    }

    pub fn has_failures(&self) -> bool {
        self.summary.failed > 0
    }

    fn push(&mut self, name: &str, status: ItemStatus, detail: Option<&str>, error: Option<&str>) {
        self.items.push(ItemOutcome {
            name: sanitize_diagnostic(name),
            status,
            detail: detail.map(sanitize_diagnostic),
            error: error.map(sanitize_diagnostic),
        });
    }
}

impl Default for ItemReport {
    fn default() -> Self {
        Self::new()
    }
}

/// Serializes the tests that touch the process-wide [`PENDING`] slot.
#[cfg(test)]
fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static TEST_LOCK: Mutex<()> = Mutex::new(());
    let guard = match TEST_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let _ = take_pending();
    guard
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::utils::format::OutputFormat;

    #[test]
    fn is_machine_mode_truth_table() {
        let cases = [
            (true, OutputFormat::Json, true),
            (true, OutputFormat::Yaml, true),
            (true, OutputFormat::Csv, true),
            (true, OutputFormat::Table, false),
            (true, OutputFormat::Plain, false),
            (true, OutputFormat::Raw, false),
            (true, OutputFormat::Template, false),
            (true, OutputFormat::Auto, false),
            (false, OutputFormat::Json, false),
            (false, OutputFormat::Yaml, false),
            (false, OutputFormat::Csv, false),
        ];
        for (explicit, format, expected) in cases {
            let config = Config {
                format_explicit: explicit,
                runtime_output_format: format,
                ..Default::default()
            };
            assert_eq!(is_machine_mode(&config), expected, "{explicit} {format:?}");
        }
    }

    #[test]
    fn item_report_counts_and_sanitizes() {
        let mut report = ItemReport::new();
        report.ok("a", Some("created"));
        report.skipped("b", Some("exists"));
        report.failed("c", "\u{1b}[31mboom\u{1b}[0m");
        report.failed("d", &"x".repeat(2000));

        assert_eq!(report.summary.total, 4);
        assert_eq!(report.summary.succeeded, 1);
        assert_eq!(report.summary.skipped, 1);
        assert_eq!(report.summary.failed, 2);
        assert!(report.has_failures());

        let escaped = report.items[2].error.as_deref().unwrap();
        assert!(!escaped.contains('\u{1b}'), "{escaped}");
        assert!(escaped.contains("\\x1B"), "{escaped}");

        let truncated = report.items[3].error.as_deref().unwrap();
        assert_eq!(truncated.chars().count(), 1024);

        let clean = ItemReport::new();
        assert!(!clean.has_failures());
    }

    #[test]
    fn render_failure_json_attaches_report() {
        let envelope = serde_json::json!({
            "error": { "code": "xv-scan-leak-detected", "message": "leak", "exit_code": 50 }
        });
        let report = serde_json::json!([{ "secret": "A" }]);
        let rendered = render_failure(OutputFormat::Json, envelope.clone(), Some(report.clone()));

        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(parsed["error"]["code"], "xv-scan-leak-detected");
        assert_eq!(parsed["error"]["exit_code"], 50);
        assert_eq!(parsed["report"], report);
        assert_eq!(parsed.as_object().unwrap().len(), 2);

        // Without a report the envelope is byte-identical to today's output.
        let bare = render_failure(OutputFormat::Json, envelope.clone(), None);
        assert_eq!(bare, serde_json::to_string(&envelope).unwrap());
    }

    #[test]
    fn render_success_csv_renders_item_report_rows() {
        let mut report = ItemReport::new();
        report.ok("alpha", Some("created"));
        report.failed("beta", "nope");
        let value = serde_json::to_value(&report).unwrap();

        let csv = render_success(OutputFormat::Csv, &value);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines[0], "name,status,detail,error");
        assert_eq!(lines[1], "alpha,ok,created,");
        assert_eq!(lines[2], "beta,failed,,nope");
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn render_success_csv_renders_array_of_objects() {
        let value = serde_json::json!([
            { "name": "a", "count": 1 },
            { "name": "b", "extra": true },
        ]);
        let csv = render_success(OutputFormat::Csv, &value);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines[0], "name,count,extra");
        assert_eq!(lines[1], "a,1,");
        assert_eq!(lines[2], "b,,true");
    }

    #[test]
    fn render_success_csv_falls_back_to_one_report_column() {
        let value = serde_json::json!({ "due": 2 });
        let csv = render_success(OutputFormat::Csv, &value);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines[0], "report");
        assert_eq!(lines[1], "\"{\"\"due\"\":2}\"");
    }

    #[test]
    fn report_is_a_no_op_outside_machine_mode() {
        let _guard = test_lock();
        let config = Config {
            format_explicit: false,
            runtime_output_format: OutputFormat::Json,
            ..Default::default()
        };
        report(&config, &serde_json::json!({ "a": 1 }));
        assert!(take_pending().is_none());
    }

    #[test]
    fn report_stores_a_single_pending_document_in_machine_mode() {
        let _guard = test_lock();
        let config = Config {
            format_explicit: true,
            runtime_output_format: OutputFormat::Yaml,
            ..Default::default()
        };
        report(&config, &serde_json::json!({ "a": 1 }));
        assert_eq!(take_pending(), Some(serde_json::json!({ "a": 1 })));
        assert!(take_pending().is_none());
    }
}
