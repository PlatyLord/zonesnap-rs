use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
    MOD_SHIFT, MOD_WIN, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_LWIN, VK_MBUTTON, VK_MENU,
    VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP, VK_XBUTTON1, VK_XBUTTON2,
};

pub const HOTKEY_ACTIVATE_OVERLAY: i32 = 1;
pub const HOTKEY_OPEN_SETTINGS: i32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HotkeySpec {
    pub modifiers: Vec<String>,
    pub key: String,
}

impl HotkeySpec {
    pub fn to_win32(&self) -> Option<(HOT_KEY_MODIFIERS, u32)> {
        let mut mods = MOD_NOREPEAT;
        for m in &self.modifiers {
            mods |= parse_modifier_str(m)?;
        }
        let vk = parse_vk_str(&self.key)?;
        Some((mods, vk))
    }
}

pub fn parse_modifier_str(s: &str) -> Option<HOT_KEY_MODIFIERS> {
    match s.trim().to_ascii_lowercase().as_str() {
        "win" | "windows" | "super" => Some(MOD_WIN),
        "alt" => Some(MOD_ALT),
        "ctrl" | "control" => Some(MOD_CONTROL),
        "shift" => Some(MOD_SHIFT),
        _ => None,
    }
}

pub fn parse_vk_str(s: &str) -> Option<u32> {
    let norm = s.trim().to_ascii_uppercase();
    if norm.len() == 1 {
        let b = norm.as_bytes()[0];
        if b.is_ascii_uppercase() || b.is_ascii_digit() {
            return Some(b as u32);
        }
    }
    if let Some(num) = norm.strip_prefix('F') {
        if let Ok(n) = num.parse::<u32>() {
            if (1..=12).contains(&n) {
                return Some(0x70 + (n - 1));
            }
        }
    }
    match norm.as_str() {
        "SPACE" => Some(VK_SPACE.0 as u32),
        "TAB" => Some(VK_TAB.0 as u32),
        "ENTER" | "RETURN" => Some(VK_RETURN.0 as u32),
        "ESC" | "ESCAPE" => Some(VK_ESCAPE.0 as u32),
        "LEFT" => Some(VK_LEFT.0 as u32),
        "RIGHT" => Some(VK_RIGHT.0 as u32),
        "UP" => Some(VK_UP.0 as u32),
        "DOWN" => Some(VK_DOWN.0 as u32),
        _ => None,
    }
}

pub fn modifier_to_vk(s: &str) -> i32 {
    match s.trim().to_ascii_lowercase().as_str() {
        "ctrl" | "control" => VK_CONTROL.0 as i32,
        "shift" => VK_SHIFT.0 as i32,
        "win" | "windows" | "super" => VK_LWIN.0 as i32,
        "mouse back" | "mouse4" | "xbutton1" | "back" => VK_XBUTTON1.0 as i32,
        "mouse forward" | "mouse5" | "xbutton2" | "forward" => VK_XBUTTON2.0 as i32,
        "middle click" | "middle" | "mbutton" | "mouse3" => VK_MBUTTON.0 as i32,
        _ => VK_MENU.0 as i32,
    }
}

/// Returns true if `vk` represents a mouse button modifier (`VK_MBUTTON`, `VK_XBUTTON1`, `VK_XBUTTON2`).
pub fn is_mouse_vk(vk: i32) -> bool {
    vk == VK_MBUTTON.0 as i32 || vk == VK_XBUTTON1.0 as i32 || vk == VK_XBUTTON2.0 as i32
}

/// Convert an optional modifier string (`"Shift"`, `"Disabled"`, etc.) to a Win32 VK (`0` if disabled).
pub fn optional_modifier_to_vk(s: &str) -> i32 {
    match s.trim().to_ascii_lowercase().as_str() {
        "" | "none" | "disabled" => 0,
        _ => modifier_to_vk(s),
    }
}

pub struct HotkeyManager {
    registered_ids: Vec<i32>,
}

impl HotkeyManager {
    pub fn new() -> Self {
        Self {
            registered_ids: Vec::new(),
        }
    }

