use super::*;
use windows_sys::Win32::UI::Controls::*;

/// A tab keeps editing state and its original open stream. Page images and parsed
/// text belong only to the active canvas and worker.
pub(super) struct Tab {
    path: PathBuf,
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
        return Err("タブを初期化できません。".into());
    }
    Ok(hwnd)
}

pub(super) fn item_size(hwnd: HWND) {
    let packed = (dips(hwnd, 29) << 16) | dips(hwnd, 150);
    unsafe {
        SendMessageW(hwnd, TCM_SETITEMSIZE, 0, packed as isize);
    }
}

fn label(hwnd: HWND, msg: u32, index: usize, name: &str) -> isize {
    let mut text = wide(name);
    let item = TCITEMW {
        mask: TCIF_TEXT,
        pszText: text.as_mut_ptr(),
        ..Default::default()
    };
    unsafe { SendMessageW(hwnd, msg, index, (&raw const item) as isize) }
}

pub(super) fn update_label(hwnd: HWND, index: usize, name: &str) {
    label(hwnd, TCM_SETITEMW, index, name);
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

unsafe extern "system" fn tab_proc(
    hwnd: HWND,
    msg: u32,
    w: WPARAM,
    l: LPARAM,
    id: usize,
    _: usize,
) -> LRESULT {
    unsafe {
        if !theme::palette().high_contrast {
            match msg {
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
                r.left += dips(hwnd, 12);
                r.right -= dips(hwnd, 8);
                SetTextColor(dc, if current { palette.text } else { palette.muted });
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
            let (tx, rx) = mpsc::channel();
            let events = self.events.clone();
            let hwnd = self.hwnd as usize;
            std::thread::Builder::new()
                .name("pdf-render".into())
                .spawn(move || worker::run(hwnd, rx, events))
                .map_err(|e| e.to_string())?;
            self.worker = Some(tx);
        }
        Ok(())
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
            self.ensure_worker()?;
            let index = self.tabs.len();
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if label(self.ui.tab_bar, TCM_INSERTITEMW, index, &name) < 0 {
                return Err("タブを追加できません。".into());
            }
            self.tabs.push(Tab { path, state: None });
            target = Some(index);
        }
        if let Some(index) = target {
            self.switch_tab(index)?;
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

    pub(super) fn close_tab(&mut self) -> Result<(), String> {
        let Some(index) = self.active_tab else {
            return Ok(());
        };
        if self.writing || !self.discard_ok() {
            return Ok(());
        }
        self.active_tab = None;
        self.tabs.remove(index);
        unsafe {
            SendMessageW(self.ui.tab_bar, TCM_DELETEITEM, index, 0);
        }
        self.activate_tab(self.tabs.len().checked_sub(1).map(|last| index.min(last)))
    }

    fn activate_tab(&mut self, index: Option<usize>) -> Result<(), String> {
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
        self.ui.show_signing(self.signing.is_some());
        self.restore_signing_controls()?;
        let show = if index.is_some() { SW_SHOW } else { SW_HIDE };
        unsafe {
            ShowWindow(self.ui.tab_bar, show);
            ShowWindow(self.ui.tab_close, show);
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
        self.reset_canvas("PDF を開いています…");
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
        set_text(self.ui.status, "PDF を開いています…");
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
