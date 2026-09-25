use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{self, IsTerminal as _, Write as _};
use std::sync::Mutex;
use std::time::Duration;

use mettle_capability::{OperationReport, ReportOutcome, ReportSection, Value};
use mettle_runtime::{
    ExecutionEvent, ExecutionEventKind, ExecutionObserver, ExecutionScope, OperationEvent,
    WorkloadKind, WorkloadOperation, WorkloadPhase, WorkloadSnapshot,
};

const DEFAULT_PREVIEW_CHARS: usize = 8 * 1024;
const MAX_WORKLOAD_ECHOES: usize = 50;
const MAX_TRACKED_WORKLOADS: usize = 32;
const MAX_ACTION_SITES: usize = 32;
const MAX_VISIBLE_WORKLOADS: usize = 3;
const MAX_VISIBLE_ACTIONS: usize = 3;
const MAX_FAILURE_SAMPLES: usize = 3;
const ACTION_HISTOGRAM_SUB_BUCKETS: usize = 8;
const ACTION_HISTOGRAM_BUCKETS: usize = 1 + 64 * ACTION_HISTOGRAM_SUB_BUCKETS;

#[derive(Default)]
pub struct CapturedEvents {
    pub events: Vec<ExecutionEvent>,
    pub omitted_workload_echoes: usize,
    pub workloads: Vec<WorkloadView>,
    pub omitted_workloads: u64,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ActionSite {
    source: usize,
    start: usize,
    flow: String,
    capability: String,
    operation: String,
}

#[derive(Clone, Debug)]
pub struct ActionStats {
    site: ActionSite,
    line: Option<usize>,
    calls: u64,
    errors: u64,
    http_4xx: u64,
    http_5xx: u64,
    histogram: Vec<u64>,
    failure_samples: Vec<String>,
}

impl ActionStats {
    fn new(site: ActionSite, line: Option<usize>) -> Self {
        Self {
            site,
            line,
            calls: 0,
            errors: 0,
            http_4xx: 0,
            http_5xx: 0,
            histogram: vec![0; ACTION_HISTOGRAM_BUCKETS],
            failure_samples: Vec::new(),
        }
    }

    fn record(&mut self, operation: &WorkloadOperation) {
        self.calls += 1;
        let nanos = u64::try_from(operation.duration.as_nanos()).unwrap_or(u64::MAX);
        let bucket = if nanos == 0 {
            0
        } else {
            let exponent = 63 - nanos.leading_zeros() as usize;
            let base = 1_u64 << exponent;
            let sub = (u128::from(nanos - base) * ACTION_HISTOGRAM_SUB_BUCKETS as u128
                / u128::from(base)) as usize;
            1 + exponent * ACTION_HISTOGRAM_SUB_BUCKETS + sub.min(ACTION_HISTOGRAM_SUB_BUCKETS - 1)
        };
        self.histogram[bucket] += 1;
        let failure = if operation.failed {
            self.errors += 1;
            Some("operation failed".to_owned())
        } else {
            match operation.http_status {
                Some(400..=499) => {
                    self.http_4xx += 1;
                    Some(format!(
                        "HTTP {}",
                        operation.http_status.unwrap_or_default()
                    ))
                }
                Some(500..=599) => {
                    self.http_5xx += 1;
                    Some(format!(
                        "HTTP {}",
                        operation.http_status.unwrap_or_default()
                    ))
                }
                _ => None,
            }
        };
        if let Some(failure) = failure
            && self.failure_samples.len() < MAX_FAILURE_SAMPLES
            && !self.failure_samples.contains(&failure)
        {
            self.failure_samples.push(failure);
        }
    }

