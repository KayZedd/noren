//! Switches the profile between dark and light by the clock or by the sun.
//!
//! ```toml
//! [schedule]
//! light = "sunrise+30m"   # "HH:MM", "sunrise" or "sunset", with an optional +/-Nh or +/-Nm
//! dark = "21:00"
//! latitude = 52.23        # needed when either time follows the sun
//! longitude = 21.01       # east is positive
//! ```
//!
//! The sun times come from the NOAA sunrise equation (a refracted horizon at -0.833°),
//! good to a couple of minutes away from the poles. It is written here instead of pulling
//! the `sunrise` crate because that crate brings `chrono` for one date conversion. Where the
//! sun never rises or never sets that day, the mode is dark or light all day.

use serde::{Deserialize, Serialize};

use crate::palette::Mode;

const DAY: i32 = 24 * 60;

/// `[schedule]` as written in the profile.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleConfig {
    pub light: String,
    pub dark: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
}

/// One end of the light period.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum At {
    /// Minutes after local midnight.
    Clock(i32),
    /// Sunrise plus the offset in minutes.
    Sunrise(i32),
    /// Sunset plus the offset in minutes.
    Sunset(i32),
}

impl At {
    pub fn parse(s: &str) -> Result<Self, String> {
        let t = s.trim();
        let bad =
            || format!("schedule: {s:?} is not \"HH:MM\", \"sunrise\" or \"sunset\" (with +/-Nh or +/-Nm)");
        for (word, make) in [("sunrise", At::Sunrise as fn(i32) -> At), ("sunset", At::Sunset)] {
            if let Some(rest) = t.strip_prefix(word) {
                return offset(rest).map(make).ok_or_else(bad);
            }
        }
        let (h, m) = t.split_once(':').ok_or_else(bad)?;
        let (h, m): (i32, i32) = (h.parse().map_err(|_| bad())?, m.parse().map_err(|_| bad())?);
        if !(0..24).contains(&h) || !(0..60).contains(&m) || t.len() != 5 {
            return Err(bad());
        }
        Ok(At::Clock(h * 60 + m))
    }

    fn follows_sun(self) -> bool {
        !matches!(self, At::Clock(_))
    }
}

/// `""`, `"+30m"`, `"-1h"`.
fn offset(s: &str) -> Option<i32> {
    if s.is_empty() {
        return Some(0);
    }
    let (sign, rest) = match s.as_bytes()[0] {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => return None,
    };
    let (n, scale) = if let Some(n) = rest.strip_suffix('h') {
        (n, 60)
    } else {
        (rest.strip_suffix('m')?, 1)
    };
    let n: i32 = n.parse().ok().filter(|n| (0..=12 * 60).contains(n))?;
    Some(sign * n * scale)
}

/// The sun on one day, in minutes after UTC midnight.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sun {
    Rises {
        rise: f64,
        set: f64,
    },
    /// Never sets.
    PolarDay,
    /// Never rises.
    PolarNight,
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
pub fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = i64::from(y) - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Sunrise and sunset on day `days` (since 1970-01-01) at `lat`, `lon` (degrees, east positive).
pub fn sun(days: i64, lat: f64, lon: f64) -> Sun {
    let rad = f64::to_radians;
    let midnight = days as f64 + 2_440_587.5;
    let n = (midnight + 0.5 - 2_451_545.0 + 0.0008).round();
    let mean_noon = n - lon / 360.0;
    let m = (357.5291 + 0.985_600_28 * mean_noon).rem_euclid(360.0);
    let c = 1.9148 * rad(m).sin() + 0.02 * rad(2.0 * m).sin() + 0.0003 * rad(3.0 * m).sin();
    let lambda = (m + c + 180.0 + 102.9372).rem_euclid(360.0);
    let transit = 2_451_545.0 + mean_noon + 0.0053 * rad(m).sin() - 0.0069 * rad(2.0 * lambda).sin();
    let decl = (rad(lambda).sin() * rad(23.4397).sin()).asin();
    let cos_h = (rad(-0.833).sin() - rad(lat).sin() * decl.sin()) / (rad(lat).cos() * decl.cos());
    if cos_h > 1.0 {
        return Sun::PolarNight;
    }
    if cos_h < -1.0 {
        return Sun::PolarDay;
    }
    let half = cos_h.acos().to_degrees() / 360.0;
    Sun::Rises {
        rise: (transit - half - midnight) * 1440.0,
        set: (transit + half - midnight) * 1440.0,
    }
}

