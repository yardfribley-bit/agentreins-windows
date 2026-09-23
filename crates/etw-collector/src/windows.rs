mod etw_health;
mod event_time;
mod identity;
mod resource_usage;
mod rolling_output;

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ferrisetw::EventRecord;
use ferrisetw::native::ExtendedDataItem;
use ferrisetw::parser::{Parser, ParserError, Pointer};
use ferrisetw::provider::Provider;
use ferrisetw::provider::kernel_providers::{
    FILE_INIT_IO_PROVIDER, FILE_IO_PROVIDER, KernelProvider, PROCESS_PROVIDER, TCP_IP_PROVIDER,
    THREAD_PROVIDER,
};
use ferrisetw::schema_locator::SchemaLocator;
use ferrisetw::trace::{KernelTrace, TraceProperties, TraceTrait};
use native_contracts::{
    Action, ActionKind, Actor, AgentIdentity, CausalEdge, CausalEdgeKind,
    EVENT_CLOCK_SKEW_LIMIT_MS, EVENT_SCHEMA_VERSION, EventAggregation, EventOccurrence, EventPhase,
    Evidence, EvidenceSource, NativeEvidence, NativeIdKind, NativeIdSource, NativeIdentifier,
    ObservationEvent, ObservationMode, ObservationOutcome, ObservationResult, OperationIdOrigin,
    ProcessEvidenceEvent, ProcessRef, Resource, ResourceKind, RetentionClass, SessionRef, Verdict,
};
use serde::Serialize;
use windows::Win32::System::Diagnostics::Etw::EVENT_RECORD;
use workbuddy_adapter::{ProcessObservation, classify_process};

use self::etw_health::{EtwSessionHealth, query_trace_health, stop_trace_and_query_health};
use self::event_time::filetime_to_unix_ms;
use self::identity::{paths_equal, query_process_image_path};
use self::resource_usage::AgentResourceSampler;
use self::rolling_output::{RollingOutputConfiguration, RollingOutputWriter};
use crate::retention::{
    RetentionDisposition, decide_retention, evidence_retention, summary_window,
};

const PROCESS_PROVIDER_ID: &str = "3d6fa8d0-fe05-11d0-9dda-00c04fd7ba7c";
const FILE_PROVIDER_ID: &str = "90cbdc39-4a3e-11d1-84f4-0000f80464e3";
const TCP_PROVIDER_ID: &str = "9a280ac0-c8e0-11d1-84e2-00c04fb998a2";
const PROCESS_START_OPCODE: u8 = 1;
const PROCESS_STOP_OPCODE: u8 = 2;
const PROCESS_RUNDOWN_START_OPCODE: u8 = 3;
const PROCESS_RUNDOWN_STOP_OPCODE: u8 = 4;
const OUTPUT_QUEUE_CAPACITY: usize = 64;
const OUTPUT_BUFFER_CAPACITY: usize = 4 * 1_024 * 1_024;

struct Arguments {
    lifetime: CollectionLifetime,
    output: OutputConfiguration,
    identity_path: PathBuf,
}

enum OutputConfiguration {
    Fixed {
        full_output_path: PathBuf,
        filtered_output_path: PathBuf,
    },
    Rolling {
        directory: PathBuf,
        max_segment_bytes: u64,
        max_segment_duration: Duration,
    },
}

enum CollectionLifetime {
    Timed {
        duration: Duration,
        stop_signal_path: Option<PathBuf>,
    },
    Persistent(PersistentConfiguration),
}

struct PersistentConfiguration {
    stop_signal_path: PathBuf,
    health_output_path: PathBuf,
    health_interval: Duration,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum CollectionStopReason {
    DurationElapsed,
    StopSignal,
}

struct CollectorState {
    session_id: String,
    identity: AgentIdentity,
    next_sequence: u64,
    provider_events_received: u64,
    parsed_events: u64,
    parse_failures: u64,
    output_source_events: u64,
    output_batches_generated: u64,
    output_batches_dropped: u64,
    output_queue_wait_count: u64,
    output_queue_wait_micros: u64,
    peak_aggregation_keys: usize,
    retained_detail_source_events: u64,
    retained_summary_source_events: u64,
    retained_review_source_events: u64,
    discarded_source_events: u64,
    process_events_parsed: u64,
    thread_events_parsed: u64,
    file_events_parsed: u64,
    network_events_parsed: u64,
    last_event_timestamp_unix_ms: Option<u64>,
    last_observed_at_unix_ms: Option<u64>,
    identity_collision_count: u64,
    unverified_root_count: u64,
    tracked_process_ids: HashSet<u32>,
    all_processes_by_pid: HashMap<u32, ProcessRef>,
    processes_by_pid: HashMap<u32, ProcessRef>,
    actors_by_pid: HashMap<u32, Actor>,
    process_ids_by_thread_id: HashMap<u32, u32>,
    paths_by_file_object: HashMap<u64, String>,
    pending_file_operations: HashMap<u64, OutputBatch>,
    aggregated_outputs: HashMap<AggregationKey, OutputBatch>,
    pending_outputs: Vec<OutputBatch>,
    aggregation_second: Option<u64>,
    output_sender: Option<SyncSender<Vec<OutputBatch>>>,
    output_queue_metrics: Arc<OutputQueueMetrics>,
}

struct OutputBatch {
    full_event: ProcessEvidenceEvent,
    filtered_event: Option<ObservationEvent>,
}

#[derive(Eq, Hash, PartialEq)]
struct AggregationKey {
    second: u64,
    process_id: u32,
    process_instance_id: String,
    action: ActionKind,
    operation_id: Option<String>,
    resource_identifier: String,
    destination_identifier: Option<String>,
    status_code: Option<u32>,
    success: Option<bool>,
    provider_event_id: u16,
    provider_opcode: u8,
    provider_event_version: u8,
}

#[derive(Default)]
struct OutputQueueMetrics {
    current_depth: AtomicUsize,
    peak_depth: AtomicUsize,
}

#[derive(Default)]
struct OutputRuntimeMetrics {
    full_events_written: AtomicU64,
    filtered_events_written: AtomicU64,
    full_bytes_written: AtomicU64,
    filtered_bytes_written: AtomicU64,
    write_failures: AtomicU64,
    current_segment_index: AtomicU64,
    segments_published: AtomicU64,
}

#[derive(Default)]
struct OutputSummary {
    full_events_written: u64,
    filtered_events_written: u64,
    write_failures: u64,
    first_error: Option<String>,
    segments_published: u64,
}

#[derive(Serialize)]
struct CollectionSummary {
    session_id: String,
    full_output_path: Option<PathBuf>,
    filtered_output_path: Option<PathBuf>,
    output_directory: Option<PathBuf>,
    provider_events_received: u64,
    events_handled: u64,
    parsed_events: u64,
    full_events_written: u64,
    filtered_events_written: u64,
    parse_failures: u64,
    write_failures: u64,
    segments_published: u64,
    output_queue_capacity: usize,
    peak_output_queue_depth: usize,
    output_source_events: u64,
    output_batches_generated: u64,
    source_events_collapsed: u64,
    output_batches_dropped: u64,
    retained_detail_source_events: u64,
    retained_summary_source_events: u64,
    retained_review_source_events: u64,
    discarded_source_events: u64,
    output_queue_wait_count: u64,
    output_queue_wait_ms: f64,
    peak_aggregation_keys: usize,
    process_events_parsed: u64,
    thread_events_parsed: u64,
    file_events_parsed: u64,
    network_events_parsed: u64,
    last_event_timestamp_unix_ms: Option<u64>,
    last_observed_at_unix_ms: Option<u64>,
    event_clock_skew_ms: Option<u64>,
    identity_collision_count: u64,
    unverified_root_count: u64,
    lost_events_available: bool,
    events_lost: Option<u32>,
    log_buffers_lost: Option<u32>,
    realtime_buffers_lost: Option<u32>,
    number_of_buffers: Option<u32>,
    free_buffers: Option<u32>,
    buffers_written: Option<u32>,
    health_query_error: Option<String>,
    stop_reason: Option<CollectionStopReason>,
    coverage_status: String,
}

struct FixedOutputWriter {
    full_path: PathBuf,
    filtered_path: PathBuf,
    full_writer: BufWriter<File>,
    filtered_writer: BufWriter<File>,
}

enum OutputDestination {
    Fixed(FixedOutputWriter),
    Rolling(RollingOutputWriter),
}

struct OsEventDetails {
    process_id: u32,
    action_kind: ActionKind,
    resource_kind: ResourceKind,
    resource_identifier: String,
    related_resource: Option<Resource>,
    operation_id: Option<String>,
    operation_id_origin: Option<OperationIdOrigin>,
    thread_id: Option<u32>,
    irp: Option<u64>,
    file_object: Option<u64>,
    tcp_connection_id: Option<u64>,
    result: ObservationResult,
    provider_id: &'static str,
}

pub fn run(raw_arguments: Vec<String>) -> Result<(), String> {
    let arguments = parse_arguments(&raw_arguments)?;
    let identity = load_identity(&arguments.identity_path)?;
    let session_id = format!("gate0-{}", unix_time_ms()?);
    let mut health_writer = match &arguments.lifetime {
        CollectionLifetime::Timed { .. } => None,
        CollectionLifetime::Persistent(configuration) => {
            Some(create_health_output(&configuration.health_output_path)?)
        }
    };
    let output_destination = create_output_destination(&arguments.output, &session_id)?;
    let (output_sender, output_receiver) = sync_channel(OUTPUT_QUEUE_CAPACITY);
    let output_queue_metrics = Arc::new(OutputQueueMetrics::default());
    let output_runtime_metrics = Arc::new(OutputRuntimeMetrics::default());
    let output_writer = start_output_writer(
        output_destination,
        output_receiver,
        Arc::clone(&output_queue_metrics),
        Arc::clone(&output_runtime_metrics),
    );
    let state = Arc::new(Mutex::new(CollectorState {
        session_id: session_id.clone(),
        identity,
        next_sequence: 1,
        provider_events_received: 0,
        parsed_events: 0,
        parse_failures: 0,
        output_source_events: 0,
        output_batches_generated: 0,
        output_batches_dropped: 0,
        output_queue_wait_count: 0,
        output_queue_wait_micros: 0,
        peak_aggregation_keys: 0,
        retained_detail_source_events: 0,
        retained_summary_source_events: 0,
        retained_review_source_events: 0,
        discarded_source_events: 0,
        process_events_parsed: 0,
        thread_events_parsed: 0,
        file_events_parsed: 0,
        network_events_parsed: 0,
        last_event_timestamp_unix_ms: None,
        last_observed_at_unix_ms: None,
        identity_collision_count: 0,
        unverified_root_count: 0,
        tracked_process_ids: HashSet::new(),
        all_processes_by_pid: HashMap::new(),
        processes_by_pid: HashMap::new(),
        actors_by_pid: HashMap::new(),
        process_ids_by_thread_id: HashMap::new(),
        paths_by_file_object: HashMap::new(),
        pending_file_operations: HashMap::new(),
        aggregated_outputs: HashMap::new(),
        pending_outputs: Vec::new(),
        aggregation_second: None,
        output_sender: Some(output_sender),
        output_queue_metrics,
    }));
    let callback_events_handled = Arc::new(AtomicU64::new(0));
    let callback_state = Arc::clone(&state);
    let callback_counter = Arc::clone(&callback_events_handled);
    let process_provider = Provider::kernel(&PROCESS_PROVIDER)
        .add_callback(move |record, schema_locator| {
            callback_counter.fetch_add(1, Ordering::Relaxed);
            handle_process_event(record, schema_locator, &callback_state);
        })
        .build();
    let callback_state = Arc::clone(&state);
    let callback_counter = Arc::clone(&callback_events_handled);
    let thread_provider = Provider::kernel(&THREAD_PROVIDER)
        .add_callback(move |record, schema_locator| {
            callback_counter.fetch_add(1, Ordering::Relaxed);
            handle_thread_event(record, schema_locator, &callback_state);
        })
        .build();
    let file_kernel_provider = KernelProvider::new(
        FILE_IO_PROVIDER.guid,
        FILE_IO_PROVIDER.flags | FILE_INIT_IO_PROVIDER.flags,
    );
    let callback_state = Arc::clone(&state);
    let callback_counter = Arc::clone(&callback_events_handled);
    let file_provider = Provider::kernel(&file_kernel_provider)
        .add_callback(move |record, schema_locator| {
            callback_counter.fetch_add(1, Ordering::Relaxed);
            handle_file_event(record, schema_locator, &callback_state);
        })
        .build();
    let callback_state = Arc::clone(&state);
    let callback_counter = Arc::clone(&callback_events_handled);
    let tcp_provider = Provider::kernel(&TCP_IP_PROVIDER)
        .add_callback(move |record, schema_locator| {
            callback_counter.fetch_add(1, Ordering::Relaxed);
            handle_network_event(record, schema_locator, &callback_state);
        })
        .build();
    let trace_name = format!("AgentReins-Gate0-{}", std::process::id());
    let trace_properties = TraceProperties {
        buffer_size: 64,
        min_buffer: 32,
        max_buffer: 128,
        ..TraceProperties::default()
    };
    let trace_result = KernelTrace::new()
        .named(trace_name.clone())
        .set_trace_properties(trace_properties)
        .enable(process_provider)
        .enable(thread_provider)
        .enable(file_provider)
        .enable(tcp_provider)
        .start();
    let (trace, trace_handle) = match trace_result {
        Ok(value) => value,
        Err(error) => {
            let mut current_state = state
                .lock()
                .map_err(|_| String::from("ETW 采集状态锁已损坏"))?;
            drop(current_state.output_sender.take());
            drop(current_state);
            output_writer
                .join()
                .map_err(|_| String::from("ETW 输出线程异常退出"))?;
            return Err(format!("无法启动 ETW 实时会话 error={error:?}"));
        }
    };
    let trace_processor = thread::spawn(move || {
        KernelTrace::process_from_handle(trace_handle)
            .map_err(|error| format!("ETW ProcessTrace 失败 error={error:?}"))
    });

    let stop_result = match &arguments.lifetime {
        CollectionLifetime::Timed {
            duration,
            stop_signal_path,
        } => wait_for_collection_end(*duration, stop_signal_path.as_ref()),
        CollectionLifetime::Persistent(configuration) => wait_for_persistent_stop(
            configuration,
            &trace_name,
            &session_id,
            &state,
            &callback_events_handled,
            &output_runtime_metrics,
            health_writer.as_mut().expect("常驻观测必须创建健康输出"),
        ),
    };
    let stop_reason = stop_result.as_ref().ok().copied();
    let wait_error = stop_result.err();
    let trace_health = stop_trace_and_query_health(&trace_name);
    let mut trace = Some(trace);
    let trace_stop_error = trace_health.as_ref().err().cloned();
    let trace_cleanup_error = if trace_stop_error.is_some() {
        trace
            .take()
            .expect("ETW 会话必须存在")
            .stop()
            .err()
            .map(|error| format!("ETW 会话停止失败后的清理失败 error={error:?}"))
    } else {
        None
    };
    let trace_process_error = match trace_processor.join() {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error),
        Err(_) => Some(String::from("ETW ProcessTrace 线程异常退出")),
    };
    drop(trace.take());
    let events_handled = callback_events_handled.load(Ordering::Relaxed);

