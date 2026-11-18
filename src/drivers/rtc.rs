//! MC146818 uyumlu CMOS gerçek zamanlı saat.

use core::fmt::{self, Write};
use core::sync::atomic::{AtomicU8, Ordering};

use crate::arch::cpu::{self, inb, outb};
use crate::kernel::initcall::{Detail, self_test as check};

const INDEX: u16 = 0x70;
const DATA: u16 = 0x71;

const REG_SECONDS: u8 = 0x00;
const REG_MINUTES: u8 = 0x02;
const REG_HOURS: u8 = 0x04;
const REG_DAY: u8 = 0x07;
const REG_MONTH: u8 = 0x08;
const REG_YEAR: u8 = 0x09;
const REG_STATUS_A: u8 = 0x0a;
const REG_STATUS_B: u8 = 0x0b;

const STATUS_A_UPDATING: u8 = 1 << 7;
const STATUS_B_24H: u8 = 1 << 1;
const STATUS_B_BINARY: u8 = 1 << 2;

static CENTURY_REGISTER: AtomicU8 = AtomicU8::new(0);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl DateTime {
    pub fn unix_timestamp(&self) -> i64 {
        let days = days_from_civil(self.year as i64, self.month as i64, self.day as i64);
        days * 86_400 + self.hour as i64 * 3600 + self.minute as i64 * 60 + self.second as i64
    }

    fn is_valid(&self) -> bool {
        (1..=12).contains(&self.month)
            && (1..=31).contains(&self.day)
            && self.hour < 24
            && self.minute < 60
            && self.second < 60
    }
}

impl fmt::Display for DateTime {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// Yerel saat dilimi; varsayılan Türkiye (UTC+3, yaz saati yok). Ayarlardan değişir.
static LOCAL_OFFSET_MINUTES: core::sync::atomic::AtomicI64 =
    core::sync::atomic::AtomicI64::new(180);

pub fn local_offset_minutes() -> i64 {
    LOCAL_OFFSET_MINUTES.load(core::sync::atomic::Ordering::Relaxed)
}

pub fn set_local_offset_minutes(minutes: i64) {
    LOCAL_OFFSET_MINUTES.store(
        minutes.clamp(-12 * 60, 14 * 60),
        core::sync::atomic::Ordering::Relaxed,
    );
}

/// Saat diliminin kısa adı: "TRT" ya da "UTC+05:30".
pub fn local_zone() -> alloc::string::String {
    let m = local_offset_minutes();
    if m == 180 {
        return alloc::string::String::from("TRT");
    }
    if m == 0 {
        return alloc::string::String::from("UTC");
    }
    alloc::format!(
        "UTC{}{:02}:{:02}",
        if m < 0 { '-' } else { '+' },
        m.abs() / 60,
        m.abs() % 60
    )
}

impl DateTime {
    pub fn from_unix(timestamp: i64) -> Self {
        let days = timestamp.div_euclid(86_400);
        let secs = timestamp.rem_euclid(86_400);
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
        Self {
            year: year as u16,
            month: month as u8,
            day: day as u8,
            hour: (secs / 3600) as u8,
            minute: ((secs / 60) % 60) as u8,
            second: (secs % 60) as u8,
        }
    }

