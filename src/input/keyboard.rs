//! Keyboard input engine via CDP `Input.dispatchKeyEvent` and `Input.insertText`.
//!
//! Distinguishes:
//!   - `type`: produces keyDown+char sequences for ASCII (good for English).
//!   - `insert-text`: uses `Input.insertText` for non-ASCII (한글/CJK/emoji),
//!     which is far more reliable than synthesizing key events for complex
//!     scripts and avoids IME state machines.
//!
//! ## Key name normalization
//! Users may type `Control`, `Ctrl`, `Command`, `Cmd`, `Meta`, `Enter`, `Return`,
//! etc. We normalize these to a single [`Key`] representation and the correct
//! CDP `key`/`code`/`windowsVirtualKeyCode`/`text` fields.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{json, Value};

use crate::browser::{BrowserError, Page};
use crate::input::types::Modifiers;

/// A normalized key descriptor.
#[derive(Debug, Clone)]
pub struct Key {
    /// DOM `key` value, e.g. `a`, `Enter`, `Shift`.
    pub key: String,
    /// DOM `code` value, e.g. `KeyA`, `Enter`, `ShiftLeft`.
    pub code: String,
    /// Windows virtual key code.
    pub key_code: u32,
    /// Text to insert for printable keys (the `text` field).
    pub text: Option<String>,
    /// Location: 0=standard, 1=left, 2=right, 3=numpad.
    pub location: u32,
}

impl Key {
    /// A simple printable key from a single char.
    pub fn printable(c: char) -> Self {
        let code = if c.is_ascii_alphabetic() {
            format!("Key{}", c.to_ascii_uppercase())
        } else {
            char_to_code(c).to_string()
        };
        let key_code = if c.is_ascii_alphabetic() {
            // 'A'..'Z' virtual key codes 65..90
            c.to_ascii_uppercase() as u32
        } else {
            char_to_vk(c)
        };
        Key {
            key: c.to_string(),
            code,
            key_code,
            text: Some(c.to_string()),
            location: 0,
        }
    }
}

/// Lookup a named key (Enter, Control, ArrowUp, F1...). Returns None if unknown.
pub fn named(name: &str) -> Option<Key> {
    let table = key_table();
    table.get(name).cloned()
}

/// Parse a chord like `Control+Shift+P` into (modifiers, final key).
pub fn parse_chord(s: &str) -> Result<(Modifiers, Option<Key>), String> {
    let parts: Vec<&str> = s.split('+').map(|p| p.trim()).collect();
    let mut mods = Modifiers::NONE;
    let mut final_key: Option<Key> = None;
    for p in parts {
        if p.is_empty() {
            continue;
        }
        let lower = p.to_ascii_lowercase();
        match lower.as_str() {
            "control" | "ctrl" => mods = mods.union(Modifiers::CTRL),
            "shift" => mods = mods.union(Modifiers::SHIFT),
            "alt" | "option" => mods = mods.union(Modifiers::ALT),
            "meta" | "cmd" | "command" => mods = mods.union(Modifiers::META),
            _ => {
                // Not a modifier: try named key (case-insensitive), else single char.
                if let Some(k) = named(&lower) {
                    final_key = Some(k);
                } else if p.chars().count() == 1 {
                    // Preserve original case for printable chars (P vs p).
                    final_key = Some(Key::printable(p.chars().next().unwrap()));
                } else {
                    return Err(format!("unknown key '{p}'"));
                }
            }
        }
    }
    Ok((mods, final_key))
}

/// Keyboard engine.
#[derive(Clone)]
pub struct Keyboard<'a> {
    page: &'a Page,
}

impl<'a> Keyboard<'a> {
    /// Create a keyboard engine for `page`.
    pub fn new(page: &'a Page) -> Self {
        Self { page }
    }

    /// Press and release `key` with modifiers.
    pub async fn press(&self, key: &Key, mods: Modifiers) -> Result<(), BrowserError> {
        self.down(key, mods).await?;
        self.up(key, mods).await?;
        Ok(())
    }

    /// Press `key` down with modifiers.
    pub async fn down(&self, key: &Key, mods: Modifiers) -> Result<(), BrowserError> {
        let mut params = json!({
            "type": "keyDown",
            "key": key.key,
            "code": key.code,
            "windowsVirtualKeyCode": key.key_code,
            "modifiers": mods.0 as u64,
            "location": key.location,
        });
        if let Some(text) = &key.text {
            // For printable keys, also generate a char event via text field.
            params["text"] = json!(text);
            params["unmodifiedText"] = json!(text);
        }
        self.page
            .call::<Value>(
                "Input.dispatchKeyEvent",
                Some(params),
                Duration::from_secs(10),
            )
            .await?;
        Ok(())
    }

