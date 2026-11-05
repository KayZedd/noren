//! Where slot bytes live: the real system, or memory for tests.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;

use super::Slot;

pub trait Store {
    /// Current bytes, `None` when the value or file does not exist.
    fn read(&self, slot: &Slot) -> io::Result<Option<Vec<u8>>>;
    /// `None` deletes. Files get their parent directories created.
    fn write(&mut self, slot: &Slot, value: Option<&[u8]>) -> io::Result<()>;
    /// Called once after a batch of writes so running programs pick the changes up.
    fn settle(&mut self) {}
}

#[derive(Default)]
pub struct MemStore {
    values: HashMap<Slot, Vec<u8>>,
    failing: HashSet<Slot>,
}

impl MemStore {
    pub fn get(&self, slot: &Slot) -> Option<Vec<u8>> {
        self.values.get(slot).cloned()
    }

    pub fn set(&mut self, slot: &Slot, value: Option<Vec<u8>>) {
        match value {
            Some(v) => self.values.insert(slot.clone(), v),
            None => self.values.remove(slot),
        };
    }

    /// Makes writes to `slot` fail, to test partial-apply recovery.
    pub fn fail_on(&mut self, slot: &Slot) {
        self.failing.insert(slot.clone());
    }
}

impl Store for MemStore {
    fn read(&self, slot: &Slot) -> io::Result<Option<Vec<u8>>> {
        Ok(self.get(slot))
    }

    fn write(&mut self, slot: &Slot, value: Option<&[u8]>) -> io::Result<()> {
        if self.failing.contains(slot) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"));
        }
        self.set(slot, value.map(<[u8]>::to_vec));
        Ok(())
    }
}

/// Files through `std::fs`; registry through Win32 (on other systems registry slots fail with
/// `Unsupported`, file slots work, which is what the tests and Linux dotfiles use).
#[derive(Default)]
pub struct SystemStore {
    registry_touched: bool,
}

impl Store for SystemStore {
    fn read(&self, slot: &Slot) -> io::Result<Option<Vec<u8>>> {
        match slot {
            Slot::File { path } => match fs::read(path) {
                Ok(v) => Ok(Some(v)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e),
            },
            Slot::Registry { key, name, .. } => registry::read(key, name),
        }
    }

    fn write(&mut self, slot: &Slot, value: Option<&[u8]>) -> io::Result<()> {
        match slot {
            Slot::File { path } => match value {
                Some(v) => {
                    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                        fs::create_dir_all(parent)?;
                    }
                    fs::write(path, v)
                }
                None => match fs::remove_file(path) {
                    Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                    other => other,
                },
            },
            Slot::Registry { key, name, kind } => {
                self.registry_touched = true;
                registry::write(key, name, *kind, value)
            }
        }
    }

    fn settle(&mut self) {
        if std::mem::take(&mut self.registry_touched) {
            registry::broadcast_change();
        }
    }
}

#[cfg(windows)]
pub(crate) mod registry {
    use std::io;

    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, LPARAM, WIN32_ERROR, WPARAM};
    use windows::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_BINARY, REG_DWORD, REG_VALUE_TYPE, RRF_RT_ANY, RegDeleteKeyValueW,
        RegGetValueW, RegSetKeyValueW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
    };
    use windows::core::{HSTRING, PCWSTR, w};

    use crate::rice::RegKind;

    fn check(status: WIN32_ERROR) -> io::Result<()> {
        if status.is_ok() {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(status.0 as i32))
        }
    }

    pub fn read(key: &str, name: &str) -> io::Result<Option<Vec<u8>>> {
        let (key, name) = (HSTRING::from(key), HSTRING::from(name));
        let mut len = 0u32;
        let mut kind = REG_VALUE_TYPE::default();
        // SAFETY: size query with a null buffer, then a read into a buffer of that size.
        unsafe {
            let status = RegGetValueW(
                HKEY_CURRENT_USER,
                &key,
                &name,
                RRF_RT_ANY,
                Some(&mut kind),
                None,
                Some(&mut len),
            );
            if status == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            check(status)?;
            let mut buf = vec![0u8; len as usize];
            check(RegGetValueW(
                HKEY_CURRENT_USER,
                &key,
                &name,
                RRF_RT_ANY,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut len),
            ))?;
            buf.truncate(len as usize);
            Ok(Some(buf))
        }
    }

    pub fn write(key: &str, name: &str, kind: RegKind, value: Option<&[u8]>) -> io::Result<()> {
        let (key, name) = (HSTRING::from(key), HSTRING::from(name));
        // SAFETY: data pointer and length come from the same slice.
        unsafe {
            match value {
                Some(v) => {
                    let kind = match kind {
                        RegKind::Dword => REG_DWORD,
                        RegKind::Binary => REG_BINARY,
                    };
                    check(RegSetKeyValueW(
                        HKEY_CURRENT_USER,
                        &key,
                        &name,
                        kind.0,
                        Some(v.as_ptr().cast()),
                        v.len() as u32,
                    ))
                }
                None => {
                    let status = RegDeleteKeyValueW(HKEY_CURRENT_USER, &key, &name);
                    if status == ERROR_FILE_NOT_FOUND {
                        Ok(())
                    } else {
                        check(status)
                    }
                }
            }
        }
    }

    /// Tells Explorer, DWM and apps that the color set changed, as Settings does.
    pub fn broadcast_change() {
        let area: PCWSTR = w!("ImmersiveColorSet");
        // SAFETY: `area` is a static wide string that outlives the call.
        unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                WPARAM(0),
                LPARAM(area.as_ptr() as isize),
                SMTO_ABORTIFHUNG,
                1000,
                None,
            );
        }
    }
}

#[cfg(not(windows))]
pub(crate) mod registry {
    use std::io;

    use crate::rice::RegKind;

    fn unsupported() -> io::Error {
        io::Error::new(io::ErrorKind::Unsupported, "the registry exists only on Windows")
    }

    pub fn read(_: &str, _: &str) -> io::Result<Option<Vec<u8>>> {
        Err(unsupported())
    }

    pub fn write(_: &str, _: &str, _: RegKind, _: Option<&[u8]>) -> io::Result<()> {
        Err(unsupported())
    }

    pub fn broadcast_change() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_store_files_read_absent_write_and_delete() {
        let dir = std::env::temp_dir().join(format!("noren-store-{}", std::process::id()));
        let slot = Slot::File {
            path: dir.join("nested/file.txt"),
        };
        let mut s = SystemStore::default();
        assert_eq!(s.read(&slot).unwrap(), None);
        s.write(&slot, Some(b"hi")).unwrap();
        assert_eq!(s.read(&slot).unwrap(), Some(b"hi".to_vec()));
        s.write(&slot, None).unwrap();
        s.write(&slot, None).unwrap();
        assert_eq!(s.read(&slot).unwrap(), None);
        fs::remove_dir_all(dir).unwrap();
    }
}