    fn p95(&self) -> Duration {
        if self.calls == 0 {
            return Duration::ZERO;
        }
        let rank = self.calls.saturating_mul(95).div_ceil(100);
        let mut seen = 0;
        for (index, count) in self.histogram.iter().enumerate() {
            seen += count;
            if seen >= rank {
                if index == 0 {
                    return Duration::ZERO;
                }
                let exponent = (index - 1) / ACTION_HISTOGRAM_SUB_BUCKETS;
                let sub = (index - 1) % ACTION_HISTOGRAM_SUB_BUCKETS;
                let base = 1_u64 << exponent;
                let upper = u128::from(base)
                    + (u128::from(base) * (sub + 1) as u128)
                        .div_ceil(ACTION_HISTOGRAM_SUB_BUCKETS as u128)
                    - 1;
                return Duration::from_nanos(u64::try_from(upper).unwrap_or(u64::MAX));
            }
        }
        Duration::ZERO
    }
}

#[derive(Clone, Debug)]
pub struct WorkloadView {
    snapshot: WorkloadSnapshot,
    actions: BTreeMap<ActionSite, ActionStats>,
    overflow_calls: u64,
}

#[derive(Default)]
struct ObserverState {
    captured: CapturedEvents,
    workload_echoes: usize,
    rendered_lines: usize,
    workloads: BTreeMap<u64, WorkloadView>,
    omitted_workloads: u64,
}

pub struct CliObserver {
    state: Mutex<ObserverState>,
    progress: bool,
    interactive: bool,
    sources: Vec<String>,
}

impl CliObserver {
    pub fn new(progress: bool) -> Self {
        Self {
            state: Mutex::new(ObserverState::default()),
            progress,
            interactive: io::stderr().is_terminal(),
            sources: Vec::new(),
        }
    }

    pub fn with_sources(mut self, sources: Vec<String>) -> Self {
        self.sources = sources;
        self
    }

    pub fn take_events(&self) -> CapturedEvents {
        let mut state = self.state.lock().expect("CLI observer lock was poisoned");
        let mut captured = std::mem::take(&mut state.captured);
        captured.workloads = std::mem::take(&mut state.workloads).into_values().collect();
        captured.omitted_workloads = state.omitted_workloads;
        captured
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

    fn draw_workloads(&self, state: &mut ObserverState) {
        if !self.progress || !self.interactive {
            return;
        }
        let mut lines = vec![format!(
            "Mettle · {} workload{}{}",
            state.workloads.len(),
            if state.workloads.len() == 1 { "" } else { "s" },
            if state.omitted_workloads == 0 {
                String::new()
            } else {
                format!(" · {} omitted", state.omitted_workloads)
            }
        )];
        let mut visible = state.workloads.values().collect::<Vec<_>>();
        visible.sort_by_key(|view| {
            (
                matches!(
                    view.snapshot.phase,
                    WorkloadPhase::Completed | WorkloadPhase::Aborted
                ),
                view.snapshot.id,
            )
        });
        for view in visible.iter().take(MAX_VISIBLE_WORKLOADS) {
            lines.extend(workload_progress_lines(view));
        }
        if visible.len() > MAX_VISIBLE_WORKLOADS {
            lines.push(format!(
                "… {} more workloads in final report",
                visible.len() - MAX_VISIBLE_WORKLOADS
            ));
        }
        let mut stderr = io::stderr().lock();
        if state.rendered_lines > 0 {
            let _ = write!(stderr, "\x1b[{}A", state.rendered_lines);
        }
        for line in &lines {
            let _ = writeln!(stderr, "\r\x1b[2K{line}");
        }
        if state.rendered_lines > lines.len() {
            for _ in lines.len()..state.rendered_lines {
                let _ = write!(stderr, "\r\x1b[2K\n");
            }
            let _ = write!(stderr, "\x1b[{}A", state.rendered_lines - lines.len());
        }
        let _ = stderr.flush();
        state.rendered_lines = lines.len();
    }
}

impl ExecutionObserver for CliObserver {
    fn execution_event(&self, event: ExecutionEvent) {
        let mut state = self.state.lock().expect("CLI observer lock was poisoned");
        if event.inside_workload && matches!(event.kind, ExecutionEventKind::Echo(_)) {
            if state.workload_echoes == MAX_WORKLOAD_ECHOES {
                state.captured.omitted_workload_echoes += 1;
                return;
            }
            state.workload_echoes += 1;
        }
        state.captured.events.push(event);
    }

