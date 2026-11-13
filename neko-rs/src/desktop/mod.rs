//! X11 desktop control via x11rb (pure Rust — replaces the cgo bridge in
//! server/pkg/xorg). Semantics ported from xorg.c/xorg.go: warp-pointer
//! moves, XTest button clicks for scroll, XTest key events, ctrl-wrapped
//! scroll, RandR screen info.
//!
//! Keysyms outside the current keymap (CJK/unicode input) get an
//! xdotool-style dynamic remap: a free keycode is repurposed via core
//! ChangeKeyboardMapping, mirroring XkbAddKeyKeysym in xorg.c. Remaps are
//! kept for the session and updated in the local cache so repeat presses
//! reuse the same keycode.

use anyhow::{Context, Result};
use tracing::debug;
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::{self, ConnectionExt as _};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;
use x11rb::CURRENT_TIME;

// core protocol event codes (xproto event numbers)
const KEY_PRESS: u8 = 2;
const KEY_RELEASE: u8 = 3;
const BUTTON_PRESS: u8 = 4;
const BUTTON_RELEASE: u8 = 5;

/// Control_L keycode (x86 keymaps; matches the Xorg neko ships).
const CONTROL_L_KEYCODE: u8 = 37;

pub struct Desktop {
    conn: RustConnection,
    screen_num: usize,
    root: xproto::Window,
    /// first keycode + keysyms-per-keycode from GetKeyboardMapping.
    /// Mutated by remap_keycode (RefCell: all access is serialized by the
    /// tokio::Mutex the room wraps us in).
    keymap: std::cell::RefCell<(u8, Vec<Vec<u32>>)>,
    /// XTEST keyboard device id (XInput), 0 = core protocol
    xtest_keyboard: u8,
}

impl Desktop {
    pub fn new(display: &str) -> Result<Self> {
        let (conn, screen_num) = x11rb::connect(Some(display))
            .with_context(|| format!("connect to X display {display}"))?;

        // XTEST is not optional for us
        conn.extension_information("XTEST")?
            .context("XTEST extension missing")?;

        let root = conn.setup().roots[screen_num].root;
        let keymap = std::cell::RefCell::new(load_keymap(&conn)?);
        let xtest_keyboard = find_xtest_keyboard(&conn).unwrap_or(0);

        Ok(Desktop {
            conn,
            screen_num,
            root,
            keymap,
            xtest_keyboard,
        })
    }

    /// Screen size + refresh rate via RandR (matches XRRGetScreenInfo use
    /// in the C bridge). Rate 0 when RandR is unavailable.
    pub fn screen_size(&self) -> Result<(u32, u32, u32)> {
        let reply = self
            .conn
            .randr_get_screen_info(self.root)
            .context("randr get_screen_info")?
            .reply()
            .context("randr reply")?;
        let size = reply
            .sizes
            .get(reply.size_id as usize)
            .or(reply.sizes.first())
            .context("no randr screen sizes")?;
        // RandR 1.1 GetScreenInfo carries the current refresh rate directly
        let rate = reply.rate as u32;
        Ok((size.width as u32, size.height as u32, rate))
    }

    /// XWarpPointer to absolute root coordinates (XMove in xorg.c).
    pub fn move_cursor(&self, x: i32, y: i32) -> Result<()> {
        self.conn.warp_pointer(
            x11rb::NONE,
            self.root,
            0,
            0,
            0,
            0,
            x as i16,
            y as i16,
        )?;
        self.conn.flush().context("flush move")?;
        Ok(())
    }

    pub fn cursor_position(&self) -> Result<(i32, i32)> {
        let reply = self
            .conn
            .query_pointer(self.root)?
            .reply()
            .context("query pointer")?;
        Ok((reply.root_x as i32, reply.root_y as i32))
    }

