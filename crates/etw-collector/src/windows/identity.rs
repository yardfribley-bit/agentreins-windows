use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::core::PWSTR;

const MAX_PATH_CHARS: usize = 32_768;

pub fn query_process_image_path(process_id: u32) -> Result<String, String> {
    let process_handle =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) }
            .map_err(|error| format!("无法打开进程 process_id={process_id} error={error}"))?;

    let mut path_buffer = vec![0u16; MAX_PATH_CHARS];
    let mut path_length = u32::try_from(path_buffer.len())
        .map_err(|error| format!("进程路径缓冲区长度无效 error={error}"))?;
    let query_result = unsafe {
        QueryFullProcessImageNameW(
            process_handle,
            PROCESS_NAME_WIN32,
            PWSTR(path_buffer.as_mut_ptr()),
            &mut path_length,
        )
    };
    let close_result = unsafe { CloseHandle(process_handle) };
    query_result
        .map_err(|error| format!("无法查询进程完整路径 process_id={process_id} error={error}"))?;
    close_result
        .map_err(|error| format!("无法关闭进程句柄 process_id={process_id} error={error}"))?;

    path_buffer.truncate(path_length as usize);
    String::from_utf16(&path_buffer)
        .map_err(|error| format!("进程路径不是有效 UTF-16 process_id={process_id} error={error}"))
}

pub fn paths_equal(left: &str, right: &str) -> bool {
    normalize_path(left) == normalize_path(right)
}

fn normalize_path(path: &str) -> String {
    path.trim_start_matches(r"\\?\")
        .replace('/', "\\")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::paths_equal;

    #[test]
    fn compares_windows_paths_case_insensitively() {
        assert!(paths_equal(
            r"C:\Program Files\WorkBuddy\WorkBuddy.exe",
            r"c:/program files/workbuddy/workbuddy.exe"
        ));
    }

    #[test]
    fn ignores_extended_path_prefix() {
        assert!(paths_equal(
            r"\\?\C:\Program Files\WorkBuddy\WorkBuddy.exe",
            r"C:\Program Files\WorkBuddy\WorkBuddy.exe"
        ));
    }
}
