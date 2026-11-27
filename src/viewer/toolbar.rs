//! Native button captions use Segoe MDL2 Assets. Give each glyph an English
//! accessible name and a native tooltip while preserving native input and state.
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

pub(super) const OPEN_FILE: Icon = icon("\u{e8e5}", w!("Open… (Ctrl+O)"));
pub(super) const PREVIOUS: Icon = icon("\u{e76b}", w!("Previous page (←)"));
pub(super) const NEXT_PAGE: Icon = icon("\u{e76c}", w!("Next page (→)"));
pub(super) const ZOOM_OUT: Icon = icon("\u{e71f}", w!("Zoom out (Ctrl+−)"));
pub(super) const ZOOM_IN: Icon = icon("\u{e8a3}", w!("Zoom in (Ctrl++)"));
pub(super) const FIT_PAGE: Icon = icon("\u{e9a6}", w!("Fit page (Ctrl+0)"));
pub(super) const MARKER: Icon = icon("\u{e7e6}", w!("Highlight (H)"));
pub(super) const UNDERLINE_TEXT: Icon = icon("\u{e8dc}", w!("Underline (U)"));
pub(super) const UNDO_MARK: Icon = icon("\u{e7a7}", w!("Undo (Ctrl+Z)"));
pub(super) const SAVE_ANNOTATIONS: Icon = icon("\u{e74e}", w!("Save annotations… (Ctrl+Shift+S)"));
pub(super) const SIGN_PDF: Icon = icon("\u{e76d}", w!("Sign PDF… (Ctrl+S)"));
pub(super) const PFX: Icon = icon("\u{e8b5}", w!("Load certificate from PFX"));
pub(super) const CERTIFICATE: Icon =
    icon("\u{e8d7}", w!("Choose from the Windows certificate store"));
pub(super) const DEFAULT_WATERMARK: Icon = icon("\u{e81e}", w!("Choose watermark (default)"));
const CUSTOM_WATERMARK: Icon = icon("\u{e8b9}", w!("Choose watermark (custom image)"));
pub(super) const CONFIRM: Icon = icon("\u{e8fb}", w!("Sign and save… (Ctrl+S)"));
pub(super) const CANCEL: Icon = icon("\u{e711}", w!("Cancel (Esc)"));

pub(super) struct Toolbar {
    parent: HWND,
    tips: HWND,
    names: IAccPropServices,
    buttons: Vec<HWND>,
    color: Option<HWND>,
}