    let mut final_state = state
        .lock()
        .map_err(|_| String::from("ETW 采集状态锁已损坏"))?;
    let incomplete_file_operations = std::mem::take(&mut final_state.pending_file_operations);
    for batch in incomplete_file_operations.into_values() {
        emit_output(&mut final_state, batch);
    }
    flush_pending_outputs(&mut final_state);
    let output_source_events = final_state.output_source_events;
    let output_batches_generated = final_state.output_batches_generated;
    let output_batches_dropped = final_state.output_batches_dropped;
    let output_queue_wait_count = final_state.output_queue_wait_count;
    let output_queue_wait_micros = final_state.output_queue_wait_micros;
    let peak_aggregation_keys = final_state.peak_aggregation_keys;
    let retained_source_events = final_state
        .retained_detail_source_events
        .saturating_add(final_state.retained_summary_source_events)
        .saturating_add(final_state.retained_review_source_events);
    let peak_output_queue_depth = final_state
        .output_queue_metrics
        .peak_depth
        .load(Ordering::Relaxed);
    drop(final_state.output_sender.take());
    drop(final_state);
    let output_summary = output_writer
        .join()
        .map_err(|_| String::from("ETW 输出线程异常退出"))?;
    if let Some(error) = output_summary.first_error.as_deref() {
        write_warning(0, "ETW 输出线程写入失败", error);
    }
    let final_state = state
        .lock()
        .map_err(|_| String::from("ETW 采集状态锁已损坏"))?;
    let health_query_error = trace_health.as_ref().err().cloned();
    let health = trace_health.ok();
    let trace_failed = wait_error.is_some()
        || trace_stop_error.is_some()
        || trace_cleanup_error.is_some()
        || trace_process_error.is_some();
    let event_clock_skew_ms = event_clock_skew_ms(
        final_state.last_event_timestamp_unix_ms,
        final_state.last_observed_at_unix_ms,
    );
    let coverage_status = if trace_failed {
        "degraded"
    } else {
        coverage_status(
            health.as_ref(),
            final_state.parse_failures,
            output_summary.write_failures,
            output_batches_dropped,
            event_clock_skew_ms,
        )
    };
    let (full_output_path, filtered_output_path, output_directory) =
        output_locations(&arguments.output);
    let collection_summary = CollectionSummary {
        session_id,
        full_output_path,
        filtered_output_path,
        output_directory,
        provider_events_received: final_state.provider_events_received,
        events_handled,
        parsed_events: final_state.parsed_events,
        full_events_written: output_summary.full_events_written,
        filtered_events_written: output_summary.filtered_events_written,
        parse_failures: final_state.parse_failures,
        write_failures: output_summary.write_failures,
        segments_published: output_summary.segments_published,
        output_queue_capacity: OUTPUT_QUEUE_CAPACITY,
        peak_output_queue_depth,
        output_source_events,
        output_batches_generated,
        source_events_collapsed: retained_source_events.saturating_sub(output_batches_generated),
        output_batches_dropped,
        retained_detail_source_events: final_state.retained_detail_source_events,
        retained_summary_source_events: final_state.retained_summary_source_events,
        retained_review_source_events: final_state.retained_review_source_events,
        discarded_source_events: final_state.discarded_source_events,
        output_queue_wait_count,
        output_queue_wait_ms: output_queue_wait_micros as f64 / 1_000.0,
        peak_aggregation_keys,
        process_events_parsed: final_state.process_events_parsed,
        thread_events_parsed: final_state.thread_events_parsed,
        file_events_parsed: final_state.file_events_parsed,
        network_events_parsed: final_state.network_events_parsed,
        last_event_timestamp_unix_ms: final_state.last_event_timestamp_unix_ms,
        last_observed_at_unix_ms: final_state.last_observed_at_unix_ms,
        event_clock_skew_ms,
        identity_collision_count: final_state.identity_collision_count,
        unverified_root_count: final_state.unverified_root_count,
        lost_events_available: health.is_some(),
        events_lost: health.as_ref().map(|value| value.events_lost),
        log_buffers_lost: health.as_ref().map(|value| value.log_buffers_lost),
        realtime_buffers_lost: health.as_ref().map(|value| value.realtime_buffers_lost),
        number_of_buffers: health.as_ref().map(|value| value.number_of_buffers),
        free_buffers: health.as_ref().map(|value| value.free_buffers),
        buffers_written: health.as_ref().map(|value| value.buffers_written),
        health_query_error,
        stop_reason,
        coverage_status: String::from(coverage_status),
    };
    println!(
        "{}",
        serde_json::to_string(&collection_summary)
            .map_err(|error| format!("无法序列化 ETW 采集摘要 error={error}"))?
    );
    match collection_failure(
        wait_error,
        trace_stop_error,
        trace_cleanup_error,
        trace_process_error,
        output_summary.first_error,
        output_batches_dropped,
    ) {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn collection_failure(
    wait_error: Option<String>,
    trace_stop_error: Option<String>,
    trace_cleanup_error: Option<String>,
    trace_process_error: Option<String>,
    output_error: Option<String>,
    output_batches_dropped: u64,
) -> Option<String> {
    let mut fatal_errors = Vec::new();
    if let Some(error) = wait_error {
        fatal_errors.push(error);
    }
    if let Some(error) = trace_stop_error {
        fatal_errors.push(error);
    }
    if let Some(error) = trace_cleanup_error {
        fatal_errors.push(error);
    }
    if let Some(error) = trace_process_error {
        fatal_errors.push(error);
    }
    if let Some(error) = output_error {
        fatal_errors.push(error);
    }
    if output_batches_dropped > 0 {
        fatal_errors.push(format!(
            "ETW 输出队列已断开并丢弃批次 count={output_batches_dropped}"
        ));
    }
    (!fatal_errors.is_empty()).then(|| fatal_errors.join("; "))
}

fn wait_for_collection_end(
    duration: Duration,
    stop_signal_path: Option<&PathBuf>,
) -> Result<CollectionStopReason, String> {
    let Some(path) = stop_signal_path else {
        thread::sleep(duration);
        return Ok(CollectionStopReason::DurationElapsed);
    };
    let started_at = Instant::now();
    loop {
        match path.try_exists() {
            Ok(true) => return Ok(CollectionStopReason::StopSignal),
            Ok(false) => {}
            Err(error) => {
                return Err(format!(
                    "无法检查 ETW 停止信号 path={} error={error}",
                    path.display()
                ));
            }
        }
        let elapsed = started_at.elapsed();
        if elapsed >= duration {
            return Ok(CollectionStopReason::DurationElapsed);
        }
        thread::sleep((duration - elapsed).min(Duration::from_millis(200)));
    }
}

fn wait_for_persistent_stop(
    configuration: &PersistentConfiguration,
    trace_name: &str,
    session_id: &str,
    state: &Arc<Mutex<CollectorState>>,
    callback_events_handled: &Arc<AtomicU64>,
    output_runtime_metrics: &Arc<OutputRuntimeMetrics>,
    health_writer: &mut BufWriter<File>,
) -> Result<CollectionStopReason, String> {
    let mut resource_sampler = AgentResourceSampler::new()?;
    let mut next_health_at = Instant::now();
    loop {
        match configuration.stop_signal_path.try_exists() {
            Ok(true) => return Ok(CollectionStopReason::StopSignal),
            Ok(false) => {}
            Err(error) => {
                return Err(format!(
                    "无法检查常驻观测停止信号 path={} error={error}",
                    configuration.stop_signal_path.display()
                ));
            }
        }
        let now = Instant::now();
        if now >= next_health_at {
            let health = query_trace_health(trace_name)?;
            write_health_snapshot(
                health_writer,
                session_id,
                state,
                callback_events_handled,
                output_runtime_metrics,
                &health,
                &mut resource_sampler,
            )?;
            next_health_at = now + configuration.health_interval;
        }
        let sleep_duration = next_health_at
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(200));
        thread::sleep(sleep_duration);
    }
}

fn create_health_output(path: &PathBuf) -> Result<BufWriter<File>, String> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| {
            format!(
                "无法创建常驻观测健康输出 path={} error={error}",
                path.display()
            )
        })?;
    Ok(BufWriter::new(file))
}

