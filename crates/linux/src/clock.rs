//! Local wall-clock time.

/// Civil local time as `(year, month, day, hour, minute, second)`, with the
/// machine's time zone and daylight saving applied by the C library.
pub fn now_local() -> (i32, u32, u32, u32, u32, u32) {
    // SAFETY: `time` with a null pointer only returns the time, and
    // `localtime_r` writes into a struct we own.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return (1970, 1, 1, 0, 0, 0);
        }
        (
            tm.tm_year + 1900,
            (tm.tm_mon + 1) as u32,
            tm.tm_mday as u32,
            tm.tm_hour as u32,
            tm.tm_min as u32,
            tm.tm_sec as u32,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_date_is_plausible() {
        let (y, mo, d, h, mi, s) = now_local();
        assert!(y >= 2024);
        assert!((1..=12).contains(&mo) && (1..=31).contains(&d));
        assert!(h < 24 && mi < 60 && s < 61);
    }
}
