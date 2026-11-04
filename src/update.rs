//! Updates from GitHub Releases.
//!
//! A release carries `noren.exe`, `noren-studio.exe`, `SHA256SUMS` and `SHA256SUMS.minisig`.
//! An update downloads the programs installed next to the running one, checks the signature on
//! `SHA256SUMS` against the key built into noren, checks each download against its sum, and
//! only then swaps the files. The signature's trusted comment names the release tag, so sums
//! signed for one release do not pass for another.
//!
//! Nothing runs unless asked: `noren update`, or the studio's check (which can be turned off).

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

/// The repository releases come from.
pub const REPO: &str = "KayZedd/noren";

/// The release signing key (minisign, key id E7A5045288B528B1).
pub const PUBLIC_KEY: &str = "RWSxKLWIUgSl59dMk3ZRnK3/4yS1aZ16p0RAyjaJLPfpupRx0Pf8DfQ/";

pub const SUMS: &str = "SHA256SUMS";
pub const SIGNATURE: &str = "SHA256SUMS.minisig";

/// The programs a release ships.
pub const PROGRAMS: [&str; 2] = ["noren.exe", "noren-studio.exe"];

/// `major.minor.patch`; pre-releases are not offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u32, pub u32, pub u32);

impl Version {
    /// `1.2.3` or `v1.2.3`. Anything with a suffix (`1.2.3-rc.1`) is `None`.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.strip_prefix('v').unwrap_or(s);
        let mut parts = s.split('.').map(|p| {
            (!p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
                .then(|| p.parse().ok())
                .flatten()
        });
        let v = Self(parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(v)
    }

    /// The version of this build.
    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).expect("the crate version is major.minor.patch")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// The parts of a GitHub release that an update needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: Version,
    pub tag: String,
    /// The release page, with the notes.
    pub page: String,
    /// Asset name to download link.
    pub assets: BTreeMap<String, String>,
}

impl Release {
    /// A release from the GitHub API's JSON (`/repos/{repo}/releases/latest`).
    pub fn parse(json: &str) -> Result<Self, String> {
        #[derive(Deserialize)]
        struct Asset {
            name: String,
            browser_download_url: String,
        }
        #[derive(Deserialize)]
        struct Api {
            tag_name: String,
            html_url: String,
            #[serde(default)]
            draft: bool,
            #[serde(default)]
            prerelease: bool,
            assets: Vec<Asset>,
        }
        let api: Api = serde_json::from_str(json).map_err(|e| format!("release: {e}"))?;
        if api.draft || api.prerelease {
            return Err(format!("release {} is a draft or a pre-release", api.tag_name));
        }
        let version = Version::parse(&api.tag_name)
            .ok_or_else(|| format!("release tag {:?} is not a version", api.tag_name))?;
        Ok(Self {
            version,
            tag: api.tag_name,
            page: api.html_url,
            assets: api
                .assets
                .into_iter()
                .map(|a| (a.name, a.browser_download_url))
                .collect(),
        })
    }

    pub fn asset(&self, name: &str) -> Result<&str, String> {
        self.assets
            .get(name)
            .map(String::as_str)
            .ok_or_else(|| format!("release {} has no {name}", self.tag))
    }

    /// The text the release's signature must carry as its trusted comment.
    pub fn trusted_comment(&self) -> String {
        format!("noren {}", self.tag)
    }
}

/// `SHA256SUMS` as written by `sha256sum`: `<hex>  <name>` or `<hex> *<name>` per line.
pub fn parse_sums(text: &str) -> Result<BTreeMap<String, [u8; 32]>, String> {
    let mut out = BTreeMap::new();
    for line in text.lines().map(str::trim_end).filter(|l| !l.is_empty()) {
        let bad = || format!("{SUMS}: bad line {line:?}");
        let (hex, name) = line.split_once(' ').ok_or_else(bad)?;
        let name = name.strip_prefix([' ', '*']).ok_or_else(bad)?;
        if hex.len() != 64 || name.is_empty() {
            return Err(bad());
        }
        let mut sum = [0u8; 32];
        for (byte, pair) in sum.iter_mut().zip(hex.as_bytes().chunks(2)) {
            let pair = std::str::from_utf8(pair).map_err(|_| bad())?;
            *byte = u8::from_str_radix(pair, 16).map_err(|_| bad())?;
        }
        if out.insert(name.to_owned(), sum).is_some() {
            return Err(format!("{SUMS}: {name} is listed twice"));
        }
    }
    Ok(out)
}

