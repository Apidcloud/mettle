use std::fmt::Write as _;
use std::io::{self, IsTerminal as _, Write as _};
use std::sync::Mutex;
use std::time::Duration;

use mettle_capability::{OperationReport, ReportOutcome, ReportSection, Value};
use mettle_runtime::{
    ExecutionObserver, OperationEvent, WorkloadKind, WorkloadPhase, WorkloadSnapshot,
};

const DEFAULT_PREVIEW_CHARS: usize = 8 * 1024;

#[derive(Default)]
struct ObserverState {
    operations: Vec<OperationEvent>,
    rendered_lines: usize,
}

pub struct CliObserver {
    state: Mutex<ObserverState>,
    progress: bool,
    interactive: bool,
}

impl CliObserver {
    pub fn new(progress: bool) -> Self {
        Self {
            state: Mutex::new(ObserverState::default()),
            progress,
            interactive: io::stderr().is_terminal(),
        }
    }

    pub fn take_operations(&self) -> Vec<OperationEvent> {
        let mut state = self.state.lock().expect("CLI observer lock was poisoned");
        std::mem::take(&mut state.operations)
    }

    pub fn clear_progress(&self) {
        let mut state = self.state.lock().expect("CLI observer lock was poisoned");
        if state.rendered_lines == 0 {
            return;
        }
        let mut stderr = io::stderr().lock();
        let _ = write!(stderr, "\x1b[{}A", state.rendered_lines);
        for _ in 0..state.rendered_lines {
            let _ = write!(stderr, "\r\x1b[2K\n");
        }
        let _ = write!(stderr, "\x1b[{}A\r", state.rendered_lines);
        let _ = stderr.flush();
        state.rendered_lines = 0;
    }

    fn draw_workload(&self, snapshot: &WorkloadSnapshot) {
        if !self.progress || !self.interactive {
            return;
        }
        let lines = workload_progress_lines(snapshot);
        let mut state = self.state.lock().expect("CLI observer lock was poisoned");
        let mut stderr = io::stderr().lock();
        if state.rendered_lines > 0 {
            let _ = write!(stderr, "\x1b[{}A", state.rendered_lines);
        }
        for line in &lines {
            let _ = writeln!(stderr, "\r\x1b[2K{line}");
        }
        let _ = stderr.flush();
        state.rendered_lines = lines.len();
    }
}

impl ExecutionObserver for CliObserver {
    fn operation_completed(&self, event: OperationEvent) {
        self.state
            .lock()
            .expect("CLI observer lock was poisoned")
            .operations
            .push(event);
    }

    fn workload_updated(&self, snapshot: WorkloadSnapshot) {
        self.draw_workload(&snapshot);
    }
}

pub struct ExecutionReport<'a> {
    pub flow: &'a str,
    pub duration: Duration,
    pub result: &'a Value,
    pub operations: &'a [OperationEvent],
}

impl ExecutionReport<'_> {
    pub fn test_human(&self, verbose: bool, color: bool) -> String {
        let mut output = String::from("\n");
        for operation in self.operations {
            render_operation(&mut output, operation, color, verbose);
        }
        write!(
            output,
            "{} Passed in {}",
            style("✓", "32", color),
            display_duration(self.duration)
        )
        .expect("writing to a string cannot fail");
        output
    }

    pub fn test_quiet(&self, color: bool) -> String {
        format!(
            "{} {} · {}",
            style("✓", "32", color),
            self.flow,
            display_duration(self.duration)
        )
    }

    pub fn human(&self, verbose: bool, color: bool) -> String {
        if let Some(summary) = workload_result(self.result, color) {
            return format!("{}\n\n{summary}", style(self.flow, "1", color));
        }

        let mut output = String::new();
        writeln!(output, "{}\n", style(self.flow, "1", color))
            .expect("writing to a string cannot fail");

        for operation in self.operations {
            render_operation(&mut output, operation, color, verbose);
        }

        let final_operation_report = self.operations.iter().find_map(|operation| {
            (matches!(&operation.result, Ok(value) if value == self.result)
                || operation
                    .report
                    .as_ref()
                    .and_then(|report| report.payload.as_ref())
                    == Some(self.result))
            .then_some(operation.report.as_ref())
            .flatten()
        });
        let payload = final_operation_report.and_then(|report| report.payload.as_ref());
        let show_result = if verbose {
            final_operation_report.is_none()
        } else {
            self.operations.is_empty() || payload.is_some() || final_operation_report.is_none()
        };
        if show_result {
            let label = if verbose {
                "Full result"
            } else if self.operations.is_empty() || final_operation_report.is_none() {
                "Result"
            } else {
                "Response"
            };
            writeln!(output, "  {}", style(label, "2", color))
                .expect("writing to a string cannot fail");
            let formatted = pretty_value(payload.unwrap_or(self.result), verbose);
            for line in formatted.lines() {
                writeln!(output, "    {line}").expect("writing to a string cannot fail");
            }
            output.push('\n');
        }

        write!(
            output,
            "{} Completed in {}",
            style("✓", "32", color),
            display_duration(self.duration)
        )
        .expect("writing to a string cannot fail");
        output
    }

    pub fn quiet(&self, color: bool) -> String {
        format!(
            "{} {} · {}",
            style("✓", "32", color),
            self.flow,
            display_duration(self.duration)
        )
    }

    pub fn json(&self) -> String {
        serde_json::json!({
            "flow": self.flow,
            "durationNanos": duration_nanos(self.duration),
            "result": value_json(self.result),
        })
        .to_string()
    }
}