fn write_health_snapshot(
    writer: &mut BufWriter<File>,
    session_id: &str,
    state: &Arc<Mutex<CollectorState>>,
    callback_events_handled: &Arc<AtomicU64>,
    output_runtime_metrics: &Arc<OutputRuntimeMetrics>,
    health: &EtwSessionHealth,
    resource_sampler: &mut AgentResourceSampler,
) -> Result<(), String> {
    let mut current_state = state
        .lock()
        .map_err(|_| String::from("ETW 采集状态锁已损坏"))?;
    flush_pending_detail_outputs(&mut current_state);
    let root_instance_ids = current_state
        .actors_by_pid
        .values()
        .map(|actor| actor.root_process_instance_id.clone())
        .collect::<HashSet<_>>();
    let resource_processes = current_state
        .actors_by_pid
        .keys()
        .map(|process_id| {
            current_state
                .processes_by_pid
                .get(process_id)
                .cloned()
                .ok_or_else(|| format!("已验证 Agent 进程缺少进程引用 process_id={process_id}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let write_failures = output_runtime_metrics
        .write_failures
        .load(Ordering::Relaxed);
    let event_clock_skew_ms = event_clock_skew_ms(
        current_state.last_event_timestamp_unix_ms,
        current_state.last_observed_at_unix_ms,
    );
    let status = coverage_status(
        Some(health),
        current_state.parse_failures,
        write_failures,
        current_state.output_batches_dropped,
        event_clock_skew_ms,
    );
    let mut snapshot = serde_json::json!({
        "schema_version": "0.1.0",
        "captured_at_unix_ms": unix_time_ms()?,
        "session_id": session_id,
        "status": "running",
        "coverage_status": status,
        "active_workbuddy_root_instances": root_instance_ids.len(),
        "active_workbuddy_processes": current_state.actors_by_pid.len(),
        "last_event_timestamp_unix_ms": current_state.last_event_timestamp_unix_ms,
        "last_observed_at_unix_ms": current_state.last_observed_at_unix_ms,
        "event_clock_skew_ms": event_clock_skew_ms,
        "event_clock_skew_limit_ms": EVENT_CLOCK_SKEW_LIMIT_MS,
        "identity_collision_count": current_state.identity_collision_count,
        "unverified_root_count": current_state.unverified_root_count,
        "provider_events_received": current_state.provider_events_received,
        "events_handled": callback_events_handled.load(Ordering::Relaxed),
        "parsed_events": current_state.parsed_events,
        "parse_failures": current_state.parse_failures,
        "output_source_events": current_state.output_source_events,
        "output_batches_generated": current_state.output_batches_generated,
        "output_batches_dropped": current_state.output_batches_dropped,
        "output_queue_depth": current_state.output_queue_metrics.current_depth.load(Ordering::Relaxed),
        "peak_output_queue_depth": current_state.output_queue_metrics.peak_depth.load(Ordering::Relaxed),
        "peak_aggregation_keys": current_state.peak_aggregation_keys,
        "full_events_written": output_runtime_metrics.full_events_written.load(Ordering::Relaxed),
        "filtered_events_written": output_runtime_metrics.filtered_events_written.load(Ordering::Relaxed),
        "full_bytes_written": output_runtime_metrics.full_bytes_written.load(Ordering::Relaxed),
        "filtered_bytes_written": output_runtime_metrics.filtered_bytes_written.load(Ordering::Relaxed),
        "write_failures": write_failures,
        "current_segment_index": output_runtime_metrics.current_segment_index.load(Ordering::Relaxed),
        "segments_published": output_runtime_metrics.segments_published.load(Ordering::Relaxed),
        "etw": health,
    });
    drop(current_state);
    snapshot["agent_resource_sample"] =
        serde_json::to_value(resource_sampler.sample(&resource_processes)?)
            .map_err(|error| format!("无法序列化 Agent 资源样本 error={error}"))?;
    write_json_line(writer, &snapshot)?;
    writer
        .flush()
        .map_err(|error| format!("无法刷新常驻观测健康输出 error={error}"))
}

fn create_output(path: &PathBuf, label: &str) -> Result<File, String> {
    File::create(path).map_err(|error| {
        format!(
            "无法创建{label} NDJSON 输出文件 path={} error={error}",
            path.display()
        )
    })
}

fn create_output_destination(
    configuration: &OutputConfiguration,
    session_id: &str,
) -> Result<OutputDestination, String> {
    match configuration {
        OutputConfiguration::Fixed {
            full_output_path,
            filtered_output_path,
        } => Ok(OutputDestination::Fixed(FixedOutputWriter {
            full_path: full_output_path.clone(),
            filtered_path: filtered_output_path.clone(),
            full_writer: BufWriter::with_capacity(
                OUTPUT_BUFFER_CAPACITY,
                create_output(full_output_path, "完整")?,
            ),
            filtered_writer: BufWriter::with_capacity(
                OUTPUT_BUFFER_CAPACITY,
                create_output(filtered_output_path, "过滤后")?,
            ),
        })),
        OutputConfiguration::Rolling {
            directory,
            max_segment_bytes,
            max_segment_duration,
        } => RollingOutputWriter::create(RollingOutputConfiguration {
            directory: directory.clone(),
            session_id: String::from(session_id),
            max_segment_bytes: *max_segment_bytes,
            max_segment_duration: *max_segment_duration,
        })
        .map(OutputDestination::Rolling),
    }
}

fn output_locations(
    configuration: &OutputConfiguration,
) -> (Option<PathBuf>, Option<PathBuf>, Option<PathBuf>) {
    match configuration {
        OutputConfiguration::Fixed {
            full_output_path,
            filtered_output_path,
        } => (
            Some(full_output_path.clone()),
            Some(filtered_output_path.clone()),
            None,
        ),
        OutputConfiguration::Rolling { directory, .. } => (None, None, Some(directory.clone())),
    }
}

impl OutputDestination {
    fn begin_event(&mut self) -> Result<(), String> {
        match self {
            Self::Fixed(_) => Ok(()),
            Self::Rolling(writer) => writer.begin_event(),
        }
    }

    fn write_full<T: Serialize>(&mut self, event: &T) -> Result<u64, String> {
        match self {
            Self::Fixed(writer) => {
                write_json_line(&mut writer.full_writer, event).map_err(|error| {
                    format!(
                        "无法写入完整 NDJSON 输出 path={} error={error}",
                        writer.full_path.display()
                    )
                })
            }
            Self::Rolling(writer) => writer.write_full(event),
        }
    }

    fn write_filtered<T: Serialize>(&mut self, event: &T) -> Result<u64, String> {
        match self {
            Self::Fixed(writer) => {
                write_json_line(&mut writer.filtered_writer, event).map_err(|error| {
                    format!(
                        "无法写入过滤后 NDJSON 输出 path={} error={error}",
                        writer.filtered_path.display()
                    )
                })
            }
            Self::Rolling(writer) => writer.write_filtered(event),
        }
    }

    fn finish_batch(&mut self) -> Result<(), String> {
        match self {
            Self::Fixed(writer) => {
                writer.full_writer.flush().map_err(|error| {
                    format!(
                        "无法刷新完整 NDJSON 输出 path={} error={error}",
                        writer.full_path.display()
                    )
                })?;
                writer.filtered_writer.flush().map_err(|error| {
                    format!(
                        "无法刷新过滤后 NDJSON 输出 path={} error={error}",
                        writer.filtered_path.display()
                    )
                })
            }
            Self::Rolling(writer) => writer.finish_batch(),
        }
    }

    fn finish_event(&mut self) {
        if let Self::Rolling(writer) = self {
            writer.finish_event();
        }
    }

    fn current_segment_index(&self) -> u64 {
        match self {
            Self::Fixed(_) => 1,
            Self::Rolling(writer) => writer.current_segment_index(),
        }
    }

    fn published_segment_count(&self) -> Result<u64, String> {
        match self {
            Self::Fixed(_) => Ok(0),
            Self::Rolling(writer) => writer.published_segment_count(),
        }
    }

    fn complete(self) -> Result<u64, String> {
        match self {
            Self::Fixed(mut writer) => {
                writer.full_writer.flush().map_err(|error| {
                    format!(
                        "无法刷新完整 NDJSON 输出 path={} error={error}",
                        writer.full_path.display()
                    )
                })?;
                writer.filtered_writer.flush().map_err(|error| {
                    format!(
                        "无法刷新过滤后 NDJSON 输出 path={} error={error}",
                        writer.filtered_path.display()
                    )
                })?;
                Ok(1)
            }
            Self::Rolling(writer) => writer.complete(),
        }
    }
}

fn start_output_writer(
    mut destination: OutputDestination,
    receiver: Receiver<Vec<OutputBatch>>,
    queue_metrics: Arc<OutputQueueMetrics>,
    runtime_metrics: Arc<OutputRuntimeMetrics>,
) -> JoinHandle<OutputSummary> {
    thread::spawn(move || {
        let mut summary = OutputSummary::default();
        let mut destination_healthy = true;
        runtime_metrics
            .current_segment_index
            .store(destination.current_segment_index(), Ordering::Relaxed);
        'writer: for batches in receiver {
            queue_metrics.current_depth.fetch_sub(1, Ordering::Relaxed);
            for batch in batches {
                if let Err(error) = destination.begin_event() {
                    record_output_error(&mut summary, error);
                    destination_healthy = false;
                    break 'writer;
                }
                match destination.write_full(&batch.full_event) {
                    Ok(bytes_written) => {
                        summary.full_events_written += 1;
                        runtime_metrics
                            .full_events_written
                            .fetch_add(1, Ordering::Relaxed);
                        runtime_metrics
                            .full_bytes_written
                            .fetch_add(bytes_written, Ordering::Relaxed);
                    }
                    Err(error) => {
                        record_output_error(&mut summary, error);
                        destination_healthy = false;
                        break 'writer;
                    }
                }
                if let Some(filtered_event) = batch.filtered_event {
                    match destination.write_filtered(&filtered_event) {
                        Ok(bytes_written) => {
                            summary.filtered_events_written += 1;
                            runtime_metrics
                                .filtered_events_written
                                .fetch_add(1, Ordering::Relaxed);
                            runtime_metrics
                                .filtered_bytes_written
                                .fetch_add(bytes_written, Ordering::Relaxed);
                        }
                        Err(error) => {
                            record_output_error(&mut summary, error);
                            destination_healthy = false;
                            break 'writer;
                        }
                    }
                }
                destination.finish_event();
            }
            if let Err(error) = destination.finish_batch() {
                record_output_error(&mut summary, error);
                destination_healthy = false;
                break;
            }
            runtime_metrics
                .current_segment_index
                .store(destination.current_segment_index(), Ordering::Relaxed);
            match destination.published_segment_count() {
                Ok(count) => runtime_metrics
                    .segments_published
                    .store(count, Ordering::Relaxed),
                Err(error) => {
                    record_output_error(&mut summary, error);
                    destination_healthy = false;
                    break;
                }
            }
            runtime_metrics
                .write_failures
                .store(summary.write_failures, Ordering::Relaxed);
        }
        if destination_healthy {
            match destination.complete() {
                Ok(count) => {
                    summary.segments_published = count;
                    runtime_metrics
                        .segments_published
                        .store(count, Ordering::Relaxed);
                }
                Err(error) => record_output_error(&mut summary, error),
            }
        }
        runtime_metrics
            .write_failures
            .store(summary.write_failures, Ordering::Relaxed);
        summary
    })
}

fn record_output_error(summary: &mut OutputSummary, error: String) {
    summary.write_failures += 1;
    if summary.first_error.is_none() {
        summary.first_error = Some(error);
    }
}

fn emit_output(state: &mut CollectorState, mut batch: OutputBatch) {
    state.output_source_events += 1;
    state.last_event_timestamp_unix_ms = Some(
        state
            .last_event_timestamp_unix_ms
            .map_or(batch.full_event.event_timestamp_unix_ms, |value| {
                value.max(batch.full_event.event_timestamp_unix_ms)
            }),
    );
    state.last_observed_at_unix_ms = Some(
        state
            .last_observed_at_unix_ms
            .map_or(batch.full_event.observed_at_unix_ms, |value| {
                value.max(batch.full_event.observed_at_unix_ms)
            }),
    );
    let decision = decide_retention(
        batch.full_event.action.kind,
        &batch.full_event.resource.identifier,
    );
    let RetentionDisposition::Keep(retention_class) = decision.disposition else {
        state.discarded_source_events += 1;
        return;
    };
    match retention_class {
        RetentionClass::Detail => state.retained_detail_source_events += 1,
        RetentionClass::Summary => state.retained_summary_source_events += 1,
        RetentionClass::Review => state.retained_review_source_events += 1,
    }
    let retention = evidence_retention(decision).expect("保留决策必须生成保留元数据");
    batch.full_event.retention = Some(retention.clone());
    if let Some(filtered_event) = batch.filtered_event.as_mut() {
        filtered_event.retention = Some(retention);
    }
    let aggregate = retention_class == RetentionClass::Summary;
    if aggregate {
        normalize_summary_batch(&mut batch, decision.resource_category);
    }
    let second = summary_window(batch.full_event.event_timestamp_unix_ms);
    if state
        .aggregation_second
        .is_some_and(|current_second| second > current_second)
    {
        flush_pending_outputs(state);
    }
    state.aggregation_second = Some(
        state
            .aggregation_second
            .map_or(second, |current_second| current_second.max(second)),
    );
    if !aggregate {
        state.pending_outputs.push(batch);
        return;
    }
    let key = AggregationKey {
        second,
        process_id: batch.full_event.process.pid,
        process_instance_id: batch.full_event.process.process_instance_id.clone(),
        action: batch.full_event.action.kind,
        operation_id: batch.full_event.operation_id.clone(),
        resource_identifier: batch.full_event.resource.identifier.clone(),
        destination_identifier: batch
            .full_event
            .destination
            .as_ref()
            .map(|value| value.identifier.clone()),
        status_code: batch.full_event.result.status_code,
        success: batch.full_event.result.success,
        provider_event_id: batch.full_event.evidence.provider_event_id,
        provider_opcode: batch.full_event.evidence.provider_opcode,
        provider_event_version: batch.full_event.evidence.provider_event_version,
    };
    if let Some(existing) = state.aggregated_outputs.get_mut(&key) {
        merge_output_batch(existing, &batch);
    } else {
        state.aggregated_outputs.insert(key, batch);
        state.peak_aggregation_keys = state
            .peak_aggregation_keys
            .max(state.aggregated_outputs.len());
    }
}

fn merge_output_batch(existing: &mut OutputBatch, incoming: &OutputBatch) {
    merge_aggregation(
        &mut existing.full_event.aggregation,
        incoming.full_event.aggregation.as_ref(),
    );
    existing.full_event.event_timestamp_unix_ms = existing
        .full_event
        .event_timestamp_unix_ms
        .min(incoming.full_event.event_timestamp_unix_ms);
    existing.full_event.observed_at_unix_ms = existing
        .full_event
        .observed_at_unix_ms
        .max(incoming.full_event.observed_at_unix_ms);
    existing.full_event.result.bytes_transferred = existing
        .full_event
        .aggregation
        .as_ref()
        .and_then(|value| value.total_bytes_transferred);
    if let (Some(existing_filtered), Some(incoming_filtered)) = (
        existing.filtered_event.as_mut(),
        incoming.filtered_event.as_ref(),
    ) {
        merge_aggregation(
            &mut existing_filtered.aggregation,
            incoming_filtered.aggregation.as_ref(),
        );
        existing_filtered.event_timestamp_unix_ms = existing_filtered
            .event_timestamp_unix_ms
            .min(incoming_filtered.event_timestamp_unix_ms);
        existing_filtered.observed_at_unix_ms = existing_filtered
            .observed_at_unix_ms
            .max(incoming_filtered.observed_at_unix_ms);
        existing_filtered.result.bytes_transferred = existing_filtered
            .aggregation
            .as_ref()
            .and_then(|value| value.total_bytes_transferred);
    }
}

fn merge_aggregation(existing: &mut Option<EventAggregation>, incoming: Option<&EventAggregation>) {
    let (Some(existing_value), Some(incoming_value)) = (existing.as_mut(), incoming) else {
        return;
    };
    existing_value.occurrence_count += incoming_value.occurrence_count;
    existing_value.first_event_timestamp_unix_ms = existing_value
        .first_event_timestamp_unix_ms
        .min(incoming_value.first_event_timestamp_unix_ms);
    existing_value.last_event_timestamp_unix_ms = existing_value
        .last_event_timestamp_unix_ms
        .max(incoming_value.last_event_timestamp_unix_ms);
    if let (Some(existing_occurrences), Some(incoming_occurrences)) = (
        existing_value.occurrences.as_mut(),
        incoming_value.occurrences.as_ref(),
    ) {
        existing_occurrences.extend(incoming_occurrences.iter().cloned());
    }
    existing_value.total_bytes_transferred = sum_optional_bytes(
        existing_value.total_bytes_transferred,
        incoming_value.total_bytes_transferred,
    );
}

fn sum_optional_bytes(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left_value), Some(right_value)) => Some(left_value.saturating_add(right_value)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn flush_pending_outputs(state: &mut CollectorState) {
    let mut batches = state
        .aggregated_outputs
        .drain()
        .map(|(_, batch)| batch)
        .collect::<Vec<_>>();
    batches.append(&mut state.pending_outputs);
    for batch in &mut batches {
        if let Some(occurrences) = batch
            .full_event
            .aggregation
            .as_mut()
            .and_then(|aggregation| aggregation.occurrences.as_mut())
        {
            occurrences.sort_unstable_by(|left, right| {
                left.event_timestamp_unix_ms
                    .cmp(&right.event_timestamp_unix_ms)
                    .then_with(|| left.observed_at_unix_ms.cmp(&right.observed_at_unix_ms))
                    .then_with(|| left.event_id.cmp(&right.event_id))
            });
        }
    }
    batches.sort_unstable_by(|left, right| {
        left.full_event
            .event_timestamp_unix_ms
            .cmp(&right.full_event.event_timestamp_unix_ms)
            .then_with(|| {
                left.full_event
                    .observed_at_unix_ms
                    .cmp(&right.full_event.observed_at_unix_ms)
            })
            .then_with(|| left.full_event.event_id.cmp(&right.full_event.event_id))
    });
    enqueue_output(state, batches);
}

fn flush_pending_detail_outputs(state: &mut CollectorState) {
    let mut batches = std::mem::take(&mut state.pending_outputs);
    batches.sort_unstable_by(|left, right| {
        left.full_event
            .event_timestamp_unix_ms
            .cmp(&right.full_event.event_timestamp_unix_ms)
            .then_with(|| {
                left.full_event
                    .observed_at_unix_ms
                    .cmp(&right.full_event.observed_at_unix_ms)
            })
            .then_with(|| left.full_event.event_id.cmp(&right.full_event.event_id))
    });
    enqueue_output(state, batches);
}

fn normalize_summary_batch(batch: &mut OutputBatch, resource_category: &str) {
    batch.full_event.process.command_line = None;
    if batch.full_event.resource.kind == ResourceKind::File {
        let identifier = format!("category:{resource_category}");
        batch.full_event.resource.identifier = identifier.clone();
        if let Some(filtered_event) = batch.filtered_event.as_mut() {
            filtered_event.resource.identifier = identifier;
        }
    }
    if batch.full_event.resource.kind != ResourceKind::Network {
        batch.full_event.operation_id = None;
    }
    if let Some(aggregation) = batch.full_event.aggregation.as_mut() {
        aggregation.occurrences = None;
    }
    if let Some(filtered_event) = batch.filtered_event.as_mut()
        && filtered_event.resource.kind != ResourceKind::Network
    {
        filtered_event.operation_id = None;
    }
}

fn enqueue_output(state: &mut CollectorState, batches: Vec<OutputBatch>) {
    if batches.is_empty() {
        return;
    }
    let batch_count = batches.len() as u64;
    state.output_batches_generated += batch_count;
    let Some(sender) = state.output_sender.as_ref() else {
        state.output_batches_dropped += batch_count;
        return;
    };
    let depth = state
        .output_queue_metrics
        .current_depth
        .fetch_add(1, Ordering::Relaxed)
        + 1;
    let send_started_at = Instant::now();
    let send_result = sender.send(batches);
    let send_wait_micros = send_started_at.elapsed().as_micros() as u64;
    if depth > OUTPUT_QUEUE_CAPACITY {
        state.output_queue_wait_count += 1;
        state.output_queue_wait_micros = state
            .output_queue_wait_micros
            .saturating_add(send_wait_micros);
    }
    match send_result {
        Ok(()) => {
            state
                .output_queue_metrics
                .peak_depth
                .fetch_max(depth.min(OUTPUT_QUEUE_CAPACITY), Ordering::Relaxed);
        }
        Err(_) => {
            state
                .output_queue_metrics
                .current_depth
                .fetch_sub(1, Ordering::Relaxed);
            state.output_batches_dropped += batch_count;
        }
    }
}

fn load_identity(path: &PathBuf) -> Result<AgentIdentity, String> {
    let content = fs::read_to_string(path).map_err(|error| {
        format!(
            "无法读取 Agent 身份文件 path={} error={error}",
            path.display()
        )
    })?;
    serde_json::from_str(&content).map_err(|error| {
        format!(
            "无法解析 Agent 身份文件 path={} error={error}",
            path.display()
        )
    })
}

fn parse_arguments(raw_arguments: &[String]) -> Result<Arguments, String> {
    if raw_arguments.len() == 16
        && raw_arguments[1] == "--observe-until-stopped"
        && raw_arguments[2] == "--stop-signal"
        && raw_arguments[4] == "--health-output"
        && raw_arguments[6] == "--health-interval-seconds"
        && raw_arguments[8] == "--output-directory"
        && raw_arguments[10] == "--roll-size-bytes"
        && raw_arguments[12] == "--roll-interval-seconds"
        && raw_arguments[14] == "--identity"
    {
        let health_interval_seconds =
            parse_bounded_u64(&raw_arguments[7], "health-interval-seconds", 1, 3_600)?;
        let max_segment_bytes =
            parse_bounded_u64(&raw_arguments[11], "roll-size-bytes", 1, 1_099_511_627_776)?;
        let max_segment_duration_seconds =
            parse_bounded_u64(&raw_arguments[13], "roll-interval-seconds", 1, 86_400)?;
        return Ok(Arguments {
            lifetime: CollectionLifetime::Persistent(PersistentConfiguration {
                stop_signal_path: PathBuf::from(&raw_arguments[3]),
                health_output_path: PathBuf::from(&raw_arguments[5]),
                health_interval: Duration::from_secs(health_interval_seconds),
            }),
            output: OutputConfiguration::Rolling {
                directory: PathBuf::from(&raw_arguments[9]),
                max_segment_bytes,
                max_segment_duration: Duration::from_secs(max_segment_duration_seconds),
            },
            identity_path: PathBuf::from(&raw_arguments[15]),
        });
    }
    let has_stop_signal = raw_arguments.len() == 11 && raw_arguments[9] == "--stop-signal";
    if (raw_arguments.len() != 9 && !has_stop_signal)
        || raw_arguments[1] != "--duration-seconds"
        || raw_arguments[3] != "--full-output"
        || raw_arguments[5] != "--filtered-output"
        || raw_arguments[7] != "--identity"
    {
        return Err(String::from(
            "参数错误，用法: etw-collector --duration-seconds <1-3600> --full-output <path.ndjson> --filtered-output <path.ndjson> --identity <agent-identity.json> [--stop-signal <path>]；或 etw-collector --observe-until-stopped --stop-signal <path> --health-output <path.ndjson> --health-interval-seconds <1-3600> --output-directory <path> --roll-size-bytes <1-1099511627776> --roll-interval-seconds <1-86400> --identity <agent-identity.json>",
        ));
    }

    let duration_seconds = raw_arguments[2].parse::<u64>().map_err(|error| {
        format!(
            "duration-seconds 必须是整数 value={} error={error}",
            raw_arguments[2]
        )
    })?;
    if !(1..=3_600).contains(&duration_seconds) {
        return Err(format!(
            "duration-seconds 超出允许范围 value={duration_seconds} expected=1..=3600"
        ));
    }

    Ok(Arguments {
        lifetime: CollectionLifetime::Timed {
            duration: Duration::from_secs(duration_seconds),
            stop_signal_path: has_stop_signal.then(|| PathBuf::from(&raw_arguments[10])),
        },
        output: OutputConfiguration::Fixed {
            full_output_path: PathBuf::from(&raw_arguments[4]),
            filtered_output_path: PathBuf::from(&raw_arguments[6]),
        },
        identity_path: PathBuf::from(&raw_arguments[8]),
    })
}

fn parse_bounded_u64(
    raw_value: &str,
    name: &str,
    minimum: u64,
    maximum: u64,
) -> Result<u64, String> {
    let value = raw_value
        .parse::<u64>()
        .map_err(|error| format!("{name} 必须是整数 value={raw_value} error={error}"))?;
    if !(minimum..=maximum).contains(&value) {
        return Err(format!(
            "{name} 超出允许范围 value={value} expected={minimum}..={maximum}"
        ));
    }
    Ok(value)
}

fn handle_process_event(
    record: &EventRecord,
    schema_locator: &SchemaLocator,
    state: &Arc<Mutex<CollectorState>>,
) {
    let provider_event_id = record.event_id();
    let opcode = record.opcode();
    let (action_kind, phase) = match process_action(opcode) {
        Some(value) => value,
        None => return,
    };
    if !increment_received(state, provider_event_id) {
        return;
    }
    let schema = match schema_locator.event_schema(record) {
        Ok(schema) => schema,
        Err(error) => {
            record_parse_failure(
                state,
                provider_event_id,
                "无法加载 ETW 事件 Schema",
                &format!("{error:?}"),
            );
            return;
        }
    };
    let parser = Parser::create(record, &schema);
    let process_id = match required_u32(&parser, "ProcessId") {
        Ok(value) => value,
        Err(error) => {
            record_parse_failure(state, provider_event_id, "无法解析 ProcessId", &error);
            return;
        }
    };
    let image_name = match required_string(&parser, "ImageFileName") {
        Ok(value) => value,
        Err(error) => {
            record_parse_failure(state, provider_event_id, "无法解析 ImageFileName", &error);
            return;
        }
    };
    let process =
        match build_process_ref(record, &parser, process_id, image_name, action_kind, state) {
            Ok(value) => value,
            Err(error) => {
                record_parse_failure(state, provider_event_id, "无法构造进程证据", &error);
                return;
            }
        };
    let event_timestamp_unix_ms = match filetime_to_unix_ms(record.raw_timestamp()) {
        Ok(value) => value,
        Err(error) => {
            record_parse_failure(state, provider_event_id, "无法转换 ETW 事件时间", &error);
            return;
        }
    };
    let observed_at_unix_ms = match unix_time_ms() {
        Ok(value) => value,
        Err(error) => {
            record_parse_failure(state, provider_event_id, "无法生成观测时间", &error);
            return;
        }
    };

    let mut current_state = match state.lock() {
        Ok(value) => value,
        Err(_) => {
            write_warning(provider_event_id, "ETW 采集状态锁已损坏", "mutex poisoned");
            return;
        }
    };
    let event_id = format!(
        "{}:{}",
        current_state.session_id, current_state.next_sequence
    );
    current_state.next_sequence += 1;
    let action = Action { kind: action_kind };
    let resource = Resource {
        kind: ResourceKind::Process,
        identifier: process
            .image_path
            .clone()
            .unwrap_or_else(|| process.image_name.clone()),
    };
    let evidence = Evidence {
        source: EvidenceSource::Etw,
        provider_id: String::from(PROCESS_PROVIDER_ID),
        provider_event_id,
        provider_opcode: opcode,
        provider_event_version: record.version(),
    };
    let result = ObservationResult {
        status_code: process.exit_status,
        success: process.exit_status.map(|status| status == 0),
        bytes_transferred: None,
    };
    let native_evidence = etw_native_evidence(
        record,
        &current_state.session_id,
        &process,
        None,
        None,
        None,
        None,
    );
    let full_event = ProcessEvidenceEvent {
        schema_version: String::from(EVENT_SCHEMA_VERSION),
        event_id: event_id.clone(),
        event_timestamp_unix_ms,
        observed_at_unix_ms,
        phase,
        operation_id: None,
        operation_id_origin: None,
        native_evidence: native_evidence.clone(),
        session: SessionRef {
            id: current_state.session_id.clone(),
        },
        process: process.clone(),
        action: action.clone(),
        resource: resource.clone(),
        destination: None,
        result: result.clone(),
        evidence: evidence.clone(),
        retention: None,
        aggregation: Some(single_event_aggregation(event_timestamp_unix_ms, None)),
    };
    if action_kind == ActionKind::ProcessStart {
        clear_reused_process_id(&mut current_state, &process);
        current_state
            .all_processes_by_pid
            .insert(process.pid, process.clone());
    }
    let actor = resolve_actor(&mut current_state, &process, action_kind);
    let Some(actor) = actor else {
        cleanup_stopped_process(&mut current_state, process_id, action_kind);
        return;
    };
    current_state.parsed_events += 1;
    current_state.process_events_parsed += 1;
    let filtered_process = ProcessRef {
        command_line: None,
        ..process.clone()
    };
    let filtered_event = ObservationEvent {
        schema_version: String::from(EVENT_SCHEMA_VERSION),
        event_id,
        event_timestamp_unix_ms,
        observed_at_unix_ms,
        phase,
        operation_id: None,
        operation_id_origin: None,
        native_evidence,
        actor: actor.clone(),
        session: SessionRef {
            id: current_state.session_id.clone(),
        },
        process: filtered_process,
        action,
        resource,
        destination: None,
        result,
        verdict: Verdict {
            mode: ObservationMode::Observe,
            outcome: ObservationOutcome::Observed,
        },
        evidence,
        confidence: match actor.identity_status {
            native_contracts::IdentityStatus::Candidate => native_contracts::Confidence::Medium,
            native_contracts::IdentityStatus::Verified
            | native_contracts::IdentityStatus::Inherited => native_contracts::Confidence::High,
        },
        retention: None,
        aggregation: Some(single_event_aggregation(event_timestamp_unix_ms, None)),
    };
    emit_output(
        &mut current_state,
        OutputBatch {
            full_event,
            filtered_event: Some(filtered_event),
        },
    );

    cleanup_stopped_process(&mut current_state, process_id, action_kind);
}

fn cleanup_stopped_process(state: &mut CollectorState, process_id: u32, action_kind: ActionKind) {
    if action_kind != ActionKind::ProcessStop {
        return;
    }
    state.tracked_process_ids.remove(&process_id);
    state.all_processes_by_pid.remove(&process_id);
    state.processes_by_pid.remove(&process_id);
    state.actors_by_pid.remove(&process_id);
    state
        .process_ids_by_thread_id
        .retain(|_, owner_process_id| *owner_process_id != process_id);
}

fn clear_reused_process_id(state: &mut CollectorState, process: &ProcessRef) {
    let is_reused =
        has_process_identity_collision(state.all_processes_by_pid.get(&process.pid), process);
    if !is_reused {
        return;
    }
    let target_related = state.actors_by_pid.contains_key(&process.pid)
        || process
            .image_path
            .as_deref()
            .is_some_and(|path| paths_equal(path, &state.identity.executable_path))
        || process
            .parent_pid
            .is_some_and(|parent_pid| state.tracked_process_ids.contains(&parent_pid));
    if target_related {
        state.identity_collision_count += 1;
    }
    state.tracked_process_ids.remove(&process.pid);
    state.processes_by_pid.remove(&process.pid);
    state.actors_by_pid.remove(&process.pid);
    state
        .process_ids_by_thread_id
        .retain(|_, process_id| *process_id != process.pid);
}

fn has_process_identity_collision(existing: Option<&ProcessRef>, incoming: &ProcessRef) -> bool {
    existing.is_some_and(|value| value.process_instance_id != incoming.process_instance_id)
}

fn handle_thread_event(
    record: &EventRecord,
    schema_locator: &SchemaLocator,
    state: &Arc<Mutex<CollectorState>>,
) {
    let opcode = record.opcode();
    if !matches!(opcode, 1..=4) {
        return;
    }
    let provider_event_id = record.event_id();
    if !increment_received(state, provider_event_id) {
        return;
    }
    let schema = match schema_locator.event_schema(record) {
        Ok(value) => value,
        Err(error) => {
            record_parse_failure(
                state,
                provider_event_id,
                "无法加载线程事件 Schema",
                &format!("{error:?}"),
            );
            return;
        }
    };
    let parser = Parser::create(record, &schema);
    let process_id = match required_u32(&parser, "ProcessId") {
        Ok(value) => value,
        Err(error) => {
            record_parse_failure(state, provider_event_id, "无法解析线程 ProcessId", &error);
            return;
        }
    };
    let thread_id = match required_u32(&parser, "TThreadId") {
        Ok(value) => value,
        Err(error) => {
            record_parse_failure(state, provider_event_id, "无法解析 TThreadId", &error);
            return;
        }
    };
    let mut current_state = match state.lock() {
        Ok(value) => value,
        Err(_) => {
            write_warning(provider_event_id, "ETW 采集状态锁已损坏", "mutex poisoned");
            return;
        }
    };
    if matches!(opcode, 1 | 3) {
        if current_state.actors_by_pid.contains_key(&process_id) {
            current_state.parsed_events += 1;
            current_state.thread_events_parsed += 1;
            current_state
                .process_ids_by_thread_id
                .insert(thread_id, process_id);
        }
    } else if current_state
        .process_ids_by_thread_id
        .remove(&thread_id)
        .is_some()
    {
        current_state.parsed_events += 1;
        current_state.thread_events_parsed += 1;
    }
}

fn handle_file_event(
    record: &EventRecord,
    schema_locator: &SchemaLocator,
    state: &Arc<Mutex<CollectorState>>,
) {
    let opcode = record.opcode();
    if !matches!(opcode, 64 | 66 | 67 | 68 | 70 | 71 | 76) {
        return;
    }
    let provider_event_id = record.event_id();
    if !increment_received(state, provider_event_id) {
        return;
    }
    let schema = match schema_locator.event_schema(record) {
        Ok(value) => value,
        Err(error) => {
            record_parse_failure(
                state,
                provider_event_id,
                "无法加载文件事件 Schema",
                &format!("{error:?}"),
            );
            return;
        }
    };
    let parser = Parser::create(record, &schema);
    if opcode == 66 {
        close_file_object(&parser, state, provider_event_id);
        return;
    }
    if opcode == 76 {
        complete_file_operation(record, &parser, state, provider_event_id);
        return;
    }
    let Some(action_kind) = file_action(opcode) else {
        return;
    };
    let thread_id = match required_u32(&parser, "TTID") {
        Ok(value) => value,
        Err(error) => {
            record_parse_failure(state, provider_event_id, "无法解析文件事件 TTID", &error);
            return;
        }
    };
    let process_id = match target_process_id_for_thread(state, thread_id, record.process_id()) {
        Ok(Some(value)) => value,
        Ok(None) => return,
        Err(error) => {
            record_parse_failure(state, provider_event_id, "无法核实文件事件进程归属", &error);
            return;
        }
    };
    let file_object = match parse_optional_pointer_u64(&parser, "FileObject") {
        Ok(value) => value,
        Err(error) => {
            record_parse_failure(state, provider_event_id, "无法解析 FileObject", &error);
            return;
        }
    };
    let operation_id = match parse_optional_pointer_u64(&parser, "IrpPtr") {
        Ok(value) => value,
        Err(error) => {
            record_parse_failure(state, provider_event_id, "无法解析 IrpPtr", &error);
            return;
        }
    };
    let open_path = match parse_optional_string(&parser, "OpenPath") {
        Ok(value) => value,
        Err(error) => {
            record_parse_failure(state, provider_event_id, "无法解析 OpenPath", &error);
            return;
        }
    };
    let bytes_transferred = if matches!(action_kind, ActionKind::FileRead | ActionKind::FileWrite) {
        match parse_optional_u32(&parser, "IoSize") {
            Ok(value) => value.map(u64::from),
            Err(error) => {
                record_parse_failure(state, provider_event_id, "无法解析 IoSize", &error);
                return;
            }
        }
    } else {
        None
    };
    let mut current_state = match state.lock() {
        Ok(value) => value,
        Err(_) => {
            write_warning(provider_event_id, "ETW 采集状态锁已损坏", "mutex poisoned");
            return;
        }
    };
    if !current_state.actors_by_pid.contains_key(&process_id) {
        return;
    }
    let resource_identifier = open_path
        .or_else(|| {
            file_object.and_then(|key| current_state.paths_by_file_object.get(&key).cloned())
        })
        .unwrap_or_else(|| format!("file-object-{}", pointer_label(file_object)));
    if let (Some(key), Some(path)) = (file_object, resource_path(&resource_identifier)) {
        current_state.paths_by_file_object.insert(key, path);
    }
    current_state.parsed_events += 1;
    current_state.file_events_parsed += 1;
    let details = OsEventDetails {
        process_id,
        action_kind,
        resource_kind: ResourceKind::File,
        resource_identifier,
        related_resource: None,
        operation_id: operation_id.map(|value| format!("irp-{value:016x}")),
        operation_id_origin: operation_id.map(|_| OperationIdOrigin::EtwIrp),
        thread_id: Some(thread_id),
        irp: operation_id,
        file_object,
        tcp_connection_id: None,
        result: ObservationResult {
            status_code: None,
            success: None,
            bytes_transferred,
        },
        provider_id: FILE_PROVIDER_ID,
    };
    let Some(batch) = build_os_output_batch(&mut current_state, record, details) else {
        return;
    };
    if let Some(key) = operation_id {
        current_state.pending_file_operations.insert(key, batch);
    } else {
        emit_output(&mut current_state, batch);
    }
}

fn close_file_object(
    parser: &Parser<'_, '_>,
    state: &Arc<Mutex<CollectorState>>,
    provider_event_id: u16,
) {
    let file_object = match parse_optional_pointer_u64(parser, "FileObject") {
        Ok(value) => value,
        Err(error) => {
            record_parse_failure(state, provider_event_id, "无法解析关闭文件对象", &error);
            return;
        }
    };
    if let Ok(mut current_state) = state.lock()
        && file_object
            .and_then(|key| current_state.paths_by_file_object.remove(&key))
            .is_some()
    {
        current_state.parsed_events += 1;
        current_state.file_events_parsed += 1;
    }
}

fn complete_file_operation(
    record: &EventRecord,
    parser: &Parser<'_, '_>,
    state: &Arc<Mutex<CollectorState>>,
    provider_event_id: u16,
) {
    let operation_id = match parse_optional_pointer_u64(parser, "IrpPtr") {
        Ok(Some(value)) => value,
        Ok(None) => return,
        Err(error) => {
            record_parse_failure(state, provider_event_id, "无法解析完成事件 IrpPtr", &error);
            return;
        }
    };
    let status_code = match parse_optional_u32(parser, "NtStatus") {
        Ok(value) => value,
        Err(error) => {
            record_parse_failure(state, provider_event_id, "无法解析 NtStatus", &error);
            return;
        }
    };
    let mut current_state = match state.lock() {
        Ok(value) => value,
        Err(_) => {
            write_warning(provider_event_id, "ETW 采集状态锁已损坏", "mutex poisoned");
            return;
        }
    };
    let Some(mut batch) = current_state.pending_file_operations.remove(&operation_id) else {
        return;
    };
    current_state.parsed_events += 1;
    current_state.file_events_parsed += 1;
    let completion_timestamp_unix_ms = match filetime_to_unix_ms(record.raw_timestamp()) {
        Ok(value) => value,
        Err(error) => {
            current_state.parse_failures += 1;
            write_warning(provider_event_id, "无法转换文件完成事件时间", &error);
            return;
        }
    };
    let observed_at_unix_ms = match unix_time_ms() {
        Ok(value) => value,
        Err(error) => {
            current_state.parse_failures += 1;
            write_warning(provider_event_id, "无法生成文件完成观测时间", &error);
            return;
        }
    };
    apply_file_operation_result(
        &mut batch,
        status_code,
        completion_timestamp_unix_ms,
        observed_at_unix_ms,
    );
    emit_output(&mut current_state, batch);
}

fn handle_network_event(
    record: &EventRecord,
    schema_locator: &SchemaLocator,
    state: &Arc<Mutex<CollectorState>>,
) {
    let opcode = record.opcode();
    let Some(action_kind) = network_action(opcode) else {
        return;
    };
    let provider_event_id = record.event_id();
    if !increment_received(state, provider_event_id) {
        return;
    }
    let schema = match schema_locator.event_schema(record) {
        Ok(value) => value,
        Err(error) => {
            record_parse_failure(
                state,
                provider_event_id,
                "无法加载 TCP/IP 事件 Schema",
                &format!("{error:?}"),
            );
            return;
        }
    };
    let parser = Parser::create(record, &schema);
    let process_id = match required_u32(&parser, "PID") {
        Ok(value) => value,
        Err(error) => {
            record_parse_failure(state, provider_event_id, "无法解析 TCP/IP PID", &error);
            return;
        }
    };
    let is_target = match state.lock() {
        Ok(current_state) => current_state.actors_by_pid.contains_key(&process_id),
        Err(_) => {
            write_warning(provider_event_id, "ETW 采集状态锁已损坏", "mutex poisoned");
            return;
        }
    };
    if !is_target {
        return;
    }
    let destination_address = match required_ip_address(&parser, "daddr") {
        Ok(value) => value,
        Err(error) => {
            record_parse_failure(state, provider_event_id, "无法解析 TCP/IP daddr", &error);
            return;
        }
    };
    let destination_port = match required_u16(&parser, "dport") {
        Ok(value) => network_port(value),
        Err(error) => {
            record_parse_failure(state, provider_event_id, "无法解析 TCP/IP dport", &error);
            return;
        }
    };
    let source_address = match required_ip_address(&parser, "saddr") {
        Ok(value) => value,
        Err(error) => {
            record_parse_failure(state, provider_event_id, "无法解析 TCP/IP saddr", &error);
            return;
        }
    };
    let source_port = match required_u16(&parser, "sport") {
        Ok(value) => network_port(value),
        Err(error) => {
            record_parse_failure(state, provider_event_id, "无法解析 TCP/IP sport", &error);
            return;
        }
    };
    let bytes_transferred = match parse_optional_u32(&parser, "size") {
        Ok(value) => value.map(u64::from),
        Err(error) => {
            record_parse_failure(state, provider_event_id, "无法解析 TCP/IP size", &error);
            return;
        }
    };
    let connection_id = match parse_tcp_connection_id(record) {
        Ok(value) => value,
        Err(error) => {
            record_parse_failure(state, provider_event_id, "无法解析 TCP/IP connid", &error);
            return;
        }
    };
    let mut current_state = match state.lock() {
        Ok(value) => value,
        Err(_) => {
            write_warning(provider_event_id, "ETW 采集状态锁已损坏", "mutex poisoned");
            return;
        }
    };
    if !current_state.actors_by_pid.contains_key(&process_id) {
        return;
    }
    current_state.parsed_events += 1;
    current_state.network_events_parsed += 1;
    emit_os_event(
        &mut current_state,
        record,
        OsEventDetails {
            process_id,
            action_kind,
            resource_kind: ResourceKind::Network,
            resource_identifier: format!("{source_address}:{source_port}"),
            related_resource: Some(Resource {
                kind: ResourceKind::Network,
                identifier: format!("{destination_address}:{destination_port}"),
            }),
            operation_id: connection_id.map(|value| format!("tcp-connection-{value:016x}")),
            operation_id_origin: connection_id.map(|_| OperationIdOrigin::EtwTcpConnectionId),
            thread_id: Some(record.thread_id()),
            irp: None,
            file_object: None,
            tcp_connection_id: connection_id,
            result: ObservationResult {
                status_code: None,
                success: None,
                bytes_transferred,
            },
            provider_id: TCP_PROVIDER_ID,
        },
    );
}

fn emit_os_event(state: &mut CollectorState, record: &EventRecord, details: OsEventDetails) {
    if let Some(batch) = build_os_output_batch(state, record, details) {
        emit_output(state, batch);
    }
}

fn build_os_output_batch(
    state: &mut CollectorState,
    record: &EventRecord,
    details: OsEventDetails,
) -> Option<OutputBatch> {
    let bytes_transferred = details.result.bytes_transferred;
    let event_timestamp_unix_ms = match filetime_to_unix_ms(record.raw_timestamp()) {
        Ok(value) => value,
        Err(error) => {
            state.parse_failures += 1;
            write_warning(record.event_id(), "无法转换 ETW 事件时间", &error);
            return None;
        }
    };
    let observed_at_unix_ms = match unix_time_ms() {
        Ok(value) => value,
        Err(error) => {
            state.parse_failures += 1;
            write_warning(record.event_id(), "无法生成观测时间", &error);
            return None;
        }
    };
    let process = state
        .all_processes_by_pid
        .get(&details.process_id)
        .cloned()
        .unwrap_or_else(|| unknown_process_ref(details.process_id, event_timestamp_unix_ms));
    let event_id = format!("{}:{}", state.session_id, state.next_sequence);
    state.next_sequence += 1;
    let action = Action {
        kind: details.action_kind,
    };
    let resource = Resource {
        kind: details.resource_kind,
        identifier: details.resource_identifier,
    };
    let evidence = Evidence {
        source: EvidenceSource::Etw,
        provider_id: String::from(details.provider_id),
        provider_event_id: record.event_id(),
        provider_opcode: record.opcode(),
        provider_event_version: record.version(),
    };
    let destination = details
        .related_resource
        .map(|value| native_contracts::Destination {
            kind: value.kind,
            identifier: value.identifier,
        });
    let native_evidence = etw_native_evidence(
        record,
        &state.session_id,
        &process,
        details.thread_id,
        details.irp,
        details.file_object,
        details.tcp_connection_id,
    );
    let full_event = ProcessEvidenceEvent {
        schema_version: String::from(EVENT_SCHEMA_VERSION),
        event_id: event_id.clone(),
        event_timestamp_unix_ms,
        observed_at_unix_ms,
        phase: EventPhase::Live,
        operation_id: details.operation_id.clone(),
        operation_id_origin: details.operation_id_origin,
        native_evidence: native_evidence.clone(),
        session: SessionRef {
            id: state.session_id.clone(),
        },
        process: process.clone(),
        action: action.clone(),
        resource: resource.clone(),
        destination: destination.clone(),
        result: details.result.clone(),
        evidence: evidence.clone(),
        retention: None,
        aggregation: Some(full_event_aggregation(
            event_id.clone(),
            event_timestamp_unix_ms,
            observed_at_unix_ms,
            details.operation_id.clone(),
            bytes_transferred,
        )),
    };
    let actor = state.actors_by_pid.get(&details.process_id).cloned()?;
    let confidence = match actor.identity_status {
        native_contracts::IdentityStatus::Candidate => native_contracts::Confidence::Medium,
        native_contracts::IdentityStatus::Verified
        | native_contracts::IdentityStatus::Inherited => native_contracts::Confidence::High,
    };
    let outcome = match details.result.success {
        Some(true) => ObservationOutcome::Succeeded,
        Some(false) => ObservationOutcome::Failed,
        None => ObservationOutcome::Observed,
    };
    let filtered_event = ObservationEvent {
        schema_version: String::from(EVENT_SCHEMA_VERSION),
        event_id,
        event_timestamp_unix_ms,
        observed_at_unix_ms,
        phase: EventPhase::Live,
        operation_id: details.operation_id,
        operation_id_origin: details.operation_id_origin,
        native_evidence,
        actor,
        session: SessionRef {
            id: state.session_id.clone(),
        },
        process: ProcessRef {
            command_line: None,
            ..process
        },
        action,
        resource,
        destination,
        result: details.result,
        verdict: Verdict {
            mode: ObservationMode::Observe,
            outcome,
        },
        evidence,
        confidence,
        retention: None,
        aggregation: Some(single_event_aggregation(
            event_timestamp_unix_ms,
            bytes_transferred,
        )),
    };
    Some(OutputBatch {
        full_event,
        filtered_event: Some(filtered_event),
    })
}

fn apply_file_operation_result(
    batch: &mut OutputBatch,
    status_code: Option<u32>,
    completion_timestamp_unix_ms: u64,
    observed_at_unix_ms: u64,
) {
    let success = status_code.map(|value| value == 0);
    batch.full_event.result.status_code = status_code;
    batch.full_event.result.success = success;
    batch.full_event.observed_at_unix_ms = observed_at_unix_ms;
    if let Some(aggregation) = batch.full_event.aggregation.as_mut() {
        aggregation.last_event_timestamp_unix_ms = completion_timestamp_unix_ms;
    }
    if let Some(filtered_event) = batch.filtered_event.as_mut() {
        filtered_event.result.status_code = status_code;
        filtered_event.result.success = success;
        filtered_event.observed_at_unix_ms = observed_at_unix_ms;
        filtered_event.verdict.outcome = match success {
            Some(true) => ObservationOutcome::Succeeded,
            Some(false) => ObservationOutcome::Failed,
            None => ObservationOutcome::Observed,
        };
        if let Some(aggregation) = filtered_event.aggregation.as_mut() {
            aggregation.last_event_timestamp_unix_ms = completion_timestamp_unix_ms;
        }
    }
}

fn single_event_aggregation(
    event_timestamp_unix_ms: u64,
    bytes_transferred: Option<u64>,
) -> EventAggregation {
    EventAggregation {
        occurrence_count: 1,
        first_event_timestamp_unix_ms: event_timestamp_unix_ms,
        last_event_timestamp_unix_ms: event_timestamp_unix_ms,
        total_bytes_transferred: bytes_transferred,
        occurrences: None,
    }
}

fn full_event_aggregation(
    event_id: String,
    event_timestamp_unix_ms: u64,
    observed_at_unix_ms: u64,
    operation_id: Option<String>,
    bytes_transferred: Option<u64>,
) -> EventAggregation {
    EventAggregation {
        occurrence_count: 1,
        first_event_timestamp_unix_ms: event_timestamp_unix_ms,
        last_event_timestamp_unix_ms: event_timestamp_unix_ms,
        total_bytes_transferred: bytes_transferred,
        occurrences: Some(vec![EventOccurrence {
            event_id,
            event_timestamp_unix_ms,
            observed_at_unix_ms,
            operation_id,
            bytes_transferred,
        }]),
    }
}

fn file_action(opcode: u8) -> Option<ActionKind> {
    match opcode {
        64 => Some(ActionKind::FileOpen),
        67 => Some(ActionKind::FileRead),
        68 => Some(ActionKind::FileWrite),
        70 => Some(ActionKind::FileDelete),
        71 => Some(ActionKind::FileRename),
        _ => None,
    }
}

fn network_action(opcode: u8) -> Option<ActionKind> {
    match opcode {
        10 | 26 => Some(ActionKind::NetworkSend),
        11 | 27 => Some(ActionKind::NetworkReceive),
        12 | 28 => Some(ActionKind::NetworkConnect),
        13 | 29 => Some(ActionKind::NetworkDisconnect),
        15 | 31 => Some(ActionKind::NetworkAccept),
        _ => None,
    }
}

fn network_port(raw_port: u16) -> u16 {
    u16::from_be(raw_port)
}

fn usable_header_process_id(process_id: u32) -> Option<u32> {
    if matches!(process_id, 0 | 4) {
        None
    } else {
        Some(process_id)
    }
}

fn target_process_id_for_thread(
    state: &Arc<Mutex<CollectorState>>,
    thread_id: u32,
    header_process_id: u32,
) -> Result<Option<u32>, String> {
    let current_state = state
        .lock()
        .map_err(|_| String::from("ETW 采集状态锁已损坏"))?;
    let process_id = current_state
        .process_ids_by_thread_id
        .get(&thread_id)
        .copied()
        .or_else(|| usable_header_process_id(header_process_id));
    Ok(process_id.filter(|value| current_state.actors_by_pid.contains_key(value)))
}

fn pointer_label(pointer: Option<u64>) -> String {
    pointer
        .map(|value| format!("{value:016x}"))
        .unwrap_or_else(|| String::from("unknown"))
}

fn resource_path(identifier: &str) -> Option<String> {
    if identifier.starts_with("file-object-") {
        None
    } else {
        Some(String::from(identifier))
    }
}

fn unknown_process_ref(process_id: u32, event_time: u64) -> ProcessRef {
    ProcessRef {
        pid: process_id,
        parent_pid: None,
        unique_process_key: None,
        process_instance_id: process_instance_id(None, process_id, event_time),
        process_session_id: None,
        user_sid: None,
        image_name: String::from("unknown"),
        image_path: None,
        command_line_observed: false,
        command_line: None,
        exit_status: None,
    }
}

fn process_action(opcode: u8) -> Option<(ActionKind, EventPhase)> {
    match opcode {
        PROCESS_START_OPCODE => Some((ActionKind::ProcessStart, EventPhase::Live)),
        PROCESS_STOP_OPCODE => Some((ActionKind::ProcessStop, EventPhase::Live)),
        PROCESS_RUNDOWN_START_OPCODE => Some((ActionKind::ProcessStart, EventPhase::Rundown)),
        PROCESS_RUNDOWN_STOP_OPCODE => Some((ActionKind::ProcessStop, EventPhase::Rundown)),
        _ => None,
    }
}

fn build_process_ref(
    record: &EventRecord,
    parser: &Parser<'_, '_>,
    process_id: u32,
    image_name: String,
    action_kind: ActionKind,
    state: &Arc<Mutex<CollectorState>>,
) -> Result<ProcessRef, String> {
    let existing = state
        .lock()
        .map_err(|_| String::from("ETW 采集状态锁已损坏"))?
        .all_processes_by_pid
        .get(&process_id)
        .cloned();
    let unique_process_key = parse_optional_pointer_u64(parser, "UniqueProcessKey")?;
    let event_time = filetime_to_unix_ms(record.raw_timestamp())?;
    let process_instance_id = if action_kind == ActionKind::ProcessStart {
        process_instance_id(unique_process_key, process_id, event_time)
    } else {
        existing
            .as_ref()
            .map(|process| process.process_instance_id.clone())
            .unwrap_or_else(|| process_instance_id(unique_process_key, process_id, event_time))
    };
    let parent_pid = parse_optional_u32(parser, "ParentId")?
        .or_else(|| existing.as_ref().and_then(|process| process.parent_pid));
    let process_session_id = parse_optional_u32(parser, "SessionId")?.or_else(|| {
        existing
            .as_ref()
            .and_then(|process| process.process_session_id)
    });
    let user_sid = parse_optional_bytes(parser, "UserSID")?
        .and_then(|bytes| sid_bytes_to_string(&bytes))
        .or_else(|| {
            existing
                .as_ref()
                .and_then(|process| process.user_sid.clone())
        });
    let command_line = parse_optional_string(parser, "CommandLine")?.or_else(|| {
        existing
            .as_ref()
            .and_then(|process| process.command_line.clone())
    });
    let image_path = if action_kind == ActionKind::ProcessStart {
        query_process_image_path(process_id).ok()
    } else {
        existing
            .as_ref()
            .and_then(|process| process.image_path.clone())
    };
    let exit_status = if action_kind == ActionKind::ProcessStop {
        parse_optional_u32(parser, "ExitStatus")?
    } else {
        None
    };

    Ok(ProcessRef {
        pid: process_id,
        parent_pid,
        unique_process_key,
        process_instance_id,
        process_session_id,
        user_sid,
        image_name,
        image_path,
        command_line_observed: command_line.is_some(),
        command_line,
        exit_status,
    })
}

fn resolve_actor(
    state: &mut CollectorState,
    process: &ProcessRef,
    action_kind: ActionKind,
) -> Option<Actor> {
    if action_kind == ActionKind::ProcessStop {
        return state.actors_by_pid.get(&process.pid).cloned();
    }
    let identity_verified = process
        .image_path
        .as_deref()
        .is_some_and(|path| paths_equal(path, &state.identity.executable_path));
    let observation = ProcessObservation {
        pid: process.pid,
        parent_pid: process.parent_pid,
        image_name: &process.image_name,
        identity_verified,
    };
    let agent_match = classify_process(&state.tracked_process_ids, &observation)?;
    if agent_match.identity_status == native_contracts::IdentityStatus::Candidate {
        state.unverified_root_count += 1;
        return None;
    }
    let root_process_instance_id =
        if agent_match.identity_status == native_contracts::IdentityStatus::Inherited {
            process
                .parent_pid
                .and_then(|parent_pid| state.actors_by_pid.get(&parent_pid))
                .map(|actor| actor.root_process_instance_id.clone())
                .unwrap_or_else(|| process.process_instance_id.clone())
        } else {
            process.process_instance_id.clone()
        };
    let actor = Actor {
        agent_kind: agent_match.agent_kind,
        identity_status: agent_match.identity_status,
        root_process_instance_id,
        identity: state.identity.clone(),
    };
    state.tracked_process_ids.insert(process.pid);
    state.processes_by_pid.insert(process.pid, process.clone());
    state.actors_by_pid.insert(process.pid, actor.clone());
    Some(actor)
}

fn process_instance_id(
    unique_process_key: Option<u64>,
    process_id: u32,
    event_time: u64,
) -> String {
    match unique_process_key {
        Some(key) => format!("process-key-{key:016x}-pid-{process_id}-start-{event_time}"),
        None => format!("pid-{process_id}-start-{event_time}"),
    }
}

fn sid_bytes_to_string(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 8 {
        return None;
    }
    let sub_authority_count = usize::from(bytes[1]);
    let expected_length = 8usize.checked_add(sub_authority_count.checked_mul(4)?)?;
    if bytes.len() < expected_length {
        return None;
    }
    let authority = bytes[2..8]
        .iter()
        .fold(0u64, |value, byte| (value << 8) | u64::from(*byte));
    let mut sid = format!("S-{}-{authority}", bytes[0]);
    for index in 0..sub_authority_count {
        let offset = 8 + index * 4;
        let value = u32::from_le_bytes(bytes[offset..offset + 4].try_into().ok()?);
        sid.push_str(&format!("-{value}"));
    }
    Some(sid)
}

fn coverage_status(
    health: Option<&EtwSessionHealth>,
    parse_failures: u64,
    write_failures: u64,
    output_batches_dropped: u64,
    event_clock_skew_ms: Option<u64>,
) -> &'static str {
    let Some(health) = health else {
        return "unknown";
    };
    if health.events_lost == 0
        && health.log_buffers_lost == 0
        && health.realtime_buffers_lost == 0
        && parse_failures == 0
        && write_failures == 0
        && output_batches_dropped == 0
        && event_clock_skew_ms.is_none_or(|value| value <= EVENT_CLOCK_SKEW_LIMIT_MS)
    {
        "healthy"
    } else {
        "degraded"
    }
}

fn event_clock_skew_ms(
    event_timestamp_unix_ms: Option<u64>,
    observed_at_unix_ms: Option<u64>,
) -> Option<u64> {
    event_timestamp_unix_ms
        .zip(observed_at_unix_ms)
        .map(|(event_timestamp, observed_at)| event_timestamp.abs_diff(observed_at))
}

fn increment_received(state: &Arc<Mutex<CollectorState>>, provider_event_id: u16) -> bool {
    match state.lock() {
        Ok(mut value) => {
            value.provider_events_received += 1;
            true
        }
        Err(_) => {
            write_warning(provider_event_id, "ETW 采集状态锁已损坏", "mutex poisoned");
            false
        }
    }
}

fn record_parse_failure(
    state: &Arc<Mutex<CollectorState>>,
    provider_event_id: u16,
    message: &str,
    detail: &str,
) {
    if let Ok(mut value) = state.lock() {
        value.parse_failures += 1;
    }
    write_warning(provider_event_id, message, detail);
}

fn write_json_line<T: Serialize>(writer: &mut BufWriter<File>, event: &T) -> Result<u64, String> {
    let serialized =
        serde_json::to_vec(event).map_err(|error| format!("无法序列化事件 error={error}"))?;
    writer
        .write_all(&serialized)
        .map_err(|error| format!("无法写入事件 error={error}"))?;
    writer
        .write_all(b"\n")
        .map_err(|error| format!("无法写入换行符 error={error}"))?;
    u64::try_from(serialized.len() + 1)
        .map_err(|error| format!("事件长度无法转换为 u64 error={error}"))
}

fn unix_time_ms() -> Result<u64, String> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("系统时间早于 UNIX_EPOCH error={error}"))?;
    u64::try_from(elapsed.as_millis())
        .map_err(|error| format!("系统时间毫秒值超出 u64 范围 error={error}"))
}

