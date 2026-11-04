//! Transactional ricing: every change to the system is a write to a [`Slot`] (a registry value
//! or a file), recorded with its previous bytes before it happens. Any applied profile can be
//! undone exactly, and nothing a user changed afterwards is overwritten by the undo.
//!
//! ```text
//! profile.toml ──changes()──▶ [Change] ──plan()──▶ only slots that differ
//!                                         │
//!                              apply(): journal written first (pending),
//!                              then each slot; a failure undoes the slots
//!                              already written; journal marked applied
//!                                         │
//!                              rollback(): newest live transaction, each
//!                              slot restored only if it still holds what
//!                              we wrote (else reported as a conflict)
//! ```

mod merge;
mod profile;
mod store;

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub use merge::{JsonEdit, merge_jsonc, replace_block};
pub use profile::{Profile, SCHEMA};
use serde::{Deserialize, Serialize};
pub use store::{MemStore, Store, SystemStore};

/// One place on the system that holds bytes.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Slot {
    /// A value under `HKEY_CURRENT_USER\{key}`. Only per-user state: no admin rights, nothing
    /// that affects other accounts.
    Registry {
        key: String,
        name: String,
        kind: RegKind,
    },
    File {
        path: PathBuf,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RegKind {
    /// Four little-endian bytes.
    Dword,
    Binary,
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Registry { key, name, .. } => write!(f, r"HKCU\{key}\{name}"),
            Self::File { path } => write!(f, "{}", path.display()),
        }
    }
}

/// Desired state of a slot. `value: None` means "absent" (value deleted, file removed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub label: String,
    pub slot: Slot,
    pub value: Option<Vec<u8>>,
}

impl Change {
    pub fn dword(label: &str, key: &str, name: &str, value: u32) -> Self {
        Self {
            label: label.into(),
            slot: Slot::Registry {
                key: key.into(),
                name: name.into(),
                kind: RegKind::Dword,
            },
            value: Some(value.to_le_bytes().to_vec()),
        }
    }

    pub fn binary(label: &str, key: &str, name: &str, value: &[u8]) -> Self {
        Self {
            label: label.into(),
            slot: Slot::Registry {
                key: key.into(),
                name: name.into(),
                kind: RegKind::Binary,
            },
            value: Some(value.to_vec()),
        }
    }

    pub fn file(label: &str, path: impl Into<PathBuf>, contents: impl Into<Vec<u8>>) -> Self {
        Self {
            label: label.into(),
            slot: Slot::File { path: path.into() },
            value: Some(contents.into()),
        }
    }
}

/// A recorded slot write.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub label: String,
    pub slot: Slot,
    #[serde(with = "hex_bytes")]
    pub before: Option<Vec<u8>>,
    #[serde(with = "hex_bytes")]
    pub after: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Written before the first slot changes. A pending transaction found later means the
    /// process died mid-apply; rollback handles it like an applied one.
    Pending,
    Applied,
    RolledBack,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transaction {
    pub id: u64,
    pub profile: String,
    pub created_unix: u64,
    pub status: Status,
    pub entries: Vec<Entry>,
}

/// Transactions stored one JSON file each, `000001.json` upward.
pub struct Journal {
    dir: PathBuf,
}

impl Journal {
    pub fn open(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    /// `%LOCALAPPDATA%\noren\journal` on Windows, `$XDG_STATE_HOME/noren/journal` (or
    /// `~/.local/state/noren/journal`) elsewhere.
    pub fn default_dir() -> Option<PathBuf> {
        let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let base = if cfg!(windows) {
            env("LOCALAPPDATA")?
        } else {
            env("XDG_STATE_HOME").or_else(|| env("HOME").map(|h| h.join(".local/state")))?
        };
        Some(base.join("noren").join("journal"))
    }

    fn path(&self, id: u64) -> PathBuf {
        self.dir.join(format!("{id:06}.json"))
    }

    /// All transactions, oldest first.
    pub fn list(&self) -> io::Result<Vec<Transaction>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "json") {
                out.push(read_transaction(&path)?);
            }
        }
        out.sort_by_key(|t| t.id);
        Ok(out)
    }

    fn save(&self, t: &Transaction) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(t).map_err(io::Error::other)?;
        // Write-then-rename so a crash never leaves a half-written journal file.
        let tmp = self.dir.join(format!("{:06}.tmp", t.id));
        fs::write(&tmp, json)?;
        fs::rename(tmp, self.path(t.id))
    }
}

