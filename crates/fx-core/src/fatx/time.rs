//! FATX timestamps are plain FAT date/time words in local time. The 360 counts years from
//! 1980. The original Xbox counts from 2000, going by the docs.

use chrono::{Datelike, Local, NaiveDate, NaiveDateTime, TimeZone, Timelike};

use super::Flavor;

fn base_year(flavor: Flavor) -> i32 {
    match flavor {
        Flavor::Xbox360 => 1980,
        Flavor::Xbox => 2000,
    }
}

/// The date in the high word and the time in the low word.
pub fn encode(local: NaiveDateTime, flavor: Flavor) -> u32 {
    let base = base_year(flavor);
    let year = local.year().clamp(base, base + 127);
    let date = (((year - base) as u32) << 9) | (local.month() << 5) | local.day();
    let time = (local.hour() << 11) | (local.minute() << 5) | (local.second() / 2);
    (date << 16) | time
}

pub fn decode(raw: u32, flavor: Flavor) -> NaiveDateTime {
    let date = raw >> 16;
    let time = raw & 0xFFFF;
    let year = base_year(flavor) + (date >> 9) as i32;
    let month = ((date >> 5) & 0xF).clamp(1, 12);
    let day = (date & 0x1F).clamp(1, days_in_month(year, month));
    let hour = (time >> 11).min(23);
    let minute = ((time >> 5) & 0x3F).min(59);
    let second = ((time & 0x1F) * 2).min(59);
    NaiveDate::from_ymd_opt(year, month, day)
        .and_then(|d| d.and_hms_opt(hour, minute, second))
        .unwrap_or_default()
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let (next_year, next_month) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    NaiveDate::from_ymd_opt(next_year, next_month, 1)
        .and_then(|first| first.pred_opt())
        .map_or(28, |last| last.day())
}

pub fn now(flavor: Flavor) -> u32 {
    encode(Local::now().naive_local(), flavor)
}

/// The local time on the drive as a UTC time. A time that doesn't exist where the clocks
/// jumped forward is taken as UTC.
pub fn to_utc(local: NaiveDateTime) -> NaiveDateTime {
    match Local.from_local_datetime(&local).earliest() {
        Some(time) => time.naive_utc(),
        None => local,
    }
}

pub fn from_utc(utc: NaiveDateTime) -> NaiveDateTime {
    Local.from_utc_datetime(&utc).naive_local()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, s)
            .unwrap()
    }

    #[test]
    fn a_time_survives_a_round_trip() {
        for flavor in [Flavor::Xbox360, Flavor::Xbox] {
            let t = at(2024, 5, 17, 13, 45, 30);
            assert_eq!(decode(encode(t, flavor), flavor), t);
        }
    }

    #[test]
    fn seconds_are_kept_to_two() {
        let t = at(2024, 5, 17, 13, 45, 31);
        assert_eq!(
            decode(encode(t, Flavor::Xbox360), Flavor::Xbox360),
            at(2024, 5, 17, 13, 45, 30)
        );
    }

    #[test]
    fn the_console_default_clock_date_decodes() {
        // 2005-11-22 12:02:00 as the console writes it
        let raw = (((2005 - 1980) << 9 | 11 << 5 | 22) << 16) | (12 << 11 | 2 << 5);
        assert_eq!(decode(raw, Flavor::Xbox360), at(2005, 11, 22, 12, 2, 0));
    }

    #[test]
    fn the_two_flavors_count_from_different_years() {
        let t = at(2010, 1, 1, 0, 0, 0);
        assert_ne!(encode(t, Flavor::Xbox360), encode(t, Flavor::Xbox));
    }

    #[test]
    fn nonsense_words_still_give_a_date() {
        assert_eq!(decode(0, Flavor::Xbox360), at(1980, 1, 1, 0, 0, 0));
        let wild = decode(0xFFFF_FFFF, Flavor::Xbox360);
        assert_eq!((wild.month(), wild.hour()), (12, 23));
    }

    #[test]
    fn years_before_the_epoch_are_held_at_it() {
        let encoded = encode(at(1970, 6, 1, 0, 0, 0), Flavor::Xbox360);
        assert_eq!(decode(encoded, Flavor::Xbox360).year(), 1980);
    }
}
