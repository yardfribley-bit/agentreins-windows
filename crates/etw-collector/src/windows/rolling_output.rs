use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

const OUTPUT_BUFFER_CAPACITY: usize = 4 * 1_024 * 1_024;

#[derive(Clone)]
pub struct RollingOutputConfiguration {
    pub directory: PathBuf,
    pub session_id: String,
    pub max_segment_bytes: u64,
    pub max_segment_duration: Duration,
}

#[derive(Serialize)]
struct SegmentRecord {
    index: u64,
    started_at_unix_ms: u64,
    ended_at_unix_ms: u64,
    full_file: String,
    filtered_file: String,
    full_events: u64,
    filtered_events: u64,
    full_bytes: u64,
    filtered_bytes: u64,
}

#[derive(Serialize)]
struct RollingManifest<'a> {
    schema_version: &'static str,
    session_id: &'a str,
    published_at_unix_ms: u64,
    complete: bool,
    segments: &'a [SegmentRecord],
}

struct SegmentWriters {
    index: u64,
    started_at: Instant,
    started_at_unix_ms: u64,
    full_path: PathBuf,
    filtered_path: PathBuf,
    full_writer: BufWriter<File>,
    filtered_writer: BufWriter<File>,
    full_events: u64,
    filtered_events: u64,
    full_bytes: u64,
    filtered_bytes: u64,
}

pub struct RollingOutputWriter {
    configuration: RollingOutputConfiguration,
    current: SegmentWriters,
    closed_segments: Vec<SegmentRecord>,
    needs_roll: bool,
    manifest_sequence: u64,
}

impl RollingOutputWriter {
    pub fn create(configuration: RollingOutputConfiguration) -> Result<Self, String> {
        fs::create_dir_all(&configuration.directory).map_err(|error| {
            format!(
                "无法创建滚动证据目录 path={} error={error}",
                configuration.directory.display()
            )
        })?;
        let current = create_segment(&configuration, 1)?;
        Ok(Self {
            configuration,
            current,
            closed_segments: Vec::new(),
            needs_roll: false,
            manifest_sequence: 0,
        })
    }

    pub fn begin_event(&mut self) -> Result<(), String> {
        if !self.needs_roll {
            return Ok(());
        }
        self.close_current_segment(false)?;
        let next_index = self.current.index + 1;
        self.current = create_segment(&self.configuration, next_index)?;
        self.needs_roll = false;
        Ok(())
    }

    pub fn write_full<T: Serialize>(&mut self, event: &T) -> Result<u64, String> {
        let bytes = write_json_line(&mut self.current.full_writer, event)?;
        self.current.full_events += 1;
        self.current.full_bytes = self.current.full_bytes.saturating_add(bytes);
        Ok(bytes)
    }

    pub fn write_filtered<T: Serialize>(&mut self, event: &T) -> Result<u64, String> {
        let bytes = write_json_line(&mut self.current.filtered_writer, event)?;
        self.current.filtered_events += 1;
        self.current.filtered_bytes = self.current.filtered_bytes.saturating_add(bytes);
        Ok(bytes)
    }

    pub fn finish_event(&mut self) {
        self.needs_roll = self.current.full_bytes >= self.configuration.max_segment_bytes
            || self.current.filtered_bytes >= self.configuration.max_segment_bytes
            || self.current.started_at.elapsed() >= self.configuration.max_segment_duration;
    }

    pub fn finish_batch(&mut self) -> Result<(), String> {
        flush_segment(&mut self.current)
    }

    pub fn complete(mut self) -> Result<u64, String> {
        self.close_current_segment(true)?;
        u64::try_from(self.closed_segments.len())
            .map_err(|error| format!("滚动证据分段数量无法转换为 u64 error={error}"))
    }

    pub fn current_segment_index(&self) -> u64 {
        self.current.index
    }

    pub fn published_segment_count(&self) -> Result<u64, String> {
        u64::try_from(self.closed_segments.len())
            .map_err(|error| format!("滚动证据分段数量无法转换为 u64 error={error}"))
    }