    /// Release `key` with modifiers.
    pub async fn up(&self, key: &Key, mods: Modifiers) -> Result<(), BrowserError> {
        let params = json!({
            "type": "keyUp",
            "key": key.key,
            "code": key.code,
            "windowsVirtualKeyCode": key.key_code,
            "modifiers": mods.0 as u64,
            "location": key.location,
        });
        self.page
            .call::<Value>(
                "Input.dispatchKeyEvent",
                Some(params),
                Duration::from_secs(10),
            )
            .await?;
        Ok(())
    }

    /// Hold a key for `duration`, optionally auto-repeating.
    pub async fn hold(
        &self,
        key: &Key,
        mods: Modifiers,
        duration: Duration,
    ) -> Result<(), BrowserError> {
        self.down(key, mods).await?;
        if !duration.is_zero() {
            tokio::time::sleep(duration).await;
        }
        self.up(key, mods).await?;
        Ok(())
    }

    /// Repeat a key `count` times with `interval` between presses.
    pub async fn repeat(
        &self,
        key: &Key,
        mods: Modifiers,
        count: u32,
        interval: Duration,
    ) -> Result<(), BrowserError> {
        for _ in 0..count {
            self.press(key, mods).await?;
            if !interval.is_zero() {
                tokio::time::sleep(interval).await;
            }
        }
        Ok(())
    }

    /// Type text as individual key events. Best for ASCII; for CJK/emoji use
    /// [`Self::insert_text`]. `delay` is the inter-key pause.
    pub async fn type_text(&self, text: &str, delay: Duration) -> Result<(), BrowserError> {
        for c in text.chars() {
            if c.is_ascii() && !c.is_control() {
                let key = Key::printable(c);
                self.down(&key, Modifiers::NONE).await?;
                self.up(&key, Modifiers::NONE).await?;
            } else {
                // Non-ASCII or control char: fall back to insertText per char.
                self.insert_text(&c.to_string()).await?;
            }
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
        }
        Ok(())
    }

    /// Insert text verbatim via `Input.insertText` (best for CJK/emoji/long text).
    pub async fn insert_text(&self, text: &str) -> Result<(), BrowserError> {
        self.page
            .call::<Value>(
                "Input.insertText",
                Some(json!({ "text": text })),
                Duration::from_secs(10),
            )
            .await?;
        Ok(())
    }
}

/// Map a non-alphabetic ASCII char to its DOM `code`.
fn char_to_code(c: char) -> &'static str {
    match c {
        '0' => "Digit0",
        '1' => "Digit1",
        '2' => "Digit2",
        '3' => "Digit3",
        '4' => "Digit4",
        '5' => "Digit5",
        '6' => "Digit6",
        '7' => "Digit7",
        '8' => "Digit8",
        '9' => "Digit9",
        ' ' => "Space",
        '-' => "Minus",
        '=' => "Equal",
        '[' => "BracketLeft",
        ']' => "BracketRight",
        '\\' => "Backslash",
        ';' => "Semicolon",
        '\'' => "Quote",
        '`' => "Backquote",
        ',' => "Comma",
        '.' => "Period",
        '/' => "Slash",
        _ => "Unidentified",
    }
}

/// Map a non-alphabetic ASCII char to its Windows virtual key code.
fn char_to_vk(c: char) -> u32 {
    match c {
        '0' => 0x30,
        '1' => 0x31,
        '2' => 0x32,
        '3' => 0x33,
        '4' => 0x34,
        '5' => 0x35,
        '6' => 0x36,
        '7' => 0x37,
        '8' => 0x38,
        '9' => 0x39,
        ' ' => 0x20,
        '-' => 0xBD,
        '=' => 0xBB,
        '[' => 0xDB,
        ']' => 0xDD,
        '\\' => 0xDC,
        ';' => 0xBA,
        '\'' => 0xDE,
        '`' => 0xC0,
        ',' => 0xBC,
        '.' => 0xBE,
        '/' => 0xBF,
        _ => 0,
    }
}

