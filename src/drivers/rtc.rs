/// CMOS Real-Time Clock (RTC) driver
///
/// Reads date and time from the MC146818 RTC chip via CMOS ports 0x70/0x71.
/// This is the standard way to get wall-clock time on x86 PCs.
///
/// Reference: https://wiki.osdev.org/CMOS

use x86_64::instructions::port::Port;
use spin::Mutex;

/// CMOS register addresses
const CMOS_SECONDS: u8 = 0x00;
const CMOS_MINUTES: u8 = 0x02;
const CMOS_HOURS: u8 = 0x04;
const CMOS_DAY_OF_WEEK: u8 = 0x06;
const CMOS_DAY_OF_MONTH: u8 = 0x07;
const CMOS_MONTH: u8 = 0x08;
const CMOS_YEAR: u8 = 0x09;
const CMOS_STATUS_A: u8 = 0x0A;
const CMOS_STATUS_B: u8 = 0x0B;

/// Date/time structure
#[derive(Debug, Clone, Copy)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub day_of_week: u8,
}

impl DateTime {
    /// Format as "YYYY-MM-DD HH:MM:SS"
    pub fn format(&self, buf: &mut [u8]) -> usize {
        if buf.len() < 19 {
            return 0;
        }

        let year = self.year;
        buf[0] = b'0' + ((year / 1000) % 10) as u8;
        buf[1] = b'0' + ((year / 100) % 10) as u8;
        buf[2] = b'0' + ((year / 10) % 10) as u8;
        buf[3] = b'0' + (year % 10) as u8;
        buf[4] = b'-';
        buf[5] = b'0' + (self.month / 10);
        buf[6] = b'0' + (self.month % 10);
        buf[7] = b'-';
        buf[8] = b'0' + (self.day / 10);
        buf[9] = b'0' + (self.day % 10);
        buf[10] = b' ';
        buf[11] = b'0' + (self.hour / 10);
        buf[12] = b'0' + (self.hour % 10);
        buf[13] = b':';
        buf[14] = b'0' + (self.minute / 10);
        buf[15] = b'0' + (self.minute % 10);
        buf[16] = b':';
        buf[17] = b'0' + (self.second / 10);
        buf[18] = b'0' + (self.second % 10);
        19
    }

    /// Get day of week as string
    pub fn day_name(&self) -> &'static str {
        match self.day_of_week {
            1 => "Sun",
            2 => "Mon",
            3 => "Tue",
            4 => "Wed",
            5 => "Thu",
            6 => "Fri",
            7 => "Sat",
            _ => "???",
        }
    }

    /// Get month name
    pub fn month_name(&self) -> &'static str {
        match self.month {
            1 => "Jan",
            2 => "Feb",
            3 => "Mar",
            4 => "Apr",
            5 => "May",
            6 => "Jun",
            7 => "Jul",
            8 => "Aug",
            9 => "Sep",
            10 => "Oct",
            11 => "Nov",
            12 => "Dec",
            _ => "???",
        }
    }

    /// Convert to Unix timestamp (seconds since 1970-01-01 00:00:00 UTC)
    pub fn to_unix_timestamp(&self) -> u64 {
        // Days from 1970 to start of year
        let mut days: u64 = 0;
        for y in 1970..self.year {
            days += if is_leap_year(y) { 366 } else { 365 };
        }

        // Days from start of year to start of month
        let days_in_months: [u64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
        for m in 0..(self.month as usize - 1) {
            days += days_in_months[m];
            if m == 1 && is_leap_year(self.year) {
                days += 1; // February in leap year
            }
        }

        // Add day of month (1-based)
        days += (self.day as u64) - 1;

        // Convert to seconds
        days * 86400 + (self.hour as u64) * 3600 + (self.minute as u64) * 60 + self.second as u64
    }
}

fn is_leap_year(year: u16) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// CMOS port pair
static CMOS_PORTS: Mutex<(Port<u8>, Port<u8>)> = Mutex::new((
    Port::new(0x70),
    Port::new(0x71),
));

/// Read a CMOS register value
fn read_cmos(register: u8) -> u8 {
    let mut ports = CMOS_PORTS.lock();
    unsafe {
        // Select register (bit 7 = 0 to keep NMI enabled)
        ports.0.write(register & 0x7F);
        ports.1.read()
    }
}

/// Check if CMOS update is in progress (should wait if so)
fn is_update_in_progress() -> bool {
    read_cmos(CMOS_STATUS_A) & 0x80 != 0
}

/// Convert BCD (Binary-Coded Decimal) to binary
fn bcd_to_binary(bcd: u8) -> u8 {
    (bcd & 0x0F) + ((bcd >> 4) * 10)
}

/// Read current date/time from the RTC.
///
/// Handles BCD vs binary mode and 12/24 hour format automatically.
/// Reads twice to ensure consistency (RTC can update between reads).
pub fn read_datetime() -> DateTime {
    // Wait for any in-progress update to complete
    while is_update_in_progress() {
        core::hint::spin_loop();
    }

    // Read all registers
    let mut second = read_cmos(CMOS_SECONDS);
    let mut minute = read_cmos(CMOS_MINUTES);
    let mut hour = read_cmos(CMOS_HOURS);
    let day_of_week = read_cmos(CMOS_DAY_OF_WEEK);
    let mut day = read_cmos(CMOS_DAY_OF_MONTH);
    let mut month = read_cmos(CMOS_MONTH);
    let mut year = read_cmos(CMOS_YEAR);

    // Read again until two consecutive reads match (ensures no update happened mid-read)
    loop {
        let s2 = read_cmos(CMOS_SECONDS);
        let m2 = read_cmos(CMOS_MINUTES);
        let h2 = read_cmos(CMOS_HOURS);
        let d2 = read_cmos(CMOS_DAY_OF_MONTH);
        let mo2 = read_cmos(CMOS_MONTH);
        let y2 = read_cmos(CMOS_YEAR);

        if second == s2 && minute == m2 && hour == h2
            && day == d2 && month == mo2 && year == y2
        {
            break;
        }

        second = s2;
        minute = m2;
        hour = h2;
        day = d2;
        month = mo2;
        year = y2;
    }

    // Check status register B for data format
    let status_b = read_cmos(CMOS_STATUS_B);
    let is_binary = (status_b & 0x04) != 0;
    let is_24h = (status_b & 0x02) != 0;

    // Convert BCD to binary if needed
    if !is_binary {
        second = bcd_to_binary(second);
        minute = bcd_to_binary(minute);
        hour = bcd_to_binary(hour & 0x7F); // Mask PM bit for BCD conversion
        day = bcd_to_binary(day);
        month = bcd_to_binary(month);
        year = bcd_to_binary(year);
    }

    // Handle 12-hour format
    if !is_24h && (read_cmos(CMOS_HOURS) & 0x80) != 0 {
        // PM bit set
        hour = ((hour & 0x7F) + 12) % 24;
    }

    // Year: CMOS only stores 2-digit year. Assume 2000s.
    let full_year = 2000u16 + year as u16;

    DateTime {
        year: full_year,
        month,
        day,
        hour,
        minute,
        second,
        day_of_week,
    }
}