/// A local moment: the date and minutes after local midnight, plus the zone's offset from UTC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Local {
    pub days: i64,
    pub minute: i32,
    /// Local minus UTC, in minutes.
    pub offset: i32,
}

impl Local {
    /// Now. Windows reads the local clock and zone; elsewhere this is UTC.
    pub fn now() -> Self {
        #[cfg(windows)]
        {
            use windows::Win32::System::SystemInformation::{GetLocalTime, GetSystemTime};
            // SAFETY: plain reads of the system clock.
            let (local, utc) = unsafe { (GetLocalTime(), GetSystemTime()) };
            let at = |t: &windows::Win32::Foundation::SYSTEMTIME| {
                let days = days_from_civil(i32::from(t.wYear), u32::from(t.wMonth), u32::from(t.wDay));
                (days, i32::from(t.wHour) * 60 + i32::from(t.wMinute))
            };
            let ((ld, lm), (ud, um)) = (at(&local), at(&utc));
            Self {
                days: ld,
                minute: lm,
                offset: (ld - ud) as i32 * DAY + lm - um,
            }
        }
        #[cfg(not(windows))]
        {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            Self {
                days: secs.div_euclid(86_400),
                minute: (secs.rem_euclid(86_400) / 60) as i32,
                offset: 0,
            }
        }
    }
}

/// When light and dark start on one day, in local minutes after midnight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Day {
    Switches {
        light: i32,
        dark: i32,
    },
    /// The sun decides the whole day.
    All(Mode),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Schedule {
    pub light: At,
    pub dark: At,
    /// Latitude and longitude, present whenever a time follows the sun.
    pub place: Option<(f64, f64)>,
}

impl Schedule {
    pub fn new(c: &ScheduleConfig) -> Result<Self, String> {
        let (light, dark) = (At::parse(&c.light)?, At::parse(&c.dark)?);
        let place = match (c.latitude, c.longitude) {
            (Some(lat), Some(lon)) => {
                if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                    return Err(format!("schedule: {lat}, {lon} is not a place on Earth"));
                }
                Some((lat, lon))
            }
            (None, None) => None,
            _ => return Err("schedule: give both latitude and longitude".into()),
        };
        if place.is_none() && (light.follows_sun() || dark.follows_sun()) {
            return Err("schedule: sunrise and sunset need latitude and longitude".into());
        }
        if light == dark {
            return Err("schedule: light and dark start at the same time".into());
        }
        Ok(Self { light, dark, place })
    }

    /// The switch times on local day `days`.
    pub fn day(&self, days: i64, offset: i32) -> Day {
        let sun = self.place.map(|(lat, lon)| sun(days, lat, lon));
        let resolve = |at: At| -> Result<i32, Mode> {
            let (base, shift) = match at {
                At::Clock(m) => return Ok(m),
                At::Sunrise(o) => (0, o),
                At::Sunset(o) => (1, o),
            };
            match sun {
                Some(Sun::Rises { rise, set }) => {
                    let utc = if base == 0 { rise } else { set };
                    Ok((utc.round() as i32 + offset + shift).rem_euclid(DAY))
                }
                Some(Sun::PolarNight) => Err(Mode::Dark),
                _ => Err(Mode::Light),
            }
        };
        match (resolve(self.light), resolve(self.dark)) {
            (Ok(light), Ok(dark)) => Day::Switches { light, dark },
            (Err(m), _) | (_, Err(m)) => Day::All(m),
        }
    }

    /// The mode at `now`.
    pub fn mode_at(&self, now: Local) -> Mode {
        match self.day(now.days, now.offset) {
            Day::All(m) => m,
            Day::Switches { light, dark } => {
                let t = now.minute;
                let lit = if light < dark {
                    (light..dark).contains(&t)
                } else {
                    t >= light || t < dark
                };
                if lit { Mode::Light } else { Mode::Dark }
            }
        }
    }
}

