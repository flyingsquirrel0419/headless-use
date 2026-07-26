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
    /// Modifiers the physical keyboard must hold to produce this key, e.g.
    /// [`Modifiers::SHIFT`] for `A` or `?`. These are OR-ed into whatever the
    /// caller passes to [`Keyboard::down`] / [`Keyboard::up`], so a page that
    /// reads `event.shiftKey` sees the truth without the caller spelling it out.
    pub modifiers: Modifiers,
}

impl Key {
    /// A simple printable key from a single char.
    ///
    /// The physical identity (`code`, `windowsVirtualKeyCode`) and the implied
    /// [`Key::modifiers`] come from [`char_to_key`], which models a **US
    /// QWERTY** layout — see that function for why that matters.
    pub fn printable(c: char) -> Self {
        let (code, key_code, modifiers) = char_to_key(c);
        Key {
            key: c.to_string(),
            code,
            key_code,
            // The literal character, always: CDP turns `text` into the char
            // event that actually inserts it. Shift never rewrites this.
            text: Some(c.to_string()),
            location: 0,
            modifiers,
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
    ///
    /// `mods` is OR-ed with [`Key::modifiers`], so typing `A` or `?` reports
    /// `shiftKey === true` even when the caller passed [`Modifiers::NONE`].
    pub async fn down(&self, key: &Key, mods: Modifiers) -> Result<(), BrowserError> {
        let mods = mods.union(key.modifiers);
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

    /// Release `key` with modifiers. Like [`Self::down`], `mods` is OR-ed with
    /// [`Key::modifiers`] so the keyup carries the same modifier state.
    pub async fn up(&self, key: &Key, mods: Modifiers) -> Result<(), BrowserError> {
        let mods = mods.union(key.modifiers);
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

/// Resolve a printable char to its physical key identity: DOM `code`, Windows
/// virtual-key code, and the modifiers needed to produce it.
///
/// ## This is a US QWERTY mapping, deliberately
/// `code` names a *physical key position*, not a glyph, so it is only definable
/// relative to a layout. `!` reports `Digit1` because that is the key that
/// produces `!` on US QWERTY (with Shift); on AZERTY the same glyph sits
/// elsewhere entirely. Chrome's own `Input.dispatchKeyEvent` has no layout
/// concept — it forwards whatever `code` we send — and every headless driver
/// (Puppeteer, Playwright) makes the same US QWERTY assumption. Modelling other
/// layouts would require knowing the OS layout of a browser we launched
/// ourselves, which we do not, and would break every US-layout expectation in
/// existing page code. So: US QWERTY, everywhere, on purpose.
fn char_to_key(c: char) -> (String, u32, Modifiers) {
    if c.is_ascii_alphabetic() {
        // 'A'..'Z' virtual key codes 65..90 for both cases; uppercase is the
        // same physical key held with Shift.
        let upper = c.to_ascii_uppercase();
        let mods = if c.is_ascii_uppercase() {
            Modifiers::SHIFT
        } else {
            Modifiers::NONE
        };
        return (format!("Key{upper}"), upper as u32, mods);
    }
    match shift_base(c) {
        // A shifted symbol borrows the physical key of its unshifted twin.
        Some(base) => (
            char_to_code(base).to_string(),
            char_to_vk(base),
            Modifiers::SHIFT,
        ),
        None => (char_to_code(c).to_string(), char_to_vk(c), Modifiers::NONE),
    }
}

/// The unshifted char sharing a physical key with shifted `c`, on US QWERTY.
///
/// Returns `None` for chars that need no Shift (or that we cannot place). Kept
/// as a plain char->char table so the `code`/vk tables below stay the single
/// source of truth for physical key identity.
fn shift_base(c: char) -> Option<char> {
    Some(match c {
        '!' => '1',
        '@' => '2',
        '#' => '3',
        '$' => '4',
        '%' => '5',
        '^' => '6',
        '&' => '7',
        '*' => '8',
        '(' => '9',
        ')' => '0',
        '_' => '-',
        '+' => '=',
        '{' => '[',
        '}' => ']',
        '|' => '\\',
        ':' => ';',
        '"' => '\'',
        '<' => ',',
        '>' => '.',
        '?' => '/',
        '~' => '`',
        _ => return None,
    })
}

/// Map a non-alphabetic *unshifted* ASCII char to its DOM `code` (US QWERTY).
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

/// Map a non-alphabetic *unshifted* ASCII char to its Windows virtual key code
/// (US QWERTY; shifted symbols reuse their base key's code via [`shift_base`]).
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
                modifiers: Modifiers::NONE,
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
                modifiers: Modifiers::NONE,
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
                modifiers: Modifiers::NONE,
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
                modifiers: Modifiers::NONE,
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
                modifiers: Modifiers::NONE,
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
                modifiers: Modifiers::NONE,
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
                modifiers: Modifiers::NONE,
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
                modifiers: Modifiers::NONE,
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
                modifiers: Modifiers::NONE,
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
                modifiers: Modifiers::NONE,
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
                modifiers: Modifiers::NONE,
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
                modifiers: Modifiers::NONE,
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
                modifiers: Modifiers::NONE,
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
                modifiers: Modifiers::NONE,
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
                modifiers: Modifiers::NONE,
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
                modifiers: Modifiers::NONE,
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
                modifiers: Modifiers::NONE,
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
                    modifiers: Modifiers::NONE,
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
        assert_eq!(k.modifiers, Modifiers::NONE);
        let k = Key::printable('-');
        assert_eq!(k.code, "Minus");
        assert_eq!(k.key_code, 0xBD);
        assert_eq!(k.modifiers, Modifiers::NONE);
    }

    /// Uppercase is the lowercase key held with Shift: same code, same vk.
    #[test]
    fn printable_uppercase_implies_shift() {
        for (c, code, vk) in [('A', "KeyA", 0x41u32), ('Z', "KeyZ", 0x5A)] {
            let k = Key::printable(c);
            assert_eq!(k.code, code);
            assert_eq!(k.key_code, vk);
            assert_eq!(k.modifiers, Modifiers::SHIFT, "{c} should imply Shift");
            // The literal char still rides in `text` — CDP needs it verbatim.
            assert_eq!(k.text.as_deref(), Some(c.to_string().as_str()));
        }
        assert_eq!(Key::printable('a').modifiers, Modifiers::NONE);
    }

    /// Every shifted symbol keeps the physical key of its unshifted twin
    /// instead of degrading to `Unidentified`/vk 0.
    #[test]
    fn printable_shifted_symbols_keep_physical_key() {
        let cases: &[(char, &str, u32)] = &[
            ('!', "Digit1", 0x31),
            ('@', "Digit2", 0x32),
            ('#', "Digit3", 0x33),
            ('$', "Digit4", 0x34),
            ('%', "Digit5", 0x35),
            ('^', "Digit6", 0x36),
            ('&', "Digit7", 0x37),
            ('*', "Digit8", 0x38),
            ('(', "Digit9", 0x39),
            (')', "Digit0", 0x30),
            ('_', "Minus", 0xBD),
            ('+', "Equal", 0xBB),
            ('{', "BracketLeft", 0xDB),
            ('}', "BracketRight", 0xDD),
            ('|', "Backslash", 0xDC),
            (':', "Semicolon", 0xBA),
            ('"', "Quote", 0xDE),
            ('<', "Comma", 0xBC),
            ('>', "Period", 0xBE),
            ('?', "Slash", 0xBF),
            ('~', "Backquote", 0xC0),
        ];
        for &(c, code, vk) in cases {
            let k = Key::printable(c);
            assert_eq!(k.code, code, "code for {c}");
            assert_eq!(k.key_code, vk, "vk for {c}");
            assert_eq!(k.modifiers, Modifiers::SHIFT, "shift for {c}");
            assert_eq!(k.text.as_deref(), Some(c.to_string().as_str()));
            assert_eq!(k.key, c.to_string());
        }
    }

    /// Named keys carry no implied modifiers.
    #[test]
    fn named_keys_have_no_implied_modifiers() {
        assert_eq!(named("Enter").unwrap().modifiers, Modifiers::NONE);
        assert_eq!(named("F5").unwrap().modifiers, Modifiers::NONE);
        assert_eq!(named("Shift").unwrap().modifiers, Modifiers::NONE);
    }
}
