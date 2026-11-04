//! Opt-in compatibility reports. Nothing is sent from here: the report is shown first, then
//! opens as a prefilled GitHub issue that the user reads, edits and submits themselves.
//!
//! What goes in: the noren version and architecture, the Windows version and build, monitor
//! sizes and scaling, what the profile sets up ([`crate::card::features`], no names, paths or
//! program names), and how the last transactions ended. Nothing else.

use crate::host::Display;
use crate::palette::{Contrast, Mode};
use crate::rice::{Profile, Status, Transaction};

/// Where reports go.
pub const ISSUES: &str = "https://github.com/KayZedd/noren/issues/new";

/// GitHub rejects longer issue links; browsers start cutting not far above.
pub const MAX_URL: usize = 8000;

/// How many transactions a report lists, newest first.
const TRANSACTIONS: usize = 5;

#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub version: String,
    pub arch: String,
    /// "Windows 11 24H2, build 26100.2314", or the OS name elsewhere.
    pub os: String,
    /// "2560x1440 at 150% (primary)", primary first.
    pub monitors: Vec<String>,
    /// "dark, standard contrast" and the profile's features, when a profile was given.
    pub profile: Option<(String, Vec<String>)>,
    /// "applied, 7 changes", newest first.
    pub transactions: Vec<String>,
}

impl Report {
    pub fn new(os: String, displays: &[Display], profile: Option<&Profile>, journal: &[Transaction]) -> Self {
        let mut displays = displays.to_vec();
        displays.sort_by_key(|d| (!d.primary, d.left, d.top));
        let monitors = displays
            .iter()
            .map(|d| {
                let percent = (d.scale * 100.0).round() as u32;
                let primary = if d.primary { " (primary)" } else { "" };
                format!("{}x{} at {percent}%{primary}", d.width, d.height)
            })
            .collect();
        let profile = profile.map(|p| {
            let mode = match p.palette.mode {
                Mode::Dark => "dark",
                Mode::Light => "light",
            };
            let contrast = match p.palette.contrast {
                Contrast::Standard => "standard contrast",
                Contrast::High => "high contrast",
            };
            (format!("{mode}, {contrast}"), crate::card::features(p))
        });
        let mut newest: Vec<&Transaction> = journal.iter().collect();
        newest.sort_by_key(|t| std::cmp::Reverse(t.id));
        let transactions = newest
            .into_iter()
            .take(TRANSACTIONS)
            .map(|t| {
                let status = match t.status {
                    Status::Pending => "interrupted",
                    Status::Applied => "applied",
                    Status::RolledBack => "rolled back",
                };
                let n = t.entries.len();
                format!("{status}, {n} {}", if n == 1 { "change" } else { "changes" })
            })
            .collect();
        Self {
            version: env!("CARGO_PKG_VERSION").into(),
            arch: std::env::consts::ARCH.into(),
            os,
            monitors,
            profile,
            transactions,
        }
    }

    /// This machine: the OS from the registry, monitors from Windows. Either may be missing.
    pub fn collect(profile: Option<&Profile>, journal: &[Transaction]) -> Self {
        let displays = crate::host::displays().unwrap_or_default();
        Self::new(os_name(), &displays, profile, journal)
    }

    pub fn title(&self) -> String {
        format!("Compatibility: noren {} on {}", self.version, self.os)
    }

    /// The issue body, as Markdown.
    pub fn markdown(&self) -> String {
        let mut s = String::from("### Environment\n\n");
        s += &format!("- noren {} ({})\n", self.version, self.arch);
        s += &format!("- {}\n", self.os);
        if self.monitors.is_empty() {
            s += "- monitors: unknown\n";
        } else {
            s += &format!("- monitors: {}\n", self.monitors.join(", "));
        }
        if let Some((look, features)) = &self.profile {
            s += "\n### Profile\n\n";
            s += &format!("- {look}\n");
            for f in features {
                s += &format!("- {f}\n");
            }
        }
        if !self.transactions.is_empty() {
            s += "\n### Last transactions\n\n";
            for t in &self.transactions {
                s += &format!("- {t}\n");
            }
        }
        s += "\n### What happened\n\n<!-- What works, what broke, and the steps to see it. -->\n";
        s
    }

    /// A link that opens a new issue with this report filled in.
    pub fn issue_url(&self) -> String {
        issue_url(&self.title(), &self.markdown())
    }
}

/// A new-issue link with `title` and `body`, the body cut to keep the link under [`MAX_URL`].
pub fn issue_url(title: &str, body: &str) -> String {
    let head = format!("{ISSUES}?labels=compatibility&title={}&body=", encode(title));
    let full = encode(body);
    if head.len() + full.len() <= MAX_URL {
        return head + &full;
    }
    let note = encode("\n\n(cut to fit a link)");
    let budget = MAX_URL.saturating_sub(head.len() + note.len());
    let mut cut = String::new();
    for c in body.chars() {
        let e = encode(c.encode_utf8(&mut [0; 4]));
        if cut.len() + e.len() > budget {
            break;
        }
        cut += &e;
    }
    head + &cut + &note
}

/// Percent-encoding for a query value: everything but RFC 3986 unreserved characters.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out += &format!("%{b:02X}");
        }
    }
    out
}

/// "Windows 11 24H2, build 26100.2314" from the registry's build number, update revision and
/// display version. Windows 11 still calls itself Windows 10 in `ProductName`, so the name
/// comes from the build: 22000 and up is 11.
pub fn windows_name(build: u32, revision: Option<u32>, display: Option<&str>) -> String {
    let major = if build >= 22000 { 11 } else { 10 };
    let display = display
        .filter(|d| !d.is_empty())
        .map(|d| format!(" {d}"))
        .unwrap_or_default();
    let revision = revision.map(|r| format!(".{r}")).unwrap_or_default();
    format!("Windows {major}{display}, build {build}{revision}")
}

