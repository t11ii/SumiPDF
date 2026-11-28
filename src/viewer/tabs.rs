use super::*;
use std::path::Path;
use windows_sys::Win32::UI::Controls::*;

/// A tab keeps editing state and its original open stream. Page images and parsed
/// text belong only to the active canvas and worker.
pub(super) struct Tab {
    path: PathBuf,
    pub(super) close_button: HWND,
    state: Option<DocumentState>,
}

#[derive(Default)]
struct DocumentState {
    source: Option<crate::winpdf::SigningSource>,
    page: u32,
    zoom: f64,
    position: canvas::ViewPosition,
    marks: Vec<Mark>,
    saved_marks: Vec<Mark>,
    tool: Option<MarkKind>,
    color: MarkColor,
    signing: Option<signing::Session>,
    known_signed: bool,
}

impl Tab {
    pub(super) fn clear_certificate(&mut self) {
        if let Some(session) = self.state.as_mut().and_then(|s| s.signing.as_mut()) {
            session.clear_certificate();
        }
    }

    pub(super) fn dirty(&self) -> bool {
        self.state
            .as_ref()
            .is_some_and(|s| s.marks != s.saved_marks)
    }
}

pub(super) fn create(parent: HWND) -> Result<HWND, String> {
    let init = INITCOMMONCONTROLSEX {
        dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_TAB_CLASSES,
    };
    if unsafe { InitCommonControlsEx(&init) } == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let hwnd = child(
        parent,
        "SysTabControl32",
        "",
        TABS,
        WS_TABSTOP | WS_CLIPCHILDREN | TCS_FIXEDWIDTH | TCS_FOCUSONBUTTONDOWN,
    )?;
    if unsafe { SetWindowSubclass(hwnd, Some(tab_proc), 1, 0) } == 0 {
        return Err("Could not initialize the tab strip.".into());
    }
    Ok(hwnd)
}

pub(super) fn item_size(hwnd: HWND) {
    let packed = (dips(hwnd, 29) << 16) | dips(hwnd, 150);
    unsafe {
        SendMessageW(hwnd, TCM_SETITEMSIZE, 0, packed as isize);
    }
}

fn insert(hwnd: HWND, index: usize, name: &str) -> Result<HWND, String> {
    // The native BUTTON supplies capture, click cancellation, keyboard and
    // accessibility. Only its painted face differs from its spoken caption.
    let button = child(
        hwnd,
        "BUTTON",
        &format!("Close {name}"),
        0,
        BS_OWNERDRAW as u32,
    )?;
    let mut text = wide(name);
    let item = TCITEMW {
        mask: TCIF_TEXT | TCIF_PARAM,
        pszText: text.as_mut_ptr(),
        lParam: button as isize,
        ..Default::default()
    };
    unsafe {
        // Child identifiers must be unique within the strip. The full HWND
        // also travels in BN_CLICKED, independently of its 16-bit command ID.
        SetWindowLongPtrW(button, GWLP_ID, button as isize);
        ShowWindow(button, SW_HIDE);
        if SendMessageW(hwnd, TCM_INSERTITEMW, index, (&raw const item) as isize) < 0 {
            DestroyWindow(button);
            return Err("Could not add a document tab.".into());
        }
    }
    Ok(button)
}

pub(super) fn update_label(hwnd: HWND, index: usize, name: &str) {
    let mut text = wide(name);
    let item = TCITEMW {
        mask: TCIF_TEXT,
        pszText: text.as_mut_ptr(),
        ..Default::default()
    };
    unsafe {
        SendMessageW(hwnd, TCM_SETITEMW, index, (&raw const item) as isize);
    }
}

fn select(hwnd: HWND, index: usize) {
    unsafe {
        SendMessageW(hwnd, TCM_SETCURSEL, index, 0);
        InvalidateRect(hwnd, null(), 0);
    }
}

pub(super) fn selected(hwnd: HWND) -> Option<usize> {
    usize::try_from(unsafe { SendMessageW(hwnd, TCM_GETCURSEL, 0, 0) }).ok()
}

fn close_button(hwnd: HWND, index: usize) -> HWND {
    let mut item = TCITEMW {
        mask: TCIF_PARAM,
        ..Default::default()
    };
    unsafe {
        SendMessageW(hwnd, TCM_GETITEMW, index, (&raw mut item) as isize);
    }
    item.lParam as HWND
}

pub(super) fn post_selected(hwnd: HWND, command: usize) {
    if let Some(index) = selected(hwnd) {
        let button = close_button(hwnd, index);
        if !button.is_null() {
            unsafe {
                PostMessageW(GetParent(hwnd), ACTION, command, button as isize);
            }
        }
    }
}

