//! Timestamp helpers for runtime and daemon metadata.

/// Produce an RFC 3339 / ISO 8601 UTC timestamp string without a date-time dependency.
#[must_use]
pub fn iso_now_rfc3339() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let time_secs = secs % 86_400;
    let hours = time_secs / 3_600;
    let minutes = (time_secs % 3_600) / 60;
    let seconds = time_secs % 60;
    let (year, month, day) = days_to_date(secs / 86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}.{:03}Z",
        now.subsec_nanos() / 1_000_000,
    )
}

/// Convert days since Unix epoch to a Gregorian calendar date.
#[must_use]
#[allow(clippy::many_single_char_names)]
const fn days_to_date(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + 3 - 12 * (mp / 10);
    (y + mp / 10, m, d)
}