fn render_operation(output: &mut String, event: &OperationEvent, color: bool, verbose: bool) {
    match &event.result {
        Ok(value) => {
            if let Some(report) = &event.report {
                writeln!(output, "  {} {}", style("✓", "32", color), report.summary)
                    .expect("writing to a string cannot fail");
                writeln!(
                    output,
                    "    {} · {}\n",
                    style(
                        &report.outcome,
                        report_outcome_color(report.outcome_kind),
                        color
                    ),
                    display_duration(event.duration)
                )
                .expect("writing to a string cannot fail");
                if verbose {
                    render_operation_report(output, report, color);
                }
            } else {
                writeln!(
                    output,
                    "  {} {}.{} · {}\n",
                    style("✓", "32", color),
                    event.capability,
                    event.operation,
                    display_duration(event.duration)
                )
                .expect("writing to a string cannot fail");
                if verbose {
                    render_verbose_value(output, value, color);
                }
            }
        }
        Err(message) => {
            writeln!(
                output,
                "  {} {}.{} · {}\n    {}\n",
                style("✗", "31", color),
                event.capability,
                event.operation,
                display_duration(event.duration),
                message
            )
            .expect("writing to a string cannot fail");
        }
    }
}

fn workload_progress_lines(snapshot: &WorkloadSnapshot) -> Vec<String> {
    let phase = match snapshot.phase {
        WorkloadPhase::Starting => "STARTING",
        WorkloadPhase::Running => "RUNNING",
        WorkloadPhase::Draining => "DRAINING",
        WorkloadPhase::Completed => "COMPLETED",
    };
    let (description, duration, limit, target_rate) = match snapshot.kind {
        WorkloadKind::Rate {
            target,
            period,
            duration,
            limit,
            ..
        } => (
            format!("rate {target}/{}", display_duration(period)),
            duration,
            limit,
            Some(count_f64(target) / period.as_secs_f64()),
        ),
        WorkloadKind::Concurrency { limit, duration } => {
            (format!("concurrency {limit}"), duration, limit, None)
        }
    };
    let rate_window = snapshot.elapsed.min(duration);
    let achieved = if rate_window.is_zero() {
        0.0
    } else {
        count_f64(snapshot.started) / rate_window.as_secs_f64()
    };
    let rate = target_rate.map_or_else(
        || format!("throughput {achieved:.1}/s"),
        |_| format!("achieved {achieved:.1}/s"),
    );
    vec![
        format!("Mettle · {description} for {}", display_duration(duration)),
        format!(
            "{phase:<9} {} / {}   active {} / {limit}   {rate}",
            display_duration(snapshot.elapsed.min(duration)),
            display_duration(duration),
            snapshot.active
        ),
        format!(
            "started {}   completed {}   ok {}   failed {}   dropped {}",
            snapshot.started,
            snapshot.completed,
            snapshot.success,
            snapshot.failed,
            snapshot.dropped
        ),
        format!(
            "latency p50 {}   p95 {}   p99 {}",
            display_duration(snapshot.latency_p50),
            display_duration(snapshot.latency_p95),
            display_duration(snapshot.latency_p99)
        ),
    ]
}

