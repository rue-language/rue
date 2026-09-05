//! The one spelling of an instant in the measurement contract.
//!
//! Two identical measurements formatting the same instant differently would
//! produce two content addresses, so every producer — the compiler's benchmark
//! report, the runner's run identities — formats through here. Computed from
//! the Unix epoch rather than pulling in a date library for six fields.

use std::time::{SystemTime, UNIX_EPOCH};

/// The current instant as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn utc_timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    let (hour, minute, second) = (rest / 3600, (rest % 3600) / 60, rest % 60);
    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Howard Hinnant's `civil_from_days`, shifting the era to start in March so
/// the leap day lands at the end of a cycle.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_epoch_is_the_first_of_january_1970() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn a_leap_day_is_placed_correctly() {
        // 2000-02-29 is 11,016 days after the epoch; 1900 was not a leap year
        // and 2000 was, which is the case a naive rule gets wrong.
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(11_017), (2000, 3, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
    }

    #[test]
    fn the_timestamp_has_the_published_shape() {
        let stamp = utc_timestamp();
        assert_eq!(stamp.len(), 20, "{stamp}");
        assert_eq!(stamp.as_bytes()[10], b'T', "{stamp}");
        assert!(stamp.ends_with('Z'), "{stamp}");
        // Sub-second precision would make two runs of the same instant hash
        // differently, so it must not appear.
        assert!(!stamp.contains('.'), "{stamp}");
    }
}