fn close_rect(hwnd: HWND, tab: RECT) -> RECT {
    let edge = dips(hwnd, 24);
    let right = tab.right - dips(hwnd, 4);
    let top = (tab.top + tab.bottom - edge) / 2;
    RECT {
        left: right - edge,
        right,
        top,
        bottom: top + edge,
    }
}

fn position_buttons(hwnd: HWND) {
    unsafe {
        let mut limit = rect(hwnd).right;
        let arrows = FindWindowExW(hwnd, null_mut(), UPDOWN_CLASSW, null());
        if !arrows.is_null() && GetWindowLongW(arrows, GWL_STYLE) as u32 & WS_VISIBLE != 0 {
            let mut r = RECT::default();
            GetWindowRect(arrows, &mut r);
            MapWindowPoints(null_mut(), hwnd, (&raw mut r).cast(), 2);
            limit = limit.min(r.left);
        }
        let count = SendMessageW(hwnd, TCM_GETITEMCOUNT, 0, 0) as usize;
        for index in 0..count {
            let button = close_button(hwnd, index);
            if button.is_null() {
                continue;
            }
            let mut tab = RECT::default();
            let found = SendMessageW(hwnd, TCM_GETITEMRECT, index, (&raw mut tab) as isize) != 0;
            let r = close_rect(hwnd, tab);
            if found && r.left >= 0 && r.right <= limit {
                SetWindowPos(
                    button,
                    null_mut(),
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOACTIVATE | SWP_NOZORDER | SWP_SHOWWINDOW,
                );
                InvalidateRect(button, null(), 0);
            } else if GetWindowLongW(button, GWL_STYLE) as u32 & WS_VISIBLE != 0 {
                if GetCapture() == button {
                    SendMessageW(button, WM_CANCELMODE, 0, 0);
                }
                if GetFocus() == button {
                    SetFocus(hwnd);
                }
                ShowWindow(button, SW_HIDE);
            }
        }
    }
}

fn draw_close(hwnd: HWND, draw: &DRAWITEMSTRUCT) {
    let palette = theme::palette();
    let active = selected(hwnd).is_some_and(|i| close_button(hwnd, i) == draw.hwndItem);
    let pressed = draw.itemState & ODS_SELECTED != 0;
    let bg = if pressed {
        palette.hover
    } else if active {
        palette.selected
    } else {
        palette.chrome
    };
    let fg = if unsafe { IsWindowEnabled(hwnd) } == 0 {
        palette.disabled
    } else if palette.high_contrast && active && !pressed {
        palette.accent
    } else if palette.high_contrast {
        unsafe { GetSysColor(COLOR_BTNTEXT) }
    } else {
        palette.text
    };
    let dc = draw.hDC;
    theme::fill(dc, &draw.rcItem, bg);
    let r = draw.rcItem;
    let offset = if pressed { dips(hwnd, 1) } else { 0 };
    let x = (r.left + r.right) / 2 + offset;
    let y = (r.top + r.bottom) / 2 + offset;
    let half = dips(hwnd, 4);
    unsafe {
        let pen = CreatePen(PS_SOLID, dips(hwnd, 1), fg);
        let old = SelectObject(dc, pen);
        MoveToEx(dc, x - half, y - half, null_mut());
        LineTo(dc, x + half + 1, y + half + 1);
        MoveToEx(dc, x - half, y + half, null_mut());
        LineTo(dc, x + half + 1, y - half - 1);
        SelectObject(dc, old);
        DeleteObject(pen);
        if draw.itemState & (ODS_FOCUS | ODS_NOFOCUSRECT) == ODS_FOCUS {
            DrawFocusRect(dc, &r);
        }
    }
}

unsafe extern "system" fn tab_proc(
    hwnd: HWND,
    msg: u32,
    w: WPARAM,
    l: LPARAM,
    id: usize,
    _: usize,
) -> LRESULT {
    unsafe {
        match msg {
            WM_DRAWITEM if l != 0 && (*(l as *const DRAWITEMSTRUCT)).CtlType == ODT_BUTTON => {
                draw_close(hwnd, &*(l as *const DRAWITEMSTRUCT));
                return 1;
            }
            WM_COMMAND if w >> 16 == BN_CLICKED as usize && l != 0 => {
                PostMessageW(GetParent(hwnd), ACTION, CLOSE_TAB, l);
                return 0;
            }
            WM_SIZE | WM_HSCROLL | WM_KEYDOWN | WM_LBUTTONDOWN | TCM_SETCURSEL
            | TCM_SETCURFOCUS | TCM_SETITEMSIZE | WM_ENABLE => {
                let result = DefSubclassProc(hwnd, msg, w, l);
                position_buttons(hwnd);
                return result;
            }
            WM_ERASEBKGND => return 1,
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut ps);
                paint(hwnd, dc);
                EndPaint(hwnd, &ps);
                return 0;
            }
            WM_PRINTCLIENT => {
                paint(hwnd, w as HDC);
                return 0;
            }
            _ => {}
        }
        if msg == WM_NCDESTROY {
            RemoveWindowSubclass(hwnd, Some(tab_proc), id);
        }
        DefSubclassProc(hwnd, msg, w, l)
    }
}

