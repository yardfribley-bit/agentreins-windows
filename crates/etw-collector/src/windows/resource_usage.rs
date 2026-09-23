use std::collections::HashMap;
use std::mem::{offset_of, size_of};
use std::time::{Duration, Instant};

use native_contracts::ProcessRef;
use serde::Serialize;
use windows::Win32::Foundation::{BOOL, CloseHandle, FILETIME, HANDLE};
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, LUID_AND_ATTRIBUTES,
    LookupPrivilegeNameW, SE_PRIVILEGE_ENABLED, SE_PRIVILEGE_ENABLED_BY_DEFAULT,
    SE_PRIVILEGE_REMOVED, SE_PRIVILEGE_USED_FOR_ACCESS, TOKEN_ELEVATION, TOKEN_MANDATORY_LABEL,
    TOKEN_PRIVILEGES, TOKEN_QUERY, TokenElevation, TokenIntegrityLevel, TokenIsAppContainer,
    TokenPrivileges,
};
use windows::Win32::System::JobObjects::IsProcessInJob;
use windows::Win32::System::ProcessStatus::{
    K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::SystemServices::{
    SECURITY_MANDATORY_HIGH_RID, SECURITY_MANDATORY_LOW_RID, SECURITY_MANDATORY_MEDIUM_PLUS_RID,
    SECURITY_MANDATORY_MEDIUM_RID, SECURITY_MANDATORY_PROTECTED_PROCESS_RID,
    SECURITY_MANDATORY_SYSTEM_RID,
};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::core::{PCWSTR, PWSTR};

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct AgentResourceSample {
    pub process_count: u64,
    pub unavailable_process_count: u64,
    pub working_set_bytes: u64,
    pub private_memory_bytes: u64,
    pub cpu_percent_normalized: Option<f64>,
    pub sample_interval_ms: Option<u64>,
    pub logical_processor_count: u32,
    pub job_process_count: u64,
    pub app_container_process_count: u64,
    pub elevated_process_count: u64,
    pub permission_processes: Vec<ProcessPermissionSample>,
    pub failures: Vec<ProcessSampleFailure>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ProcessPermissionSample {
    pub process_id: u32,
    pub process_instance_id: String,
    pub is_elevated: bool,
    pub integrity_level: TokenIntegrityLevel,
    pub integrity_rid: u32,
    pub privileges: Vec<TokenPrivilegeSample>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenIntegrityLevel {
    Untrusted,
    Low,
    Medium,
    MediumPlus,
    High,
    System,
    ProtectedProcess,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct TokenPrivilegeSample {
    pub name: String,
    pub enabled: bool,
    pub enabled_by_default: bool,
    pub removed: bool,
    pub used_for_access: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ProcessSampleFailure {
    pub process_id: u32,
    pub process_instance_id: String,
    pub error: String,
}

struct ProcessResourceCounters {
    working_set_bytes: u64,
    private_memory_bytes: u64,
    cpu_time_100ns: u64,
    in_job: bool,
    is_app_container: bool,
    is_elevated: bool,
    integrity_level: TokenIntegrityLevel,
    integrity_rid: u32,
    privileges: Vec<TokenPrivilegeSample>,
}

pub struct AgentResourceSampler {
    previous_cpu_by_instance: HashMap<String, u64>,
    previous_sample_at: Option<Instant>,
    logical_processor_count: u32,
}

impl AgentResourceSampler {
    pub fn new() -> Result<Self, String> {
        let logical_processor_count = u32::try_from(
            std::thread::available_parallelism()
                .map_err(|error| format!("无法读取逻辑处理器数量 error={error}"))?
                .get(),
        )
        .map_err(|error| format!("逻辑处理器数量超出 u32 error={error}"))?;
        Ok(Self {
            previous_cpu_by_instance: HashMap::new(),
            previous_sample_at: None,
            logical_processor_count,
        })
    }

    pub fn sample(&mut self, processes: &[ProcessRef]) -> Result<AgentResourceSample, String> {
        let sampled_at = Instant::now();
        let mut current_cpu_by_instance = HashMap::new();
        let mut failures = Vec::new();
        let mut working_set_bytes = 0_u64;
        let mut private_memory_bytes = 0_u64;
        let mut cpu_delta_100ns = 0_u64;
        let mut process_count = 0_u64;
        let mut job_process_count = 0_u64;
        let mut app_container_process_count = 0_u64;
        let mut elevated_process_count = 0_u64;
        let mut permission_processes = Vec::new();

        for process in processes {
            match query_process_resources(process.pid) {
                Ok(counters) => {
                    process_count = process_count
                        .checked_add(1)
                        .ok_or_else(|| String::from("Agent 资源采样进程数量超出范围"))?;
                    working_set_bytes =
                        working_set_bytes
                            .checked_add(counters.working_set_bytes)
                            .ok_or_else(|| String::from("Agent 工作集字节数超出范围"))?;
                    private_memory_bytes = private_memory_bytes
                        .checked_add(counters.private_memory_bytes)
                        .ok_or_else(|| String::from("Agent 私有内存字节数超出范围"))?;
                    if let Some(previous) = self
                        .previous_cpu_by_instance
                        .get(&process.process_instance_id)
                    {
                        cpu_delta_100ns = cpu_delta_100ns
                            .checked_add(counters.cpu_time_100ns.saturating_sub(*previous))
                            .ok_or_else(|| String::from("Agent CPU 时间增量超出范围"))?;
                    }
                    current_cpu_by_instance
                        .insert(process.process_instance_id.clone(), counters.cpu_time_100ns);
                    job_process_count =
                        job_process_count.saturating_add(u64::from(counters.in_job));
                    app_container_process_count = app_container_process_count
                        .saturating_add(u64::from(counters.is_app_container));
                    elevated_process_count =
                        elevated_process_count.saturating_add(u64::from(counters.is_elevated));
                    permission_processes.push(ProcessPermissionSample {
                        process_id: process.pid,
                        process_instance_id: process.process_instance_id.clone(),
                        is_elevated: counters.is_elevated,
                        integrity_level: counters.integrity_level,
                        integrity_rid: counters.integrity_rid,
                        privileges: counters.privileges,
                    });
                }
                Err(error) => failures.push(ProcessSampleFailure {
                    process_id: process.pid,
                    process_instance_id: process.process_instance_id.clone(),
                    error,
                }),
            }
        }

        let sample_interval = self
            .previous_sample_at
            .map(|previous| sampled_at.saturating_duration_since(previous));
        let cpu_percent_normalized = sample_interval.map(|interval| {
            normalized_cpu_percent(cpu_delta_100ns, interval, self.logical_processor_count)
        });
        self.previous_cpu_by_instance = current_cpu_by_instance;
        self.previous_sample_at = Some(sampled_at);
        permission_processes.sort_by(|left, right| {
            left.process_id
                .cmp(&right.process_id)
                .then_with(|| left.process_instance_id.cmp(&right.process_instance_id))
        });

        Ok(AgentResourceSample {
            process_count,
            unavailable_process_count: u64::try_from(failures.len())
                .map_err(|error| format!("Agent 资源采样失败数量超出范围 error={error}"))?,
            working_set_bytes,
            private_memory_bytes,
            cpu_percent_normalized,
            sample_interval_ms: sample_interval
                .map(|interval| u64::try_from(interval.as_millis()))
                .transpose()
                .map_err(|error| format!("Agent 资源采样间隔超出范围 error={error}"))?,
            logical_processor_count: self.logical_processor_count,
            job_process_count,
            app_container_process_count,
            elevated_process_count,
            permission_processes,
            failures,
        })
    }
}

fn query_process_resources(process_id: u32) -> Result<ProcessResourceCounters, String> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) }
        .map_err(|error| {
            format!("无法打开目标 Agent 进程 process_id={process_id} error={error}")
        })?;
    let query_result = (|| {
        let mut memory = PROCESS_MEMORY_COUNTERS_EX {
            cb: u32::try_from(size_of::<PROCESS_MEMORY_COUNTERS_EX>())
                .map_err(|error| format!("进程内存结构长度超出范围 error={error}"))?,
            ..PROCESS_MEMORY_COUNTERS_EX::default()
        };
        let memory_result = unsafe {
            K32GetProcessMemoryInfo(
                handle,
                (&mut memory as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(),
                memory.cb,
            )
        };
        if !memory_result.as_bool() {
            return Err(format!(
                "无法查询目标 Agent 进程内存 process_id={process_id} error={}",
                windows::core::Error::from_win32()
            ));
        }

        let mut creation_time = FILETIME::default();
        let mut exit_time = FILETIME::default();
        let mut kernel_time = FILETIME::default();
        let mut user_time = FILETIME::default();
        unsafe {
            GetProcessTimes(
                handle,
                &mut creation_time,
                &mut exit_time,
                &mut kernel_time,
                &mut user_time,
            )
        }
        .map_err(|error| {
            format!("无法查询目标 Agent 进程 CPU 时间 process_id={process_id} error={error}")
        })?;
        let mut in_job = BOOL::default();
        unsafe { IsProcessInJob(handle, HANDLE::default(), &mut in_job) }.map_err(|error| {
            format!("无法查询目标 Agent Job 状态 process_id={process_id} error={error}")
        })?;
        let token_security = query_token_security(handle, process_id)?;

        Ok(ProcessResourceCounters {
            working_set_bytes: u64::try_from(memory.WorkingSetSize)
                .map_err(|error| format!("目标 Agent 工作集超出范围 error={error}"))?,
            private_memory_bytes: u64::try_from(memory.PrivateUsage)
                .map_err(|error| format!("目标 Agent 私有内存超出范围 error={error}"))?,
            cpu_time_100ns: filetime_ticks(kernel_time)
                .checked_add(filetime_ticks(user_time))
                .ok_or_else(|| String::from("目标 Agent CPU 时间超出范围"))?,
            in_job: in_job.as_bool(),
            is_app_container: token_security.is_app_container,
            is_elevated: token_security.is_elevated,
            integrity_level: token_security.integrity_level,
            integrity_rid: token_security.integrity_rid,
            privileges: token_security.privileges,
        })
    })();
    let close_result = unsafe { CloseHandle(handle) };
    if let Err(error) = close_result {
        return Err(format!(
            "无法关闭目标 Agent 进程句柄 process_id={process_id} error={error}"
        ));
    }
    query_result
}

struct TokenSecuritySample {
    is_app_container: bool,
    is_elevated: bool,
    integrity_level: TokenIntegrityLevel,
    integrity_rid: u32,
    privileges: Vec<TokenPrivilegeSample>,
}

fn query_token_security(
    process_handle: HANDLE,
    process_id: u32,
) -> Result<TokenSecuritySample, String> {
    let mut token_handle = HANDLE::default();
    unsafe { OpenProcessToken(process_handle, TOKEN_QUERY, &mut token_handle) }.map_err(
        |error| format!("无法打开目标 Agent 进程 Token process_id={process_id} error={error}"),
    )?;
    let query_result = (|| {
        let mut app_container = 0_u32;
        let mut returned = 0_u32;
        unsafe {
            GetTokenInformation(
                token_handle,
                TokenIsAppContainer,
                Some((&mut app_container as *mut u32).cast()),
                u32::try_from(size_of::<u32>())
                    .map_err(|error| format!("AppContainer 结构长度超出范围 error={error}"))?,
                &mut returned,
            )
        }
        .map_err(|error| {
            format!("无法查询目标 Agent AppContainer 状态 process_id={process_id} error={error}")
        })?;

        let mut elevation = TOKEN_ELEVATION::default();
        unsafe {
            GetTokenInformation(
                token_handle,
                TokenElevation,
                Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
                u32::try_from(size_of::<TOKEN_ELEVATION>())
                    .map_err(|error| format!("Token elevation 结构长度超出范围 error={error}"))?,
                &mut returned,
            )
        }
        .map_err(|error| {
            format!("无法查询目标 Agent Token elevation process_id={process_id} error={error}")
        })?;
        let integrity_rid = query_integrity_rid(token_handle, process_id)?;
        let privileges = query_token_privileges(token_handle, process_id)?;
        Ok(TokenSecuritySample {
            is_app_container: app_container != 0,
            is_elevated: elevation.TokenIsElevated != 0,
            integrity_level: integrity_level(integrity_rid),
            integrity_rid,
            privileges,
        })
    })();
    let close_result = unsafe { CloseHandle(token_handle) };
    if let Err(error) = close_result {
        return Err(format!(
            "无法关闭目标 Agent Token 句柄 process_id={process_id} error={error}"
        ));
    }
    query_result
}

fn query_integrity_rid(token_handle: HANDLE, process_id: u32) -> Result<u32, String> {
    let buffer = query_token_information_buffer(
        token_handle,
        TokenIntegrityLevel,
        process_id,
        "完整性级别",
    )?;
    if buffer.len() < size_of::<TOKEN_MANDATORY_LABEL>() {
        return Err(format!(
            "目标 Agent Token 完整性级别结构长度不足 process_id={process_id} actual={} required={}",
            buffer.len(),
            size_of::<TOKEN_MANDATORY_LABEL>()
        ));
    }
    let label =
        unsafe { std::ptr::read_unaligned(buffer.as_ptr().cast::<TOKEN_MANDATORY_LABEL>()) };
    if label.Label.Sid.0.is_null() {
        return Err(format!(
            "目标 Agent Token 完整性级别 SID 为空 process_id={process_id}"
        ));
    }
    let sub_authority_count = unsafe { GetSidSubAuthorityCount(label.Label.Sid) };
    if sub_authority_count.is_null() || unsafe { *sub_authority_count } == 0 {
        return Err(format!(
            "目标 Agent Token 完整性级别 SID 缺少子权限 process_id={process_id}"
        ));
    }
    let last_index = u32::from(unsafe { *sub_authority_count }) - 1;
    let rid = unsafe { GetSidSubAuthority(label.Label.Sid, last_index) };
    if rid.is_null() {
        return Err(format!(
            "无法读取目标 Agent Token 完整性级别 RID process_id={process_id}"
        ));
    }
    Ok(unsafe { *rid })
}

fn query_token_privileges(
    token_handle: HANDLE,
    process_id: u32,
) -> Result<Vec<TokenPrivilegeSample>, String> {
    let buffer =
        query_token_information_buffer(token_handle, TokenPrivileges, process_id, "特权清单")?;
    if buffer.len() < size_of::<u32>() {
        return Err(format!(
            "目标 Agent Token 特权结构长度不足 process_id={process_id} actual={} required={}",
            buffer.len(),
            size_of::<u32>()
        ));
    }
    let privilege_count = unsafe { std::ptr::read_unaligned(buffer.as_ptr().cast::<u32>()) };
    let entries_offset = offset_of!(TOKEN_PRIVILEGES, Privileges);
    let entries_length = usize::try_from(privilege_count)
        .map_err(|error| format!("目标 Agent Token 特权数量超出范围 error={error}"))?
        .checked_mul(size_of::<LUID_AND_ATTRIBUTES>())
        .ok_or_else(|| String::from("目标 Agent Token 特权结构长度超出范围"))?;
    let required_length = entries_offset
        .checked_add(entries_length)
        .ok_or_else(|| String::from("目标 Agent Token 特权缓冲区长度超出范围"))?;
    if buffer.len() < required_length {
        return Err(format!(
            "目标 Agent Token 特权缓冲区长度不足 process_id={process_id} actual={} required={required_length}",
            buffer.len()
        ));
    }
    let mut privileges = Vec::with_capacity(
        usize::try_from(privilege_count)
            .map_err(|error| format!("目标 Agent Token 特权数量超出范围 error={error}"))?,
    );
    for index in 0..privilege_count {
        let offset = entries_offset
            .checked_add(
                usize::try_from(index)
                    .map_err(|error| format!("目标 Agent Token 特权索引超出范围 error={error}"))?
                    .checked_mul(size_of::<LUID_AND_ATTRIBUTES>())
                    .ok_or_else(|| String::from("目标 Agent Token 特权条目偏移超出范围"))?,
            )
            .ok_or_else(|| String::from("目标 Agent Token 特权条目位置超出范围"))?;
        let entry = unsafe {
            std::ptr::read_unaligned(buffer.as_ptr().add(offset).cast::<LUID_AND_ATTRIBUTES>())
        };
        privileges.push(TokenPrivilegeSample {
            name: privilege_name(&entry, process_id)?,
            enabled: entry.Attributes.contains(SE_PRIVILEGE_ENABLED),
            enabled_by_default: entry.Attributes.contains(SE_PRIVILEGE_ENABLED_BY_DEFAULT),
            removed: entry.Attributes.contains(SE_PRIVILEGE_REMOVED),
            used_for_access: entry.Attributes.contains(SE_PRIVILEGE_USED_FOR_ACCESS),
        });
    }
    privileges.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(privileges)
}

fn query_token_information_buffer(
    token_handle: HANDLE,
    information_class: windows::Win32::Security::TOKEN_INFORMATION_CLASS,
    process_id: u32,
    field_name: &str,
) -> Result<Vec<u8>, String> {
    let mut required_length = 0_u32;
    let first_error = match unsafe {
        GetTokenInformation(
            token_handle,
            information_class,
            None,
            0,
            &mut required_length,
        )
    } {
        Ok(()) => {
            return Err(format!(
                "目标 Agent Token {field_name}长度探测未返回缓冲区需求 process_id={process_id}"
            ));
        }
        Err(error) => error,
    };
    if required_length == 0 {
        return Err(format!(
            "无法探测目标 Agent Token {field_name}长度 process_id={process_id} error={first_error}"
        ));
    }
    let mut buffer = vec![
        0_u8;
        usize::try_from(required_length).map_err(|error| format!(
            "目标 Agent Token {field_name}长度超出范围 error={error}"
        ))?
    ];
    let mut returned_length = 0_u32;
    unsafe {
        GetTokenInformation(
            token_handle,
            information_class,
            Some(buffer.as_mut_ptr().cast()),
            required_length,
            &mut returned_length,
        )
    }
    .map_err(|error| {
        format!(
            "无法查询目标 Agent Token {field_name} process_id={process_id} required_length={required_length} error={error}"
        )
    })?;
    if returned_length > required_length {
        return Err(format!(
            "目标 Agent Token {field_name}返回长度超出缓冲区 process_id={process_id} returned={returned_length} allocated={required_length}"
        ));
    }
    buffer.truncate(
        usize::try_from(returned_length).map_err(|error| {
            format!("目标 Agent Token {field_name}返回长度超出范围 error={error}")
        })?,
    );
    Ok(buffer)
}

fn privilege_name(entry: &LUID_AND_ATTRIBUTES, process_id: u32) -> Result<String, String> {
    let mut required_length = 0_u32;
    let first_error = match unsafe {
        LookupPrivilegeNameW(
            PCWSTR::null(),
            &entry.Luid,
            PWSTR::null(),
            &mut required_length,
        )
    } {
        Ok(()) => {
            return Err(format!(
                "目标 Agent Token 特权名称长度探测未返回缓冲区需求 process_id={process_id}"
            ));
        }
        Err(error) => error,
    };
    if required_length == 0 {
        return Err(format!(
            "无法探测目标 Agent Token 特权名称长度 process_id={process_id} luid_high={} luid_low={} error={first_error}",
            entry.Luid.HighPart, entry.Luid.LowPart
        ));
    }
    let mut buffer = vec![
        0_u16;
        usize::try_from(required_length).map_err(|error| format!(
            "目标 Agent Token 特权名称长度超出范围 error={error}"
        ))?
    ];
    let mut returned_length = required_length;
    unsafe {
        LookupPrivilegeNameW(
            PCWSTR::null(),
            &entry.Luid,
            PWSTR(buffer.as_mut_ptr()),
            &mut returned_length,
        )
    }
    .map_err(|error| {
        format!(
            "无法查询目标 Agent Token 特权名称 process_id={process_id} luid_high={} luid_low={} error={error}",
            entry.Luid.HighPart, entry.Luid.LowPart
        )
    })?;
    String::from_utf16(
        &buffer[..usize::try_from(returned_length)
            .map_err(|error| format!("目标 Agent Token 特权名称返回长度超出范围 error={error}"))?],
    )
    .map_err(|error| {
        format!("目标 Agent Token 特权名称不是有效 UTF-16 process_id={process_id} error={error}")
    })
}

fn integrity_level(rid: u32) -> TokenIntegrityLevel {
    if rid < SECURITY_MANDATORY_LOW_RID as u32 {
        TokenIntegrityLevel::Untrusted
    } else if rid < SECURITY_MANDATORY_MEDIUM_RID as u32 {
        TokenIntegrityLevel::Low
    } else if rid < SECURITY_MANDATORY_MEDIUM_PLUS_RID {
        TokenIntegrityLevel::Medium
    } else if rid < SECURITY_MANDATORY_HIGH_RID as u32 {
        TokenIntegrityLevel::MediumPlus
    } else if rid < SECURITY_MANDATORY_SYSTEM_RID as u32 {
        TokenIntegrityLevel::High
    } else if rid < SECURITY_MANDATORY_PROTECTED_PROCESS_RID as u32 {
        TokenIntegrityLevel::System
    } else {
        TokenIntegrityLevel::ProtectedProcess
    }
}

fn filetime_ticks(value: FILETIME) -> u64 {
    (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime)
}

fn normalized_cpu_percent(
    cpu_delta_100ns: u64,
    interval: Duration,
    logical_processor_count: u32,
) -> f64 {
    let elapsed_100ns = interval.as_secs_f64() * 10_000_000.0;
    if elapsed_100ns == 0.0 || logical_processor_count == 0 {
        return 0.0;
    }
    (cpu_delta_100ns as f64 / elapsed_100ns / f64::from(logical_processor_count) * 100.0).max(0.0)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{normalized_cpu_percent, query_process_resources};

    #[test]
    fn normalizes_cpu_time_by_interval_and_logical_processors() {
        let percent = normalized_cpu_percent(10_000_000, Duration::from_secs(1), 4);

        assert!((percent - 25.0).abs() < f64::EPSILON);
    }

    #[test]
    fn queries_current_process_memory_and_cpu() -> Result<(), String> {
        let counters = query_process_resources(std::process::id())?;

        assert!(counters.working_set_bytes > 0);
        assert!(counters.private_memory_bytes > 0);
        let _ = counters.in_job;
        let _ = counters.is_app_container;
        let _ = counters.is_elevated;
        assert!(counters.integrity_rid > 0);
        assert!(!counters.privileges.is_empty());
        Ok(())
    }
}
