//! Paint the frame and toolbar with GDI while keeping input, accessibility and
//! the system contrast theme in the standard Windows controls.
use super::*;
use std::cell::Cell;
use windows_sys::Win32::{
    Graphics::Dwm::{
        DWMWA_CAPTION_COLOR, DWMWA_COLOR_DEFAULT, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE,
        DwmSetWindowAttribute,
    },
    UI::{
        Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW},
        Controls::{BST_CHECKED, WM_MOUSELEAVE},
    },
};

#[derive(Clone, Copy)]
pub(super) struct Palette {
    pub(super) high_contrast: bool,
    pub(super) chrome: COLORREF,
    pub(super) canvas: COLORREF,
    pub(super) edge: COLORREF,
    pub(super) field: COLORREF,
    pub(super) hover: COLORREF,
    pub(super) selected: COLORREF,
    pub(super) text: COLORREF,
    pub(super) muted: COLORREF,
    pub(super) disabled: COLORREF,
    pub(super) accent: COLORREF,
}

const fn rgb(hex: u32) -> COLORREF {
    ((hex & 0xff) << 16) | (hex & 0xff00) | (hex >> 16)
}

const DARK: Palette = Palette {
    high_contrast: false,
    chrome: rgb(0x22252b),
    canvas: rgb(0x191b20),
    edge: rgb(0x424750),
    field: rgb(0x2b2f36),
    hover: rgb(0x343a44),
    selected: rgb(0x30445f),
    text: rgb(0xdce0e6),
    muted: rgb(0xb0b7c2),
    disabled: rgb(0x7f8793),
    accent: rgb(0x91b7ed),
};

fn system_palette() -> Palette {
    unsafe {
        let mut contrast = HIGHCONTRASTW {
            cbSize: size_of::<HIGHCONTRASTW>() as u32,
            ..Default::default()
        };
        if SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            contrast.cbSize,
            (&raw mut contrast).cast(),
            0,
        ) != 0
            && contrast.dwFlags & HCF_HIGHCONTRASTON == 0
        {
            return DARK;
        }
        Palette {
            high_contrast: true,
            chrome: GetSysColor(COLOR_BTNFACE),
            canvas: GetSysColor(COLOR_WINDOW),
            edge: GetSysColor(COLOR_WINDOWFRAME),
            field: GetSysColor(COLOR_WINDOW),
            hover: GetSysColor(COLOR_BTNFACE),
            selected: GetSysColor(COLOR_HIGHLIGHT),
            text: GetSysColor(COLOR_WINDOWTEXT),
            muted: GetSysColor(COLOR_WINDOWTEXT),
            disabled: GetSysColor(COLOR_GRAYTEXT),
            accent: GetSysColor(COLOR_HIGHLIGHTTEXT),
        }
    }
}

struct Brushes(HBRUSH, HBRUSH);
impl Drop for Brushes {
    fn drop(&mut self) {
        unsafe {
            DeleteObject(self.0);
            DeleteObject(self.1);
        }
    }
}

thread_local! {
    static PALETTE: Cell<Palette> = Cell::new(system_palette());
    // A control may use its returned color brush on a different DC.
    static BRUSHES: Brushes = unsafe { Brushes(CreateSolidBrush(DARK.chrome), CreateSolidBrush(DARK.field)) };
}

pub(super) fn palette() -> Palette {
    PALETTE.with(Cell::get)
}

pub(super) fn refresh(hwnd: HWND) {
    let p = system_palette();
    PALETTE.set(p);
    unsafe {
        // DWM ignores unsupported attributes on earlier Windows versions.
        for (attribute, value) in [
            (DWMWA_USE_IMMERSIVE_DARK_MODE, u32::from(!p.high_contrast)),
            (
                DWMWA_CAPTION_COLOR,
                if p.high_contrast {
                    DWMWA_COLOR_DEFAULT
                } else {
                    p.chrome
                },
            ),
            (
                DWMWA_TEXT_COLOR,
                if p.high_contrast {
                    DWMWA_COLOR_DEFAULT
                } else {
                    p.text
                },
            ),
        ] {
            DwmSetWindowAttribute(
                hwnd,
                attribute as u32,
                (&raw const value).cast(),
                size_of::<u32>() as u32,
            );
        }
        RedrawWindow(
            hwnd,
            null(),
            null_mut(),
            RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_FRAME,
        );
    }
}