    fn workload_updated(&self, snapshot: WorkloadSnapshot) {
        let mut state = self.state.lock().expect("CLI observer lock was poisoned");
        if !state.workloads.contains_key(&snapshot.id) {
            if snapshot.phase != WorkloadPhase::Starting {
                return;
            }
            if state.workloads.len() == MAX_TRACKED_WORKLOADS {
                let completed = state.workloads.iter().find_map(|(id, view)| {
                    matches!(
                        view.snapshot.phase,
                        WorkloadPhase::Completed | WorkloadPhase::Aborted
                    )
                    .then_some(*id)
                });
                if let Some(id) = completed {
                    state.workloads.remove(&id);
                } else {
                    state.omitted_workloads += 1;
                    return;
                }
                state.omitted_workloads += 1;
            }
        }
        state
            .workloads
            .entry(snapshot.id)
            .and_modify(|view| {
                view.snapshot = snapshot.clone();
            })
            .or_insert_with(|| WorkloadView {
                snapshot,
                actions: BTreeMap::new(),
                overflow_calls: 0,
            });
        self.draw_workloads(&mut state);
    }

    fn workload_operation(&self, operation: WorkloadOperation) {
        let mut state = self.state.lock().expect("CLI observer lock was poisoned");
        let Some(view) = state.workloads.get_mut(&operation.workload_id) else {
            return;
        };
        let site = ActionSite {
            source: operation.span.source,
            start: operation.span.start,
            flow: operation.flow.clone(),
            capability: operation.capability.clone(),
            operation: operation.operation.clone(),
        };
        if let Some(action) = view.actions.get_mut(&site) {
            action.record(&operation);
            return;
        }
        if view.actions.len() == MAX_ACTION_SITES {
            view.overflow_calls += 1;
            return;
        }
        let line = self.sources.get(operation.span.source).map(|source| {
            source
                .bytes()
                .take(operation.span.start)
                .filter(|byte| *byte == b'\n')
                .count()
                + 1
        });
        let mut action = ActionStats::new(site.clone(), line);
        action.record(&operation);
        view.actions.insert(site, action);
    }
}

pub struct ExecutionReport<'a> {
    pub flow: &'a str,
    pub duration: Duration,
    pub result: &'a Value,
    pub events: &'a [ExecutionEvent],
    pub omitted_workload_echoes: usize,
    pub workloads: &'a [WorkloadView],
    pub omitted_workloads: u64,
}

impl ExecutionReport<'_> {
    pub fn test_human(&self, verbose: bool, color: bool) -> String {
        let mut output = String::from("\n");
        render_events(
            &mut output,
            self.events,
            self.omitted_workload_echoes,
            color,
            verbose,
        );
        if !self.workloads.is_empty() {
            output.push_str(&workloads_result(
                self.workloads,
                self.omitted_workloads,
                color,
            ));
            output.push_str("\n\n");
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
        let mut output = String::new();
        writeln!(output, "{}\n", style(self.flow, "1", color))
            .expect("writing to a string cannot fail");
        render_events(
            &mut output,
            self.events,
            self.omitted_workload_echoes,
            color,
            verbose,
        );
        if !self.workloads.is_empty() {
            output.push_str(&workloads_result(
                self.workloads,
                self.omitted_workloads,
                color,
            ));
            if !is_workload_tree(self.result) {
                writeln!(output, "\n\n  {}", style("Result", "2", color))
                    .expect("writing to a string cannot fail");
                for line in pretty_value(self.result, verbose).lines() {
                    writeln!(output, "    {line}").expect("writing to a string cannot fail");
                }
            }
            write!(
                output,
                "\n\n{} Completed in {}",
                style("✓", "32", color),
                display_duration(self.duration)
            )
            .expect("writing to a string cannot fail");
            return output;
        }
        if let Some(summary) = workload_result(self.result, color) {
            output.push_str(&summary);
            return output;
        }

        let operations = self
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                ExecutionEventKind::Operation(operation) => Some(operation),
                _ => None,
            })
            .collect::<Vec<_>>();
        let final_operation_report = operations.iter().find_map(|operation| {
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
            operations.is_empty() || payload.is_some() || final_operation_report.is_none()
        };
        if show_result {
            let label = if verbose {
                "Full result"
            } else if operations.is_empty() || final_operation_report.is_none() {
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
            "events": events_json(self.events, self.omitted_workload_echoes),
            "workloads": workloads_json(self.workloads),
            "omittedWorkloads": self.omitted_workloads,
        })
        .to_string()
    }
}