/// Checks `signature` over `sums` with `public_key`, and that its trusted comment is
/// `trusted_comment`. Returns the parsed sums.
pub fn verify_sums(
    sums: &[u8],
    signature: &str,
    public_key: &str,
    trusted_comment: &str,
) -> Result<BTreeMap<String, [u8; 32]>, String> {
    let key = minisign_verify::PublicKey::from_base64(public_key).map_err(|e| format!("signing key: {e}"))?;
    let sig = minisign_verify::Signature::decode(signature).map_err(|e| format!("{SIGNATURE}: {e}"))?;
    key.verify(sums, &sig, false)
        .map_err(|e| format!("{SIGNATURE} does not match {SUMS}: {e}"))?;
    if sig.trusted_comment() != trusted_comment {
        return Err(format!(
            "{SIGNATURE} is for {:?}, not {trusted_comment:?}",
            sig.trusted_comment()
        ));
    }
    let text = std::str::from_utf8(sums).map_err(|_| format!("{SUMS} is not text"))?;
    parse_sums(text)
}

/// Checks a download against its signed sum.
pub fn check_download(name: &str, bytes: &[u8], sums: &BTreeMap<String, [u8; 32]>) -> Result<(), String> {
    let want = sums.get(name).ok_or_else(|| format!("{SUMS} has no {name}"))?;
    let got: [u8; 32] = Sha256::digest(bytes).into();
    if &got == want {
        Ok(())
    } else {
        Err(format!("{name} does not match its sum in {SUMS}"))
    }
}

/// The programs to update: the running one, and the others found next to it.
pub fn installed(exe: &Path) -> Vec<PathBuf> {
    let dir = exe.parent().unwrap_or(Path::new("."));
    PROGRAMS
        .iter()
        .map(|p| dir.join(p))
        .filter(|p| p == exe || p.is_file())
        .collect()
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Swaps `path` for `contents`: writes `path.new`, moves `path` to `path.old` and `path.new` to
/// `path`, and moves the old file back if that fails. Windows lets a running program be moved
/// but not overwritten, which is why the old file is moved aside rather than replaced. The
/// `.old` file is removed now if it can be, or by the next update.
pub fn replace(path: &Path, contents: &[u8]) -> io::Result<()> {
    let new = with_suffix(path, ".new");
    let old = with_suffix(path, ".old");
    std::fs::write(&new, contents)?;
    let swap = || -> io::Result<()> {
        // A leftover from an earlier update whose program was still running then.
        if old.exists() {
            std::fs::remove_file(&old)?;
        }
        if path.exists() {
            std::fs::rename(path, &old)?;
        }
        if let Err(e) = std::fs::rename(&new, path) {
            if old.exists() {
                std::fs::rename(&old, path)?;
            }
            return Err(e);
        }
        Ok(())
    };
    match swap() {
        Ok(()) => {
            // Fails while the old program runs; the next update removes it.
            let _ = std::fs::remove_file(&old);
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&new);
            Err(e)
        }
    }
}

/// What `noren update` reports.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    UpToDate(Version),
    Updated { release: Release, programs: Vec<String> },
}

/// The newest release, if it is newer than this build.
pub fn check() -> Result<Option<Release>, String> {
    let release = Release::parse(&net::text(&format!(
        "https://api.github.com/repos/{REPO}/releases/latest"
    ))?)?;
    Ok((release.version > Version::current()).then_some(release))
}

/// Downloads `release`, checks it, and swaps the installed programs. Nothing changes unless
/// every download passes.
pub fn install(release: &Release) -> Result<Vec<String>, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find this program: {e}"))?;
    let sums = net::bytes(release.asset(SUMS)?)?;
    let signature = String::from_utf8(net::bytes(release.asset(SIGNATURE)?)?)
        .map_err(|_| format!("{SIGNATURE} is not text"))?;
    let sums = verify_sums(&sums, &signature, PUBLIC_KEY, &release.trusted_comment())?;
    let mut downloads = Vec::new();
    for path in installed(&exe) {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let bytes = net::bytes(release.asset(&name)?)?;
        check_download(&name, &bytes, &sums)?;
        downloads.push((path, name, bytes));
    }
    // The running program goes last, so a failure before it leaves this one as it was.
    downloads.sort_by_key(|(path, ..)| *path == exe);
    let mut done = Vec::new();
    for (path, name, bytes) in downloads {
        swap(&path, &exe, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        done.push(name);
    }
    Ok(done)
}