    fn close_current_segment(&mut self, complete: bool) -> Result<(), String> {
        flush_segment(&mut self.current)?;
        self.current
            .full_writer
            .get_ref()
            .sync_data()
            .map_err(|error| {
                format!(
                    "无法同步完整滚动证据 path={} error={error}",
                    self.current.full_path.display()
                )
            })?;
        self.current
            .filtered_writer
            .get_ref()
            .sync_data()
            .map_err(|error| {
                format!(
                    "无法同步过滤滚动证据 path={} error={error}",
                    self.current.filtered_path.display()
                )
            })?;
        self.closed_segments.push(SegmentRecord {
            index: self.current.index,
            started_at_unix_ms: self.current.started_at_unix_ms,
            ended_at_unix_ms: unix_time_ms()?,
            full_file: file_name(&self.current.full_path)?,
            filtered_file: file_name(&self.current.filtered_path)?,
            full_events: self.current.full_events,
            filtered_events: self.current.filtered_events,
            full_bytes: self.current.full_bytes,
            filtered_bytes: self.current.filtered_bytes,
        });
        self.manifest_sequence += 1;
        publish_manifest(
            &self.configuration,
            self.manifest_sequence,
            complete,
            &self.closed_segments,
        )
    }
}

fn create_segment(
    configuration: &RollingOutputConfiguration,
    index: u64,
) -> Result<SegmentWriters, String> {
    let prefix = format!("{}-{index:06}", configuration.session_id);
    let full_path = configuration
        .directory
        .join(format!("{prefix}.full.ndjson"));
    let filtered_path = configuration
        .directory
        .join(format!("{prefix}.filtered.ndjson"));
    let full_file = create_new_file(&full_path, "完整滚动证据")?;
    let filtered_file = create_new_file(&filtered_path, "过滤滚动证据")?;
    Ok(SegmentWriters {
        index,
        started_at: Instant::now(),
        started_at_unix_ms: unix_time_ms()?,
        full_path,
        filtered_path,
        full_writer: BufWriter::with_capacity(OUTPUT_BUFFER_CAPACITY, full_file),
        filtered_writer: BufWriter::with_capacity(OUTPUT_BUFFER_CAPACITY, filtered_file),
        full_events: 0,
        filtered_events: 0,
        full_bytes: 0,
        filtered_bytes: 0,
    })
}

fn flush_segment(segment: &mut SegmentWriters) -> Result<(), String> {
    segment.full_writer.flush().map_err(|error| {
        format!(
            "无法刷新完整滚动证据 path={} error={error}",
            segment.full_path.display()
        )
    })?;
    segment.filtered_writer.flush().map_err(|error| {
        format!(
            "无法刷新过滤滚动证据 path={} error={error}",
            segment.filtered_path.display()
        )
    })
}

fn publish_manifest(
    configuration: &RollingOutputConfiguration,
    sequence: u64,
    complete: bool,
    segments: &[SegmentRecord],
) -> Result<(), String> {
    let published_at_unix_ms = unix_time_ms()?;
    let final_path = configuration.directory.join(format!(
        "{}-manifest-{sequence:06}.json",
        configuration.session_id
    ));
    let temporary_path = configuration.directory.join(format!(
        ".{}-manifest-{sequence:06}-{}.tmp",
        configuration.session_id,
        std::process::id()
    ));
    let manifest = RollingManifest {
        schema_version: "0.1.0",
        session_id: &configuration.session_id,
        published_at_unix_ms,
        complete,
        segments,
    };
    let mut temporary_file = create_new_file(&temporary_path, "滚动证据临时 manifest")?;
    serde_json::to_writer(&mut temporary_file, &manifest)
        .map_err(|error| format!("无法序列化滚动证据 manifest error={error}"))?;
    temporary_file
        .write_all(b"\n")
        .map_err(|error| format!("无法写入滚动证据 manifest 换行符 error={error}"))?;
    temporary_file
        .sync_all()
        .map_err(|error| format!("无法同步滚动证据 manifest error={error}"))?;
    drop(temporary_file);
    fs::rename(&temporary_path, &final_path).map_err(|error| {
        format!(
            "无法原子发布滚动证据 manifest source={} destination={} error={error}",
            temporary_path.display(),
            final_path.display()
        )
    })
}

fn create_new_file(path: &Path, label: &str) -> Result<File, String> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("无法创建{label} path={} error={error}", path.display()))
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

fn file_name(path: &Path) -> Result<String, String> {
    path.file_name()
        .and_then(|value| value.to_str())
        .map(String::from)
        .ok_or_else(|| format!("滚动证据文件名不是有效 UTF-8 path={}", path.display()))
}

fn unix_time_ms() -> Result<u64, String> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("系统时间早于 UNIX_EPOCH error={error}"))?;
    u64::try_from(elapsed.as_millis())
        .map_err(|error| format!("系统时间毫秒值超出 u64 范围 error={error}"))
}