fn is_workload_tree(value: &Value) -> bool {
    match value {
        Value::Object(fields) => {
            if fields.contains_key("count")
                && fields.contains_key("started")
                && fields.contains_key("latency")
            {
                true
            } else {
                !fields.is_empty() && fields.values().all(is_workload_tree)
            }
        }
        Value::Array(items) => !items.is_empty() && items.iter().all(is_workload_tree),
        _ => false,
    }
}

fn render_events(
    output: &mut String,
    events: &[ExecutionEvent],
    omitted: usize,
    color: bool,
    verbose: bool,
) {
    for event in events {
        let label = scope_label(&event.scope_path);
        match &event.kind {
            ExecutionEventKind::Operation(operation) => {
                render_operation(output, operation, &label, color, verbose);
            }
            ExecutionEventKind::Echo(value) => {
                let value = echo_value(value, verbose);
                let mut lines = value.lines();
                writeln!(
                    output,
                    "  {label}{} {}",
                    style("echo", "36", color),
                    lines.next().unwrap_or_default()
                )
                .expect("writing to a string cannot fail");
                for line in lines {
                    writeln!(output, "    {line}").expect("writing to a string cannot fail");
                }
                output.push('\n');
            }
            ExecutionEventKind::BranchCancelled => {
                writeln!(
                    output,
                    "  {label}{} branch cancelled\n",
                    style("↯", "33", color)
                )
                .expect("writing to a string cannot fail");
            }
        }
    }
    if omitted > 0 {
        writeln!(
            output,
            "  … {omitted} echo message(s) omitted from workload\n"
        )
        .expect("writing to a string cannot fail");
    }
}

pub fn scope_label(path: &[ExecutionScope]) -> String {
    if path.is_empty() {
        return String::new();
    }
    let parts = path
        .iter()
        .map(|scope| match scope {
            ExecutionScope::Parallel {
                invocation,
                branch,
                total,
            } => format!("p{invocation}:b{branch}/{total}"),
            ExecutionScope::NamedParallel {
                invocation, name, ..
            } => {
                format!("p{invocation}:{name}")
            }
            ExecutionScope::Retry {
                invocation,
                attempt,
                total,
            } => format!("r{invocation}:a{attempt}/{total}"),
        })
        .collect::<Vec<_>>();
    format!("[{}] ", parts.join(" > "))
}

pub fn events_json(events: &[ExecutionEvent], omitted: usize) -> serde_json::Value {
    let mut entries = events.iter().map(|event| {
        let path = scope_json(&event.scope_path);
        let mut entry = match &event.kind {
            ExecutionEventKind::Operation(operation) => serde_json::json!({
                "type":"operation", "capability":operation.capability, "operation":operation.operation,
                "durationNanos":duration_nanos(operation.duration),
                "status": if operation.result.is_ok() {"ok"} else {"error"},
            }),
            ExecutionEventKind::Echo(value) => serde_json::json!({"type":"echo", "value":value_json(value)}),
            ExecutionEventKind::BranchCancelled => serde_json::json!({"type":"branch_cancelled"}),
        };
        entry["scope"] = path;
        entry
    }).collect::<Vec<_>>();
    if omitted > 0 {
        entries.push(serde_json::json!({"type":"echo_omitted", "count":omitted}));
    }
    serde_json::json!(entries)
}

pub fn scope_json(path: &[ExecutionScope]) -> serde_json::Value {
    serde_json::json!(path.iter().map(|scope| match scope {
        ExecutionScope::Parallel { invocation, branch, total } => serde_json::json!({"kind":"parallel","invocation":invocation,"branch":branch,"total":total}),
        ExecutionScope::NamedParallel { invocation, branch, total, name } => serde_json::json!({"kind":"parallel","invocation":invocation,"branch":branch,"total":total,"name":name}),
        ExecutionScope::Retry { invocation, attempt, total } => serde_json::json!({"kind":"retry","invocation":invocation,"attempt":attempt,"total":total}),
    }).collect::<Vec<_>>())
}