fn paint(hwnd: HWND, dc: HDC) {
    let palette = theme::palette();
    let bounds = rect(hwnd);
    unsafe {
        let saved = SaveDC(dc);
        let brush = GetStockObject(DC_BRUSH) as HBRUSH;
        SetDCBrushColor(dc, palette.chrome);
        FillRect(dc, &bounds, brush);
        SelectObject(dc, SendMessageW(hwnd, WM_GETFONT, 0, 0) as _);
        SetBkMode(dc, TRANSPARENT as i32);
        let active = selected(hwnd);
        let mut test = TCHITTESTINFO {
            pt: POINT {
                x: dips(hwnd, 7),
                y: bounds.bottom / 2,
            },
            ..Default::default()
        };
        let first = SendMessageW(hwnd, TCM_HITTEST, 0, (&raw mut test) as isize).max(0) as usize;
        let count = SendMessageW(hwnd, TCM_GETITEMCOUNT, 0, 0).max(0) as usize;
        for index in first..count {
            let mut r = RECT::default();
            if SendMessageW(hwnd, TCM_GETITEMRECT, index, (&raw mut r) as isize) == 0
                || r.left >= bounds.right
            {
                break;
            }
            if r.right <= 0 {
                continue;
            }
            let current = active == Some(index);
            if current {
                SetDCBrushColor(dc, palette.selected);
                FillRect(dc, &r, brush);
                let line = RECT {
                    top: r.bottom - dips(hwnd, 2),
                    ..r
                };
                SetDCBrushColor(dc, palette.accent);
                FillRect(dc, &line, brush);
                if GetFocus() == hwnd
                    && SendMessageW(hwnd, WM_QUERYUISTATE, 0, 0) as u32 & UISF_HIDEFOCUS == 0
                {
                    let mut focus = r;
                    InflateRect(&mut focus, -2, -2);
                    DrawFocusRect(dc, &focus);
                }
            }
            let mut text = [0u16; 512];
            let mut item = TCITEMW {
                mask: TCIF_TEXT,
                pszText: text.as_mut_ptr(),
                cchTextMax: text.len() as i32,
                ..Default::default()
            };
            if SendMessageW(hwnd, TCM_GETITEMW, index, (&raw mut item) as isize) != 0 {
                r.right = close_rect(hwnd, r).left - dips(hwnd, 3);
                r.left += dips(hwnd, 12);
                let color = if palette.high_contrast && current {
                    palette.accent
                } else if palette.high_contrast {
                    GetSysColor(COLOR_BTNTEXT)
                } else if current {
                    palette.text
                } else {
                    palette.muted
                };
                SetTextColor(dc, color);
                DrawTextW(
                    dc,
                    text.as_ptr(),
                    -1,
                    &mut r,
                    DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS,
                );
            }
        }
        RestoreDC(dc, saved);
    }
}

impl App {
    fn ensure_worker(&mut self) -> Result<(), String> {
        if self.worker.is_none() {
            self.worker = Some(worker::spawn(self.hwnd as usize, self.events.clone())?);
        }
        Ok(())
    }

    fn add_tab(&mut self, path: PathBuf, state: Option<DocumentState>) -> Result<usize, String> {
        self.ensure_worker()?;
        let index = self.tabs.len();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let close_button = insert(self.ui.tab_bar, index, &name)?;
        self.tabs.push(Tab {
            path,
            close_button,
            state,
        });
        Ok(index)
    }

    pub(super) fn open_signed_copy(&mut self, path: &Path, page: u32) -> Result<(), String> {
        let path = std::path::absolute(path).map_err(|e| e.to_string())?;
        let index = self.add_tab(
            path,
            Some(DocumentState {
                page,
                zoom: 1.0,
                known_signed: true,
                ..Default::default()
            }),
        )?;
        self.switch_tab(index)
    }

