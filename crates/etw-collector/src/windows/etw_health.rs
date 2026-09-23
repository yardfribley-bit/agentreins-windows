use std::mem::size_of;

use serde::Serialize;
use windows::Win32::System::Diagnostics::Etw::{
    CONTROLTRACE_HANDLE, ControlTraceW, EVENT_TRACE_CONTROL, EVENT_TRACE_CONTROL_QUERY,
    EVENT_TRACE_CONTROL_STOP, EVENT_TRACE_PROPERTIES,
};
use windows::core::PCWSTR;

const MAX_SESSION_NAME_CHARS: usize = 1_024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EtwSessionHealth {
    pub number_of_buffers: u32,
    pub free_buffers: u32,
    pub events_lost: u32,
    pub buffers_written: u32,
    pub log_buffers_lost: u32,
    pub realtime_buffers_lost: u32,
}

#[repr(C)]
struct TracePropertiesBuffer {
    properties: EVENT_TRACE_PROPERTIES,
    session_name: [u16; MAX_SESSION_NAME_CHARS + 1],
}

pub fn stop_trace_and_query_health(trace_name: &str) -> Result<EtwSessionHealth, String> {
    control_trace(trace_name, EVENT_TRACE_CONTROL_STOP, "停止")
}

pub fn query_trace_health(trace_name: &str) -> Result<EtwSessionHealth, String> {
    control_trace(trace_name, EVENT_TRACE_CONTROL_QUERY, "查询")
}

fn control_trace(
    trace_name: &str,
    control_code: EVENT_TRACE_CONTROL,
    action_label: &str,
) -> Result<EtwSessionHealth, String> {
    let mut trace_name_wide: Vec<u16> = trace_name.encode_utf16().collect();
    trace_name_wide.push(0);
    let mut properties_buffer = TracePropertiesBuffer {
        properties: EVENT_TRACE_PROPERTIES::default(),
        session_name: [0; MAX_SESSION_NAME_CHARS + 1],
    };
    properties_buffer.properties.Wnode.BufferSize =
        u32::try_from(size_of::<TracePropertiesBuffer>())
            .map_err(|error| format!("ETW 属性缓冲区长度无效 error={error}"))?;
    properties_buffer.properties.LoggerNameOffset =
        u32::try_from(size_of::<EVENT_TRACE_PROPERTIES>())
            .map_err(|error| format!("ETW 会话名称偏移无效 error={error}"))?;
    properties_buffer.properties.LogFileNameOffset = 0;

    unsafe {
        ControlTraceW(
            CONTROLTRACE_HANDLE { Value: 0 },
            PCWSTR(trace_name_wide.as_ptr()),
            &mut properties_buffer.properties,
            control_code,
        )
    }
    .ok()
    .map_err(|error| {
        format!("无法{action_label} ETW 会话统计 trace_name={trace_name} error={error}")
    })?;

    let properties = properties_buffer.properties;
    Ok(EtwSessionHealth {
        number_of_buffers: properties.NumberOfBuffers,
        free_buffers: properties.FreeBuffers,
        events_lost: properties.EventsLost,
        buffers_written: properties.BuffersWritten,
        log_buffers_lost: properties.LogBuffersLost,
        realtime_buffers_lost: properties.RealTimeBuffersLost,
    })
}