impl Toolbar {
    pub fn new(parent: HWND) -> Result<Self, String> {
        let init = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_BAR_CLASSES,
        };
        let names = unsafe { CoCreateInstance(&CAccPropServices, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| format!("Could not set the toolbar accessibility labels. {e}"))?;
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
            color: None,
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
                return Err("Could not register the toolbar tooltip.".into());
            }
        }
        Ok(())
    }

    pub fn page_tip(&self, button: HWND) -> Result<(), String> {
        self.tooltip(button, w!("Go to page (Ctrl+G)"), TTM_ADDTOOLW)
    }

    pub fn color_tip(&mut self, picker: HWND) -> Result<(), String> {
        self.describe(picker, w!("Markup color"), TTM_ADDTOOLW)?;
        self.color = Some(picker);
        Ok(())
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
        theme::style_control(hwnd)?;
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
        for button in self.buttons.drain(..).chain(self.color.take()) {
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

    fn snapshot<T>(button: HWND, dc: HDC, inspect: impl FnOnce(HDC, RECT) -> T) -> T {
        let bounds = rect(button);
        unsafe {
            let memory = CreateCompatibleDC(dc);
            let image = CreateCompatibleBitmap(dc, bounds.right, bounds.bottom);
            assert!(!memory.is_null() && !image.is_null());
            let old = SelectObject(memory, image);
            FillRect(memory, &bounds, GetSysColorBrush(COLOR_BTNFACE));
            SendMessageW(button, WM_PRINTCLIENT, memory as usize, PRF_CLIENT as isize);
            let result = inspect(memory, bounds);
            SelectObject(memory, old);
            DeleteObject(image);
            DeleteDC(memory);
            result
        }
    }

    #[test]
    fn color_picker_paints_each_swatch_in_its_field_and_native_dropdown() {
        use windows_sys::Win32::UI::Controls::{COMBOBOXINFO, GetComboBoxInfo};

        const COLORS: [(&str, COLORREF); 4] = [
            ("Yellow", 0x00ffff),
            ("Red", 0x0000ff),
            ("Green", 0x00ff00),
            ("Blue", 0xff0000),
        ];
        unsafe {
            let class = "SumiPDF.ColorPickerTest";
            assert!(register(class, Some(window_proc), null_mut()));
            let parent = CreateWindowExW(
                0,
                wide(class).as_ptr(),
                null(),
                WS_POPUP,
                0,
                0,
                240,
                200,
                null_mut(),
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            );
            assert!(!parent.is_null());
            let picker = Controls::color_picker(parent).unwrap();
            theme::style_control(picker).unwrap();
            SendMessageW(
                picker,
                WM_SETFONT,
                GetStockObject(DEFAULT_GUI_FONT) as usize,
                0,
            );
            MoveWindow(picker, 0, 0, dips(parent, 82), dips(parent, 140), 1);
            ShowWindow(parent, SW_SHOWNOACTIVATE);
            let dc = GetDC(parent);
            for (index, (name, expected)) in COLORS.into_iter().enumerate() {
                assert_eq!(
                    SendMessageW(
                        picker,
                        CB_FINDSTRINGEXACT,
                        usize::MAX,
                        wide(name).as_ptr() as isize
                    ),
                    index as isize
                );
                assert_eq!(SendMessageW(picker, CB_SETCURSEL, index, 0), index as isize);
                let pixel = snapshot(picker, dc, |image, bounds| {
                    GetPixel(image, bounds.right * 2 / 5, bounds.bottom / 2)
                });
                assert_eq!(pixel, expected, "closed picker: {name}");
            }
            SendMessageW(picker, CB_SHOWDROPDOWN, 1, 0);
            let mut combo = COMBOBOXINFO {
                cbSize: size_of::<COMBOBOXINFO>() as u32,
                ..Default::default()
            };
            assert_ne!(GetComboBoxInfo(picker, &mut combo), 0);
            assert_ne!(IsWindowVisible(combo.hwndList), 0);
            SendMessageW(combo.hwndList, LB_SETTOPINDEX, 0, 0);
            snapshot(combo.hwndList, dc, |image, _| {
                for (index, (name, expected)) in COLORS.into_iter().enumerate() {
                    let mut row = RECT::default();
                    assert_ne!(
                        SendMessageW(
                            combo.hwndList,
                            LB_GETITEMRECT,
                            index,
                            (&raw mut row) as isize
                        ),
                        LB_ERR as isize
                    );
                    let pixel = GetPixel(
                        image,
                        (row.left + row.right) / 2,
                        (row.top + row.bottom) / 2,
                    );
                    assert_eq!(pixel, expected, "dropdown row: {name}");
                }
            });
            SendMessageW(picker, CB_SHOWDROPDOWN, 0, 0);
            ReleaseDC(parent, dc);
            DestroyWindow(parent);
        }
    }

    fn glyph_pixels(button: HWND, dc: HDC) -> Vec<bool> {
        snapshot(button, dc, |image, bounds| {
            let mut ink = Vec::new();
            let background = unsafe { GetPixel(image, bounds.right / 6, bounds.bottom / 2) };
            for y in bounds.bottom / 4..bounds.bottom * 3 / 4 {
                for x in bounds.right / 4..bounds.right * 3 / 4 {
                    let pixel = unsafe { GetPixel(image, x, y) };
                    ink.push([0, 8, 16].into_iter().any(|shift| {
                        ((pixel >> shift) & 0xffu32).abs_diff((background >> shift) & 0xff) > 48
                    }));
                }
            }
            assert!(
                ink.contains(&true) && ink.contains(&false),
                "button did not paint a glyph"
            );
            ink
        })
    }

    #[test]
    fn toolbar_buttons_render_distinct_glyphs_and_expose_their_english_labels() {
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
            let (tip, name) = labels(&ui.toolbar, ui.color);
            assert_eq!(tip, "Markup color");
            assert_eq!(name, tip);
            assert_eq!(
                labels(&ui.toolbar, ui.certificate).1,
                "Choose from the Windows certificate store"
            );
            assert_eq!(labels(&ui.toolbar, ui.sign).1, "Sign PDF… (Ctrl+S)");
            ui.toolbar.watermark(ui.watermark, true).unwrap();
            let (tip, name) = labels(&ui.toolbar, ui.watermark);
            assert!(tip.contains("custom image") && tip == name);
            SelectObject(dc, old);
            ReleaseDC(parent, dc);
            ui.show_signing(true);
            Controls::layout(parent);
            let dc = GetDC(parent);
            let p = theme::palette();
            if !p.high_contrast {
                let surface = |button| snapshot(button, dc, |image, _| GetPixel(image, 3, 3));
                for toggle in [ui.highlighter, ui.underline] {
                    assert_eq!(surface(toggle), p.chrome);
                    SendMessageW(toggle, BM_SETCHECK, 1, 0);
                    assert_eq!(SendMessageW(toggle, BM_GETCHECK, 0, 0), 1);
                    assert_eq!(surface(toggle), p.selected);
                    SendMessageW(toggle, BM_SETCHECK, 0, 0);
                    assert_eq!(surface(toggle), p.chrome);
                }
                assert_eq!(surface(ui.color), p.field);
                assert_eq!(surface(ui.position), p.field);
                assert_eq!(surface(ui.page), p.field);
                let mut prior_ink = 0;
                for label in [w!("I"), w!("MMMM")] {
                    let index = SendMessageW(ui.position, CB_ADDSTRING, 0, label.0 as isize);
                    assert!(index >= 0);
                    SendMessageW(ui.position, CB_SETCURSEL, index as usize, 0);
                    let ink = snapshot(ui.position, dc, |image, bounds| {
                        let mut count = 0;
                        for y in 3..bounds.bottom - 3 {
                            for x in 3..bounds.right - dips(ui.position, 18) {
                                count += usize::from(GetPixel(image, x, y) != p.field);
                            }
                        }
                        count
                    });
                    assert!(ink > prior_ink, "closed combo did not paint its selection");
                    prior_ink = ink;
                }
                let old_focus = GetFocus();
                ShowWindow(parent, SW_SHOWNOACTIVATE);
                SetFocus(ui.page);
                assert_eq!(GetFocus(), ui.page);
                assert_eq!(
                    snapshot(ui.page, dc, |image, _| GetPixel(image, 0, 0)),
                    p.accent
                );
                SetFocus(old_focus);
                ShowWindow(parent, SW_HIDE);
            }
            let icons = [ui.fit, ui.sign, ui.pfx];
            let reference = icons.map(|button| glyph_pixels(button, dc));
            for width in [440, 649, 650, 900] {
                SetWindowPos(
                    parent,
                    null_mut(),
                    0,
                    0,
                    dips(parent, width),
                    480,
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
                Controls::layout(parent);
                assert_eq!(
                    icons.map(|button| glyph_pixels(button, dc)),
                    reference,
                    "width {width}"
                );
            }
            ReleaseDC(parent, dc);
            ui.toolbar.clear_names();
            DestroyWindow(parent);
            drop(ui);
            CoUninitialize();
        }
    }
}