pub(super) fn control_color(dc: HDC, field: bool, enabled: bool) -> LRESULT {
    let p = palette();
    let text = if !enabled {
        p.disabled
    } else if field {
        p.text
    } else {
        p.muted
    };
    unsafe {
        SetTextColor(dc, text);
        SetBkColor(dc, if field { p.field } else { p.chrome });
    }
    BRUSHES.with(|brushes| if field { brushes.1 } else { brushes.0 }) as LRESULT
}

pub(super) fn fill(dc: HDC, area: &RECT, color: COLORREF) {
    unsafe {
        SetDCBrushColor(dc, color);
        FillRect(dc, area, GetStockObject(DC_BRUSH));
    }
}

fn frame(dc: HDC, bounds: &RECT, color: COLORREF) {
    unsafe {
        SetDCBrushColor(dc, color);
        FrameRect(dc, bounds, GetStockObject(DC_BRUSH));
    }
}

pub(super) fn paint_chrome(hwnd: HWND, dc: HDC) {
    let p = palette();
    let bounds = rect(hwnd);
    fill(dc, &bounds, p.chrome);
    let mut canvas_bounds = RECT::default();
    unsafe {
        let canvas = GetDlgItem(hwnd, CANVAS_ID as i32);
        if !canvas.is_null() && GetWindowRect(canvas, &mut canvas_bounds) != 0 {
            MapWindowPoints(null_mut(), hwnd, (&raw mut canvas_bounds).cast(), 2);
            for y in [canvas_bounds.top - 1, canvas_bounds.bottom] {
                fill(
                    dc,
                    &RECT {
                        top: y,
                        bottom: y + 1,
                        ..bounds
                    },
                    p.edge,
                );
            }
        }
    }
}

fn paint_control(hwnd: HWND, dc: HDC, hot: bool) {
    let bounds = rect(hwnd);
    let p = palette();
    unsafe {
        let saved = SaveDC(dc);
        let id = GetDlgCtrlID(hwnd) as usize;
        let combo = matches!(id, COLOR | SIGN_POSITION);
        let enabled = IsWindowEnabled(hwnd) != 0;
        let selected = matches!(id, HIGHLIGHT | UNDERLINE)
            && SendMessageW(hwnd, BM_GETCHECK, 0, 0) == BST_CHECKED as isize;
        let pressed = if combo {
            SendMessageW(hwnd, CB_GETDROPPEDSTATE, 0, 0) != 0
        } else {
            SendMessageW(hwnd, BM_GETSTATE, 0, 0) as u32 & BST_PUSHED != 0
        };
        let bg = if enabled && pressed {
            rgb(0x414854)
        } else if selected && hot {
            rgb(0x394e69)
        } else if selected {
            p.selected
        } else if enabled && hot {
            p.hover
        } else if combo || id == PAGE {
            p.field
        } else {
            p.chrome
        };
        let fg = if !enabled {
            p.disabled
        } else if selected {
            p.accent
        } else {
            p.text
        };
        fill(dc, &bounds, bg);
        if GetFocus() == hwnd {
            frame(dc, &bounds, p.accent);
        } else if combo {
            frame(dc, &bounds, p.edge);
        }
        let font = SendMessageW(hwnd, WM_GETFONT, 0, 0) as HFONT;
        if !font.is_null() {
            SelectObject(dc, font);
        }
        SetTextColor(dc, fg);
        SetBkMode(dc, TRANSPARENT as i32);
        let mut label_bounds = bounds;
        let alignment = if combo {
            label_bounds.left += dips(hwnd, 5);
            label_bounds.right -= dips(hwnd, 17);
            let x = bounds.right - dips(hwnd, 10);
            let y = bounds.bottom / 2 - dips(hwnd, 1);
            let half = dips(hwnd, 3);
            SelectObject(dc, GetStockObject(DC_PEN));
            SetDCPenColor(dc, fg);
            let chevron = [
                POINT { x: x - half, y },
                POINT { x, y: y + half },
                POINT { x: x + half, y },
            ];
            Polyline(dc, chevron.as_ptr(), chevron.len() as i32);
            DT_LEFT | DT_END_ELLIPSIS
        } else {
            DT_CENTER
        };
        // Common labels fit on the stack; ellipsize long signature field names.
        let mut text = [0u16; 256];
        let len = GetWindowTextW(hwnd, text.as_mut_ptr(), text.len() as i32);
        DrawTextW(
            dc,
            text.as_ptr(),
            len,
            &mut label_bounds,
            alignment | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
        );
        RestoreDC(dc, saved);
    }
}