    pub(super) fn open_tabs(
        &mut self,
        paths: impl IntoIterator<Item = PathBuf>,
    ) -> Result<(), String> {
        if self.writing {
            return Ok(());
        }
        let mut target = None;
        for path in paths {
            let path = std::path::absolute(path).map_err(|e| e.to_string())?;
            if let Some(index) = self.tabs.iter().position(|t| t.path == path) {
                target = Some(index);
                continue;
            }
            target = Some(self.add_tab(path, None)?);
        }
        if let Some(index) = target {
            if self.active_tab == Some(index) {
                position_buttons(self.ui.tab_bar);
            } else {
                self.switch_tab(index)?;
            }
        }
        Ok(())
    }

    fn park_tab(&mut self) {
        if let Some(index) = self.active_tab.take() {
            self.tabs[index].state = Some(DocumentState {
                source: self.resume_source.take(),
                page: self.page,
                zoom: self.zoom,
                position: VIEW.with_borrow(|v| v.position()),
                marks: std::mem::take(&mut self.marks),
                saved_marks: std::mem::take(&mut self.saved_marks),
                tool: self.tool.take(),
                color: self.color,
                signing: self.signing.take(),
                known_signed: self.known_signed,
            });
        }
    }

    pub(super) fn switch_tab(&mut self, index: usize) -> Result<(), String> {
        if index >= self.tabs.len() {
            return Ok(());
        }
        if self.writing {
            if let Some(current) = self.active_tab {
                select(self.ui.tab_bar, current);
            }
            return Ok(());
        }
        if self.active_tab == Some(index) {
            return Ok(());
        }
        self.park_tab();
        self.activate_tab(Some(index))
    }

    pub(super) fn cycle_tab(&mut self, direction: isize) -> Result<(), String> {
        if let Some(index) = self.active_tab {
            let next = (index as isize + direction).rem_euclid(self.tabs.len() as isize) as usize;
            self.switch_tab(next)?;
        }
        Ok(())
    }

    pub(super) fn close_tab(&mut self, index: usize) -> Result<(), String> {
        let Some(active) = self.active_tab else {
            return Ok(());
        };
        let tab = &self.tabs[index];
        let dirty = if index == active {
            self.marks != self.saved_marks
        } else {
            tab.dirty()
        };
        if self.writing || (dirty && !self.confirm_discard(&tab.path)) {
            return Ok(());
        }
        unsafe {
            if SendMessageW(self.ui.tab_bar, TCM_DELETEITEM, index, 0) == 0 {
                return Err("Could not remove the document tab.".into());
            }
            let focus = GetFocus();
            if focus == tab.close_button || (focus == self.ui.tab_bar && self.tabs.len() == 1) {
                SetFocus(if self.tabs.len() == 1 {
                    self.ui.open
                } else {
                    self.ui.tab_bar
                });
            }
            DestroyWindow(tab.close_button);
        }
        self.tabs.remove(index);
        if index != active {
            let current = active - usize::from(index < active);
            self.active_tab = Some(current);
            if selected(self.ui.tab_bar) == Some(current) {
                position_buttons(self.ui.tab_bar);
            } else {
                select(self.ui.tab_bar, current);
            }
            return Ok(());
        }
        self.active_tab = None;
        self.activate_tab(self.tabs.len().checked_sub(1).map(|last| index.min(last)))
    }