fn required_u32(parser: &Parser<'_, '_>, property_name: &str) -> Result<u32, String> {
    parser
        .try_parse::<u32>(property_name)
        .map_err(|error| format!("ETW 必需属性解析失败 property={property_name} error={error}"))
}

fn required_u16(parser: &Parser<'_, '_>, property_name: &str) -> Result<u16, String> {
    parser
        .try_parse::<u16>(property_name)
        .map_err(|error| format!("ETW 必需属性解析失败 property={property_name} error={error}"))
}

fn etw_native_evidence(
    record: &EventRecord,
    session_id: &str,
    process: &ProcessRef,
    thread_id: Option<u32>,
    irp: Option<u64>,
    file_object: Option<u64>,
    tcp_connection_id: Option<u64>,
) -> NativeEvidence {
    let scope = format!("etw-session:{session_id}");
    let process_id = etw_identifier(NativeIdKind::Process, &scope, &process.pid.to_string());
    let mut identifiers = vec![process_id.clone()];
    let mut edges = Vec::new();
    let activity_id = (record.activity_id().to_u128() != 0).then(|| {
        etw_identifier(
            NativeIdKind::Activity,
            &scope,
            &format!("{:032x}", record.activity_id().to_u128()),
        )
    });
    let mut related_activity_id = None;
    let mut process_start_key = None;
    for item in record.extended_data() {
        match item.to_extended_data_item() {
            ExtendedDataItem::RelatedActivityId(value) if value.to_u128() != 0 => {
                related_activity_id = Some(etw_identifier(
                    NativeIdKind::RelatedActivity,
                    &scope,
                    &format!("{:032x}", value.to_u128()),
                ));
            }
            ExtendedDataItem::ProcessStartKey(value) if value != 0 => {
                process_start_key = Some(etw_identifier(
                    NativeIdKind::ProcessStartKey,
                    &scope,
                    &format!("{value:016x}"),
                ));
            }
            _ => {}
        }
    }
    if let Some(activity_id) = activity_id {
        identifiers.push(activity_id.clone());
        edges.push(CausalEdge {
            kind: CausalEdgeKind::CoRecordedMapping,
            from: activity_id.clone(),
            to: process_id.clone(),
            evidence_fields: vec![
                String::from("EventHeader.ActivityId"),
                String::from("ProcessId"),
            ],
        });
        if let Some(related_activity_id) = related_activity_id {
            identifiers.push(related_activity_id.clone());
            edges.push(CausalEdge {
                kind: CausalEdgeKind::ExplicitParent,
                from: activity_id,
                to: related_activity_id,
                evidence_fields: vec![
                    String::from("EventHeader.ActivityId"),
                    String::from("ExtendedData.RelatedActivityId"),
                ],
            });
        }
    } else if let Some(related_activity_id) = related_activity_id {
        identifiers.push(related_activity_id);
    }
    if let Some(process_start_key) = process_start_key {
        identifiers.push(process_start_key.clone());
        edges.push(CausalEdge {
            kind: CausalEdgeKind::CoRecordedMapping,
            from: process_start_key,
            to: process_id.clone(),
            evidence_fields: vec![
                String::from("ExtendedData.ProcessStartKey"),
                String::from("ProcessId"),
            ],
        });
    }
    if let Some(value) = process.unique_process_key {
        let unique_process_key = etw_identifier(
            NativeIdKind::UniqueProcessKey,
            &scope,
            &format!("{value:016x}"),
        );
        identifiers.push(unique_process_key.clone());
        edges.push(CausalEdge {
            kind: CausalEdgeKind::CoRecordedMapping,
            from: unique_process_key,
            to: process_id.clone(),
            evidence_fields: vec![String::from("UniqueProcessKey"), String::from("ProcessId")],
        });
    }
    if let Some(value) = process.parent_pid {
        let parent_process =
            etw_identifier(NativeIdKind::ParentProcess, &scope, &value.to_string());
        identifiers.push(parent_process.clone());
        edges.push(CausalEdge {
            kind: CausalEdgeKind::ExplicitParent,
            from: process_id.clone(),
            to: parent_process,
            evidence_fields: vec![String::from("ProcessId"), String::from("ParentId")],
        });
    }
    let thread = thread_id.map(|value| {
        let identifier = etw_identifier(NativeIdKind::Thread, &scope, &value.to_string());
        identifiers.push(identifier.clone());
        edges.push(CausalEdge {
            kind: CausalEdgeKind::RuntimeOwnership,
            from: identifier.clone(),
            to: process_id.clone(),
            evidence_fields: vec![
                String::from("TTID/EventHeader.ThreadId"),
                String::from("ProcessId"),
            ],
        });
        identifier
    });
    let irp = irp.map(|value| {
        let identifier = etw_identifier(NativeIdKind::Irp, &scope, &format!("{value:016x}"));
        identifiers.push(identifier.clone());
        if let Some(thread) = &thread {
            edges.push(CausalEdge {
                kind: CausalEdgeKind::RuntimeOwnership,
                from: identifier.clone(),
                to: thread.clone(),
                evidence_fields: vec![String::from("IrpPtr"), String::from("TTID")],
            });
        }
        identifier
    });
    if let Some(value) = file_object {
        let identifier = etw_identifier(NativeIdKind::FileObject, &scope, &format!("{value:016x}"));
        identifiers.push(identifier.clone());
        if let Some(irp) = irp {
            edges.push(CausalEdge {
                kind: CausalEdgeKind::Lifecycle,
                from: identifier,
                to: irp,
                evidence_fields: vec![String::from("FileObject"), String::from("IrpPtr")],
            });
        }
    }
    if let Some(value) = tcp_connection_id {
        let identifier = etw_identifier(
            NativeIdKind::TcpConnection,
            &scope,
            &format!("{value:016x}"),
        );
        identifiers.push(identifier.clone());
        edges.push(CausalEdge {
            kind: CausalEdgeKind::RuntimeOwnership,
            from: identifier,
            to: process_id,
            evidence_fields: vec![String::from("connid"), String::from("PID")],
        });
    }
    NativeEvidence { identifiers, edges }
}