/// [`check`], then [`install`] when there is something newer.
pub fn update() -> Result<Outcome, String> {
    match check()? {
        None => Ok(Outcome::UpToDate(Version::current())),
        Some(release) => {
            let programs = install(&release)?;
            Ok(Outcome::Updated { release, programs })
        }
    }
}

#[cfg(windows)]
fn swap(path: &Path, exe: &Path, bytes: &[u8]) -> io::Result<()> {
    if path != exe {
        return replace(path, bytes);
    }
    // The running program: self-replace moves it aside and removes it once it exits.
    let new = with_suffix(path, ".new");
    std::fs::write(&new, bytes)?;
    let r = self_replace::self_replace(&new);
    let _ = std::fs::remove_file(&new);
    r
}

#[cfg(not(windows))]
fn swap(path: &Path, _exe: &Path, bytes: &[u8]) -> io::Result<()> {
    replace(path, bytes)
}

#[cfg(windows)]
mod net {
    use std::sync::OnceLock;
    use std::time::Duration;

    use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

    /// Releases are a few MB; this only stops a runaway download.
    const LIMIT: u64 = 256 << 20;

    fn agent() -> &'static ureq::Agent {
        static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
        AGENT.get_or_init(|| {
            ureq::Agent::config_builder()
                .user_agent(concat!("noren-updater/", env!("CARGO_PKG_VERSION")))
                .timeout_global(Some(Duration::from_secs(120)))
                // Windows' own TLS and certificate store.
                .tls_config(
                    TlsConfig::builder()
                        .provider(TlsProvider::NativeTls)
                        .root_certs(RootCerts::PlatformVerifier)
                        .build(),
                )
                .build()
                .into()
        })
    }

    fn get(url: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
        agent()
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .call()
            .map_err(|e| format!("{url}: {e}"))
    }

    pub fn text(url: &str) -> Result<String, String> {
        get(url)?
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("{url}: {e}"))
    }

    pub fn bytes(url: &str) -> Result<Vec<u8>, String> {
        get(url)?
            .body_mut()
            .with_config()
            .limit(LIMIT)
            .read_to_vec()
            .map_err(|e| format!("{url}: {e}"))
    }
}

#[cfg(not(windows))]
mod net {
    const NO: &str = "updates are published for Windows; elsewhere, build noren from source";

    pub fn text(_url: &str) -> Result<String, String> {
        Err(NO.into())
    }

