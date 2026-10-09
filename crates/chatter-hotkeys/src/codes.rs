//! DOM `KeyboardEvent.code` tables: per-platform key identifiers and human labels.
//!
//! Every platform field lives in the same table (they are plain numbers/strings), so the
//! whole table is unit-tested on every OS even though each backend only reads its column.

/// One supported keyboard key.
#[derive(Debug)]
pub(crate) struct KeyDef {
    /// DOM `KeyboardEvent.code`.
    pub code: &'static str,
    /// Human-readable label shown in the UI.
    pub label: &'static str,
    /// Linux evdev keycode (`KEY_*` in `linux/input-event-codes.h`). X keycode = evdev + 8.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub evdev: u16,
    /// Windows PC/AT set-1 scan code; extended keys carry the `0xE0` prefix (`0xE01D` = Right Ctrl).
    /// This is the identity Chromium uses to derive `code` on Windows, so it is layout independent.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub win_scan: u16,
    /// Windows virtual-key code as reported in `KBDLLHOOKSTRUCT::vkCode` (left/right specific
    /// for modifiers, e.g. `VK_LSHIFT`). Used for injected events that carry no scan code.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub win_vk: u16,
    /// XKB keysym name, used as the preferred trigger for the XDG GlobalShortcuts portal.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub keysym: &'static str,
}

const fn k(
    code: &'static str,
    label: &'static str,
    evdev: u16,
    win_scan: u16,
    win_vk: u16,
    keysym: &'static str,
) -> KeyDef {
    KeyDef {
        code,
        label,
        evdev,
        win_scan,
        win_vk,
        keysym,
    }
}