    pub fn register_all(&mut self, hwnd: HWND, activate: &HotkeySpec, settings: &HotkeySpec) {
        self.unregister_all(hwnd);

        if let Some((mods, vk)) = activate.to_win32() {
            unsafe {
                if RegisterHotKey(hwnd, HOTKEY_ACTIVATE_OVERLAY, mods, vk).is_ok() {
                    self.registered_ids.push(HOTKEY_ACTIVATE_OVERLAY);
                }
            }
        }
        if let Some((mods, vk)) = settings.to_win32() {
            unsafe {
                if RegisterHotKey(hwnd, HOTKEY_OPEN_SETTINGS, mods, vk).is_ok() {
                    self.registered_ids.push(HOTKEY_OPEN_SETTINGS);
                }
            }
        }
    }

    pub fn unregister_all(&mut self, hwnd: HWND) {
        for id in self.registered_ids.drain(..) {
            unsafe {
                let _ = UnregisterHotKey(hwnd, id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkey_parsers_resolve_modifiers_and_keys() {
        assert_eq!(parse_modifier_str("Win"), Some(MOD_WIN));
        assert_eq!(parse_modifier_str("alt"), Some(MOD_ALT));
        assert_eq!(parse_modifier_str("Ctrl"), Some(MOD_CONTROL));
        assert_eq!(parse_modifier_str("SHIFT"), Some(MOD_SHIFT));
        assert_eq!(parse_modifier_str("Unknown"), None);

        assert_eq!(parse_vk_str("Z"), Some(b'Z' as u32));
        assert_eq!(parse_vk_str("e"), Some(b'E' as u32));
        assert_eq!(parse_vk_str("F1"), Some(0x70));
        assert_eq!(parse_vk_str("F12"), Some(0x7B));
        assert_eq!(parse_vk_str("Space"), Some(VK_SPACE.0 as u32));

        assert_eq!(modifier_to_vk("Alt"), VK_MENU.0 as i32);
        assert_eq!(modifier_to_vk("Ctrl"), VK_CONTROL.0 as i32);
        assert_eq!(modifier_to_vk("Shift"), VK_SHIFT.0 as i32);
        assert_eq!(modifier_to_vk("Win"), VK_LWIN.0 as i32);
        assert_eq!(modifier_to_vk("Mouse Back"), VK_XBUTTON1.0 as i32);
        assert_eq!(modifier_to_vk("XButton1"), VK_XBUTTON1.0 as i32);
        assert_eq!(modifier_to_vk("Mouse Forward"), VK_XBUTTON2.0 as i32);
        assert_eq!(modifier_to_vk("XButton2"), VK_XBUTTON2.0 as i32);
        assert_eq!(modifier_to_vk("Middle Click"), VK_MBUTTON.0 as i32);
        assert_eq!(modifier_to_vk("Middle"), VK_MBUTTON.0 as i32);
        assert!(is_mouse_vk(VK_XBUTTON1.0 as i32));
        assert!(is_mouse_vk(VK_XBUTTON2.0 as i32));
        assert!(is_mouse_vk(VK_MBUTTON.0 as i32));
        assert!(!is_mouse_vk(VK_MENU.0 as i32));
        assert!(!is_mouse_vk(VK_CONTROL.0 as i32));
        assert!(!is_mouse_vk(VK_SHIFT.0 as i32));
        assert!(!is_mouse_vk(VK_LWIN.0 as i32));
        assert!(!is_mouse_vk(0));
        assert_eq!(optional_modifier_to_vk("Disabled"), 0);
        assert_eq!(optional_modifier_to_vk("Shift"), VK_SHIFT.0 as i32);

        let spec = HotkeySpec {
            modifiers: vec!["Win".to_string(), "Alt".to_string()],
            key: "Z".to_string(),
        };
        let (mods, vk) = spec.to_win32().expect("valid hotkey spec");
        assert_eq!(mods, MOD_NOREPEAT | MOD_WIN | MOD_ALT);
        assert_eq!(vk, b'Z' as u32);
    }
}
