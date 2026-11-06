//! The text caret's blink, as the platform's own text fields blink it.
//!
//! Each blink repaints the window, so a caret that blinks forever keeps an idle window busy.
//! Windows and GTK stop blinking, with the caret shown, a while after the last edit or move.

use std::time::Duration;

use gpui_kit::component::input::CursorBlink;

/// `CaretTimeout`'s value when the user never set it.
#[cfg(windows)]
const WINDOWS_CARET_TIMEOUT_MS: u32 = 5_000;

/// The user's Windows caret settings: the blink rate (`GetCaretBlinkTime`, `None` when set to
/// not blink) and `HKCU\Control Panel\Desktop\CaretTimeout`.
#[cfg(windows)]
pub fn blink() -> CursorBlink {
    use winsafe::{HKEY, RegistryValue, co};
    let rate = winsafe::GetCaretBlinkTime().ok();
    let timeout =
        match HKEY::CURRENT_USER.RegGetValue(Some("Control Panel\\Desktop"), Some("CaretTimeout"), co::RRF::RT_DWORD) {
            Ok(RegistryValue::Dword(ms)) => ms,
            _ => WINDOWS_CARET_TIMEOUT_MS,
        };
    windows_blink(rate, timeout)
}

/// GTK's defaults: `gtk-cursor-blink-time` 1200 ms per cycle, `gtk-cursor-blink-timeout` 10 s.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn blink() -> CursorBlink {
    CursorBlink::blinking(Duration::from_millis(600)).with_idle_timeout(Duration::from_secs(10))
}

/// AppKit's insertion point blinks for as long as the field has focus.
#[cfg(target_os = "macos")]
pub fn blink() -> CursorBlink {
    CursorBlink::default()
}

/// A rate of `None` or 0 keeps the caret steady; a timeout of `u32::MAX` never stops it.
#[cfg_attr(all(not(windows), not(test)), expect(dead_code, reason = "only Windows reads these settings"))]
fn windows_blink(rate_ms: Option<u32>, timeout_ms: u32) -> CursorBlink {
    let Some(rate) = rate_ms.filter(|&ms| ms > 0) else { return CursorBlink::steady() };
    let blink = CursorBlink::blinking(Duration::from_millis(rate.into()));
    if timeout_ms == u32::MAX { blink } else { blink.with_idle_timeout(Duration::from_millis(timeout_ms.into())) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_caret_follows_the_rate_and_stops_after_the_timeout() {
        let blink = windows_blink(Some(530), 5_000);
        assert_eq!(blink.interval(), Some(Duration::from_millis(530)));
        assert_eq!(blink.idle_timeout(), Some(Duration::from_secs(5)));
        assert_eq!(windows_blink(Some(530), u32::MAX).idle_timeout(), None);
        assert!(windows_blink(None, 5_000).is_steady());
        assert!(windows_blink(Some(0), 5_000).is_steady());
    }
}