#[rustfmt::skip]
pub(crate) static KEYS: &[KeyDef] = &[
    // Letters (VK = ASCII uppercase).
    k("KeyA", "A", 30, 0x1E, 0x41, "a"),
    k("KeyB", "B", 48, 0x30, 0x42, "b"),
    k("KeyC", "C", 46, 0x2E, 0x43, "c"),
    k("KeyD", "D", 32, 0x20, 0x44, "d"),
    k("KeyE", "E", 18, 0x12, 0x45, "e"),
    k("KeyF", "F", 33, 0x21, 0x46, "f"),
    k("KeyG", "G", 34, 0x22, 0x47, "g"),
    k("KeyH", "H", 35, 0x23, 0x48, "h"),
    k("KeyI", "I", 23, 0x17, 0x49, "i"),
    k("KeyJ", "J", 36, 0x24, 0x4A, "j"),
    k("KeyK", "K", 37, 0x25, 0x4B, "k"),
    k("KeyL", "L", 38, 0x26, 0x4C, "l"),
    k("KeyM", "M", 50, 0x32, 0x4D, "m"),
    k("KeyN", "N", 49, 0x31, 0x4E, "n"),
    k("KeyO", "O", 24, 0x18, 0x4F, "o"),
    k("KeyP", "P", 25, 0x19, 0x50, "p"),
    k("KeyQ", "Q", 16, 0x10, 0x51, "q"),
    k("KeyR", "R", 19, 0x13, 0x52, "r"),
    k("KeyS", "S", 31, 0x1F, 0x53, "s"),
    k("KeyT", "T", 20, 0x14, 0x54, "t"),
    k("KeyU", "U", 22, 0x16, 0x55, "u"),
    k("KeyV", "V", 47, 0x2F, 0x56, "v"),
    k("KeyW", "W", 17, 0x11, 0x57, "w"),
    k("KeyX", "X", 45, 0x2D, 0x58, "x"),
    k("KeyY", "Y", 21, 0x15, 0x59, "y"),
    k("KeyZ", "Z", 44, 0x2C, 0x5A, "z"),
    // Digit row.
    k("Digit1", "1", 2, 0x02, 0x31, "1"),
    k("Digit2", "2", 3, 0x03, 0x32, "2"),
    k("Digit3", "3", 4, 0x04, 0x33, "3"),
    k("Digit4", "4", 5, 0x05, 0x34, "4"),
    k("Digit5", "5", 6, 0x06, 0x35, "5"),
    k("Digit6", "6", 7, 0x07, 0x36, "6"),
    k("Digit7", "7", 8, 0x08, 0x37, "7"),
    k("Digit8", "8", 9, 0x09, 0x38, "8"),
    k("Digit9", "9", 10, 0x0A, 0x39, "9"),
    k("Digit0", "0", 11, 0x0B, 0x30, "0"),
    // Function keys.
    k("F1", "F1", 59, 0x3B, 0x70, "F1"),
    k("F2", "F2", 60, 0x3C, 0x71, "F2"),
    k("F3", "F3", 61, 0x3D, 0x72, "F3"),
    k("F4", "F4", 62, 0x3E, 0x73, "F4"),
    k("F5", "F5", 63, 0x3F, 0x74, "F5"),
    k("F6", "F6", 64, 0x40, 0x75, "F6"),
    k("F7", "F7", 65, 0x41, 0x76, "F7"),
    k("F8", "F8", 66, 0x42, 0x77, "F8"),
    k("F9", "F9", 67, 0x43, 0x78, "F9"),
    k("F10", "F10", 68, 0x44, 0x79, "F10"),
    k("F11", "F11", 87, 0x57, 0x7A, "F11"),
    k("F12", "F12", 88, 0x58, 0x7B, "F12"),
    k("F13", "F13", 183, 0x64, 0x7C, "F13"),
    k("F14", "F14", 184, 0x65, 0x7D, "F14"),
    k("F15", "F15", 185, 0x66, 0x7E, "F15"),
    k("F16", "F16", 186, 0x67, 0x7F, "F16"),
    k("F17", "F17", 187, 0x68, 0x80, "F17"),
    k("F18", "F18", 188, 0x69, 0x81, "F18"),
    k("F19", "F19", 189, 0x6A, 0x82, "F19"),
    k("F20", "F20", 190, 0x6B, 0x83, "F20"),
    k("F21", "F21", 191, 0x6C, 0x84, "F21"),
    k("F22", "F22", 192, 0x6D, 0x85, "F22"),
    k("F23", "F23", 193, 0x6E, 0x86, "F23"),
    k("F24", "F24", 194, 0x76, 0x87, "F24"),
    // Punctuation (US positions; `code` is positional so these are layout independent).
    k("Backquote", "`", 41, 0x29, 0xC0, "grave"),
    k("Minus", "-", 12, 0x0C, 0xBD, "minus"),
    k("Equal", "=", 13, 0x0D, 0xBB, "equal"),
    k("BracketLeft", "[", 26, 0x1A, 0xDB, "bracketleft"),
    k("BracketRight", "]", 27, 0x1B, 0xDD, "bracketright"),
    k("Backslash", "\\", 43, 0x2B, 0xDC, "backslash"),
    k("Semicolon", ";", 39, 0x27, 0xBA, "semicolon"),
    k("Quote", "'", 40, 0x28, 0xDE, "apostrophe"),
    k("Comma", ",", 51, 0x33, 0xBC, "comma"),
    k("Period", ".", 52, 0x34, 0xBE, "period"),
    k("Slash", "/", 53, 0x35, 0xBF, "slash"),
    k("IntlBackslash", "ISO \\", 86, 0x56, 0xE2, "less"),
    // Whitespace / editing.
    k("Space", "Space", 57, 0x39, 0x20, "space"),
    k("Tab", "Tab", 15, 0x0F, 0x09, "Tab"),
    k("CapsLock", "Caps Lock", 58, 0x3A, 0x14, "Caps_Lock"),
    k("Enter", "Enter", 28, 0x1C, 0x0D, "Return"),
    k("Backspace", "Backspace", 14, 0x0E, 0x08, "BackSpace"),
    k("Escape", "Esc", 1, 0x01, 0x1B, "Escape"),
    // Modifiers (left/right distinct).
    k("ShiftLeft", "Left Shift", 42, 0x2A, 0xA0, "Shift_L"),
    k("ShiftRight", "Right Shift", 54, 0x36, 0xA1, "Shift_R"),
    k("ControlLeft", "Left Ctrl", 29, 0x1D, 0xA2, "Control_L"),
    k("ControlRight", "Right Ctrl", 97, 0xE01D, 0xA3, "Control_R"),
    k("AltLeft", "Left Alt", 56, 0x38, 0xA4, "Alt_L"),
    k("AltRight", "Right Alt", 100, 0xE038, 0xA5, "Alt_R"),
    k("MetaLeft", META_LEFT_LABEL, 125, 0xE05B, 0x5B, "Super_L"),
    k("MetaRight", META_RIGHT_LABEL, 126, 0xE05C, 0x5C, "Super_R"),
    k("ContextMenu", "Menu", 127, 0xE05D, 0x5D, "Menu"),
    // Navigation.
    k("ArrowUp", "Up Arrow", 103, 0xE048, 0x26, "Up"),
    k("ArrowDown", "Down Arrow", 108, 0xE050, 0x28, "Down"),
    k("ArrowLeft", "Left Arrow", 105, 0xE04B, 0x25, "Left"),
    k("ArrowRight", "Right Arrow", 106, 0xE04D, 0x27, "Right"),
    k("Insert", "Insert", 110, 0xE052, 0x2D, "Insert"),
    k("Delete", "Delete", 111, 0xE053, 0x2E, "Delete"),
    k("Home", "Home", 102, 0xE047, 0x24, "Home"),
    k("End", "End", 107, 0xE04F, 0x23, "End"),
    k("PageUp", "Page Up", 104, 0xE049, 0x21, "Page_Up"),
    k("PageDown", "Page Down", 109, 0xE051, 0x22, "Page_Down"),
    // Numpad (scan codes are NumLock independent; the VK is the NumLock-on value).
    k("Numpad0", "Num 0", 82, 0x52, 0x60, "KP_0"),
    k("Numpad1", "Num 1", 79, 0x4F, 0x61, "KP_1"),
    k("Numpad2", "Num 2", 80, 0x50, 0x62, "KP_2"),
    k("Numpad3", "Num 3", 81, 0x51, 0x63, "KP_3"),
    k("Numpad4", "Num 4", 75, 0x4B, 0x64, "KP_4"),
    k("Numpad5", "Num 5", 76, 0x4C, 0x65, "KP_5"),
    k("Numpad6", "Num 6", 77, 0x4D, 0x66, "KP_6"),
    k("Numpad7", "Num 7", 71, 0x47, 0x67, "KP_7"),
    k("Numpad8", "Num 8", 72, 0x48, 0x68, "KP_8"),
    k("Numpad9", "Num 9", 73, 0x49, 0x69, "KP_9"),
    k("NumpadAdd", "Num +", 78, 0x4E, 0x6B, "KP_Add"),
    k("NumpadSubtract", "Num -", 74, 0x4A, 0x6D, "KP_Subtract"),
    k("NumpadMultiply", "Num *", 55, 0x37, 0x6A, "KP_Multiply"),
    k("NumpadDivide", "Num /", 98, 0xE035, 0x6F, "KP_Divide"),
    k("NumpadDecimal", "Num .", 83, 0x53, 0x6E, "KP_Decimal"),
    k("NumpadEnter", "Num Enter", 96, 0xE01C, 0x0D, "KP_Enter"),
    k("NumLock", "Num Lock", 69, 0xE045, 0x90, "Num_Lock"),
    // System.
    k("Pause", "Pause", 119, 0x0045, 0x13, "Pause"),
    k("ScrollLock", "Scroll Lock", 70, 0x46, 0x91, "Scroll_Lock"),
    k("PrintScreen", "Print Screen", 99, 0xE037, 0x2C, "Print"),
];