fn etw_identifier(kind: NativeIdKind, scope: &str, value: &str) -> NativeIdentifier {
    NativeIdentifier {
        source: NativeIdSource::Etw,
        kind,
        scope: String::from(scope),
        value: String::from(value),
    }
}

fn required_ip_address(parser: &Parser<'_, '_>, property_name: &str) -> Result<IpAddr, String> {
    parser
        .try_parse::<IpAddr>(property_name)
        .map_err(|error| format!("ETW 必需属性解析失败 property={property_name} error={error}"))
}

fn required_string(parser: &Parser<'_, '_>, property_name: &str) -> Result<String, String> {
    parser
        .try_parse::<String>(property_name)
        .map_err(|error| format!("ETW 必需属性解析失败 property={property_name} error={error}"))
}

fn parse_optional_u32(parser: &Parser<'_, '_>, property_name: &str) -> Result<Option<u32>, String> {
    match parser.try_parse::<u32>(property_name) {
        Ok(value) => Ok(Some(value)),
        Err(ParserError::NotFound) => Ok(None),
        Err(error) => Err(format!(
            "ETW 可选属性解析失败 property={property_name} error={error}"
        )),
    }
}

fn parse_optional_pointer_u64(
    parser: &Parser<'_, '_>,
    property_name: &str,
) -> Result<Option<u64>, String> {
    match parser.try_parse::<Pointer>(property_name) {
        Ok(value) => Ok(Some(*value as u64)),
        Err(ParserError::NotFound) => Ok(None),
        Err(error) => Err(format!(
            "ETW 可选指针属性解析失败 property={property_name} error={error}"
        )),
    }
}