fn render_operation(
    output: &mut String,
    event: &OperationEvent,
    label: &str,
    color: bool,
    verbose: bool,
) {
    match &event.result {
        Ok(value) => {
            if let Some(report) = &event.report {
                writeln!(
                    output,
                    "  {label}{} {}",
                    style("✓", "32", color),
                    report.summary
                )
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
                    "  {label}{} {}.{} · {}\n",
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
                "  {label}{} {}.{} · {}\n    {}\n",
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

fn workload_title(snapshot: &WorkloadSnapshot) -> String {
    let branch = snapshot
        .scope_path
        .iter()
        .rev()
        .find_map(|scope| match scope {
            ExecutionScope::NamedParallel { name, .. } => Some(name.clone()),
            ExecutionScope::Parallel { branch, .. } => Some(format!("branch {branch}")),
            ExecutionScope::Retry { .. } => None,
        });
    branch.unwrap_or_else(|| format!("workload #{}", snapshot.id))
}

const fn workload_phase_label(phase: WorkloadPhase) -> &'static str {
    match phase {
        WorkloadPhase::Starting => "STARTING",
        WorkloadPhase::Running => "RUNNING",
        WorkloadPhase::Draining => "DRAINING",
        WorkloadPhase::Completed => "COMPLETED",
        WorkloadPhase::Aborted => "ABORTED",
    }
}

fn action_label(action: &ActionStats, index: usize) -> String {
    let operation = if action.site.capability == "http" {
        action.site.operation.to_ascii_uppercase()
    } else {
        format!("{}.{}", action.site.capability, action.site.operation)
    };
    let location = action
        .line
        .map_or_else(|| format!("#{index}"), |line| format!("L{line}"));
    if action.site.flow.is_empty() {
        format!("{operation} {location}")
    } else {
        format!("{} · {operation} {location}", action.site.flow)
    }
}

fn action_summary(action: &ActionStats, index: usize) -> String {
    let mut summary = format!(
        "{} · {} {} · p95 {}",
        action_label(action, index),
        action.calls,
        if action.calls == 1 { "call" } else { "calls" },
        display_duration(action.p95())
    );
    if action.errors > 0 {
        write!(summary, " · {} errors", action.errors).expect("writing to a string cannot fail");
    }
    if action.http_4xx > 0 || action.http_5xx > 0 {
        write!(
            summary,
            " · 4xx {} / 5xx {}",
            action.http_4xx, action.http_5xx
        )
        .expect("writing to a string cannot fail");
    }
    summary
}

fn short_label(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    format!("{}…", value.chars().take(max_chars - 1).collect::<String>())
}

fn workload_progress_lines(view: &WorkloadView) -> Vec<String> {
    let snapshot = &view.snapshot;
    let phase = workload_phase_label(snapshot.phase);
    let (description, duration, limit) = match snapshot.kind {
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
        ),
        WorkloadKind::Concurrency { limit, duration } => {
            (format!("concurrency {limit}"), duration, limit)
        }
    };
    let rate_window = if snapshot.phase == WorkloadPhase::Completed {
        duration
    } else {
        snapshot.elapsed.min(duration)
    };
    let achieved = if rate_window.is_zero() {
        0.0
    } else {
        count_f64(snapshot.started) / rate_window.as_secs_f64()
    };
    let rate = format!("{achieved:.1}/s");
    let mut lines = vec![
        format!(
            "{} · {description} for {} · {phase}",
            short_label(&workload_title(snapshot), 24),
            display_duration(duration)
        ),
        format!(
            "  {}/{} · active {}/{} · {rate} · ok {} · fail {} · drop {}",
            if snapshot.elapsed < Duration::from_millis(1) {
                "0ms".to_owned()
            } else {
                display_duration(snapshot.elapsed.min(duration))
            },
            display_duration(duration),
            snapshot.active,
            limit,
            snapshot.success,
            snapshot.failed,
            snapshot.dropped
        ),
        format!(
            "  iterations {}/{} · p50 {} · p95 {} · p99 {}",
            snapshot.started,
            snapshot.completed,
            display_duration(snapshot.latency_p50),
            display_duration(snapshot.latency_p95),
            display_duration(snapshot.latency_p99)
        ),
    ];
    if snapshot.cancelled > 0 {
        lines.push(format!(
            "  {} in-flight iterations cancelled",
            snapshot.cancelled
        ));
    }
    for (index, action) in view.actions.values().take(MAX_VISIBLE_ACTIONS).enumerate() {
        let label = short_label(&action_label(action, index + 1), 34);
        lines.push(format!(
            "    {label} · {} {} · p95 {}{}",
            action.calls,
            if action.calls == 1 { "call" } else { "calls" },
            display_duration(action.p95()),
            if action.errors + action.http_5xx > 0 {
                format!(" · {} err/5xx", action.errors + action.http_5xx)
            } else {
                String::new()
            }
        ));
    }
    let hidden = view.actions.len().saturating_sub(MAX_VISIBLE_ACTIONS);
    if hidden > 0 || view.overflow_calls > 0 {
        lines.push(format!(
            "    … {hidden} more action sites · {} calls beyond site limit",
            view.overflow_calls
        ));
    }
    lines
}

pub fn workloads_result(workloads: &[WorkloadView], omitted: u64, color: bool) -> String {
    let mut output = String::new();
    for (index, view) in workloads.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        let has_action_issues = view
            .actions
            .values()
            .any(|action| action.errors > 0 || action.http_5xx > 0);
        let phase = workload_phase_label(view.snapshot.phase);
        let status = if view.snapshot.failed > 0 || view.snapshot.dropped > 0 || has_action_issues {
            style(&format!("{phase} · ISSUES"), "33;1", color)
        } else {
            style(phase, "36;1", color)
        };
        let (kind, achieved) = match view.snapshot.kind {
            WorkloadKind::Rate {
                target,
                period,
                duration,
                ..
            } => (
                format!(
                    "rate {target}/{} for {}",
                    display_duration(period),
                    display_duration(duration)
                ),
                format!(
                    "{:.1}/s",
                    count_f64(view.snapshot.started)
                        / if view.snapshot.phase == WorkloadPhase::Completed {
                            duration.as_secs_f64()
                        } else {
                            view.snapshot.elapsed.as_secs_f64().max(f64::EPSILON)
                        }
                ),
            ),
            WorkloadKind::Concurrency { limit, duration } => (
                format!("concurrency {limit} for {}", display_duration(duration)),
                format!(
                    "{:.1}/s",
                    count_f64(view.snapshot.completed)
                        / view.snapshot.elapsed.as_secs_f64().max(f64::EPSILON)
                ),
            ),
        };
        writeln!(
            output,
            "{} · {kind} · {status}",
            workload_title(&view.snapshot)
        )
        .expect("writing to a string cannot fail");
        writeln!(
            output,
            "  {} started · {} successful · {} failed · {} dropped · achieved {achieved} · p95 {}",
            view.snapshot.started,
            view.snapshot.success,
            view.snapshot.failed,
            view.snapshot.dropped,
            display_duration(view.snapshot.latency_p95)
        )
        .expect("writing to a string cannot fail");
        if view.snapshot.cancelled > 0 {
            writeln!(
                output,
                "  {} in-flight iterations cancelled",
                view.snapshot.cancelled
            )
            .expect("writing to a string cannot fail");
        }
        for (action_index, action) in view.actions.values().enumerate() {
            writeln!(output, "  {}", action_summary(action, action_index + 1))
                .expect("writing to a string cannot fail");
            if !action.failure_samples.is_empty() {
                writeln!(output, "    samples: {}", action.failure_samples.join(", "))
                    .expect("writing to a string cannot fail");
            }
        }
        if view.overflow_calls > 0 {
            writeln!(
                output,
                "  … {} calls beyond the 32-site detail limit",
                view.overflow_calls
            )
            .expect("writing to a string cannot fail");
        }
    }
    if omitted > 0 {
        writeln!(
            output,
            "\n… {omitted} earlier or excess workloads omitted from this view"
        )
        .expect("writing to a string cannot fail");
    }
    output.trim_end().to_owned()
}

pub fn workloads_json(workloads: &[WorkloadView]) -> serde_json::Value {
    serde_json::Value::Array(workloads.iter().map(|view| {
        serde_json::json!({
            "id": view.snapshot.id,
            "name": workload_title(&view.snapshot),
            "phase": format!("{:?}", view.snapshot.phase).to_ascii_lowercase(),
            "started": view.snapshot.started,
            "completed": view.snapshot.completed,
            "success": view.snapshot.success,
            "failed": view.snapshot.failed,
            "dropped": view.snapshot.dropped,
            "cancelled": view.snapshot.cancelled,
            "actions": view.actions.values().enumerate().map(|(index, action)| serde_json::json!({
                "name": action_label(action, index + 1),
                "source": action.site.source,
                "offset": action.site.start,
                "line": action.line,
                "calls": action.calls,
                "errors": action.errors,
                "http4xx": action.http_4xx,
                "http5xx": action.http_5xx,
                "p95Nanos": u64::try_from(action.p95().as_nanos()).unwrap_or(u64::MAX),
                "failureSamples": action.failure_samples,
            })).collect::<Vec<_>>(),
            "overflowCalls": view.overflow_calls,
        })
    }).collect())
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
    events: &[ExecutionEvent],
    omitted_workload_echoes: usize,
    color: bool,
    test: bool,
) -> String {
    let mut output = String::new();
    if !test {
        writeln!(output, "{}\n", style(flow, "1", color)).expect("writing to a string cannot fail");
    }
    render_events(&mut output, events, omitted_workload_echoes, color, false);
    write!(
        output,
        "{} {} failed",
        style("✗", "31", color),
        if test { "Test" } else { "Flow" }
    )
    .expect("writing to a string cannot fail");
    output
}

pub fn cancellation_summary(
    flow: &str,
    events: &[ExecutionEvent],
    omitted: usize,
    color: bool,
) -> String {
    let mut output = format!("{}\n\n", style(flow, "1", color));
    render_events(&mut output, events, omitted, color, false);
    write!(output, "{} Execution cancelled", style("↯", "33", color))
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

fn echo_value(value: &Value, complete: bool) -> String {
    let formatted = format_value(value);
    if complete {
        return formatted;
    }
    let mut characters = formatted.chars();
    let preview = characters
        .by_ref()
        .take(DEFAULT_PREVIEW_CHARS)
        .collect::<String>();
    if characters.next().is_none() {
        formatted
    } else {
        format!("{preview}\n… echo truncated after 8 KiB; use --verbose for the full value")
    }
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

    use super::{CliObserver, ExecutionReport, display_duration, events_json, truncate};
    use mettle_capability::{Capability, Span, Value};
    use mettle_http::HttpCapability;
    use mettle_runtime::{
        ExecutionEvent, ExecutionEventKind, ExecutionObserver, ExecutionScope, OperationEvent,
        WorkloadKind, WorkloadOperation, WorkloadPhase, WorkloadSnapshot,
    };

    fn operation_event(operation: OperationEvent) -> ExecutionEvent {
        ExecutionEvent {
            span: operation.span,
            kind: ExecutionEventKind::Operation(operation),
            scope_path: Vec::new(),
            inside_workload: false,
        }
    }

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
            events: &[],
            omitted_workload_echoes: 0,
            workloads: &[],
            omitted_workloads: 0,
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
            events: &[operation_event(operation)],
            omitted_workload_echoes: 0,
            workloads: &[],
            omitted_workloads: 0,
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
            events: &[operation_event(operation)],
            omitted_workload_echoes: 0,
            workloads: &[],
            omitted_workloads: 0,
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

    #[test]
    fn workload_echo_capture_is_bounded_and_reports_omissions() {
        let observer = CliObserver::new(false);
        for _ in 0..55 {
            observer.execution_event(ExecutionEvent {
                kind: ExecutionEventKind::Echo(Value::String("sample".to_owned())),
                scope_path: Vec::new(),
                span: Span::default(),
                inside_workload: true,
            });
        }
        let captured = observer.take_events();
        assert_eq!(captured.events.len(), 50);
        assert_eq!(captured.omitted_workload_echoes, 5);
        let json = events_json(&captured.events, captured.omitted_workload_echoes);
        assert_eq!(json[50]["type"], "echo_omitted");
        assert_eq!(json[50]["count"], 5);
    }

    fn workload_snapshot(id: u64, name: &str, phase: WorkloadPhase) -> WorkloadSnapshot {
        WorkloadSnapshot {
            id,
            scope_path: vec![ExecutionScope::NamedParallel {
                invocation: 1,
                branch: usize::try_from(id).expect("test id fits usize"),
                total: 2,
                name: name.to_owned(),
            }],
            kind: WorkloadKind::Rate {
                target: 2,
                period: std::time::Duration::from_secs(1),
                duration: std::time::Duration::from_secs(2),
                limit: 4,
                planned: 4,
            },
            phase,
            elapsed: std::time::Duration::from_secs(2),
            active: 0,
            started: 4,
            completed: 4,
            success: 4,
            failed: 0,
            dropped: 0,
            cancelled: 0,
            latency_p50: std::time::Duration::from_millis(10),
            latency_p95: std::time::Duration::from_millis(20),
            latency_p99: std::time::Duration::from_millis(30),
        }
    }

    #[test]
    fn parallel_workloads_keep_separate_bounded_redacted_action_summaries() {
        let observer = CliObserver::new(false)
            .with_sources(vec!["flow main {\n  http.get(\"/secret\")\n}".to_owned()]);
        observer.workload_updated(workload_snapshot(1, "browsing", WorkloadPhase::Starting));
        observer.workload_updated(workload_snapshot(2, "posts", WorkloadPhase::Starting));
        for workload_id in [1, 2] {
            observer.workload_operation(WorkloadOperation {
                workload_id,
                span: Span::new(14, 22),
                flow: "main".to_owned(),
                capability: "http".to_owned(),
                operation: "get".to_owned(),
                duration: std::time::Duration::from_millis(12),
                http_status: Some(if workload_id == 1 { 200 } else { 503 }),
                failed: false,
            });
            observer.workload_updated(workload_snapshot(
                workload_id,
                if workload_id == 1 {
                    "browsing"
                } else {
                    "posts"
                },
                WorkloadPhase::Completed,
            ));
        }
        let captured = observer.take_events();
        assert_eq!(captured.workloads.len(), 2);
        assert_eq!(captured.workloads[0].actions.len(), 1);
        assert_eq!(captured.workloads[1].actions.len(), 1);
        let report = ExecutionReport {
            flow: "main",
            duration: std::time::Duration::from_secs(2),
            result: &Value::Null,
            events: &captured.events,
            omitted_workload_echoes: 0,
            workloads: &captured.workloads,
            omitted_workloads: 0,
        };
        let human = report.human(false, false);
        assert!(human.contains("browsing · rate"), "{human}");
        assert!(human.contains("posts · rate"), "{human}");
        assert!(human.contains("main · GET L2"), "{human}");
        assert!(human.contains("samples: HTTP 503"), "{human}");
        assert!(!human.contains("/secret"), "{human}");
        let json: serde_json::Value = serde_json::from_str(&report.json()).expect("valid JSON");
        assert_eq!(json["workloads"][0]["name"], "browsing");
        assert_eq!(json["workloads"][1]["actions"][0]["http5xx"], 1);
        assert!(!report.json().contains("/secret"));
    }

    #[test]
    fn workload_action_sites_and_history_are_bounded() {
        let observer = CliObserver::new(false);
        observer.workload_updated(workload_snapshot(1, "first", WorkloadPhase::Starting));
        for offset in 0..33 {
            observer.workload_operation(WorkloadOperation {
                workload_id: 1,
                span: Span::new(offset, offset + 1),
                flow: "main".to_owned(),
                capability: "http".to_owned(),
                operation: "get".to_owned(),
                duration: std::time::Duration::from_millis(1),
                http_status: Some(200),
                failed: false,
            });
        }
        {
            let state = observer.state.lock().expect("observer lock");
            assert_eq!(state.workloads[&1].actions.len(), 32);
            assert_eq!(state.workloads[&1].overflow_calls, 1);
        }
        observer.workload_updated(workload_snapshot(1, "first", WorkloadPhase::Completed));
        for id in 2..=33 {
            observer.workload_updated(workload_snapshot(id, "later", WorkloadPhase::Starting));
            observer.workload_updated(workload_snapshot(id, "later", WorkloadPhase::Completed));
        }
        let captured = observer.take_events();
        assert_eq!(captured.workloads.len(), 32);
        assert_eq!(captured.omitted_workloads, 1);
        assert!(!captured.workloads.iter().any(|view| view.snapshot.id == 1));
    }
}