#[cfg(windows)]
const META_LEFT_LABEL: &str = "Left Win";
#[cfg(windows)]
const META_RIGHT_LABEL: &str = "Right Win";
#[cfg(target_os = "macos")]
const META_LEFT_LABEL: &str = "Left Cmd";
#[cfg(target_os = "macos")]
const META_RIGHT_LABEL: &str = "Right Cmd";
#[cfg(not(any(windows, target_os = "macos")))]
const META_LEFT_LABEL: &str = "Left Super";
#[cfg(not(any(windows, target_os = "macos")))]
const META_RIGHT_LABEL: &str = "Right Super";

/// Mouse buttons that may be bound. Left/right click are deliberately unsupported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MouseButton {
    /// "Mouse3"
    Middle,
    /// "Mouse4" (back / XBUTTON1)
    Back,
    /// "Mouse5" (forward / XBUTTON2)
    Forward,
}

impl MouseButton {
    pub(crate) fn label(self) -> &'static str {
        match self {
            MouseButton::Middle => "Middle Mouse",
            MouseButton::Back => "Mouse 4",
            MouseButton::Forward => "Mouse 5",
        }
    }

    /// X11 core button number (also what XI2 raw button events report).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn x11_button(self) -> u32 {
        match self {
            MouseButton::Middle => 2,
            MouseButton::Back => 8,
            MouseButton::Forward => 9,
        }
    }
}