    /// Write text to the X CLIPBOARD selection. xclip (shipped in the
    /// image) forks and serves the selection until replaced; a later
    /// native xfixes implementation can replace this.
    pub fn set_clipboard(&self, text: &str) -> Result<()> {
        use std::io::Write as _;
        use std::process::{Command, Stdio};
        let mut child = Command::new("xclip")
            .args(["-selection", "clipboard"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("spawn xclip")?;
        child
            .stdin
            .as_mut()
            .context("xclip stdin")?
            .write_all(text.as_bytes())
            .context("write clipboard text")?;
        let _ = child.wait();
        Ok(())
    }

    /// Buttons 4/5 = vertical wheel, 6/7 = horizontal (XScroll in xorg.c);
    /// one click per unit of delta. `control_key` holds Control around the
    /// clicks (XSetKeyboardModifier equivalent).
    pub fn scroll(&self, delta_x: i32, delta_y: i32, control_key: bool) -> Result<()> {
        if control_key {
            self.key_event(CONTROL_L_KEYCODE, true)?;
        }
        let result = (|| -> Result<()> {
            let ydir = if delta_y > 0 { 4 } else { 5 };
            let xdir = if delta_x > 0 { 6 } else { 7 };
            for _ in 0..delta_y.abs() {
                self.button_click(ydir)?;
            }
            for _ in 0..delta_x.abs() {
                self.button_click(xdir)?;
            }
            Ok(())
        })();
        if control_key {
            let release = self.key_event(CONTROL_L_KEYCODE, false);
            result.and(release)
        } else {
            result
        }
    }

    pub fn button(&self, button: u32, down: bool) -> Result<()> {
        if button == 0 {
            return Ok(());
        }
        self.button_event(button as u8, down)
    }

    /// Keysym (X11 keysym, not keycode) press/release. Resolution order
    /// mirrors XKey: cached keymap lookup first; keysyms not present
    /// (CJK/unicode) get a dynamic remap onto a free keycode, exactly like
    /// XkbAddKeyKeysym/xdotool, so IME-composed text reaches X clients.
    pub fn key(&self, keysym: u32, down: bool) -> Result<()> {
        if keysym == 0 {
            return Ok(());
        }
        let keycode = self.resolve_keycode(keysym)?;
        self.key_event(keycode, down)
    }

    // ---- internals ----

    fn button_click(&self, button: u8) -> Result<()> {
        self.button_event(button, true)?;
        self.button_event(button, false)
    }

    fn button_event(&self, button: u8, down: bool) -> Result<()> {
        let type_ = if down { BUTTON_PRESS } else { BUTTON_RELEASE };
        self.conn
            .xtest_fake_input(type_, button, CURRENT_TIME, 0, 0, 0, 0)?;
        self.conn.flush().context("flush button")?;
        Ok(())
    }

    fn key_event(&self, keycode: u8, down: bool) -> Result<()> {
        let type_ = if down { KEY_PRESS } else { KEY_RELEASE };
        self.conn
            .xtest_fake_input(type_, keycode, CURRENT_TIME, 0, 0, 0, self.xtest_keyboard)?;
        self.conn.flush().context("flush key")?;
        Ok(())
    }

    /// XKey resolution: existing keymap first, dynamic remap as fallback.
    /// On remap the new mapping is pushed to the server and the local
    /// cache, so subsequent presses of the same keysym reuse the keycode.
    fn resolve_keycode(&self, keysym: u32) -> Result<u8> {
        let mut keymap = self.keymap.borrow_mut();
        if let Some(keycode) = keysym_to_keycode_in(&keymap, keysym) {
            return Ok(keycode);
        }

        let (first, map) = &mut *keymap;
        // free keycode = every column NoSymbol; scan max→min like the
        // TigerVNC code in xorg.c
        let idx = map
            .iter()
            .rposition(|codes| codes.iter().all(|&sym| sym == 0))
            .context("no free keycode for dynamic keysym remap")?;
        let keycode = first.wrapping_add(idx as u8);

        // column layout follows the server's keysyms-per-keycode so the
        // group/level geometry stays intact; group 1 gets (lower, upper),
        // remaining columns NoSymbol
        let per = map.first().map_or(1, Vec::len).max(1);
        let (lower, upper) = convert_case(keysym);
        let mut syms = vec![0u32; per];
        syms[0] = lower;
        if per > 1 {
            // same keysym at the shift level when caseless (CJK), so a
            // held Shift can't corrupt the injected char
            syms[1] = upper;
        }

        self.conn
            .change_keyboard_mapping(1, keycode, per as u8, &syms)
            .context("change_keyboard_mapping")?;
        self.conn.flush().context("flush keymap change")?;

        map[idx] = syms;
        debug!(keysym, keycode, "dynamically remapped keysym to free keycode");
        Ok(keycode)
    }
}

/// keymap tuple lookup shared by the method wrapper and the resolver.
fn keysym_to_keycode_in(keymap: &(u8, Vec<Vec<u32>>), keysym: u32) -> Option<u8> {
    let (first, map) = keymap;
    for (i, codes) in map.iter().enumerate() {
        if codes.contains(&keysym) {
            return Some(first.wrapping_add(i as u8));
        }
    }
    None
}

/// XConvertCase equivalent, Latin-1 only: covers the case pairs X11 knows
/// in the single-byte range. Unicode keysyms (≥ 0x01000000, i.e. all CJK)
/// are caseless → OneLevel keysyms, same as XkbAddKeyKeysym.
fn convert_case(keysym: u32) -> (u32, u32) {
    let lower = match keysym {
        0x41..=0x5a => keysym + 32,                     // A-Z
        0xc0..=0xde if keysym != 0xd7 => keysym + 32,   // À-Þ except ×
        _ => keysym,
    };
    let upper = match keysym {
        0x61..=0x7a => keysym - 32,                     // a-z
        0xe0..=0xfe if keysym != 0xf7 => keysym - 32,   // à-þ except ÷
        _ => keysym,
    };
    (lower, upper)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// keymap with keycodes min=8: kc8=[a,A], kc9=[b,B], kc10 free, kc11 free
    fn sample_keymap() -> (u8, Vec<Vec<u32>>) {
        (
            8,
            vec![
                vec![0x61, 0x41],
                vec![0x62, 0x42],
                vec![0, 0],
                vec![0, 0],
            ],
        )
    }

    #[test]
    fn lookup_finds_keysym() {
        let map = sample_keymap();
        assert_eq!(keysym_to_keycode_in(&map, 0x61), Some(8));
        assert_eq!(keysym_to_keycode_in(&map, 0x41), Some(8)); // shift level
        assert_eq!(keysym_to_keycode_in(&map, 0x62), Some(9));
        assert_eq!(keysym_to_keycode_in(&map, 0x4e2d), None); // 中 not mapped
    }

    #[test]
    fn free_keycode_scan_picks_highest_empty() {
        let map = sample_keymap();
        let idx = map
            .1
            .iter()
            .rposition(|codes| codes.iter().all(|&s| s == 0));
        assert_eq!(idx, Some(3)); // max→min, like XkbAddKeyKeysym
    }

    #[test]
    fn convert_case_latin1() {
        assert_eq!(convert_case(0x61), (0x61, 0x41)); // a → (a, A)
        assert_eq!(convert_case(0x41), (0x61, 0x41)); // A → (a, A)
        assert_eq!(convert_case(0xe9), (0xe9, 0xc9)); // é → (é, É)
        assert_eq!(convert_case(0xd7), (0xd7, 0xd7)); // × has no case
        assert_eq!(convert_case(0xf7), (0xf7, 0xf7)); // ÷ has no case
    }

    #[test]
    fn convert_case_cjk_is_caseless() {
        // 中 U+4E2D → keysym 0x01004E2D, OneLevel like XkbAddKeyKeysym
        let zhong = 0x01004e2d;
        assert_eq!(convert_case(zhong), (zhong, zhong));
    }
}

/// GetKeyboardMapping → (first_keycode, keycode → keysyms).
fn load_keymap(conn: &RustConnection) -> Result<(u8, Vec<Vec<u32>>)> {
    let setup = conn.setup();
    let first = setup.min_keycode;
    let count = setup
        .max_keycode
        .checked_sub(first)
        .map(|d| d + 1)
        .context("bad keycode range")?;
    let reply = conn
        .get_keyboard_mapping(first, count)?
        .reply()
        .context("keyboard mapping")?;

    let per = reply.keysyms_per_keycode.max(1) as usize;
    let map = reply
        .keysyms
        .chunks(per)
        .map(|c| c.to_vec())
        .collect();
    Ok((first, map))
}

/// The XTEST keyboard device makes XI2 clients (GDK3/Firefox) see injected
/// keys. Falls back to 0 (core protocol) when XInput is unavailable.
fn find_xtest_keyboard(conn: &RustConnection) -> Option<u8> {
    use x11rb::protocol::xinput::ConnectionExt as _;
    let reply = conn.xinput_list_input_devices().ok()?.reply().ok()?;
    // names is a parallel array to devices
    for (i, device) in reply.devices.iter().enumerate() {
        let name = reply
            .names
            .get(i)
            .map(|s| String::from_utf8_lossy(&s.name).into_owned())
            .unwrap_or_default();
        if name.contains("XTEST keyboard") {
            return Some(device.device_id);
        }
    }
    None
}