pub(super) fn style_control(hwnd: HWND) -> Result<(), String> {
    let procedure: SUBCLASSPROC = if unsafe { GetDlgCtrlID(hwnd) } == PAGE_INPUT as i32 {
        Some(edit_proc)
    } else {
        Some(control_proc)
    };
    if unsafe { SetWindowSubclass(hwnd, procedure, 1, 0) } == 0 {
        return Err("ツールバーの外観を設定できません。".into());
    }
    Ok(())
}

// The EDIT paints its own text, selection and caret. Its WS_BORDER lives
// outside the client area, so color just that border after native painting.
unsafe extern "system" fn edit_proc(
    hwnd: HWND,
    msg: u32,
    w: WPARAM,
    l: LPARAM,
    id: usize,
    _: usize,
) -> LRESULT {
    unsafe {
        if msg == WM_NCDESTROY {
            RemoveWindowSubclass(hwnd, Some(edit_proc), id);
        }
        let result = DefSubclassProc(hwnd, msg, w, l);
        if matches!(msg, WM_NCPAINT | WM_SETFOCUS | WM_KILLFOCUS) && !palette().high_contrast {
            let dc = GetWindowDC(hwnd);
            if !dc.is_null() {
                let mut bounds = RECT::default();
                GetWindowRect(hwnd, &mut bounds);
                let (x, y) = (bounds.left, bounds.top);
                OffsetRect(&mut bounds, -x, -y);
                frame(
                    dc,
                    &bounds,
                    if GetFocus() == hwnd {
                        palette().accent
                    } else {
                        palette().edge
                    },
                );
                ReleaseDC(hwnd, dc);
            }
        }
        result
    }
}

unsafe extern "system" fn control_proc(
    hwnd: HWND,
    msg: u32,
    w: WPARAM,
    l: LPARAM,
    id: usize,
    hot: usize,
) -> LRESULT {
    unsafe {
        if msg == WM_NCDESTROY {
            RemoveWindowSubclass(hwnd, Some(control_proc), id);
        } else if !palette().high_contrast {
            match msg {
                WM_PAINT => {
                    let mut ps = PAINTSTRUCT::default();
                    let dc = BeginPaint(hwnd, &mut ps);
                    paint_control(hwnd, dc, hot != 0);
                    EndPaint(hwnd, &ps);
                    return 0;
                }
                WM_PRINT if l as i32 & PRF_CLIENT != 0 => {
                    if l as i32 & PRF_CHECKVISIBLE == 0 || IsWindowVisible(hwnd) != 0 {
                        paint_control(hwnd, w as HDC, hot != 0);
                    }
                    return 0;
                }
                WM_PRINTCLIENT => {
                    paint_control(hwnd, w as HDC, hot != 0);
                    return 0;
                }
                WM_ERASEBKGND => return 1,
                WM_CTLCOLORLISTBOX => return control_color(w as HDC, true, true),
                WM_MOUSEMOVE if hot == 0 => {
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    if TrackMouseEvent(&mut track) != 0 {
                        SetWindowSubclass(hwnd, Some(control_proc), id, 1);
                        InvalidateRect(hwnd, null(), 0);
                    }
                }
                WM_MOUSELEAVE => {
                    SetWindowSubclass(hwnd, Some(control_proc), id, 0);
                    InvalidateRect(hwnd, null(), 0);
                }
                WM_SETFOCUS | WM_KILLFOCUS | WM_ENABLE | WM_UPDATEUISTATE | WM_CAPTURECHANGED
                | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_LBUTTONDBLCLK | WM_KEYDOWN | WM_KEYUP
                | WM_CHAR | WM_MOUSEWHEEL | WM_COMMAND | WM_SETTEXT | BM_SETCHECK | BM_SETSTATE
                | CB_SETCURSEL | CB_SHOWDROPDOWN | CB_SELECTSTRING => {
                    let result = DefSubclassProc(hwnd, msg, w, l);
                    InvalidateRect(hwnd, null(), 0);
                    return result;
                }
                _ => {}
            }
        }
        DefSubclassProc(hwnd, msg, w, l)
    }
}
