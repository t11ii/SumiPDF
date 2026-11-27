//! Native button captions use Segoe MDL2 Assets. Give each glyph a Japanese
//! accessible name and a native tooltip while leaving painting and input to Windows.
use super::*;
use windows::{
    Win32::{
        Foundation::HWND as WinHwnd,
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
        UI::Accessibility::{CAccPropServices, IAccPropServices, PROPID_ACC_NAME},
    },
    core::{PCWSTR, w},
};
use windows_sys::Win32::UI::Controls::{
    ICC_BAR_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, TOOLTIPS_CLASSW, TTF_IDISHWND,
    TTF_SUBCLASS, TTM_ADDTOOLW, TTM_UPDATETIPTEXTW, TTS_ALWAYSTIP, TTS_NOPREFIX, TTTOOLINFOW,
};

#[derive(Clone, Copy)]
pub(super) struct Icon {
    glyph: &'static str,
    label: PCWSTR,
}

const fn icon(glyph: &'static str, label: PCWSTR) -> Icon {
    Icon { glyph, label }
}

pub(super) const OPEN_FILE: Icon = icon("\u{e8e5}", w!("開く… (Ctrl+O)"));
pub(super) const PREVIOUS: Icon = icon("\u{e76b}", w!("前のページ (←)"));
pub(super) const NEXT_PAGE: Icon = icon("\u{e76c}", w!("次のページ (→)"));
pub(super) const ZOOM_OUT: Icon = icon("\u{e71f}", w!("縮小 (Ctrl+−)"));
pub(super) const ZOOM_IN: Icon = icon("\u{e8a3}", w!("拡大 (Ctrl++)"));
pub(super) const FIT_PAGE: Icon = icon("\u{e9a6}", w!("全体表示 (Ctrl+0)"));
pub(super) const MARKER: Icon = icon("\u{e7e6}", w!("蛍光ペン (H)"));
pub(super) const UNDERLINE_TEXT: Icon = icon("\u{e8dc}", w!("下線 (U)"));
pub(super) const UNDO_MARK: Icon = icon("\u{e7a7}", w!("元に戻す (Ctrl+Z)"));
pub(super) const SAVE_ANNOTATIONS: Icon = icon("\u{e74e}", w!("注釈を保存… (Ctrl+Shift+S)"));
pub(super) const SIGN_PDF: Icon = icon("\u{e76d}", w!("電子署名… (Ctrl+S)"));
pub(super) const PFX: Icon = icon("\u{e8b5}", w!("PFX ファイルから署名用証明書を読み込む"));
pub(super) const CERTIFICATE: Icon = icon("\u{e8d7}", w!("Windows 証明書ストアから選ぶ"));
pub(super) const DEFAULT_WATERMARK: Icon = icon("\u{e81e}", w!("透かしを選択（現在は標準）"));
const CUSTOM_WATERMARK: Icon = icon("\u{e8b9}", w!("透かしを選択（現在は指定画像）"));
pub(super) const CONFIRM: Icon = icon("\u{e8fb}", w!("署名して保存… (Ctrl+S)"));
pub(super) const CANCEL: Icon = icon("\u{e711}", w!("キャンセル (Esc)"));

pub(super) struct Toolbar {
    parent: HWND,
    tips: HWND,
    names: IAccPropServices,
    buttons: Vec<HWND>,
}

impl Toolbar {
    pub fn new(parent: HWND) -> Result<Self, String> {
        let init = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_BAR_CLASSES,
        };
        let names = unsafe { CoCreateInstance(&CAccPropServices, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| format!("ツールバーの読み上げ名を設定できません。{e}"))?;
        let tips = unsafe {
            if InitCommonControlsEx(&init) == 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                TOOLTIPS_CLASSW,
                null(),
                WS_POPUP | TTS_ALWAYSTIP | TTS_NOPREFIX,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                parent,
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            )
        };
        if tips.is_null() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(Self {
            parent,
            tips,
            names,
            buttons: Vec::new(),
        })
    }

    fn tooltip(&self, button: HWND, label: PCWSTR, message: u32) -> Result<(), String> {
        // All labels are static UTF-16 strings and outlive the tooltip.
        let tool = TTTOOLINFOW {
            cbSize: size_of::<TTTOOLINFOW>() as u32,
            uFlags: TTF_IDISHWND | TTF_SUBCLASS,
            hwnd: self.parent,
            uId: button as usize,
            lpszText: label.0.cast_mut(),
            ..Default::default()
        };
        unsafe {
            if SendMessageW(self.tips, message, 0, &tool as *const _ as isize) == 0
                && message == TTM_ADDTOOLW
            {
                return Err("ツールバーの操作説明を登録できません。".into());
            }
        }
        Ok(())
    }

    pub fn page_tip(&self, button: HWND) -> Result<(), String> {
        self.tooltip(button, w!("ページを指定 (Ctrl+G)"), TTM_ADDTOOLW)
    }

    fn describe(&self, button: HWND, label: PCWSTR, message: u32) -> Result<(), String> {
        self.tooltip(button, label, message)?;
        unsafe {
            self.names
                .SetHwndPropStr(
                    WinHwnd(button),
                    OBJID_CLIENT as u32,
                    0,
                    PROPID_ACC_NAME,
                    label,
                )
                .map_err(|e| e.to_string())
        }
    }

    pub fn button(&mut self, id: usize, icon: Icon) -> Result<HWND, String> {
        let toggle = matches!(id, HIGHLIGHT | UNDERLINE);
        let style = if toggle {
            (BS_CHECKBOX | BS_PUSHLIKE) as u32
        } else {
            0
        };
        let hwnd = child(self.parent, "BUTTON", icon.glyph, id, WS_TABSTOP | style)?;
        self.describe(hwnd, icon.label, TTM_ADDTOOLW)?;
        self.buttons.push(hwnd);
        Ok(hwnd)
    }

    pub fn watermark(&self, button: HWND, custom: bool) -> Result<(), String> {
        let icon = if custom {
            CUSTOM_WATERMARK
        } else {
            DEFAULT_WATERMARK
        };
        self.describe(button, icon.label, TTM_UPDATETIPTEXTW)?;
        set_text(button, icon.glyph);
        Ok(())
    }

    pub fn set_font(&self, font: HFONT) {
        for &button in &self.buttons {
            unsafe {
                SendMessageW(button, WM_SETFONT, font as usize, 1);
            }
        }
    }

    // MSAA annotations must be removed while their HWNDs still exist.
    pub fn clear_names(&mut self) {
        for button in self.buttons.drain(..) {
            unsafe {
                let _ = self.names.ClearHwndProps(
                    WinHwnd(button),
                    OBJID_CLIENT as u32,
                    0,
                    &[PROPID_ACC_NAME],
                );
            }
        }
    }
}