    fn activate_tab(&mut self, index: Option<usize>) -> Result<(), String> {
        self.close_find();
        self.stop_motion();
        self.jump_input(false);
        self.release_text();
        self.revision += 1;
        self.ticket += 1;
        self.document = None;
        self.requested = None;
        self.checking_fields = None;
        self.wheel_delta = 0;
        self.text_ready = false;
        self.has_text = false;
        self.active_tab = index;
        let state = index
            .and_then(|i| self.tabs[i].state.take())
            .unwrap_or_else(|| DocumentState {
                zoom: 1.0,
                ..Default::default()
            });
        self.path = index.map(|i| self.tabs[i].path.clone()).unwrap_or_default();
        self.page = state.page;
        self.zoom = state.zoom;
        self.tool = state.tool;
        self.color = state.color;
        self.known_signed = state.known_signed;
        self.resume_source = state.source;
        self.signing = state.signing;
        self.marks = state.marks;
        self.saved_marks = state.saved_marks;
        self.restore_signing_controls()?;
        let show = if index.is_some() { SW_SHOW } else { SW_HIDE };
        unsafe {
            ShowWindow(self.ui.tab_bar, show);
            let color = MarkColor::ALL
                .iter()
                .position(|c| *c == self.color)
                .unwrap_or(0);
            SendMessageW(self.ui.color, CB_SETCURSEL, color, 0);
        }
        if let Some(i) = index {
            select(self.ui.tab_bar, i);
        }
        Controls::layout(self.hwnd);
        self.title();
        self.reset_canvas("Opening PDF…");
        VIEW.with_borrow_mut(|v| {
            v.set_tool(None);
            v.set_signature(false, None);
            v.empty = index.is_none();
            v.restore_position(state.position);
        });
        self.sync_signature();
        self.controls();
        let Some(_) = index else {
            self.send(Job::Close);
            set_text(self.ui.status, "");
            return Ok(());
        };
        set_text(self.ui.status, "Opening PDF…");
        let job = match &self.resume_source {
            Some(source) => Job::Restore {
                revision: self.revision,
                source: source.clone(),
            },
            None => Job::Open {
                revision: self.revision,
                path: self.path.clone(),
            },
        };
        self.send(job);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::{
        Win32::{
            Foundation::HWND as WinHwnd,
            UI::Accessibility::{AccessibleObjectFromWindow, IAccessible},
        },
        core::{Interface, w},
    };

    fn next_close(parent: HWND) -> Option<HWND> {
        let mut msg = MSG::default();
        if unsafe { PeekMessageW(&mut msg, parent, ACTION, ACTION, PM_REMOVE) } == 0 {
            return None;
        }
        assert_eq!(msg.wParam, CLOSE_TAB);
        Some(msg.lParam as HWND)
    }

    #[test]
    fn inline_close_buttons_track_overflow_and_cancel_pointer_input() {
        unsafe {
            assert!(CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32) >= 0);
            let parent = CreateWindowExW(
                0,
                w!("STATIC").0,
                null(),
                WS_POPUP,
                0,
                0,
                640,
                120,
                null_mut(),
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            );
            assert!(!parent.is_null());
            let strip = create(parent).unwrap();
            item_size(strip);
            MoveWindow(strip, 0, 0, dips(strip, 450), dips(strip, 34), 1);
            ShowWindow(parent, SW_SHOWNOACTIVATE);
            let buttons = ["one.pdf", "two.pdf", "three.pdf", "four.pdf"]
                .into_iter()
                .enumerate()
                .map(|(index, name)| insert(strip, index, name).unwrap())
                .collect::<Vec<_>>();
            select(strip, 0);
            let button = buttons[1];
            assert_ne!(IsWindowVisible(button), 0);
            assert_eq!(IsWindowVisible(buttons[3]), 0);
            let mut area = RECT::default();
            GetWindowRect(button, &mut area);
            MapWindowPoints(null_mut(), strip, (&raw mut area).cast(), 2);
            let point = POINT {
                x: (area.right + area.left) / 2,
                y: (area.top + area.bottom) / 2,
            };
            assert_eq!(
                ChildWindowFromPointEx(strip, point, CWP_SKIPINVISIBLE),
                button
            );
            let mut hit = TCHITTESTINFO {
                pt: point,
                ..Default::default()
            };
            assert_eq!(
                SendMessageW(strip, TCM_HITTEST, 0, (&raw mut hit) as isize),
                1
            );
            let mut raw = null_mut();
            AccessibleObjectFromWindow(
                WinHwnd(button),
                OBJID_CLIENT as u32,
                &IAccessible::IID,
                &mut raw,
            )
            .unwrap();
            let accessible = IAccessible::from_raw(raw);
            assert_eq!(
                accessible.get_accName(&0i32.into()).unwrap().to_string(),
                "Close two.pdf"
            );
            drop(accessible);

            let inside = 0x00070007;
            let outside = -1; // (-1, -1) in button client coordinates.
            SendMessageW(button, WM_LBUTTONDOWN, 1, inside);
            SendMessageW(button, WM_MOUSEMOVE, 1, outside);
            SendMessageW(button, WM_LBUTTONUP, 0, outside);
            assert_eq!(next_close(parent), None);
            SendMessageW(button, WM_LBUTTONDOWN, 1, inside);
            SetCapture(parent);
            ReleaseCapture();
            SendMessageW(button, WM_LBUTTONUP, 0, inside);
            assert_eq!(next_close(parent), None);
            SendMessageW(button, WM_LBUTTONDOWN, 1, inside);
            SendMessageW(button, WM_LBUTTONUP, 0, inside);
            assert_eq!(next_close(parent), Some(button));
            assert_eq!(selected(strip), Some(0));

            select(strip, 3);
            assert_ne!(IsWindowVisible(buttons[3]), 0);
            assert_eq!(IsWindowVisible(buttons[0]), 0);
            DestroyWindow(parent);
            CoUninitialize();
        }
    }
}