fn parse_tcp_connection_id(record: &EventRecord) -> Result<Option<u64>, String> {
    let payload = event_user_data(record)?;
    tcp_connection_id_from_payload(record.version(), payload).map(|value| value.map(u64::from))
}

fn tcp_connection_id_from_payload(
    event_version: u8,
    payload: &[u8],
) -> Result<Option<u32>, String> {
    let bytes_after_connection_id = match event_version {
        0 => return Ok(None),
        1 => 4,
        2 => 0,
        value => {
            return Err(format!(
                "不支持的 TCP/IP 事件版本 version={value} payload_length={}",
                payload.len()
            ));
        }
    };
    let required_length = bytes_after_connection_id + std::mem::size_of::<u32>();
    if payload.len() < required_length {
        return Err(format!(
            "TCP/IP 事件 payload 不足 version={event_version} payload_length={} required_length={required_length}",
            payload.len()
        ));
    }
    let offset = payload.len() - required_length;
    let bytes: [u8; 4] = payload[offset..offset + 4]
        .try_into()
        .map_err(|error| format!("无法读取 TCP/IP connid version={event_version} error={error}"))?;
    let value = u32::from_ne_bytes(bytes);
    Ok((value != 0).then_some(value))
}

fn event_user_data(record: &EventRecord) -> Result<&[u8], String> {
    // ferrisetw 的 EventRecord 使用透明布局包装 Windows EVENT_RECORD。
    let raw_record = unsafe { &*(record as *const EventRecord).cast::<EVENT_RECORD>() };
    let length = usize::from(raw_record.UserDataLength);
    if length == 0 {
        return Ok(&[]);
    }
    if raw_record.UserData.is_null() {
        return Err(format!(
            "TCP/IP 事件 payload 指针为空 payload_length={length}"
        ));
    }
    // ETW 回调期间 UserData 指向的 payload 至少在 record 生命周期内有效且只读。
    Ok(unsafe { std::slice::from_raw_parts(raw_record.UserData.cast::<u8>(), length) })
}