/// The named-key table. Built once.
fn key_table() -> &'static HashMap<&'static str, Key> {
    static TABLE: OnceLock<HashMap<&'static str, Key>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut m: HashMap<&'static str, Key> = HashMap::new();
        let mut add = |names: &'static [&'static str], key: Key| {
            for &n in names {
                m.insert(n, key.clone());
            }
        };
        add(
            &["Enter", "Return", "enter", "return"],
            Key {
                key: "Enter".into(),
                code: "Enter".into(),
                key_code: 0x0D,
                text: Some("\r".into()),
                location: 0,
            },
        );
        add(
            &["Tab", "tab"],
            Key {
                key: "Tab".into(),
                code: "Tab".into(),
                key_code: 0x09,
                text: Some("\t".into()),
                location: 0,
            },
        );
        add(
            &["Backspace", "Backspace", "backspace"],
            Key {
                key: "Backspace".into(),
                code: "Backspace".into(),
                key_code: 0x08,
                text: Some("\u{0008}".into()),
                location: 0,
            },
        );
        add(
            &["Delete", "Del", "delete", "del"],
            Key {
                key: "Delete".into(),
                code: "Delete".into(),
                key_code: 0x2E,
                text: None,
                location: 0,
            },
        );
        add(
            &["Escape", "Esc", "escape", "esc"],
            Key {
                key: "Escape".into(),
                code: "Escape".into(),
                key_code: 0x1B,
                text: None,
                location: 0,
            },
        );
        add(
            &["Home", "home"],
            Key {
                key: "Home".into(),
                code: "Home".into(),
                key_code: 0x24,
                text: None,
                location: 0,
            },
        );
        add(
            &["End", "end"],
            Key {
                key: "End".into(),
                code: "End".into(),
                key_code: 0x23,
                text: None,
                location: 0,
            },
        );
        add(
            &["PageUp", "pageup"],
            Key {
                key: "PageUp".into(),
                code: "PageUp".into(),
                key_code: 0x21,
                text: None,
                location: 0,
            },
        );
        add(
            &["PageDown", "pagedown"],
            Key {
                key: "PageDown".into(),
                code: "PageDown".into(),
                key_code: 0x22,
                text: None,
                location: 0,
            },
        );
        add(
            &["ArrowUp", "Up", "up", "arrowup"],
            Key {
                key: "ArrowUp".into(),
                code: "ArrowUp".into(),
                key_code: 0x26,
                text: None,
                location: 0,
            },
        );
        add(
            &["ArrowDown", "Down", "down", "arrowdown"],
            Key {
                key: "ArrowDown".into(),
                code: "ArrowDown".into(),
                key_code: 0x28,
                text: None,
                location: 0,
            },
        );
        add(
            &["ArrowLeft", "Left", "left", "arrowleft"],
            Key {
                key: "ArrowLeft".into(),
                code: "ArrowLeft".into(),
                key_code: 0x25,
                text: None,
                location: 0,
            },
        );
        add(
            &["ArrowRight", "Right", "right", "arrowright"],
            Key {
                key: "ArrowRight".into(),
                code: "ArrowRight".into(),
                key_code: 0x27,
                text: None,
                location: 0,
            },
        );
        add(
            &["Control", "Ctrl", "control", "ctrl"],
            Key {
                key: "Control".into(),
                code: "ControlLeft".into(),
                key_code: 0x11,
                text: None,
                location: 1,
            },
        );
        add(
            &["Shift", "shift"],
            Key {
                key: "Shift".into(),
                code: "ShiftLeft".into(),
                key_code: 0x10,
                text: None,
                location: 1,
            },
        );
        add(
            &["Alt", "Option", "alt", "option"],
            Key {
                key: "Alt".into(),
                code: "AltLeft".into(),
                key_code: 0x12,
                text: None,
                location: 1,
            },
        );
        add(
            &["Meta", "Command", "Cmd", "meta", "command", "cmd"],
            Key {
                key: "Meta".into(),
                code: "MetaLeft".into(),
                key_code: 0x5B,
                text: None,
                location: 1,
            },
        );
        // F1-F12
        for (i, n) in (1..=12).zip([
            "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
        ]) {
            m.insert(
                n,
                Key {
                    key: n.into(),
                    code: n.into(),
                    key_code: 0x70 + (i - 1) as u32,
                    text: None,
                    location: 0,
                },
            );
        }
        m
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_chord_modifiers_and_key() {
        let (mods, key) = parse_chord("Control+Shift+P").unwrap();
        assert_eq!(mods, Modifiers::CTRL.union(Modifiers::SHIFT));
        assert_eq!(key.as_ref().unwrap().key, "P");

        let (mods, key) = parse_chord("Meta+K").unwrap();
        assert_eq!(mods, Modifiers::META);
        assert_eq!(key.as_ref().unwrap().key, "K");

        let (mods, _) = parse_chord("Control").unwrap();
        assert_eq!(mods, Modifiers::CTRL);
    }

    #[test]
    fn parse_chord_rejects_unknown() {
        assert!(parse_chord("Foo+Bar").is_err());
    }

    #[test]
    fn named_keys_resolved() {
        assert_eq!(named("Enter").unwrap().key_code, 0x0D);
        assert_eq!(named("ArrowLeft").unwrap().key_code, 0x25);
        assert_eq!(named("F5").unwrap().key_code, 0x74);
        assert!(named("NotAKey").is_none());
    }

    #[test]
    fn printable_alphabetic() {
        let k = Key::printable('a');
        assert_eq!(k.code, "KeyA");
        assert_eq!(k.key_code, 65);
        assert_eq!(k.text.as_deref(), Some("a"));
    }

    #[test]
    fn printable_digit_and_symbol() {
        let k = Key::printable('5');
        assert_eq!(k.code, "Digit5");
        assert_eq!(k.key_code, 0x35);
        let k = Key::printable('-');
        assert_eq!(k.code, "Minus");
        assert_eq!(k.key_code, 0xBD);
    }
}