    pub fn bytes(_url: &str) -> Result<Vec<u8>, String> {
        Err(NO.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway key, only for these tests.
    const TEST_KEY: &str = "RWQZaWFx8i/6QnXgBUafVDn9iCzaGlwJ86WCM4Ut1cn3i0xBcgUwxUI3";
    const TEST_SUMS: &str = "\
33655c6e900f3794f9019e66c17a845ae606ea3c4c39eaad602a1ca59082f60e  noren.exe
662cce162359efe8c1eb551ba8e28b9e909856b7576250cbac6985cf5eb1d6bb  noren-studio.exe
";
    /// `rsign sign -t "noren v9.9.9"` over [`TEST_SUMS`] with the test key.
    const TEST_SIGNATURE: &str = "\
untrusted comment: signature from rsign secret key
RUQZaWFx8i/6QqJFU7lweGCvLUMxAzKIZtLtfnPLPZcCiqOLSNZyR/NZYfgFTnEq81wzh4q+XWLMK++VbGmn21/J2O79/ZB0mAs=
trusted comment: noren v9.9.9
SCmxYKS9V+UnmVKzWbiZFvLH0jvLOzI7J6Vgns8eU5ASuglMondFinv3EVvGhe9DJp+93iQIapjKwEWX0Fc9AA==
";

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("noren-update-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn versions_parse_and_order() {
        assert_eq!(Version::parse("v1.2.3"), Some(Version(1, 2, 3)));
        assert_eq!(Version::parse("0.10.0"), Some(Version(0, 10, 0)));
        for bad in ["1.2", "1.2.3.4", "1.2.3-rc.1", "v", "1..3", "+1.2.3", "a.b.c"] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
        assert!(Version(0, 10, 0) > Version(0, 9, 9));
        assert!(Version(1, 0, 0) > Version(0, 99, 99));
        assert_eq!(Version::current().to_string(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn releases_parse_from_the_github_api() {
        let json = r#"{"tag_name":"v0.2.0","html_url":"https://github.com/KayZedd/noren/releases/tag/v0.2.0",
            "draft":false,"prerelease":false,"body":"notes",
            "assets":[{"name":"noren.exe","browser_download_url":"https://x/noren.exe","size":1}]}"#;
        let r = Release::parse(json).unwrap();
        assert_eq!(r.version, Version(0, 2, 0));
        assert_eq!(r.asset("noren.exe").unwrap(), "https://x/noren.exe");
        assert!(r.asset("SHA256SUMS").unwrap_err().contains("has no SHA256SUMS"));
        assert_eq!(r.trusted_comment(), "noren v0.2.0");
        let pre = json.replace(r#""prerelease":false"#, r#""prerelease":true"#);
        assert!(Release::parse(&pre).is_err());
        let odd = json.replace("v0.2.0\",\"html", "nightly\",\"html");
        assert!(Release::parse(&odd).unwrap_err().contains("not a version"));
    }

    #[test]
    fn sums_parse_both_sha256sum_styles() {
        let text = format!("{}  a.exe\n{} *b.exe\n\n", "00".repeat(32), "ff".repeat(32));
        let sums = parse_sums(&text).unwrap();
        assert_eq!(sums["a.exe"], [0; 32]);
        assert_eq!(sums["b.exe"], [255; 32]);
        for bad in [
            "zz  a.exe",
            &format!("{}  ", "00".repeat(32)),
            &format!("{}a.exe", "00".repeat(32)),
        ] {
            assert!(parse_sums(bad).is_err(), "{bad:?}");
        }
        let twice = format!("{}  a.exe\n{}  a.exe\n", "00".repeat(32), "11".repeat(32));
        assert!(parse_sums(&twice).unwrap_err().contains("twice"));
    }

    #[test]
    fn signed_sums_verify_and_check_downloads() {
        let sums = verify_sums(TEST_SUMS.as_bytes(), TEST_SIGNATURE, TEST_KEY, "noren v9.9.9").unwrap();
        assert!(check_download("noren.exe", b"noren.exe\n", &sums).is_ok());
        assert!(check_download("noren-studio.exe", b"studio\n", &sums).is_ok());
        let tampered = check_download("noren.exe", b"noren.exe!\n", &sums).unwrap_err();
        assert!(tampered.contains("does not match"), "{tampered}");
        assert!(check_download("other.exe", b"", &sums).is_err());
    }

    #[test]
    fn a_changed_sum_file_fails_the_signature() {
        let changed = TEST_SUMS.replacen("33655c", "33655d", 1);
        assert!(verify_sums(changed.as_bytes(), TEST_SIGNATURE, TEST_KEY, "noren v9.9.9").is_err());
    }

    #[test]
    fn a_signature_for_another_release_or_key_fails() {
        let other_tag =
            verify_sums(TEST_SUMS.as_bytes(), TEST_SIGNATURE, TEST_KEY, "noren v10.0.0").unwrap_err();
        assert!(other_tag.contains("not \"noren v10.0.0\""), "{other_tag}");
        // The real release key did not sign the test sums.
        assert!(verify_sums(TEST_SUMS.as_bytes(), TEST_SIGNATURE, PUBLIC_KEY, "noren v9.9.9").is_err());
    }

    #[test]
    fn the_release_key_decodes() {
        assert!(minisign_verify::PublicKey::from_base64(PUBLIC_KEY).is_ok());
    }

    #[test]
    fn installed_programs_are_the_running_one_and_its_neighbours() {
        let dir = tmp("installed");
        let exe = dir.join("noren.exe");
        assert_eq!(installed(&exe), std::slice::from_ref(&exe));
        std::fs::write(dir.join("noren-studio.exe"), b"").unwrap();
        std::fs::write(dir.join("other.exe"), b"").unwrap();
        assert_eq!(installed(&exe), [exe.clone(), dir.join("noren-studio.exe")]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn replace_swaps_the_file_and_clears_leftovers() {
        let dir = tmp("replace");
        let path = dir.join("noren.exe");
        std::fs::write(&path, b"old").unwrap();
        std::fs::write(with_suffix(&path, ".old"), b"from an earlier update").unwrap();
        replace(&path, b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert!(!with_suffix(&path, ".old").exists());
        assert!(!with_suffix(&path, ".new").exists());
        // A program that was not there yet is written fresh.
        replace(&dir.join("noren-studio.exe"), b"studio").unwrap();
        assert_eq!(std::fs::read(dir.join("noren-studio.exe")).unwrap(), b"studio");
        let _ = std::fs::remove_dir_all(dir);
    }
}
