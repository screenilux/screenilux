//! Local wall-clock time.

/// Civil local time as `(year, month, day, hour, minute, second)`.
///
/// `GetLocalTime` already applies the machine's timezone and daylight saving,
/// which is the whole reason to use it rather than converting by hand. Only
/// two display formats are ever needed from this, so it is cheaper than a date
/// crate.
pub fn now_local() -> (i32, u32, u32, u32, u32, u32) {
    #[repr(C)]
    #[derive(Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }
    unsafe extern "system" {
        fn GetLocalTime(lpSystemTime: *mut SystemTime);
    }
    let mut st = SystemTime::default();
    // SAFETY: writes through a pointer to a local we own, with no other
    // preconditions.
    unsafe { GetLocalTime(&mut st) };
    (
        st.year as i32,
        st.month as u32,
        st.day as u32,
        st.hour as u32,
        st.minute as u32,
        st.second as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_a_plausible_civil_time() {
        let (y, mo, d, h, mi, s) = now_local();
        assert!((2020..2100).contains(&y), "year {y}");
        assert!((1..=12).contains(&mo), "month {mo}");
        assert!((1..=31).contains(&d), "day {d}");
        assert!(h < 24 && mi < 60 && s < 60, "{h}:{mi}:{s}");
    }

    #[test]
    fn the_clock_advances() {
        let a = now_local();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        assert_ne!(a, now_local(), "the second should have ticked");
    }
}