fn workload_result(value: &Value, color: bool) -> Option<String> {
    let fields = value.as_object()?;
    let completed = integer(fields.get("count")?)?;
    let started = integer(fields.get("started")?)?;
    let success = integer(fields.get("success")?)?;
    let failed = integer(fields.get("failed")?)?;
    let dropped = integer(fields.get("dropped")?)?;
    let elapsed = duration(fields.get("duration")?)?;
    let latency = fields.get("latency")?.as_object()?;
    let p50 = duration(latency.get("p50")?)?;
    let p95 = duration(latency.get("p95")?)?;
    let p99 = duration(latency.get("p99")?)?;
    let max = duration(latency.get("max")?)?;

    let mut output = String::new();
    let saturated = matches!(fields.get("saturated"), Some(Value::Boolean(true)));
    let status = if saturated {
        "COMPLETED · SATURATED"
    } else {
        "COMPLETED"
    };
    writeln!(
        output,
        "{}  {}",
        style(status, if saturated { "33;1" } else { "36;1" }, color),
        display_duration(elapsed)
    )
    .expect("writing to a string cannot fail");
    writeln!(
        output,
        "\n{started} started · {success} successful · {failed} failed · {dropped} dropped · {completed} completed"
    )
    .expect("writing to a string cannot fail");
    if let Some(Value::Object(rate)) = fields.get("rate") {
        let target = integer(rate.get("target")?)?;
        let period = duration(rate.get("period")?)?;
        let actual = number(rate.get("actual")?)?;
        let target_per_second = integer_f64(target)? / period.as_secs_f64();
        let actual_per_second = actual / period.as_secs_f64();
        writeln!(
            output,
            "Rate {actual_per_second:.1}/s · target {target_per_second:.1}/s"
        )
        .expect("writing to a string cannot fail");
    } else if let Some(Value::Object(concurrency)) = fields.get("concurrency") {
        let limit = integer(concurrency.get("limit")?)?;
        writeln!(output, "Concurrency {limit}").expect("writing to a string cannot fail");
    }
    write!(
        output,
        "Latency p50 {} · p95 {} · p99 {} · max {}",
        display_duration(p50),
        display_duration(p95),
        display_duration(p99),
        display_duration(max)
    )
    .expect("writing to a string cannot fail");
    Some(output)
}

pub fn failure_summary(
    flow: &str,
    operations: &[OperationEvent],
    color: bool,
    test: bool,
) -> String {
    let mut output = String::new();
    if !test {
        writeln!(output, "{}\n", style(flow, "1", color)).expect("writing to a string cannot fail");
    }
    for operation in operations {
        render_operation(&mut output, operation, color, false);
    }
    write!(
        output,
        "{} {} failed",
        style("✗", "31", color),
        if test { "Test" } else { "Flow" }
    )
    .expect("writing to a string cannot fail");
    output
}

fn render_verbose_value(output: &mut String, value: &Value, color: bool) {
    writeln!(output, "    {}", style("Details", "2", color))
        .expect("writing to a string cannot fail");
    for line in pretty_value(value, true).lines() {
        writeln!(output, "      {line}").expect("writing to a string cannot fail");
    }
    output.push('\n');
}

fn render_operation_report(output: &mut String, report: &OperationReport, color: bool) {
    for section in &report.sections {
        match section {
            ReportSection::Fields { title, fields } => {
                writeln!(output, "    {}", style(title, "2", color))
                    .expect("writing to a string cannot fail");
                if fields.is_empty() {
                    writeln!(output, "      (none)").expect("writing to a string cannot fail");
                } else {
                    for (name, value) in fields {
                        writeln!(output, "      {name}: {value}")
                            .expect("writing to a string cannot fail");
                    }
                }
            }
            ReportSection::Value { title, value } => {
                writeln!(output, "    {}", style(title, "2", color))
                    .expect("writing to a string cannot fail");
                for line in format_value(value).lines() {
                    writeln!(output, "      {line}").expect("writing to a string cannot fail");
                }
            }
        }
    }
    output.push('\n');
}