fn parse_optional_string(
    parser: &Parser<'_, '_>,
    property_name: &str,
) -> Result<Option<String>, String> {
    match parser.try_parse::<String>(property_name) {
        Ok(value) => Ok(Some(value)),
        Err(ParserError::NotFound) => Ok(None),
        Err(error) => Err(format!(
            "ETW 可选属性解析失败 property={property_name} error={error}"
        )),
    }
}

fn parse_optional_bytes(
    parser: &Parser<'_, '_>,
    property_name: &str,
) -> Result<Option<Vec<u8>>, String> {
    match parser.try_parse::<Vec<u8>>(property_name) {
        Ok(value) => Ok(Some(value)),
        Err(ParserError::NotFound) => Ok(None),
        Err(error) => Err(format!(
            "ETW 可选二进制属性解析失败 property={property_name} error={error}"
        )),
    }
}

fn write_warning(provider_event_id: u16, message: &str, detail: &str) {
    let warning = serde_json::json!({
        "level": "warning",
        "provider_event_id": provider_event_id,
        "message": message,
        "detail": detail,
    });
    eprintln!("{warning}");
}

#[cfg(test)]
mod tests {
    use super::{
        CollectionLifetime, CollectionStopReason, OutputConfiguration, collection_failure,
        coverage_status, event_clock_skew_ms, has_process_identity_collision, merge_aggregation,
        network_port, parse_arguments, process_action, process_instance_id, sid_bytes_to_string,
        tcp_connection_id_from_payload, wait_for_collection_end, write_json_line,
    };
    use crate::windows::etw_health::EtwSessionHealth;
    use native_contracts::{ActionKind, EventAggregation, EventOccurrence, EventPhase, ProcessRef};
    use std::fs::{self, File};
    use std::io::{BufWriter, Write};
    use std::time::Duration;