#[cfg(windows)]
fn os_name() -> String {
    use windows::Win32::System::Registry::{
        HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
    };
    use windows::core::{HSTRING, w};
    let key = w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion");
    let text = |name: &str| -> Option<String> {
        let mut buf = [0u16; 128];
        let mut len = size_of_val(&buf) as u32;
        // SAFETY: the buffer and its byte length match; the value is read as a string.
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                key,
                &HSTRING::from(name),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut len),
            )
        };
        status.is_ok().then(|| {
            let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            String::from_utf16_lossy(&buf[..end])
        })
    };
    let mut ubr = 0u32;
    let mut len = size_of::<u32>() as u32;
    // SAFETY: a DWORD read into a u32 of the stated size.
    let revision = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key,
            w!("UBR"),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut ubr).cast()),
            Some(&mut len),
        )
    }
    .is_ok()
    .then_some(ubr);
    match text("CurrentBuild").and_then(|b| b.parse().ok()) {
        Some(build) => windows_name(build, revision, text("DisplayVersion").as_deref()),
        None => "Windows, unknown build".into(),
    }
}

#[cfg(not(windows))]
fn os_name() -> String {
    std::env::consts::OS.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rice::{Entry, Slot};

    fn display(primary: bool, left: i32, scale: f32) -> Display {
        Display {
            name: r"\\.\DISPLAY1".into(),
            primary,
            left,
            top: 0,
            width: 2560,
            height: 1440,
            scale,
        }
    }

    fn transaction(id: u64, status: Status, changes: usize) -> Transaction {
        let entry = Entry {
            label: "secret label".into(),
            slot: Slot::File {
                path: r"C:\Users\kacper\x.json".into(),
            },
            before: None,
            after: None,
        };
        Transaction {
            id,
            profile: "Kacper's rice".into(),
            created_unix: 0,
            status,
            entries: vec![entry; changes],
        }
    }

    fn report() -> Report {
        let profile = Profile::parse(
            "schema = 1\nname = \"My Private Rice\"\n[palette]\naccent = \"#d27e99\"\n\
             [[window]]\nexe = \"secret-app.exe\"\nopacity = 0.9\n",
        )
        .unwrap();
        Report::new(
            windows_name(26100, Some(2314), Some("24H2")),
            &[display(false, 2560, 1.0), display(true, 0, 1.5)],
            Some(&profile),
            &[
                transaction(1, Status::RolledBack, 3),
                transaction(3, Status::Pending, 1),
                transaction(2, Status::Applied, 7),
            ],
        )
    }

    #[test]
    fn the_report_lists_the_setup_and_no_names_or_paths() {
        let r = report();
        assert_eq!(r.os, "Windows 11 24H2, build 26100.2314");
        assert_eq!(r.monitors, ["2560x1440 at 150% (primary)", "2560x1440 at 100%"]);
        assert_eq!(
            r.transactions,
            [
                "interrupted, 1 change",
                "applied, 7 changes",
                "rolled back, 3 changes"
            ]
        );
        let md = r.markdown();
        assert!(
            md.contains("- dark, standard contrast\n- 1 window rule\n"),
            "{md}"
        );
        for private in [
            "My Private Rice",
            "secret-app",
            "Kacper",
            "secret label",
            "Users",
            "DISPLAY",
        ] {
            assert!(!md.contains(private), "{private} leaked:\n{md}");
            assert!(!r.title().contains(private));
        }
    }

    #[test]
    fn transactions_are_capped_newest_first() {
        let journal: Vec<_> = (1..=9)
            .map(|id| transaction(id, Status::Applied, id as usize))
            .collect();
        let r = Report::new("linux".into(), &[], None, &journal);
        assert_eq!(r.transactions.len(), TRANSACTIONS);
        assert_eq!(r.transactions[0], "applied, 9 changes");
        assert!(r.markdown().contains("- monitors: unknown"));
        assert!(!r.markdown().contains("### Profile"));
    }

    #[test]
    fn windows_names_come_from_the_build() {
        assert_eq!(
            windows_name(19045, Some(5011), Some("22H2")),
            "Windows 10 22H2, build 19045.5011"
        );
        assert_eq!(windows_name(22000, None, None), "Windows 11, build 22000");
        assert_eq!(windows_name(22631, None, Some("")), "Windows 11, build 22631");
    }

    #[test]
    fn encoding_keeps_only_unreserved_characters() {
        assert_eq!(encode("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(encode("a b&c=d#\n"), "a%20b%26c%3Dd%23%0A");
        assert_eq!(encode("ł·"), "%C5%82%C2%B7");
    }

    #[test]
    fn the_issue_link_carries_title_and_body() {
        let url = report().issue_url();
        assert!(url.starts_with("https://github.com/KayZedd/noren/issues/new?labels=compatibility&title=Compatibility%3A%20noren%20"));
        assert!(url.contains("&body=%23%23%23%20Environment"));
        assert!(url.len() <= MAX_URL);
    }

    #[test]
    fn long_bodies_are_cut_on_a_character_boundary() {
        let body = "ł".repeat(5000);
        let url = issue_url("t", &body);
        assert!(url.len() <= MAX_URL, "{}", url.len());
        assert!(url.ends_with(&encode("\n\n(cut to fit a link)")));
        // Every encoded "ł" is whole: the cut part is a run of %C5%82.
        let body = url.split("&body=").nth(1).unwrap();
        let cut = body.strip_suffix(&encode("\n\n(cut to fit a link)")).unwrap();
        assert_eq!(cut.len() % 6, 0);
        assert!(cut.len() > 6000);
    }
}