fn pretty_value(value: &Value, complete: bool) -> String {
    let formatted = format_value(value);
    if complete {
        return formatted;
    }
    truncate(&formatted, DEFAULT_PREVIEW_CHARS)
}

fn format_value(value: &Value) -> String {
    match value {
        Value::Array(_) | Value::Object(_) | Value::Bytes(_) => {
            serde_json::to_string_pretty(&value_json(value))
                .expect("Mettle values always convert to JSON")
        }
        Value::String(value) => value.clone(),
        Value::Sensitive(_) => "[REDACTED]".to_owned(),
        _ => value.to_string(),
    }
}

fn truncate(value: &str, limit: usize) -> String {
    let mut characters = value.chars();
    let preview = characters.by_ref().take(limit).collect::<String>();
    if characters.next().is_none() {
        return value.to_owned();
    }
    format!(
        "{preview}\n… response truncated after {} KiB; use --verbose or --raw for the complete value",
        limit / 1024
    )
}

pub fn value_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Boolean(value) => serde_json::Value::Bool(*value),
        Value::Integer(value) => serde_json::Value::Number((*value).into()),
        Value::Float(value) => serde_json::Number::from_f64(*value)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Value::String(value) => serde_json::Value::String(value.clone()),
        Value::Bytes(value) => serde_json::Value::Array(
            value
                .iter()
                .map(|value| serde_json::Value::Number((*value).into()))
                .collect(),
        ),
        Value::Duration(value) => serde_json::Value::String(format!("{}ns", value.as_nanos())),
        Value::Array(values) => serde_json::Value::Array(values.iter().map(value_json).collect()),
        Value::Object(values) => serde_json::Value::Object(
            values
                .iter()
                .map(|(name, value)| (name.clone(), value_json(value)))
                .collect(),
        ),
        Value::Sensitive(_) => serde_json::Value::String("[REDACTED]".to_owned()),
    }
}

pub fn raw_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        _ => value_json(value).to_string(),
    }
}

fn integer(value: &Value) -> Option<i64> {
    match value {
        Value::Integer(value) => Some(*value),
        _ => None,
    }
}

fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Integer(value) => integer_f64(*value),
        Value::Float(value) => Some(*value),
        _ => None,
    }
}

fn count_f64(value: usize) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}

fn integer_f64(value: i64) -> Option<f64> {
    i32::try_from(value).ok().map(f64::from)
}

fn duration(value: &Value) -> Option<Duration> {
    match value {
        Value::Duration(value) => Some(*value),
        _ => None,
    }
}

fn duration_nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

pub fn display_duration(duration: Duration) -> String {
    if duration >= Duration::from_secs(1) {
        if duration.as_nanos().is_multiple_of(1_000_000_000) {
            format!("{}s", duration.as_secs())
        } else {
            format!("{:.2}s", duration.as_secs_f64())
        }
    } else if duration >= Duration::from_millis(1) {
        if duration.as_nanos().is_multiple_of(1_000_000) {
            format!("{}ms", duration.as_millis())
        } else {
            format!("{:.2}ms", duration.as_secs_f64() * 1_000.0)
        }
    } else if duration >= Duration::from_micros(1) {
        format!("{:.3}ms", duration.as_secs_f64() * 1_000.0)
    } else if duration.is_zero() {
        "0ms".to_owned()
    } else {
        format!("{:.6}ms", duration.as_secs_f64() * 1_000.0)
    }
}

fn report_outcome_color(outcome: ReportOutcome) -> &'static str {
    match outcome {
        ReportOutcome::Success => "32",
        ReportOutcome::Warning => "33",
        ReportOutcome::Failure => "31",
        ReportOutcome::Neutral => "36",
    }
}