    #[test]
    fn separates_live_and_rundown_process_events() {
        assert_eq!(
            process_action(1),
            Some((ActionKind::ProcessStart, EventPhase::Live))
        );
        assert_eq!(
            process_action(3),
            Some((ActionKind::ProcessStart, EventPhase::Rundown))
        );
    }

    #[test]
    fn stable_key_is_preferred_for_process_instance() {
        assert_eq!(
            process_instance_id(Some(42), 100, 200),
            "process-key-000000000000002a-pid-100-start-200"
        );
    }

    #[test]
    fn detects_pid_reuse_as_process_identity_collision() {
        let existing = process_ref(42, "process-old");
        let same_instance = process_ref(42, "process-old");
        let reused_pid = process_ref(42, "process-new");

        assert!(!has_process_identity_collision(
            Some(&existing),
            &same_instance
        ));
        assert!(has_process_identity_collision(Some(&existing), &reused_pid));
        assert!(!has_process_identity_collision(None, &reused_pid));
    }

    fn process_ref(pid: u32, process_instance_id: &str) -> ProcessRef {
        ProcessRef {
            pid,
            parent_pid: None,
            unique_process_key: None,
            process_instance_id: String::from(process_instance_id),
            process_session_id: None,
            user_sid: None,
            image_name: String::from("test.exe"),
            image_path: None,
            command_line_observed: false,
            command_line: None,
            exit_status: None,
        }
    }

    #[test]
    fn converts_tcp_port_from_network_byte_order() {
        assert_eq!(network_port(0x72ca), 51_826);
    }

    #[test]
    fn reads_tcp_connection_id_from_native_version_layout() {
        let version_one = [1, 2, 3, 4, 0x78, 0x56, 0x34, 0x12, 9, 10, 11, 12];
        let version_two = [1, 2, 3, 4, 0x78, 0x56, 0x34, 0x12];

        assert_eq!(
            tcp_connection_id_from_payload(1, &version_one),
            Ok(Some(0x1234_5678))
        );
        assert_eq!(
            tcp_connection_id_from_payload(2, &version_two),
            Ok(Some(0x1234_5678))
        );
        assert_eq!(tcp_connection_id_from_payload(0, &[]), Ok(None));
        assert_eq!(tcp_connection_id_from_payload(2, &[0, 0, 0, 0]), Ok(None));
        assert!(tcp_connection_id_from_payload(2, &[1, 2, 3]).is_err());
    }

    #[test]
    fn converts_binary_sid_to_canonical_string() {
        let bytes = [1, 2, 0, 0, 0, 0, 0, 5, 32, 0, 0, 0, 32, 2, 0, 0];
        assert_eq!(
            sid_bytes_to_string(&bytes),
            Some(String::from("S-1-5-32-544"))
        );
    }

    #[test]
    fn loss_or_parser_failure_degrades_coverage() {
        let health = EtwSessionHealth {
            number_of_buffers: 2,
            free_buffers: 1,
            events_lost: 0,
            buffers_written: 2,
            log_buffers_lost: 0,
            realtime_buffers_lost: 0,
        };
        assert_eq!(coverage_status(Some(&health), 0, 0, 0, None), "healthy");
        assert_eq!(
            coverage_status(Some(&health), 0, 0, 0, Some(60_000)),
            "healthy"
        );
        assert_eq!(
            coverage_status(Some(&health), 0, 0, 0, Some(60_001)),
            "degraded"
        );
        assert_eq!(coverage_status(Some(&health), 1, 0, 0, None), "degraded");
        assert_eq!(coverage_status(Some(&health), 0, 0, 1, None), "degraded");
        assert_eq!(coverage_status(None, 0, 0, 0, None), "unknown");
    }

    #[test]
    fn event_clock_skew_uses_absolute_difference() {
        assert_eq!(event_clock_skew_ms(Some(100), Some(175)), Some(75));
        assert_eq!(event_clock_skew_ms(Some(175), Some(100)), Some(75));
        assert_eq!(event_clock_skew_ms(None, Some(100)), None);
    }

    #[test]
    fn aggregation_preserves_occurrences_and_timestamp_bounds() {
        let mut aggregation = Some(EventAggregation {
            occurrence_count: 1,
            first_event_timestamp_unix_ms: 200,
            last_event_timestamp_unix_ms: 200,
            total_bytes_transferred: Some(10),
            occurrences: Some(vec![EventOccurrence {
                event_id: String::from("session:2"),
                event_timestamp_unix_ms: 200,
                observed_at_unix_ms: 220,
                operation_id: Some(String::from("irp-2")),
                bytes_transferred: Some(10),
            }]),
        });
        let incoming = EventAggregation {
            occurrence_count: 1,
            first_event_timestamp_unix_ms: 100,
            last_event_timestamp_unix_ms: 100,
            total_bytes_transferred: Some(20),
            occurrences: Some(vec![EventOccurrence {
                event_id: String::from("session:1"),
                event_timestamp_unix_ms: 100,
                observed_at_unix_ms: 120,
                operation_id: Some(String::from("irp-1")),
                bytes_transferred: Some(20),
            }]),
        };

        merge_aggregation(&mut aggregation, Some(&incoming));

        let merged = aggregation.expect("聚合结果必须存在");
        assert_eq!(merged.occurrence_count, 2);
        assert_eq!(merged.first_event_timestamp_unix_ms, 100);
        assert_eq!(merged.last_event_timestamp_unix_ms, 200);
        assert_eq!(merged.total_bytes_transferred, Some(30));
        assert_eq!(merged.occurrences.expect("完整事件必须存在").len(), 2);
    }

    #[test]
    fn write_failure_is_returned_to_caller() {
        let path = std::env::temp_dir().join(format!(
            "agentreins-read-only-output-{}.json",
            std::process::id()
        ));
        fs::write(&path, b"fixture").expect("应创建只读写入夹具");
        let file = File::open(&path).expect("应以只读方式打开夹具");
        let mut writer = BufWriter::new(file);

        let write_result = write_json_line(&mut writer, &serde_json::json!({"event": "fixture"}));
        let flush_result = writer.flush();

        drop(writer);
        fs::remove_file(&path).expect("应删除测试夹具");
        assert!(write_result.is_err() || flush_result.is_err());
    }

    #[test]
    fn output_failure_makes_collection_fail() {
        let result = collection_failure(None, None, None, None, Some(String::from("disk full")), 0);

        assert_eq!(result, Some(String::from("disk full")));
    }

    #[test]
    fn parses_optional_stop_signal() {
        let arguments = parse_arguments(&[
            String::from("etw-collector"),
            String::from("--duration-seconds"),
            String::from("30"),
            String::from("--full-output"),
            String::from("full.ndjson"),
            String::from("--filtered-output"),
            String::from("filtered.ndjson"),
            String::from("--identity"),
            String::from("identity.json"),
            String::from("--stop-signal"),
            String::from("stop.requested"),
        ])
        .expect("停止信号参数应解析成功");

        let CollectionLifetime::Timed {
            stop_signal_path, ..
        } = arguments.lifetime
        else {
            panic!("限时采集参数不应解析为常驻模式");
        };
        assert_eq!(
            stop_signal_path,
            Some(std::path::PathBuf::from("stop.requested"))
        );
    }

    #[test]
    fn parses_persistent_observer_arguments() {
        let arguments = parse_arguments(&[
            String::from("etw-collector"),
            String::from("--observe-until-stopped"),
            String::from("--stop-signal"),
            String::from("stop.requested"),
            String::from("--health-output"),
            String::from("health.ndjson"),
            String::from("--health-interval-seconds"),
            String::from("30"),
            String::from("--output-directory"),
            String::from("evidence"),
            String::from("--roll-size-bytes"),
            String::from("1048576"),
            String::from("--roll-interval-seconds"),
            String::from("3600"),
            String::from("--identity"),
            String::from("identity.json"),
        ])
        .expect("常驻观测参数应解析成功");

        let CollectionLifetime::Persistent(configuration) = arguments.lifetime else {
            panic!("常驻观测参数不应解析为限时模式");
        };
        assert_eq!(
            configuration.stop_signal_path,
            std::path::PathBuf::from("stop.requested")
        );
        assert_eq!(configuration.health_interval, Duration::from_secs(30));
        let OutputConfiguration::Rolling {
            directory,
            max_segment_bytes,
            max_segment_duration,
        } = arguments.output
        else {
            panic!("常驻观测必须使用滚动输出");
        };
        assert_eq!(directory, std::path::PathBuf::from("evidence"));
        assert_eq!(max_segment_bytes, 1_048_576);
        assert_eq!(max_segment_duration, Duration::from_secs(3_600));
    }

    #[test]
    fn existing_signal_stops_collection_without_waiting_for_deadline() {
        let path =
            std::env::temp_dir().join(format!("agentreins-stop-signal-{}", std::process::id()));
        fs::write(&path, b"stop").expect("应创建停止信号夹具");

        let result = wait_for_collection_end(Duration::from_secs(30), Some(&path));

        fs::remove_file(&path).expect("应删除停止信号夹具");
        assert!(matches!(result, Ok(CollectionStopReason::StopSignal)));
    }
}