impl Schedule {
    /// Today's switch times and the mode at `now`, for `noren schedule`.
    pub fn report(&self, now: Local) -> String {
        let name = |m: Mode| if m == Mode::Light { "light" } else { "dark" };
        let today = match self.day(now.days, now.offset) {
            Day::Switches { light, dark } => format!("light  {}\ndark   {}\n", clock(light), clock(dark)),
            Day::All(m) => format!(
                "today  {} all day, the sun {}\n",
                name(m),
                if m == Mode::Light {
                    "never sets"
                } else {
                    "never rises"
                }
            ),
        };
        format!("{today}now    {}\n", name(self.mode_at(now)))
    }
}

/// Formats local minutes as `HH:MM`.
pub fn clock(minute: i32) -> String {
    let m = minute.rem_euclid(DAY);
    format!("{:02}:{:02}", m / 60, m % 60)
}

/// Follows a schedule: [`Tracker::due`] answers the mode to switch to, once per change.
#[derive(Debug)]
pub struct Tracker {
    pub schedule: Schedule,
    last: Option<Mode>,
}

impl Tracker {
    pub fn new(schedule: Schedule) -> Self {
        Self { schedule, last: None }
    }

    /// The mode at `now` the first time it is asked and whenever it changes; `None` otherwise.
    pub fn due(&mut self, now: Local) -> Option<Mode> {
        let mode = self.schedule.mode_at(now);
        (self.last != Some(mode)).then(|| {
            self.last = Some(mode);
            mode
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WARSAW: (f64, f64) = (52.2297, 21.0122);

    fn cfg(light: &str, dark: &str, place: Option<(f64, f64)>) -> ScheduleConfig {
        ScheduleConfig {
            light: light.into(),
            dark: dark.into(),
            latitude: place.map(|p| p.0),
            longitude: place.map(|p| p.1),
        }
    }

    fn at(days: i64, hh: i32, mm: i32, offset: i32) -> Local {
        Local {
            days,
            minute: hh * 60 + mm,
            offset,
        }
    }

    #[test]
    fn times_parse() {
        assert_eq!(At::parse("07:30"), Ok(At::Clock(450)));
        assert_eq!(At::parse("00:00"), Ok(At::Clock(0)));
        assert_eq!(At::parse("sunrise"), Ok(At::Sunrise(0)));
        assert_eq!(At::parse("sunrise+30m"), Ok(At::Sunrise(30)));
        assert_eq!(At::parse("sunset-1h"), Ok(At::Sunset(-60)));
        for bad in [
            "24:00",
            "7:30",
            "07:60",
            "noon",
            "sunrise30m",
            "sunset+1d",
            "sunrise+",
            "sunset+999h",
        ] {
            assert!(At::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn civil_dates_count_days() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_from_civil(2024, 3, 1) - days_from_civil(2024, 2, 28), 2);
        assert_eq!(days_from_civil(2026, 11, 4), 20_761);
    }

    /// Reference values: timeanddate.com for Warsaw, 2026 solstices (CEST and CET).
    #[test]
    fn warsaw_sun_matches_the_almanac() {
        let s = Schedule::new(&cfg("sunrise", "sunset", Some(WARSAW))).unwrap();
        let close =
            |got: i32, want: i32| assert!((got - want).abs() <= 3, "{} vs {}", clock(got), clock(want));
        let Day::Switches { light, dark } = s.day(days_from_civil(2026, 6, 21), 120) else {
            panic!("the sun sets in Warsaw");
        };
        close(light, 4 * 60 + 14);
        close(dark, 21 * 60 + 1);
        let Day::Switches { light, dark } = s.day(days_from_civil(2026, 12, 21), 60) else {
            panic!("the sun rises in Warsaw");
        };
        close(light, 7 * 60 + 43);
        close(dark, 15 * 60 + 25);
    }

    #[test]
    fn polar_days_hold_one_mode() {
        let tromso = Some((69.65, 18.96));
        let s = Schedule::new(&cfg("sunrise", "20:00", tromso)).unwrap();
        assert_eq!(s.day(days_from_civil(2026, 12, 21), 60), Day::All(Mode::Dark));
        assert_eq!(s.day(days_from_civil(2026, 6, 21), 120), Day::All(Mode::Light));
        assert_eq!(
            s.mode_at(at(days_from_civil(2026, 6, 21), 23, 0, 120)),
            Mode::Light
        );
    }

    #[test]
    fn the_clock_picks_the_mode() {
        let s = Schedule::new(&cfg("07:00", "19:30", None)).unwrap();
        assert_eq!(s.mode_at(at(0, 6, 59, 0)), Mode::Dark);
        assert_eq!(s.mode_at(at(0, 7, 0, 0)), Mode::Light);
        assert_eq!(s.mode_at(at(0, 19, 29, 0)), Mode::Light);
        assert_eq!(s.mode_at(at(0, 19, 30, 0)), Mode::Dark);
        // A light period across midnight.
        let night_owl = Schedule::new(&cfg("22:00", "06:00", None)).unwrap();
        assert_eq!(night_owl.mode_at(at(0, 23, 0, 0)), Mode::Light);
        assert_eq!(night_owl.mode_at(at(0, 3, 0, 0)), Mode::Light);
        assert_eq!(night_owl.mode_at(at(0, 12, 0, 0)), Mode::Dark);
    }

    #[test]
    fn offsets_shift_sun_times() {
        let d = days_from_civil(2026, 6, 21);
        let plain = Schedule::new(&cfg("sunrise", "sunset", Some(WARSAW))).unwrap();
        let shifted = Schedule::new(&cfg("sunrise+30m", "sunset-1h", Some(WARSAW))).unwrap();
        let (Day::Switches { light: l0, dark: d0 }, Day::Switches { light: l1, dark: d1 }) =
            (plain.day(d, 120), shifted.day(d, 120))
        else {
            panic!("Warsaw has a sunrise in June");
        };
        assert_eq!((l1 - l0, d1 - d0), (30, -60));
    }

    #[test]
    fn bad_schedules_are_refused() {
        assert!(Schedule::new(&cfg("sunrise", "20:00", None)).is_err());
        assert!(Schedule::new(&cfg("08:00", "08:00", None)).is_err());
        assert!(Schedule::new(&cfg("08:00", "20:00", Some((95.0, 0.0)))).is_err());
        let half = ScheduleConfig {
            latitude: Some(52.0),
            ..cfg("08:00", "20:00", None)
        };
        assert!(Schedule::new(&half).is_err());
    }

    #[test]
    fn the_tracker_reports_each_change_once() {
        let mut t = Tracker::new(Schedule::new(&cfg("07:00", "19:00", None)).unwrap());
        assert_eq!(t.due(at(0, 6, 0, 0)), Some(Mode::Dark));
        assert_eq!(t.due(at(0, 6, 30, 0)), None);
        assert_eq!(t.due(at(0, 7, 0, 0)), Some(Mode::Light));
        assert_eq!(t.due(at(0, 12, 0, 0)), None);
        assert_eq!(t.due(at(0, 19, 0, 0)), Some(Mode::Dark));
    }

    #[test]
    fn the_report_lists_today() {
        let s = Schedule::new(&cfg("07:00", "19:30", None)).unwrap();
        assert_eq!(
            s.report(at(0, 8, 0, 0)),
            "light  07:00\ndark   19:30\nnow    light\n"
        );
        let polar = Schedule::new(&cfg("sunrise", "sunset", Some((69.65, 18.96)))).unwrap();
        assert_eq!(
            polar.report(at(days_from_civil(2026, 12, 21), 12, 0, 60)),
            "today  dark all day, the sun never rises\nnow    dark\n"
        );
    }

    #[test]
    fn clock_formats_and_wraps() {
        assert_eq!(clock(450), "07:30");
        assert_eq!(clock(-30), "23:30");
        assert_eq!(clock(DAY + 5), "00:05");
    }
}