fn style(value: &str, code: &str, enabled: bool) -> String {
    if enabled {
        format!("\x1b[{code}m{value}\x1b[0m")
    } else {
        value.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{ExecutionReport, display_duration, truncate};
    use mettle_capability::{Capability, Span, Value};
    use mettle_http::HttpCapability;
    use mettle_runtime::OperationEvent;

    #[test]
    fn report_formats_a_named_value() {
        let value = Value::Object(BTreeMap::from([(
            "active".to_owned(),
            Value::Boolean(true),
        )]));
        let output = ExecutionReport {
            flow: "health",
            duration: std::time::Duration::from_millis(12),
            result: &value,
            operations: &[],
        }
        .human(false, false);
        assert!(output.contains("health"));
        assert!(output.contains("\"active\": true"));
        assert!(output.contains("✓ Completed in 12ms"));
    }

    #[test]
    fn submillisecond_duration_is_displayed_in_milliseconds() {
        assert_eq!(
            display_duration(std::time::Duration::from_micros(9)),
            "0.009ms"
        );
        assert_eq!(
            display_duration(std::time::Duration::from_nanos(9)),
            "0.000009ms"
        );
        assert_eq!(display_duration(std::time::Duration::ZERO), "0ms");
    }

    #[test]
    fn human_report_shows_transformed_result_after_an_operation() {
        let response = Value::Object(BTreeMap::from([
            ("method".to_owned(), Value::String("GET".to_owned())),
            ("status".to_owned(), Value::Integer(200)),
            (
                "url".to_owned(),
                Value::String("https://example.test/post".to_owned()),
            ),
        ]));
        let result = Value::Object(BTreeMap::from([(
            "profile".to_owned(),
            Value::String("qa".to_owned()),
        )]));
        let operation = OperationEvent {
            capability: "http".to_owned(),
            operation: "get".to_owned(),
            duration: std::time::Duration::from_millis(10),
            span: Span::default(),
            result: Ok(response.clone()),
            report: HttpCapability::new().report(0, &response),
        };
        let output = ExecutionReport {
            flow: "main",
            duration: std::time::Duration::from_millis(12),
            result: &result,
            operations: &[operation],
        }
        .human(false, false);
        assert!(output.contains("Result"), "{output}");
        assert!(output.contains("\"profile\": \"qa\""), "{output}");
    }

    #[test]
    fn preview_reports_truncation() {
        let output = truncate("abcdef", 3);
        assert!(output.starts_with("abc"));
        assert!(output.contains("truncated"));
    }

    #[test]
    fn verbose_output_includes_full_http_response_details() {
        let value = Value::Object(BTreeMap::from([
            ("body".to_owned(), Value::String("raw body".to_owned())),
            (
                "bodyBytes".to_owned(),
                Value::Bytes(std::sync::Arc::from([114, 97, 119])),
            ),
            (
                "headers".to_owned(),
                Value::Object(BTreeMap::from([
                    (
                        "content-type".to_owned(),
                        Value::String("application/json".to_owned()),
                    ),
                    (
                        "set-cookie".to_owned(),
                        Value::String("session=secret".to_owned()).sensitive(),
                    ),
                ])),
            ),
            (
                "json".to_owned(),
                Value::Object(BTreeMap::from([(
                    "status".to_owned(),
                    Value::String("ok".to_owned()),
                )])),
            ),
            ("method".to_owned(), Value::String("GET".to_owned())),
            ("status".to_owned(), Value::Integer(200)),
            (
                "url".to_owned(),
                Value::String("https://example.test/health".to_owned()),
            ),
        ]));
        let operation = OperationEvent {
            capability: "http".to_owned(),
            operation: "get".to_owned(),
            duration: std::time::Duration::from_millis(10),
            span: Span::default(),
            result: Ok(value.clone()),
            report: HttpCapability::new().report(0, &value),
        };
        let report = ExecutionReport {
            flow: "health",
            duration: std::time::Duration::from_millis(12),
            result: &value,
            operations: &[operation],
        };

        let normal = report.human(false, false);
        let verbose = report.human(true, false);
        assert!(!normal.contains("\"headers\""));
        assert!(verbose.contains("GET    https://example.test/health"));
        assert!(verbose.contains("Headers"));
        assert!(verbose.contains("content-type: application/json"));
        assert!(verbose.contains("set-cookie: [REDACTED]"));
        assert!(!verbose.contains("session=secret"));
        assert!(verbose.contains("JSON body"));
        assert!(verbose.contains("\"status\": \"ok\""));
        assert!(verbose.contains("https://example.test/health"));
        assert!(!verbose.contains("bodyBytes"));
        assert!(!verbose.contains("raw body"));
    }
}
