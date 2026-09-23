const WINDOWS_TO_UNIX_EPOCH_100NS: i64 = 116_444_736_000_000_000;
const HUNDRED_NS_PER_MILLISECOND: i64 = 10_000;

pub fn filetime_to_unix_ms(raw_timestamp: i64) -> Result<u64, String> {
    let unix_100ns = raw_timestamp
        .checked_sub(WINDOWS_TO_UNIX_EPOCH_100NS)
        .ok_or_else(|| format!("ETW 时间戳减法溢出 raw_timestamp={raw_timestamp}"))?;
    if unix_100ns < 0 {
        return Err(format!(
            "ETW 时间戳早于 UNIX_EPOCH raw_timestamp={raw_timestamp}"
        ));
    }
    u64::try_from(unix_100ns / HUNDRED_NS_PER_MILLISECOND)
        .map_err(|error| format!("ETW 时间戳无法转换为 u64 error={error}"))
}

#[cfg(test)]
mod tests {
    use super::filetime_to_unix_ms;

    #[test]
    fn converts_windows_epoch_to_unix_epoch() {
        assert_eq!(filetime_to_unix_ms(116_444_736_000_000_000), Ok(0));
    }

    #[test]
    fn rejects_timestamp_before_unix_epoch() {
        assert!(filetime_to_unix_ms(0).is_err());
    }
}