/// A binding code resolved against the tables.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Resolved {
    Key(&'static KeyDef),
    Mouse(MouseButton),
}

pub(crate) fn lookup_key(code: &str) -> Option<&'static KeyDef> {
    KEYS.iter().find(|k| k.code == code)
}

pub(crate) fn resolve(code: &str) -> Option<Resolved> {
    match code {
        "Mouse3" => Some(Resolved::Mouse(MouseButton::Middle)),
        "Mouse4" => Some(Resolved::Mouse(MouseButton::Back)),
        "Mouse5" => Some(Resolved::Mouse(MouseButton::Forward)),
        _ => lookup_key(code).map(Resolved::Key),
    }
}

/// Label for any code; unknown codes fall back to the raw code string.
pub(crate) fn label(code: &str) -> String {
    match resolve(code) {
        Some(Resolved::Key(k)) => k.label.to_string(),
        Some(Resolved::Mouse(m)) => m.label().to_string(),
        None => code.to_string(),
    }
}

/// XDG "shortcuts" spec trigger string for the GlobalShortcuts portal (best effort).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn portal_trigger(code: &str) -> Option<String> {
    match resolve(code)? {
        Resolved::Key(k) => Some(k.keysym.to_string()),
        // The shortcuts spec has no mouse-button syntax; the user assigns it in the DE dialog.
        Resolved::Mouse(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn codes_are_unique_and_ids_distinct() {
        let mut codes = HashSet::new();
        let mut evdev = HashSet::new();
        let mut scans = HashSet::new();
        let mut keysyms = HashSet::new();
        for k in KEYS {
            assert!(codes.insert(k.code), "duplicate code {}", k.code);
            assert!(evdev.insert(k.evdev), "duplicate evdev for {}", k.code);
            assert!(scans.insert(k.win_scan), "duplicate scan for {}", k.code);
            assert!(keysyms.insert(k.keysym), "duplicate keysym for {}", k.code);
            assert!(!k.label.is_empty());
            assert!(k.win_vk != 0 && k.win_vk < 0xFF, "bad vk for {}", k.code);
        }
        // VKs are unique except Enter/NumpadEnter, which share VK_RETURN (told apart by scan code).
        let mut vks = HashSet::new();
        for k in KEYS.iter().filter(|k| k.code != "NumpadEnter") {
            assert!(vks.insert(k.win_vk), "duplicate vk for {}", k.code);
        }
    }

    #[test]
    fn evdev_matches_set1_for_basic_keys() {
        // For the original 83/84-key block, evdev codes are numerically the set-1 scan codes.
        for k in KEYS {
            if k.win_scan & 0xE000 == 0 && k.win_scan < 0x59 && k.code != "Pause" {
                assert_eq!(k.evdev, k.win_scan, "{}", k.code);
            }
        }
    }

    #[test]
    fn requested_codes_present() {
        let mut required: Vec<String> = Vec::new();
        required.extend((b'A'..=b'Z').map(|c| format!("Key{}", c as char)));
        required.extend((0..=9).map(|d| format!("Digit{d}")));
        required.extend((1..=24).map(|n| format!("F{n}")));
        required.extend((0..=9).map(|d| format!("Numpad{d}")));
        for c in [
            "Backquote",
            "Minus",
            "Equal",
            "BracketLeft",
            "BracketRight",
            "Backslash",
            "Semicolon",
            "Quote",
            "Comma",
            "Period",
            "Slash",
            "Space",
            "Tab",
            "CapsLock",
            "ShiftLeft",
            "ShiftRight",
            "ControlLeft",
            "ControlRight",
            "AltLeft",
            "AltRight",
            "MetaLeft",
            "MetaRight",
            "ArrowUp",
            "ArrowDown",
            "ArrowLeft",
            "ArrowRight",
            "Insert",
            "Delete",
            "Home",
            "End",
            "PageUp",
            "PageDown",
            "NumpadAdd",
            "NumpadSubtract",
            "NumpadMultiply",
            "NumpadDivide",
            "NumpadDecimal",
            "NumpadEnter",
            "Pause",
            "ScrollLock",
            "PrintScreen",
            "ContextMenu",
        ] {
            required.push(c.to_string());
        }
        for c in &required {
            assert!(lookup_key(c).is_some(), "missing {c}");
        }
    }

    #[test]
    fn sequential_ranges() {
        for n in 1..=10u16 {
            let k = lookup_key(&format!("F{n}")).unwrap();
            assert_eq!(k.win_vk, 0x6F + n);
            assert_eq!(k.win_scan, 0x3A + n);
            assert_eq!(k.evdev, 58 + n);
        }
        for n in 13..=23u16 {
            let k = lookup_key(&format!("F{n}")).unwrap();
            assert_eq!(k.win_vk, 0x6F + n);
            assert_eq!(k.win_scan, 0x64 + (n - 13));
            assert_eq!(k.evdev, 183 + (n - 13));
        }
        for d in 0..=9u16 {
            assert_eq!(lookup_key(&format!("Numpad{d}")).unwrap().win_vk, 0x60 + d);
            assert_eq!(lookup_key(&format!("Digit{d}")).unwrap().win_vk, 0x30 + d);
        }
        for c in b'A'..=b'Z' {
            let k = lookup_key(&format!("Key{}", c as char)).unwrap();
            assert_eq!(k.win_vk, c as u16);
            assert_eq!(k.label, (c as char).to_string());
            assert_eq!(k.keysym, (c as char).to_ascii_lowercase().to_string());
        }
    }

    #[test]
    fn labels() {
        assert_eq!(label("Backquote"), "`");
        assert_eq!(label("KeyV"), "V");
        assert_eq!(label("F13"), "F13");
        assert_eq!(label("ControlLeft"), "Left Ctrl");
        assert_eq!(label("ShiftRight"), "Right Shift");
        assert_eq!(label("Space"), "Space");
        assert_eq!(label("Numpad5"), "Num 5");
        assert_eq!(label("Mouse3"), "Middle Mouse");
        assert_eq!(label("Mouse4"), "Mouse 4");
        assert_eq!(label("Mouse5"), "Mouse 5");
        assert_eq!(label("SomethingWeird"), "SomethingWeird");
        #[cfg(windows)]
        assert_eq!(label("MetaLeft"), "Left Win");
    }

    #[test]
    fn mouse_and_unknown_resolution() {
        assert!(matches!(
            resolve("Mouse4"),
            Some(Resolved::Mouse(MouseButton::Back))
        ));
        assert!(
            resolve("Mouse1").is_none(),
            "left click must not be bindable"
        );
        assert!(
            resolve("Mouse2").is_none(),
            "right click must not be bindable"
        );
        assert!(
            resolve("keyv").is_none(),
            "codes are case sensitive like the DOM"
        );
        assert_eq!(MouseButton::Middle.x11_button(), 2);
        assert_eq!(MouseButton::Back.x11_button(), 8);
        assert_eq!(MouseButton::Forward.x11_button(), 9);
    }

    #[test]
    fn portal_triggers() {
        assert_eq!(portal_trigger("Backquote").as_deref(), Some("grave"));
        assert_eq!(portal_trigger("F13").as_deref(), Some("F13"));
        assert_eq!(portal_trigger("KeyV").as_deref(), Some("v"));
        assert_eq!(portal_trigger("Mouse4"), None);
        assert_eq!(portal_trigger("Nope"), None);
    }
}
