//! The main window's native frame colors, kept in step with the UI theme.
//!
//! Windows 11 paints title bars with the user's accent color when "Show accent
//! color on title bars" is on, which clashes with the app's own surfaces. The
//! frontend reports its effective background and text colors whenever the theme
//! changes, and the title bar takes them over (DWM caption/text colors).

/// Paint the main window's title bar with the UI's background and text colors.
/// Both are `#rrggbb`. Windows 11 only; a no-op on other platforms and on older
/// Windows builds, which ignore these DWM attributes.
#[tauri::command]
#[specta::specta]
pub fn set_window_chrome(
    app: tauri::AppHandle,
    background: String,
    text: String,
) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use tauri::Manager;
        use windows::Win32::Graphics::Dwm::{
            DwmSetWindowAttribute, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR,
        };

        let caption = colorref(&background)?;
        let caption_text = colorref(&text)?;
        let window = app
            .get_webview_window("main")
            .ok_or("main window not found")?;
        let hwnd = window.hwnd().map_err(|e| e.to_string())?;
        for (attribute, color) in [
            (DWMWA_CAPTION_COLOR, caption),
            (DWMWA_TEXT_COLOR, caption_text),
        ] {
            // SAFETY: `hwnd` is the live main window and `color` outlives the
            // call; DWM copies the 4-byte COLORREF.
            unsafe {
                DwmSetWindowAttribute(
                    hwnd,
                    attribute,
                    std::ptr::from_ref(&color).cast(),
                    std::mem::size_of::<u32>() as u32,
                )
            }
            .map_err(|e| format!("DwmSetWindowAttribute failed: {e}"))?;
        }
    }
    #[cfg(not(target_os = "windows"))]
    let _ = (app, background, text);
    Ok(())
}

/// `#rrggbb` to a Win32 COLORREF (`0x00bbggrr`).
#[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
fn colorref(hex: &str) -> Result<u32, String> {
    let digits = hex.trim().trim_start_matches('#');
    if digits.len() != 6 {
        return Err(format!("expected a #rrggbb color, got {hex:?}"));
    }
    let rgb = u32::from_str_radix(digits, 16)
        .map_err(|_| format!("expected a #rrggbb color, got {hex:?}"))?;
    let (r, g, b) = ((rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff);
    Ok((b << 16) | (g << 8) | r)
}

#[cfg(test)]
mod tests {
    use super::colorref;

    #[test]
    fn colorref_swaps_red_and_blue() {
        assert_eq!(colorref("#202020"), Ok(0x0020_2020));
        assert_eq!(colorref("#ff8000"), Ok(0x0000_80ff));
        assert_eq!(colorref(" 5ccf95 "), Ok(0x0095_cf5c));
    }

    #[test]
    fn colorref_rejects_other_formats() {
        assert!(colorref("#fff").is_err());
        assert!(colorref("rgb(1, 2, 3)").is_err());
        assert!(colorref("#gggggg").is_err());
    }
}
