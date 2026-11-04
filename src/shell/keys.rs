//! Key combos like `"Ctrl+Alt+Space"` and the hold-to-activate state machine behind x-ray.
//! Pure: the Win32 side feeds it virtual-key events from a low-level keyboard hook.

use std::collections::BTreeSet;
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Mod {
    Ctrl,
    Alt,
    Shift,
    Win,
}

impl Mod {
    const ALL: [Mod; 4] = [Mod::Ctrl, Mod::Alt, Mod::Shift, Mod::Win];

    /// Left/right specific keys as a low-level hook reports them, plus the generic code.
    fn vks(self) -> &'static [u32] {
        match self {
            Self::Ctrl => &[0x11, 0xA2, 0xA3],
            Self::Alt => &[0x12, 0xA4, 0xA5],
            Self::Shift => &[0x10, 0xA0, 0xA1],
            Self::Win => &[0x5B, 0x5C],
        }
    }

    /// The code `GetAsyncKeyState` answers for either side.
    pub fn generic_vk(self) -> u32 {
        match self {
            Self::Ctrl => 0x11,
            Self::Alt => 0x12,
            Self::Shift => 0x10,
            Self::Win => 0x5B,
        }
    }

    pub(crate) fn of(vk: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.vks().contains(&vk))
    }

    fn name(self) -> &'static str {
        match self {
            Self::Ctrl => "Ctrl",
            Self::Alt => "Alt",
            Self::Shift => "Shift",
            Self::Win => "Win",
        }
    }
}

/// Modifiers plus at most one other key. `"Ctrl+Win"` (modifiers only) is allowed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Combo {
    pub mods: BTreeSet<Mod>,
    pub key: Option<u32>,
}

const NAMED: [(&str, u32); 14] = [
    ("space", 0x20),
    ("tab", 0x09),
    ("esc", 0x1B),
    ("escape", 0x1B),
    ("enter", 0x0D),
    ("capslock", 0x14),
    ("backquote", 0xC0),
    ("`", 0xC0),
    ("insert", 0x2D),
    ("pause", 0x13),
    ("scrolllock", 0x91),
    ("apps", 0x5D),
    ("menu", 0x5D),
    ("backspace", 0x08),
];

/// The virtual-key code for a key name: a letter or digit, `F1`..`F24`, or a named key.
pub(crate) fn key_vk(name: &str) -> Option<u32> {
    let lower = name.to_ascii_lowercase();
    if let Some(&(_, vk)) = NAMED.iter().find(|(n, _)| *n == lower) {
        return Some(vk);
    }
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next())
        && c.is_ascii_alphanumeric()
    {
        return Some(u32::from(c.to_ascii_uppercase()));
    }
    let n: u32 = lower.strip_prefix('f')?.parse().ok()?;
    (1..=24).contains(&n).then_some(0x70 + n - 1)
}

/// The canonical name of a virtual key, the inverse of [`key_vk`].
pub(crate) fn key_name(vk: u32) -> String {
    match NAMED.iter().find(|(_, v)| *v == vk) {
        Some((n, _)) => n.to_string(),
        None if (0x70..0x88).contains(&vk) => format!("F{}", vk - 0x6F),
        None => char::from_u32(vk).map(String::from).unwrap_or_default(),
    }
}

impl Combo {
    pub fn parse(s: &str) -> Result<Self, String> {
        let bad = |why: &str| format!("bad key combo {s:?}: {why}");
        let mut mods = BTreeSet::new();
        let mut key = None;
        for part in s.split('+').map(str::trim) {
            if part.is_empty() {
                return Err(bad("empty part"));
            }
            let m = match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => Some(Mod::Ctrl),
                "alt" => Some(Mod::Alt),
                "shift" => Some(Mod::Shift),
                "win" | "super" | "meta" => Some(Mod::Win),
                _ => None,
            };
            match m {
                Some(m) if !mods.insert(m) => return Err(bad("modifier repeated")),
                Some(_) => {}
                None if key.is_some() => return Err(bad("more than one non-modifier key")),
                None => key = Some(key_vk(part).ok_or_else(|| bad(&format!("unknown key {part:?}")))?),
            }
        }
        if mods.is_empty() {
            return Err(bad("needs at least one of Ctrl, Alt, Shift, Win"));
        }
        Ok(Self { mods, key })
    }
}

impl fmt::Display for Combo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts: Vec<String> = self.mods.iter().map(|m| m.name().to_string()).collect();
        if let Some(vk) = self.key {
            parts.push(key_name(vk));
        }
        f.write_str(&parts.join("+"))
    }
}

/// What the hook should do with one key event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Step {
    /// Keep the event from reaching the focused app (the combo's non-modifier key while it
    /// is part of the combo, so holding Ctrl+Alt+Space does not type spaces).
    pub swallow: bool,
    /// `Some(true)` when the combo just became held, `Some(false)` when it was just released.
    pub changed: Option<bool>,
}

/// Tracks pressed keys and reports when the combo starts and stops being held. Modifiers must
/// match exactly: Ctrl+Shift+Alt+Space does not trigger Ctrl+Alt+Space.
#[derive(Clone, Debug)]
pub struct Hold {
    combo: Combo,
    down: BTreeSet<u32>,
    active: bool,
    swallowing: bool,
}