    pub fn to_local(self) -> Self {
        Self::from_unix(self.unix_timestamp() + local_offset_minutes() * 60)
    }
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn read_register(register: u8) -> u8 {
    unsafe {
        outb(INDEX, register);
        inb(DATA)
    }
}

fn bcd(value: u8) -> u8 {
    (value & 0x0f) + (value >> 4) * 10
}

struct Raw([u8; 7]);

fn read_raw() -> Raw {
    let century = CENTURY_REGISTER.load(Ordering::Relaxed);
    Raw([
        read_register(REG_SECONDS),
        read_register(REG_MINUTES),
        read_register(REG_HOURS),
        read_register(REG_DAY),
        read_register(REG_MONTH),
        read_register(REG_YEAR),
        if century != 0 {
            read_register(century)
        } else {
            0
        },
    ])
}

fn wait_for_update_end() -> bool {
    (0..1_000_000).any(|_| read_register(REG_STATUS_A) & STATUS_A_UPDATING == 0)
}

/// NTP ile ölçülen fark (saniye): gerçek zaman = RTC + düzeltme. Donanım saatine yazılmaz
/// (Windows'la çift önyüklemede RTC yerel saat tutabilir).
static CORRECTION: core::sync::atomic::AtomicI64 = core::sync::atomic::AtomicI64::new(0);
static SYNCHRONIZED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Şu anki zaman (UTC): RTC + NTP düzeltmesi.
pub fn now() -> Option<DateTime> {
    let rtc = hardware_now()?;
    let correction = CORRECTION.load(core::sync::atomic::Ordering::Relaxed);
    Some(if correction == 0 {
        rtc
    } else {
        DateTime::from_unix(rtc.unix_timestamp() + correction)
    })
}

/// Doğru Unix zamanını bildirir (NTP); sonraki `now` çağrıları buna göre düzeltilir.
pub fn set_unix_time(unix: i64) -> Option<i64> {
    let rtc = hardware_now()?.unix_timestamp();
    let correction = unix - rtc;
    CORRECTION.store(correction, core::sync::atomic::Ordering::Relaxed);
    SYNCHRONIZED.store(true, core::sync::atomic::Ordering::Relaxed);
    Some(correction)
}

/// Saat NTP ile eşitlendi mi?
pub fn synchronized() -> bool {
    SYNCHRONIZED.load(core::sync::atomic::Ordering::Relaxed)
}

/// CMOS RTC'nin kendi okuması.
pub fn hardware_now() -> Option<DateTime> {
    cpu::without_interrupts(|| {
        let mut previous = None;
        for _ in 0..8 {
            if !wait_for_update_end() {
                return None;
            }
            let raw = read_raw();
            if previous.as_ref().is_some_and(|p: &Raw| p.0 == raw.0) {
                return decode(&raw);
            }
            previous = Some(raw);
        }
        None
    })
}

fn decode(raw: &Raw) -> Option<DateTime> {
    let status_b = read_register(REG_STATUS_B);
    let binary = status_b & STATUS_B_BINARY != 0;
    let convert = |v: u8| if binary { v } else { bcd(v) };

    let pm = raw.0[2] & 0x80 != 0;
    let mut hour = convert(raw.0[2] & 0x7f);
    if status_b & STATUS_B_24H == 0 {
        hour %= 12;
        if pm {
            hour += 12;
        }
    }

    let year_low = convert(raw.0[5]) as u16;
    let year = if raw.0[6] != 0 {
        convert(raw.0[6]) as u16 * 100 + year_low
    } else {
        2000 + year_low
    };

    let time = DateTime {
        year,
        month: convert(raw.0[4]),
        day: convert(raw.0[3]),
        hour,
        minute: convert(raw.0[1]),
        second: convert(raw.0[0]),
    };
    time.is_valid().then_some(time)
}

pub fn set_century_register(register: u8) {
    CENTURY_REGISTER.store(register, Ordering::Relaxed);
}

fn self_test() -> bool {
    let epoch = DateTime {
        year: 1970,
        month: 1,
        day: 1,
        hour: 0,
        minute: 0,
        second: 0,
    };
    let leap = DateTime {
        year: 2024,
        month: 2,
        day: 29,
        hour: 12,
        minute: 34,
        second: 56,
    };
    epoch.unix_timestamp() == 0
        && leap.unix_timestamp() == 1_709_210_096
        && DateTime::from_unix(1_709_210_096) == leap
        && bcd(0x59) == 59
}

fn init(detail: &mut Detail) -> Result<(), &'static str> {
    check("rtc", self_test())?;
    let time = now().ok_or("CMOS clock unreadable")?;
    crate::log!("rtc: {} (unix {})", time, time.unix_timestamp());
    let _ = write!(detail, "{}", time);
    Ok(())
}

crate::initcall!(driver, 10, "RTC (CMOS clock)", init);
