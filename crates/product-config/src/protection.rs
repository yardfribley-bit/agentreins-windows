#[cfg(windows)]
use windows::Win32::Foundation::{HLOCAL, LocalFree};
#[cfg(windows)]
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
#[cfg(windows)]
use windows::core::PCWSTR;

#[cfg(windows)]
pub fn protect(plain_text: &[u8]) -> Result<Vec<u8>, String> {
    let input_length = u32::try_from(plain_text.len())
        .map_err(|error| format!("DPAPI 输入过大 bytes={} error={error}", plain_text.len()))?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: input_length,
        pbData: plain_text.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptProtectData(
            &input,
            PCWSTR::null(),
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .map_err(|error| format!("Windows DPAPI 加密失败 error={error}"))?;
    }
    copy_and_free(output, "加密")
}

#[cfg(windows)]
pub fn unprotect(cipher_text: &[u8]) -> Result<Vec<u8>, String> {
    let input_length = u32::try_from(cipher_text.len())
        .map_err(|error| format!("DPAPI 输入过大 bytes={} error={error}", cipher_text.len()))?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: input_length,
        pbData: cipher_text.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .map_err(|error| format!("Windows DPAPI 解密失败 error={error}"))?;
    }
    copy_and_free(output, "解密")
}

#[cfg(windows)]
fn copy_and_free(output: CRYPT_INTEGER_BLOB, operation: &str) -> Result<Vec<u8>, String> {
    if output.pbData.is_null() {
        return Err(format!("Windows DPAPI {operation}返回空结果"));
    }
    let bytes =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe {
        let _ = LocalFree(HLOCAL(output.pbData.cast()));
    }
    Ok(bytes)
}

#[cfg(not(windows))]
pub fn protect(_plain_text: &[u8]) -> Result<Vec<u8>, String> {
    Err(String::from("DPAPI 仅能在 Windows 产品环境使用"))
}

#[cfg(not(windows))]
pub fn unprotect(_cipher_text: &[u8]) -> Result<Vec<u8>, String> {
    Err(String::from("DPAPI 仅能在 Windows 产品环境使用"))
}