impl Hold {
    pub fn new(combo: Combo) -> Self {
        Self {
            combo,
            down: BTreeSet::new(),
            active: false,
            swallowing: false,
        }
    }

    pub fn active(&self) -> bool {
        self.active
    }

    fn mods_held(&self) -> BTreeSet<Mod> {
        self.down.iter().filter_map(|&vk| Mod::of(vk)).collect()
    }

    fn satisfied(&self) -> bool {
        self.mods_held() == self.combo.mods && self.combo.key.is_none_or(|k| self.down.contains(&k))
    }

    pub fn event(&mut self, vk: u32, down: bool) -> Step {
        let is_key = self.combo.key == Some(vk);
        if down {
            self.down.insert(vk);
        } else {
            self.down.remove(&vk);
        }
        let now = self.satisfied();
        let mut step = Step::default();
        if is_key && down && now {
            self.swallowing = true;
        }
        if is_key && self.swallowing {
            step.swallow = true;
            if !down {
                self.swallowing = false;
            }
        }
        if now != self.active {
            self.active = now;
            step.changed = Some(now);
        }
        step
    }

    /// Key-ups get lost (lock screen, UAC prompt, a hook timeout). `is_down` asks the system;
    /// when the combo is not really held any more, everything resets and the release is
    /// reported.
    pub fn verify(&mut self, is_down: impl Fn(u32) -> bool) -> Option<bool> {
        if !self.active {
            return None;
        }
        let held =
            self.combo.mods.iter().all(|m| is_down(m.generic_vk())) && self.combo.key.is_none_or(&is_down);
        if held {
            return None;
        }
        self.down.clear();
        self.active = false;
        self.swallowing = false;
        Some(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LCTRL: u32 = 0xA2;
    const RALT: u32 = 0xA5;
    const LSHIFT: u32 = 0xA0;
    const SPACE: u32 = 0x20;

    #[test]
    fn combos_parse_case_insensitively_and_print_canonically() {
        let c = Combo::parse("alt + ctrl+SPACE").unwrap();
        assert_eq!(c.mods, BTreeSet::from([Mod::Ctrl, Mod::Alt]));
        assert_eq!(c.key, Some(SPACE));
        assert_eq!(c.to_string(), "Ctrl+Alt+space");
        assert_eq!(Combo::parse("Win+F12").unwrap().key, Some(0x7B));
        assert_eq!(Combo::parse("Ctrl+Win").unwrap().key, None);
        assert_eq!(Combo::parse("Ctrl+x").unwrap().key, Some(u32::from(b'X')));
        assert_eq!(Combo::parse("Win+F12").unwrap().to_string(), "Win+F12");
    }

    #[test]
    fn bad_combos_say_why() {
        for (s, why) in [
            ("Space", "at least one"),
            ("Ctrl+Ctrl+A", "repeated"),
            ("Ctrl+A+B", "more than one"),
            ("Ctrl+Nope", "unknown key"),
            ("Ctrl++A", "empty"),
            ("Ctrl+F25", "unknown key"),
        ] {
            let err = Combo::parse(s).unwrap_err();
            assert!(err.contains(why), "{s}: {err}");
        }
    }

    #[test]
    fn hold_activates_on_the_last_key_and_swallows_only_that_key() {
        let mut h = Hold::new(Combo::parse("Ctrl+Alt+Space").unwrap());
        assert_eq!(h.event(LCTRL, true), Step::default());
        assert_eq!(h.event(RALT, true), Step::default());
        assert_eq!(
            h.event(SPACE, true),
            Step {
                swallow: true,
                changed: Some(true)
            }
        );
        assert_eq!(
            h.event(SPACE, true),
            Step {
                swallow: true,
                changed: None
            },
            "auto-repeat"
        );
        assert_eq!(
            h.event(RALT, false),
            Step {
                swallow: false,
                changed: Some(false)
            }
        );
        assert_eq!(
            h.event(SPACE, false),
            Step {
                swallow: true,
                changed: None
            },
            "matching key-up"
        );
        assert_eq!(
            h.event(SPACE, true),
            Step::default(),
            "Ctrl+Space alone is the app's"
        );
    }

    #[test]
    fn extra_modifiers_do_not_trigger() {
        let mut h = Hold::new(Combo::parse("Ctrl+Alt+Space").unwrap());
        for vk in [LCTRL, RALT, LSHIFT] {
            h.event(vk, true);
        }
        assert_eq!(h.event(SPACE, true), Step::default());
        assert!(!h.active());
    }

    #[test]
    fn modifier_only_combo_and_lost_key_up_recovery() {
        let mut h = Hold::new(Combo::parse("Ctrl+Win").unwrap());
        h.event(LCTRL, true);
        assert_eq!(h.event(0x5C, true).changed, Some(true));
        // The real keyboard says Win is up: the hook missed the key-up.
        assert_eq!(h.verify(|vk| vk == 0x11), Some(false));
        assert!(!h.active());
        assert_eq!(h.verify(|_| false), None, "nothing to release twice");
        h.event(LCTRL, true);
        assert_eq!(
            h.event(0x5B, true).changed,
            Some(true),
            "works again after a reset"
        );
        assert_eq!(h.verify(|_| true), None);
    }
}
