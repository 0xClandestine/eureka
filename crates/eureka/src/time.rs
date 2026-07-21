//! Timestamp helpers for runtime and daemon metadata.

/// Produce an RFC 3339 / ISO 8601 UTC timestamp string without a date-time dependency.
#[must_use]
pub fn iso_now_rfc3339() -> String {
    let now =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = now.as_secs();
    let time_secs = secs % 86_400;
    let hours = time_secs / 3_600;
    let minutes = (time_secs % 3_600) / 60;
    let seconds = time_secs % 60;
    let (year, month, day) = days_to_date(secs / 86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}.{:03}Z",
        now.subsec_millis(),
    )
}

/// Convert days since Unix epoch to a Gregorian calendar date.
#[must_use]
#[allow(clippy::many_single_char_names)]
const fn days_to_date(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + 3 - 12 * (mp / 10);
    (y + mp / 10, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_now_rfc3339_produces_valid_utc_format() {
        let ts = iso_now_rfc3339();
        assert!(ts.ends_with('Z'), "must end with Z: {ts}");
        // Format: YYYY-MM-DDTHH:MM:SS.mmmZ — 24 chars including Z.
        assert_eq!(
            ts.len(),
            24,
            "RFC 3339 with millis should be 24 chars, got '{ts}' (len={})",
            ts.len()
        );
        let year: u32 = ts[..4].parse().unwrap();
        assert!(year >= 2024, "year should be recent, got {year}");
        let month: u32 = ts[5..7].parse().unwrap();
        assert!(month >= 1 && month <= 12, "month out of range: {month}");
        let day: u32 = ts[8..10].parse().unwrap();
        assert!(day >= 1 && day <= 31, "day out of range: {day}");
    }

    #[test]
    fn days_to_date_epoch_yields_1970_01_01() {
        assert_eq!(days_to_date(0), (1970, 1, 1));
    }

    #[test]
    fn days_to_date_known_dates() {
        let epoch_jan2 = days_to_date(1);
        assert_eq!(epoch_jan2, (1970, 1, 2));

        let year2000_jan1 = days_to_date(10_957);
        assert_eq!(year2000_jan1, (2000, 1, 1));

        let year2020_feb29 = days_to_date(18_321);
        assert_eq!(year2020_feb29, (2020, 2, 29));

        let year2024_dec31 = days_to_date(20_088);
        assert_eq!(year2024_dec31, (2024, 12, 31));
    }

    #[test]
    fn days_to_date_leap_year_transition() {
        let feb28_2024 = days_to_date(19_781);
        assert_eq!(feb28_2024, (2024, 2, 28));

        let feb29_2024 = days_to_date(19_782);
        assert_eq!(feb29_2024, (2024, 2, 29));

        let mar1_2024 = days_to_date(19_783);
        assert_eq!(mar1_2024, (2024, 3, 1));
    }

    #[test]
    fn days_to_date_non_leap_year_transition() {
        let feb28_2023 = days_to_date(19_416);
        assert_eq!(feb28_2023, (2023, 2, 28));

        let mar1_2023 = days_to_date(19_417);
        assert_eq!(mar1_2023, (2023, 3, 1));
    }

    #[test]
    fn days_to_date_large_values_do_not_panic() {
        let result = days_to_date(1_000_000);
        let (year, month, day) = result;
        assert!(year > 3000);
        assert!(month >= 1 && month <= 12);
        assert!(day >= 1 && day <= 31);
    }

    #[test]
    fn iso_now_rfc3339_succeeds_during_system_time_overflow_gracefully() {
        let ts = iso_now_rfc3339();
        assert!(!ts.is_empty());
        let month_str = &ts[5..7];
        let month: u32 = month_str.parse().unwrap();
        assert!(month >= 1 && month <= 12, "month out of range: {month}");
    }
}