fn read_transaction(path: &Path) -> io::Result<Transaction> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", path.display())))
}

/// Changes whose slot currently holds something else, with the current bytes as `before`.
/// Duplicate slots: the last change wins.
pub fn plan(store: &dyn Store, changes: &[Change]) -> io::Result<Vec<Entry>> {
    let mut out: Vec<Entry> = Vec::new();
    for change in changes {
        out.retain(|e| e.slot != change.slot);
        let before = store.read(&change.slot)?;
        if before != change.value {
            out.push(Entry {
                label: change.label.clone(),
                slot: change.slot.clone(),
                before,
                after: change.value.clone(),
            });
        }
    }
    Ok(out)
}

/// Applies `changes` as one transaction. Returns `Ok(None)` when the system already matches.
pub fn apply(
    store: &mut dyn Store,
    journal: &Journal,
    profile: &str,
    changes: &[Change],
) -> io::Result<Option<Transaction>> {
    let entries = plan(store, changes)?;
    if entries.is_empty() {
        return Ok(None);
    }
    let id = journal.list()?.last().map_or(1, |t| t.id + 1);
    let mut t = Transaction {
        id,
        profile: profile.into(),
        created_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
        status: Status::Pending,
        entries,
    };
    journal.save(&t)?;
    for (done, entry) in t.entries.iter().enumerate() {
        if let Err(e) = store.write(&entry.slot, entry.after.as_deref()) {
            let undo: Vec<String> = t.entries[..done]
                .iter()
                .rev()
                .filter_map(|prev| store.write(&prev.slot, prev.before.as_deref()).err())
                .map(|e| e.to_string())
                .collect();
            t.status = Status::RolledBack;
            journal.save(&t)?;
            let mut msg = format!("{}: {e}; earlier changes undone", entry.slot);
            if !undo.is_empty() {
                msg += &format!(" except: {}", undo.join("; "));
            }
            return Err(io::Error::new(e.kind(), msg));
        }
    }
    store.settle();
    t.status = Status::Applied;
    journal.save(&t)?;
    Ok(Some(t))
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct RollbackReport {
    pub transaction: Option<u64>,
    pub restored: Vec<Slot>,
    /// Slots changed by someone else since we wrote them; left alone unless forced.
    pub conflicts: Vec<Slot>,
}

/// Undoes the newest transaction that is applied (or was interrupted while pending).
pub fn rollback(store: &mut dyn Store, journal: &Journal, force: bool) -> io::Result<RollbackReport> {
    let Some(mut t) = journal
        .list()?
        .into_iter()
        .rev()
        .find(|t| t.status != Status::RolledBack)
    else {
        return Ok(RollbackReport::default());
    };
    let mut report = RollbackReport {
        transaction: Some(t.id),
        ..RollbackReport::default()
    };
    for entry in t.entries.iter().rev() {
        let current = store.read(&entry.slot)?;
        if current == entry.before {
            continue;
        }
        if current != entry.after && !force {
            report.conflicts.push(entry.slot.clone());
            continue;
        }
        store.write(&entry.slot, entry.before.as_deref())?;
        report.restored.push(entry.slot.clone());
    }
    store.settle();
    t.status = Status::RolledBack;
    journal.save(&t)?;
    Ok(report)
}

/// Human-readable size or value of slot contents, for `diff` output.
pub fn describe(slot: &Slot, value: Option<&[u8]>) -> String {
    match (slot, value) {
        (_, None) => "absent".into(),
        (
            Slot::Registry {
                kind: RegKind::Dword, ..
            },
            Some(v),
        ) if v.len() == 4 => {
            format!("0x{:08x}", u32::from_le_bytes([v[0], v[1], v[2], v[3]]))
        }
        (Slot::Registry { .. }, Some(v)) => hex_bytes::encode(v),
        (Slot::File { .. }, Some(v)) => format!("{} bytes", v.len()),
    }
}

mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn serialize<S: Serializer>(v: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(bytes) => s.serialize_some(&encode(bytes)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        let Some(hex) = Option::<String>::deserialize(d)? else {
            return Ok(None);
        };
        if hex.len() % 2 != 0 {
            return Err(D::Error::custom("odd hex length"));
        }
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(D::Error::custom))
            .collect::<Result<_, _>>()
            .map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn journal() -> (Journal, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "noren-journal-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        (Journal::open(&dir).unwrap(), dir)
    }

    fn reg(name: &str, v: u32) -> Change {
        Change::dword(name, r"Software\Test", name, v)
    }

    #[test]
    fn apply_then_rollback_restores_previous_values_including_absent_ones() {
        let (j, dir) = journal();
        let mut store = MemStore::default();
        store.set(&reg("a", 0).slot, Some(1u32.to_le_bytes().to_vec()));
        let t = apply(&mut store, &j, "p", &[reg("a", 7), reg("b", 9)])
            .unwrap()
            .unwrap();
        assert_eq!(t.entries.len(), 2);
        assert_eq!(store.get(&reg("b", 0).slot), Some(9u32.to_le_bytes().to_vec()));
        assert_eq!(
            apply(&mut store, &j, "p", &[reg("a", 7)]).unwrap(),
            None,
            "no-op apply"
        );

        let report = rollback(&mut store, &j, false).unwrap();
        assert_eq!(report.restored.len(), 2);
        assert_eq!(store.get(&reg("a", 0).slot), Some(1u32.to_le_bytes().to_vec()));
        assert_eq!(store.get(&reg("b", 0).slot), None);
        assert_eq!(j.list().unwrap()[0].status, Status::RolledBack);
        assert_eq!(
            rollback(&mut store, &j, false).unwrap(),
            RollbackReport::default()
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rollback_leaves_slots_the_user_changed_afterwards() {
        let (j, dir) = journal();
        let mut store = MemStore::default();
        apply(&mut store, &j, "p", &[reg("a", 7), reg("b", 8)]).unwrap();
        store.set(&reg("a", 0).slot, Some(42u32.to_le_bytes().to_vec()));
        let report = rollback(&mut store, &j, false).unwrap();
        assert_eq!(report.conflicts, vec![reg("a", 0).slot]);
        assert_eq!(store.get(&reg("a", 0).slot), Some(42u32.to_le_bytes().to_vec()));
        assert_eq!(store.get(&reg("b", 0).slot), None);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rollbacks_unwind_transactions_newest_first() {
        let (j, dir) = journal();
        let mut store = MemStore::default();
        apply(&mut store, &j, "one", &[reg("a", 1)]).unwrap();
        apply(&mut store, &j, "two", &[reg("a", 2)]).unwrap();
        rollback(&mut store, &j, false).unwrap();
        assert_eq!(store.get(&reg("a", 0).slot), Some(1u32.to_le_bytes().to_vec()));
        rollback(&mut store, &j, false).unwrap();
        assert_eq!(store.get(&reg("a", 0).slot), None);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_write_undoes_earlier_writes_and_marks_the_transaction() {
        let (j, dir) = journal();
        let mut store = MemStore::default();
        let bad = reg("bad", 3);
        store.fail_on(&bad.slot);
        let err = apply(&mut store, &j, "p", &[reg("a", 1), bad]).unwrap_err();
        assert!(err.to_string().contains("earlier changes undone"), "{err}");
        assert_eq!(store.get(&reg("a", 0).slot), None);
        assert_eq!(j.list().unwrap()[0].status, Status::RolledBack);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn interrupted_apply_is_rolled_back_from_its_pending_journal() {
        let (j, dir) = journal();
        let mut store = MemStore::default();
        let a = reg("a", 5);
        let t = Transaction {
            id: 1,
            profile: "p".into(),
            created_unix: 0,
            status: Status::Pending,
            entries: plan(&store, std::slice::from_ref(&a)).unwrap(),
        };
        j.save(&t).unwrap();
        store.set(&a.slot, a.value.clone()); // the crash happened after this write
        let report = rollback(&mut store, &j, false).unwrap();
        assert_eq!(report.restored, vec![a.slot.clone()]);
        assert_eq!(store.get(&a.slot), None);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn journal_round_trips_bytes_and_absence() {
        let (j, dir) = journal();
        let t = Transaction {
            id: 3,
            profile: "p".into(),
            created_unix: 1,
            status: Status::Applied,
            entries: vec![Entry {
                label: "f".into(),
                slot: Slot::File { path: "x".into() },
                before: None,
                after: Some(vec![0, 255, 16]),
            }],
        };
        j.save(&t).unwrap();
        assert_eq!(j.list().unwrap(), vec![t]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn describe_formats_dwords() {
        let c = reg("a", 0xff00_1234);
        assert_eq!(describe(&c.slot, c.value.as_deref()), "0xff001234");
        assert_eq!(describe(&c.slot, None), "absent");
    }
}