impl Drop for Toolbar {
    fn drop(&mut self) {
        self.clear_names();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::{
        Win32::UI::Accessibility::{AccessibleObjectFromWindow, IAccessible},
        core::Interface,
    };
    use windows_sys::Win32::UI::Controls::TTM_GETTEXTW;

    fn labels(toolbar: &Toolbar, button: HWND) -> (String, String) {
        let mut text = [0u16; 128];
        let tool = TTTOOLINFOW {
            cbSize: size_of::<TTTOOLINFOW>() as u32,
            hwnd: toolbar.parent,
            uFlags: TTF_IDISHWND,
            uId: button as usize,
            lpszText: text.as_mut_ptr(),
            ..Default::default()
        };
        unsafe {
            SendMessageW(
                toolbar.tips,
                TTM_GETTEXTW,
                text.len(),
                &tool as *const _ as isize,
            );
            let tip = PCWSTR(text.as_ptr()).to_string().unwrap();
            let mut raw = null_mut();
            AccessibleObjectFromWindow(
                WinHwnd(button),
                OBJID_CLIENT as u32,
                &IAccessible::IID,
                &mut raw,
            )
            .unwrap();
            let name = IAccessible::from_raw(raw)
                .get_accName(&0i32.into())
                .unwrap();
            (tip, name.to_string())
        }
    }

    #[test]
    fn toolbar_buttons_render_distinct_glyphs_and_expose_their_japanese_labels() {
        unsafe {
            assert!(CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32) >= 0);
            assert!(register(CANVAS, Some(canvas::procedure), null_mut()));
            let parent = CreateWindowExW(
                0,
                w!("STATIC").0,
                null(),
                WS_POPUP,
                0,
                0,
                680,
                480,
                null_mut(),
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            );
            assert!(!parent.is_null());
            let mut ui = Controls::new(parent).unwrap();
            ui.fonts(parent);
            let dc = GetDC(parent);
            let old = SelectObject(dc, ui.icon_font);
            let mut indices = std::collections::HashSet::new();
            for &button in &ui.toolbar.buttons {
                let (tip, name) = labels(&ui.toolbar, button);
                assert!(
                    !tip.is_empty() && tip == name,
                    "tooltip {tip:?}, accessible name {name:?}"
                );
                let mut caption = [0u16; 8];
                assert_eq!(
                    GetWindowTextW(button, caption.as_mut_ptr(), caption.len() as i32),
                    1,
                    "{name}"
                );
                assert_eq!(
                    SendMessageW(button, WM_GETFONT, 0, 0),
                    ui.icon_font as isize
                );
                let mut index = 0xffff;
                assert_ne!(
                    GetGlyphIndicesW(
                        dc,
                        caption.as_ptr(),
                        1,
                        &mut index,
                        GGI_MARK_NONEXISTING_GLYPHS
                    ),
                    u32::MAX
                );
                assert!(index != 0xffff && indices.insert(index), "{name}: {index}");
            }
            assert_eq!(indices.len(), 16);
            assert_eq!(
                labels(&ui.toolbar, ui.certificate).1,
                "Windows 証明書ストアから選ぶ"
            );
            assert_eq!(labels(&ui.toolbar, ui.sign).1, "電子署名… (Ctrl+S)");
            ui.toolbar.watermark(ui.watermark, true).unwrap();
            let (tip, name) = labels(&ui.toolbar, ui.watermark);
            assert!(tip.contains("指定画像") && tip == name);
            SelectObject(dc, old);
            ReleaseDC(parent, dc);
            ui.toolbar.clear_names();
            DestroyWindow(parent);
            drop(ui);
            CoUninitialize();
        }
    }
}
