mod canvas;
mod dialog;
mod save;
mod signing;
mod toolbar;
mod worker;

use crate::{
    highlight::{Mark, MarkColor, MarkKind, MarkStyle},
    wincrypto::SigningCertificate,
};
use canvas::VIEW;
use std::{
    ffi::OsString,
    mem::size_of,
    os::windows::ffi::OsStringExt,
    path::PathBuf,
    ptr::{null, null_mut},
    sync::mpsc::{self, Receiver, Sender},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::{Com::*, LibraryLoader::GetModuleHandleW},
    UI::{HiDpi::*, Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
};
use worker::{Event, Job};

const OPEN: usize = 101;
const PREV: usize = 102;
const NEXT: usize = 103;
const SMALLER: usize = 104;
const LARGER: usize = 105;
const FIT: usize = 106;
const SIGN: usize = 107;
const HIGHLIGHT: usize = 108;
const SAVE_MARKS: usize = 109;
const PAGE: usize = 111;
const STATUS: usize = 112;
const PAGE_INPUT: usize = 113;
const UNDO: usize = 114;
const UNDERLINE: usize = 115;
const COLOR: usize = 116;
const FIRST: usize = 121;
const LAST: usize = 122;
const LAYOUT: usize = 123;
const DPI: usize = 124;
const CLOSE: usize = 125;
const END_JUMP: usize = 126;
const SIGN_POSITION: usize = 127;
const WATERMARK: usize = 128;
const SIGN_SAVE: usize = 129;
const SIGN_CANCEL: usize = 130;
const SIGN_CERTIFICATE: usize = 131;
const SIGN_PFX: usize = 132;
const RENDER_TIMER: usize = 1;
const ACTION: u32 = WM_APP + 1;
const READY: u32 = WM_APP + 2;
const DROPPED: u32 = WM_APP + 3;
const WHEEL: u32 = WM_APP + 4;
const ZOOM_STEP: f64 = 1.25;
const MIN_ZOOM: f64 = 0.5;
const MAX_ZOOM: f64 = 8.0;
const WINDOW: &str = "SumiPDF.Window";
const APP_ICON: *const u16 = 201 as *const u16; // Icon resource ID in assets/app.rc.
const CANVAS_ID: usize = 110;
const CANVAS: &str = "SumiPDF.Page";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}
fn rect(hwnd: HWND) -> RECT {
    let mut r = RECT::default();
    unsafe {
        GetClientRect(hwnd, &mut r);
    }
    r
}
fn dips(hwnd: HWND, value: i32) -> i32 {
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96) as i32;
    (value * dpi + 48) / 96
}
fn set_text(hwnd: HWND, s: &str) {
    unsafe {
        SetWindowTextW(hwnd, wide(s).as_ptr());
    }
}
fn notice(hwnd: HWND, s: &str, error: bool) {
    unsafe {
        MessageBoxW(
            hwnd,
            wide(s).as_ptr(),
            wide("SumiPDF").as_ptr(),
            MB_OK
                | if error {
                    MB_ICONERROR
                } else {
                    MB_ICONINFORMATION
                },
        );
    }
}
fn action(hwnd: HWND, id: usize) {
    unsafe {
        PostMessageW(hwnd, ACTION, id, 0);
    }
}

// Synchronous native calls (resizing, dialogs, capture) can re-enter this
// procedure. Changes to the application model run only in the outer loop.
unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = w & 0xffff;
            if id == PAGE_INPUT {
                if w >> 16 == EN_KILLFOCUS as usize {
                    action(hwnd, END_JUMP);
                }
            } else if id == COLOR || id == SIGN_POSITION {
                if matches!((w >> 16) as u32, CBN_SELCHANGE | CBN_CLOSEUP) {
                    action(hwnd, id);
                }
            } else if w >> 16 == 0 {
                action(hwnd, id);
            }
            0
        }
        WM_SIZE => {
            if w != SIZE_MINIMIZED as usize {
                Controls::layout(hwnd);
                action(hwnd, LAYOUT);
            }
            0
        }
        WM_EXITSIZEMOVE => {
            action(hwnd, DPI);
            0
        }
        WM_DPICHANGED => {
            let r = unsafe { &*(l as *const RECT) };
            unsafe {
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            action(hwnd, DPI);
            0
        }
        WM_GETMINMAXINFO => {
            let info = unsafe { &mut *(l as *mut MINMAXINFO) };
            info.ptMinTrackSize = POINT {
                x: dips(hwnd, 440),
                y: dips(hwnd, 290),
            };
            0
        }
        WM_CTLCOLORSTATIC => unsafe {
            SetBkColor(w as HDC, GetSysColor(COLOR_WINDOW));
            GetSysColorBrush(COLOR_WINDOW) as isize
        },
        WM_DROPFILES => {
            unsafe {
                PostMessageW(hwnd, DROPPED, w, 0);
            }
            0
        }
        // A native modal dialog may dispatch this queued message before the
        // outer loop resumes. Release the shell's file-drop handle in that case.
        DROPPED => {
            unsafe {
                DragFinish(w as HDROP);
            }
            0
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            unsafe {
                PostMessageW(hwnd, WHEEL, w, msg as isize);
            }
            0
        }
        WM_CLOSE => {
            action(hwnd, CLOSE);
            0
        }
        WM_DESTROY => {
            unsafe {
                PostQuitMessage(0);
            }
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
}

fn register(name: &str, procedure: WNDPROC, background: HBRUSH) -> bool {
    let name = wide(name);
    let instance = unsafe { GetModuleHandleW(null()) };
    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: procedure,
        hInstance: instance,
        hCursor: unsafe { LoadCursorW(null_mut(), IDC_ARROW) },
        hIcon: unsafe { LoadIconW(instance, APP_ICON) },
        hbrBackground: background,
        lpszClassName: name.as_ptr(),
        ..Default::default()
    };
    unsafe { RegisterClassExW(&wc) != 0 }
}

fn child(parent: HWND, class: &str, caption: &str, id: usize, style: u32) -> Result<HWND, String> {
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            wide(class).as_ptr(),
            wide(caption).as_ptr(),
            WS_VISIBLE | WS_CHILD | style,
            0,
            0,
            1,
            1,
            parent,
            id as HMENU,
            GetModuleHandleW(null()),
            null(),
        )
    };
    if hwnd.is_null() {
        Err(std::io::Error::last_os_error().to_string())
    } else {
        Ok(hwnd)
    }
}

struct Controls {
    canvas: HWND,
    open: HWND,
    prev: HWND,
    page: HWND,
    page_input: HWND,
    next: HWND,
    smaller: HWND,
    larger: HWND,
    fit: HWND,
    highlighter: HWND,
    underline: HWND,
    color: HWND,
    undo: HWND,
    save: HWND,
    sign: HWND,
    position: HWND,
    watermark: HWND,
    certificate: HWND,
    pfx: HWND,
    sign_save: HWND,
    sign_cancel: HWND,
    status: HWND,
    font: HFONT,
    headline: HFONT,
    icon_font: HFONT,
    toolbar: toolbar::Toolbar,
    font_dpi: u32,
}

impl Controls {
    fn color_picker(hwnd: HWND) -> Result<HWND, String> {
        let picker = child(
            hwnd,
            "COMBOBOX",
            "",
            COLOR,
            WS_TABSTOP | CBS_DROPDOWNLIST as u32,
        )?;
        for color in MarkColor::ALL {
            let label = wide(&format!("色：{}", color.label()));
            let index = unsafe { SendMessageW(picker, CB_ADDSTRING, 0, label.as_ptr() as isize) };
            if index < 0 {
                return Err("注釈の色を追加できませんでした。".into());
            }
            if color == MarkColor::default() {
                unsafe {
                    SendMessageW(picker, CB_SETCURSEL, index as usize, 0);
                }
            }
        }
        Ok(picker)
    }

    fn new(hwnd: HWND) -> Result<Self, String> {
        let page_input = child(
            hwnd,
            "EDIT",
            "",
            PAGE_INPUT,
            WS_BORDER | (ES_NUMBER | ES_CENTER) as u32,
        )?;
        unsafe {
            ShowWindow(page_input, SW_HIDE);
            SendMessageW(
                page_input,
                windows_sys::Win32::UI::Controls::EM_SETLIMITTEXT,
                10,
                0,
            );
        }
        let mut toolbar = toolbar::Toolbar::new(hwnd)?;
        let ui = Self {
            open: toolbar.button(OPEN, toolbar::OPEN_FILE)?,
            prev: toolbar.button(PREV, toolbar::PREVIOUS)?,
            page: child(hwnd, "BUTTON", "— / —", PAGE, WS_TABSTOP)?,
            page_input,
            next: toolbar.button(NEXT, toolbar::NEXT_PAGE)?,
            smaller: toolbar.button(SMALLER, toolbar::ZOOM_OUT)?,
            larger: toolbar.button(LARGER, toolbar::ZOOM_IN)?,
            fit: toolbar.button(FIT, toolbar::FIT_PAGE)?,
            highlighter: toolbar.button(HIGHLIGHT, toolbar::MARKER)?,
            underline: toolbar.button(UNDERLINE, toolbar::UNDERLINE_TEXT)?,
            color: Self::color_picker(hwnd)?,
            undo: toolbar.button(UNDO, toolbar::UNDO_MARK)?,
            save: toolbar.button(SAVE_MARKS, toolbar::SAVE_ANNOTATIONS)?,
            sign: toolbar.button(SIGN, toolbar::SIGN_PDF)?,
            position: child(
                hwnd,
                "COMBOBOX",
                "",
                SIGN_POSITION,
                WS_TABSTOP | CBS_DROPDOWNLIST as u32,
            )?,
            watermark: toolbar.button(WATERMARK, toolbar::DEFAULT_WATERMARK)?,
            certificate: toolbar.button(SIGN_CERTIFICATE, toolbar::CERTIFICATE)?,
            pfx: toolbar.button(SIGN_PFX, toolbar::PFX)?,
            sign_save: toolbar.button(SIGN_SAVE, toolbar::CONFIRM)?,
            sign_cancel: toolbar.button(SIGN_CANCEL, toolbar::CANCEL)?,
            canvas: child(hwnd, CANVAS, "", CANVAS_ID, 0)?,
            status: child(
                hwnd,
                "STATIC",
                "PDF を開いて閲覧・署名できます。",
                STATUS,
                0x4200,
            )?, // SS_CENTERIMAGE | SS_PATHELLIPSIS
            font: null_mut(),
            headline: null_mut(),
            icon_font: null_mut(),
            toolbar,
            font_dpi: 0,
        };
        ui.toolbar.page_tip(ui.page)?;
        ui.show_signing(false);
        Ok(ui)
    }

    fn show_signing(&self, show: bool) {
        for control in [
            self.position,
            self.watermark,
            self.certificate,
            self.pfx,
            self.sign_save,
            self.sign_cancel,
        ] {
            unsafe { ShowWindow(control, if show { SW_SHOW } else { SW_HIDE }) };
        }
    }

    fn fonts(&mut self, hwnd: HWND) {
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        if !self.font.is_null() && dpi == self.font_dpi {
            return;
        }
        self.font_dpi = dpi;
        let make = |face: &str, h: i32, weight: i32| unsafe {
            CreateFontW(
                -dips(hwnd, h),
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                0,
                0,
                CLEARTYPE_QUALITY as u32,
                0,
                wide(face).as_ptr(),
            )
        };
        let old = (self.font, self.headline, self.icon_font);
        self.font = make("Yu Gothic UI", 15, 400);
        self.headline = make("Yu Gothic UI", 25, 600);
        self.icon_font = make("Segoe MDL2 Assets", 16, 400);
        self.toolbar.set_font(self.icon_font);
        VIEW.with_borrow_mut(|v| {
            v.font = self.font;
            v.headline = self.headline;
        });
        for c in [
            self.page,
            self.page_input,
            self.color,
            self.position,
            self.status,
        ] {
            unsafe {
                SendMessageW(c, WM_SETFONT, self.font as usize, 1);
            }
        }
        unsafe {
            DeleteObject(old.0 as _);
            DeleteObject(old.1 as _);
            DeleteObject(old.2 as _);
        }
    }

    fn layout(hwnd: HWND) {
        let control = |id| unsafe { GetDlgItem(hwnd, id as i32) };
        if control(STATUS).is_null() {
            return;
        }
        let bounds = rect(hwnd);
        let s = |n| dips(hwnd, n);
        let compact = bounds.right < s(650);
        let toolbar = if compact { 91 } else { 50 };
        let place = |w, x, y, width, height| unsafe {
            SetWindowPos(
                w,
                null_mut(),
                x,
                y,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        };
        let button = 36;
        let page_x = 91;
        let page_width = 80;
        for (id, x, width) in [
            (OPEN, 10, button),
            (PREV, 52, button),
            (PAGE, page_x, page_width),
            (NEXT, 174, button),
            (SMALLER, 221, button),
            (LARGER, 260, button),
            (FIT, 299, button),
        ] {
            place(control(id), s(x), s(9), s(width), s(32));
        }
        place(control(PAGE_INPUT), s(page_x), s(12), s(page_width), s(26));
        let left = if compact {
            s(10)
        } else {
            bounds.right - s(300)
        };
        let y = s(if compact { 50 } else { 9 });
        for (id, x) in [
            (HIGHLIGHT, 0),
            (UNDERLINE, 39),
            (UNDO, 168),
            (SAVE_MARKS, 207),
        ] {
            place(control(id), left + s(x), y, s(button), s(32));
        }
        place(control(COLOR), left + s(78), y + s(4), s(82), s(140));
        place(control(SIGN), bounds.right - s(46), s(9), s(button), s(32));
        let mut top = toolbar;
        if unsafe { GetWindowLongW(control(SIGN_POSITION), GWL_STYLE) } as u32 & WS_VISIBLE != 0 {
            let width = (bounds.right - s(227)).min(s(390));
            place(control(SIGN_POSITION), s(10), s(toolbar + 2), width, s(280));
            for (id, x) in [
                (WATERMARK, 17),
                (SIGN_CERTIFICATE, 56),
                (SIGN_PFX, 95),
                (SIGN_SAVE, 142),
                (SIGN_CANCEL, 181),
            ] {
                place(control(id), width + s(x), s(toolbar), s(button), s(32));
            }
            top += 41;
        }
        place(
            control(CANVAS_ID),
            0,
            s(top),
            bounds.right,
            (bounds.bottom - s(top + 30)).max(1),
        );
        place(
            control(STATUS),
            s(12),
            bounds.bottom - s(29),
            (bounds.right - s(24)).max(1),
            s(28),
        );
        canvas::redraw(control(CANVAS_ID));
    }
}
impl Drop for Controls {
    fn drop(&mut self) {
        unsafe {
            DeleteObject(self.font as _);
            DeleteObject(self.headline as _);
            DeleteObject(self.icon_font as _);
        }
    }
}

struct App {
    hwnd: HWND,
    ui: Controls,
    worker: Option<Sender<Job>>,
    events: Sender<Event>,
    receiver: Receiver<Event>,
    path: PathBuf,
    document: Option<worker::OpenedDocument>,
    revision: u64,
    ticket: u64,
    page: u32,
    zoom: f64,
    writing: bool,
    wheel_delta: i32,
    requested: Option<(u64, u32, (i32, i32), u64)>,
    marks: Vec<Mark>,
    saved_marks: Vec<Mark>,
    tool: Option<MarkKind>,
    color: MarkColor,
    text_ready: bool,
    has_text: bool,
    text_worker: Option<Sender<worker::TextJob>>,
    signing: Option<signing::Session>,
    checking_fields: Option<u64>,
    known_signed: bool,
}

impl App {
    fn new(hwnd: HWND) -> Result<Self, String> {
        let (events, receiver) = mpsc::channel();
        let mut ui = Controls::new(hwnd)?;
        ui.fonts(hwnd);
        Controls::layout(hwnd);
        let app = Self {
            hwnd,
            ui,
            worker: None,
            events,
            receiver,
            path: PathBuf::new(),
            document: None,
            revision: 0,
            ticket: 0,
            page: 0,
            zoom: 1.0,
            writing: false,
            wheel_delta: 0,
            requested: None,
            marks: Vec::new(),
            saved_marks: Vec::new(),
            tool: None,
            color: MarkColor::default(),
            text_ready: false,
            has_text: false,
            text_worker: None,
            signing: None,
            checking_fields: None,
            known_signed: false,
        };
        app.controls();
        Ok(app)
    }
    fn page_count(&self) -> u32 {
        self.document.as_ref().map_or(0, |d| d.pages)
    }
    fn controls(&self) {
        let editing = self.signing.is_none() && !self.writing;
        let annotate = editing && !self.known_signed;
        for (c, enabled) in [
            (self.ui.open, !self.writing),
            (self.ui.page, self.page_count() > 0),
            (self.ui.prev, self.page > 0),
            (self.ui.next, self.page + 1 < self.page_count()),
            (self.ui.fit, self.page_count() > 0),
            (
                self.ui.smaller,
                self.page_count() > 0 && self.zoom > MIN_ZOOM,
            ),
            (
                self.ui.larger,
                self.page_count() > 0 && self.zoom < MAX_ZOOM,
            ),
            (self.ui.sign, self.page_count() > 0 && editing),
            (
                self.ui.position,
                self.signing.as_ref().is_some_and(|s| s.has_positions()) && !self.writing,
            ),
            (
                self.ui.watermark,
                self.signing
                    .as_ref()
                    .is_some_and(|s| s.can_pick_watermark())
                    && !self.writing,
            ),
            (
                self.ui.certificate,
                self.signing.as_ref().is_some_and(|s| s.has_positions()) && !self.writing,
            ),
            (
                self.ui.pfx,
                self.signing.as_ref().is_some_and(|s| s.has_positions()) && !self.writing,
            ),
            (self.ui.sign_save, self.can_sign_here() && !self.writing),
            (self.ui.sign_cancel, !self.writing),
            (self.ui.highlighter, self.page_count() > 0 && annotate),
            (self.ui.underline, self.page_count() > 0 && annotate),
            (self.ui.color, self.page_count() > 0 && annotate),
            (self.ui.undo, !self.marks.is_empty() && editing),
            (
                self.ui.save,
                (!self.marks.is_empty() || !self.saved_marks.is_empty()) && annotate,
            ),
        ] {
            unsafe {
                EnableWindow(c, i32::from(enabled));
            }
        }
        let text = if self.page_count() == 0 {
            "— / —".into()
        } else {
            format!("{} / {}", self.page + 1, self.page_count())
        };
        set_text(self.ui.page, &text);
        for (button, kind) in [
            (self.ui.highlighter, MarkKind::Highlight),
            (self.ui.underline, MarkKind::Underline),
        ] {
            unsafe {
                SendMessageW(button, BM_SETCHECK, usize::from(self.tool == Some(kind)), 0);
            }
        }
    }
    fn title(&self) {
        let name = self.path.file_name().unwrap_or_default().to_string_lossy();
        let star = if self.marks == self.saved_marks {
            ""
        } else {
            " *"
        };
        set_text(self.hwnd, &format!("{name}{star}  —  SumiPDF"));
    }
    fn discard_ok(&self) -> bool {
        self.marks == self.saved_marks
            || unsafe {
                MessageBoxW(
                    self.hwnd,
                    wide("注釈の変更がまだ保存されていません。変更を破棄して続けますか？").as_ptr(),
                    wide("SumiPDF").as_ptr(),
                    MB_YESNO | MB_DEFBUTTON2 | MB_ICONQUESTION,
                ) == IDYES
            }
    }
    fn status(&self) {
        if self.writing {
            return;
        }
        let text = if let Some(signing) = &self.signing {
            signing.instructions(self.page)
        } else if let Some(kind) = self.tool {
            let name = match kind {
                MarkKind::Highlight => "蛍光ペン",
                MarkKind::Underline => "下線",
            };
            let help = if !self.text_ready {
                "文字の位置を読み込み中…"
            } else if self.has_text {
                "文字をドラッグして選択"
            } else if kind == MarkKind::Underline {
                "線を引く位置で横にドラッグ"
            } else {
                "ハイライトする範囲をドラッグ"
            };
            format!("{name}（{}）  ·  {help}  ·  Esc で終了", self.color.label())
        } else if self.zoom == 1.0 {
            "ページ全体  ·  ホイールでページ移動".into()
        } else {
            let help = if self.zoom < 1.0 {
                "ホイールでページ移動"
            } else {
                "ドラッグで移動"
            };
            format!("全体表示の {:.0}%  ·  {help}", self.zoom * 100.0)
        };
        set_text(self.ui.status, &text);
    }
    fn update_marks(&self) {
        VIEW.with_borrow_mut(|v| v.set_marks(&self.marks, self.page));
        canvas::redraw(self.ui.canvas);
        self.title();
        self.controls();
        self.status();
    }
    fn release_text(&self) {
        if let Some(worker) = &self.text_worker {
            let _ = worker.send(worker::TextJob::Release);
        }
    }
    fn sync_canvas_tool(&self) {
        let tool = self
            .tool
            .filter(|_| self.text_ready && !self.writing)
            .map(|kind| MarkStyle {
                kind,
                color: self.color,
            });
        VIEW.with_borrow_mut(|v| v.set_tool(tool));
    }
    fn request_text(&mut self) -> Result<(), String> {
        self.text_ready = false;
        self.has_text = false;
        VIEW.with_borrow_mut(|v| {
            v.set_tool(None);
            v.set_text_layout(None);
        });
        self.status();
        if self.tool.is_none() {
            self.release_text();
            return Ok(());
        }
        let Some(doc) = &self.document else {
            return Ok(());
        };
        let source = doc.source.as_ref().map_err(Clone::clone)?.clone();
        if self.text_worker.is_none() {
            let (tx, rx) = mpsc::channel();
            let events = self.events.clone();
            let hwnd = self.hwnd as usize;
            std::thread::Builder::new()
                .name("pdf-text".into())
                .spawn(move || worker::text_pages(hwnd, rx, events))
                .map_err(|e| e.to_string())?;
            self.text_worker = Some(tx);
        }
        self.text_worker
            .as_ref()
            .unwrap()
            .send(worker::TextJob::Page {
                revision: self.revision,
                page: self.page,
                source,
            })
            .map_err(|_| "文字位置を取得するスレッドが終了しました。".to_owned())
    }
    fn text_failed(&mut self, error: &str) {
        self.tool = None;
        self.release_text();
        VIEW.with_borrow_mut(|v| {
            v.set_tool(None);
            v.set_text_layout(None);
        });
        self.controls();
        self.status();
        notice(self.hwnd, error, true);
    }
    fn jump_input(&self, show: bool) {
        unsafe {
            if show && self.page_count() > 0 {
                set_text(self.ui.page_input, &(self.page + 1).to_string());
                ShowWindow(self.ui.page, SW_HIDE);
                ShowWindow(self.ui.page_input, SW_SHOW);
                SetFocus(self.ui.page_input);
                SendMessageW(
                    self.ui.page_input,
                    windows_sys::Win32::UI::Controls::EM_SETSEL,
                    0,
                    -1,
                );
            } else if IsWindowVisible(self.ui.page_input) != 0 {
                ShowWindow(self.ui.page_input, SW_HIDE);
                ShowWindow(self.ui.page, SW_SHOW);
            }
        }
    }
    fn jump_to_input(&mut self) {
        let mut text = [0u16; 12];
        let n = unsafe { GetWindowTextW(self.ui.page_input, text.as_mut_ptr(), text.len() as i32) };
        match String::from_utf16_lossy(&text[..n as usize]).parse::<u32>() {
            Ok(page) if (1..=self.page_count()).contains(&page) => {
                self.jump_input(false);
                self.page_to(page - 1);
                unsafe {
                    SetFocus(self.hwnd);
                }
            }
            _ => {
                set_text(
                    self.ui.status,
                    &format!(
                        "1 ～ {} のページ番号を入力してください。",
                        self.page_count()
                    ),
                );
                unsafe {
                    SendMessageW(
                        self.ui.page_input,
                        windows_sys::Win32::UI::Controls::EM_SETSEL,
                        0,
                        -1,
                    );
                }
            }
        }
    }
    fn reset_canvas(&self, text: &str) {
        VIEW.with_borrow_mut(|v| {
            v.reset(false, text);
            v.zoom = self.zoom;
            v.set_marks(&self.marks, self.page);
        });
        canvas::redraw(self.ui.canvas);
    }
    fn open(&mut self, path: PathBuf) {
        if self.writing || !self.discard_ok() {
            return;
        }
        self.jump_input(false);
        let path = match std::path::absolute(path) {
            Ok(p) => p,
            Err(e) => {
                notice(self.hwnd, &e.to_string(), true);
                return;
            }
        };
        if self.worker.is_none() {
            let (tx, rx) = mpsc::channel();
            let events = self.events.clone();
            let hwnd = self.hwnd as usize;
            if let Err(e) = std::thread::Builder::new()
                .name("pdf-render".into())
                .spawn(move || worker::run(hwnd, rx, events))
            {
                notice(self.hwnd, &e.to_string(), true);
                return;
            }
            self.worker = Some(tx);
        }
        self.revision += 1;
        self.known_signed = false;
        self.ticket += 1;
        self.document = None;
        self.end_signing();
        self.marks.clear();
        self.saved_marks.clear();
        self.tool = None;
        self.text_ready = false;
        self.has_text = false;
        self.release_text();
        VIEW.with_borrow_mut(|v| v.set_tool(None));
        self.requested = None;
        self.path = path;
        self.page = 0;
        self.zoom = 1.0;
        self.wheel_delta = 0;
        self.title();
        self.reset_canvas("PDF を開いています…");
        set_text(self.ui.status, "PDF を開いています…");
        self.controls();
        self.send(Job::Open {
            revision: self.revision,
            path: self.path.clone(),
        });
    }
    fn send(&self, job: Job) {
        if let Some(w) = &self.worker
            && w.send(job).is_err()
        {
            notice(
                self.hwnd,
                "描画スレッドが終了しました。アプリを再起動してください。",
                true,
            );
        }
    }
    fn render(&mut self) {
        if self.page_count() == 0 {
            return;
        }
        let area = canvas::area(self.ui.canvas);
        let key = (self.revision, self.page, area, self.zoom.to_bits());
        if self.requested == Some(key) {
            return;
        }
        self.requested = Some(key);
        self.ticket += 1;
        self.send(Job::Render {
            revision: self.revision,
            ticket: self.ticket,
            page: self.page,
            area,
            zoom: self.zoom,
        });
    }
    fn page_to(&mut self, page: u32) {
        if self.page_count() == 0 || self.page == page || page >= self.page_count() {
            return;
        }
        self.page = page;
        self.reset_canvas("ページを表示中…");
        self.sync_signature();
        if self.tool.is_some()
            && let Err(e) = self.request_text()
        {
            self.text_failed(&e);
        }
        if !self.writing {
            set_text(self.ui.status, "ページを表示中…");
        }
        self.controls();
        self.render();
    }
    fn scale(&mut self, zoom: f64) {
        if self.page_count() == 0 {
            return;
        }
        let zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        if self.zoom == zoom {
            return;
        }
        self.zoom = zoom;
        VIEW.with_borrow_mut(|v| {
            v.zoom = zoom;
            if zoom <= 1.0 {
                v.center();
            }
        });
        canvas::redraw(self.ui.canvas);
        self.controls();
        self.render();
    }
    fn save_copy(&mut self, signed: bool) -> Result<(), String> {
        if self.writing
            || (signed && !self.can_sign_here())
            || (!signed
                && (self.known_signed
                    || self.signing.is_some()
                    || (self.marks.is_empty() && self.saved_marks.is_empty())))
        {
            return Ok(());
        }
        let Some(doc) = &self.document else {
            return Ok(());
        };
        let source = doc.source.as_ref().map_err(Clone::clone)?.clone();
        let (cert, purpose, appearance) = if signed {
            let session = self.signing.as_ref().ok_or("署名位置を選んでください。")?;
            let appearance = session.appearance()?;
            let certificate = match &session.certificate {
                Some(choice) => choice.resolve(self.hwnd)?,
                None => SigningCertificate::choose(self.hwnd)?,
            };
            let Some(cert) = certificate else {
                return Ok(());
            };
            (
                Some(cert),
                dialog::Purpose::SignedCopy(&self.path),
                appearance,
            )
        } else {
            (None, dialog::Purpose::HighlightedCopy(&self.path), None)
        };
        let Some(path) = dialog::choose(self.hwnd, purpose)? else {
            return Ok(());
        };
        let marks = self.marks.clone();
        let events = self.events.clone();
        let hwnd = self.hwnd as usize;
        if self.text_ready {
            self.release_text();
        }
        std::thread::Builder::new()
            .name("pdf-save".into())
            .spawn(move || {
                let signature = cert.as_ref().map(|cert| (cert, appearance.as_ref()));
                let result = save::copy(&source, &marks, signature, &path).map(|_| path);
                worker::report(hwnd, &events, Event::Saved { signed, result });
            })
            .map_err(|e| e.to_string())?;
        self.writing = true;
        self.sync_canvas_tool();
        self.sync_signature();
        set_text(
            self.ui.status,
            if signed {
                "署名しています…"
            } else {
                "注釈を保存しています…"
            },
        );
        self.controls();
        Ok(())
    }
    fn event(&mut self, event: Event) {
        match event {
            Event::Opened(id, result) if id == self.revision => match result {
                Ok(document) => {
                    self.document = Some(document);
                    self.controls();
                    self.render();
                }
                Err(e) => {
                    self.reset_canvas("この PDF を開けませんでした。");
                    set_text(self.ui.status, "読み込みエラー");
                    notice(self.hwnd, &e, true);
                }
            },
            Event::Painted(ticket, result) if ticket == self.ticket => match result {
                Ok(bitmap) => {
                    VIEW.with_borrow_mut(|v| {
                        v.bitmap = Some(bitmap);
                    });
                    canvas::redraw(self.ui.canvas);
                    self.status();
                    if self.signing.is_some() {
                        self.controls();
                    }
                }
                Err(e) => {
                    VIEW.with_borrow_mut(|v| v.text = "このページを描画できません。".into());
                    canvas::redraw(self.ui.canvas);
                    set_text(self.ui.status, &e);
                    self.requested = None;
                }
            },
            Event::Layout {
                revision,
                page,
                result,
            } if revision == self.revision && page == self.page && self.tool.is_some() => {
                match result {
                    Ok(layout) => {
                        self.text_ready = true;
                        self.has_text = !layout.is_empty();
                        VIEW.with_borrow_mut(|v| v.set_text_layout(Some(layout)));
                        self.sync_canvas_tool();
                        self.status();
                    }
                    Err(e) => self.text_failed(&e),
                }
            }
            Event::SignatureFields { revision, result }
                if self.checking_fields == Some(revision) =>
            {
                self.checking_fields = None;
                if revision == self.revision
                    && self.signing.is_some()
                    && let Err(e) = result.and_then(|fields| self.show_fields(fields))
                {
                    self.end_signing();
                    notice(self.hwnd, &e, true);
                }
            }
            Event::Saved { signed, result } => {
                self.writing = false;
                self.sync_canvas_tool();
                self.sync_signature();
                self.controls();
                match result {
                    Ok(path) => {
                        if signed {
                            self.end_signing();
                        }
                        self.saved_marks.clone_from(&self.marks);
                        self.title();
                        self.controls();
                        let what = if signed {
                            "署名済みPDF"
                        } else {
                            "注釈付きPDF"
                        };
                        let s = format!("{what}を保存しました。\n{}", path.display());
                        set_text(
                            self.ui.status,
                            &format!("保存しました  ·  {}", path.display()),
                        );
                        notice(self.hwnd, &s, false);
                    }
                    Err(e) => {
                        set_text(self.ui.status, "PDFを保存できませんでした。");
                        notice(self.hwnd, &e, true);
                    }
                }
            }
            _ => {}
        }
    }
    fn command(&mut self, id: usize) -> Result<(), String> {
        match id {
            OPEN if !self.writing => {
                if let Some(p) = dialog::choose(self.hwnd, dialog::Purpose::Open)? {
                    self.open(p);
                }
            }
            PREV => self.page_to(self.page.saturating_sub(1)),
            NEXT => self.page_to(self.page + 1),
            FIRST => self.page_to(0),
            LAST => self.page_to(self.page_count().saturating_sub(1)),
            SMALLER => self.scale(self.zoom / ZOOM_STEP),
            LARGER => self.scale(self.zoom * ZOOM_STEP),
            FIT => self.scale(1.0),
            SIGN => self.begin_signing()?,
            SIGN_POSITION if !self.writing => self.choose_position(),
            SIGN_CERTIFICATE if !self.writing => self.choose_certificate(false)?,
            SIGN_PFX if !self.writing => self.choose_certificate(true)?,
            WATERMARK if !self.writing => self.choose_watermark()?,
            SIGN_SAVE => self.save_copy(true)?,
            SIGN_CANCEL if !self.writing => self.end_signing(),
            SAVE_MARKS => self.save_copy(false)?,
            HIGHLIGHT | UNDERLINE
                if !self.writing
                    && !self.known_signed
                    && self.signing.is_none()
                    && self.page_count() > 0 =>
            {
                let kind = if id == HIGHLIGHT {
                    MarkKind::Highlight
                } else {
                    MarkKind::Underline
                };
                let previous = self.tool;
                self.tool = (previous != Some(kind)).then_some(kind);
                if previous.is_some() && self.tool.is_some() {
                    self.sync_canvas_tool();
                    self.status();
                } else if let Err(e) = self.request_text() {
                    self.text_failed(&e);
                }
                self.controls();
                canvas::redraw(self.ui.canvas);
            }
            COLOR if !self.writing && self.signing.is_none() && self.page_count() > 0 => {
                let index = unsafe { SendMessageW(self.ui.color, CB_GETCURSEL, 0, 0) };
                if let Some(&color) = MarkColor::ALL.get(index as usize) {
                    self.color = color;
                    self.sync_canvas_tool();
                    self.status();
                }
            }
            UNDO if !self.writing && self.signing.is_none() => {
                if let Some(mark) = self.marks.pop() {
                    self.page_to(mark.page);
                    self.update_marks();
                }
            }
            PAGE => self.jump_input(true),
            END_JUMP => self.jump_input(false),
            LAYOUT | DPI => {
                if id == DPI {
                    self.ui.fonts(self.hwnd);
                    Controls::layout(self.hwnd);
                }
                if self.page_count() > 0 {
                    unsafe {
                        SetTimer(self.hwnd, RENDER_TIMER, 85, None);
                    }
                }
            }
            CLOSE if self.writing => {
                notice(self.hwnd, "保存処理が終わるまでお待ちください。", false)
            }
            CLOSE if self.discard_ok() => unsafe {
                self.ui.toolbar.clear_names();
                DestroyWindow(self.hwnd);
            },
            _ => {}
        }
        Ok(())
    }
    fn wheel(&mut self, w: WPARAM, horizontal: bool) {
        if self.page_count() == 0 {
            return;
        }
        let delta = ((w >> 16) as i16) as i32;
        let ctrl = w & 8 != 0;
        if !ctrl && self.zoom > 1.0 {
            let distance = delta * dips(self.hwnd, 60) / 120;
            let (dx, dy) = if horizontal {
                (-distance, 0)
            } else if w & 4 != 0 {
                (distance, 0)
            } else {
                (0, distance)
            };
            canvas::pan(self.ui.canvas, dx, dy);
        } else if !horizontal {
            self.wheel_delta += delta;
            let ticks = self.wheel_delta / 120;
            self.wheel_delta %= 120;
            if ticks == 0 {
                return;
            }
            if ctrl {
                self.scale(self.zoom * ZOOM_STEP.powi(ticks.clamp(-16, 16)));
            } else {
                self.page_to(
                    (i64::from(self.page) - i64::from(ticks))
                        .clamp(0, i64::from(self.page_count() - 1)) as u32,
                );
            }
        }
    }
    fn shortcut(&self, key: usize) -> Option<usize> {
        let ctrl = unsafe { GetKeyState(VK_CONTROL as i32) } < 0;
        let alt = unsafe { GetKeyState(VK_MENU as i32) } < 0;
        if alt {
            return None;
        }
        match (ctrl, key as u16) {
            (true, 0x4F) => Some(OPEN),
            (true, 0x47) => Some(PAGE),
            (true, 0x53) if unsafe { GetKeyState(VK_SHIFT as i32) } < 0 => Some(SAVE_MARKS),
            (true, 0x53) => Some(if self.signing.is_some() {
                SIGN_SAVE
            } else {
                SIGN
            }),
            (true, 0x5A) => Some(UNDO),
            (false, 0x48) => Some(HIGHLIGHT),
            (false, 0x55) => Some(UNDERLINE),
            (false, VK_ESCAPE) if self.signing.is_some() => Some(SIGN_CANCEL),
            (false, VK_ESCAPE) => self.tool.map(|kind| match kind {
                MarkKind::Highlight => HIGHLIGHT,
                MarkKind::Underline => UNDERLINE,
            }),
            (true, VK_ADD | VK_OEM_PLUS) => Some(LARGER),
            (true, VK_SUBTRACT | VK_OEM_MINUS) => Some(SMALLER),
            (true, 0x30 | VK_NUMPAD0) => Some(FIT),
            (false, VK_PRIOR | VK_LEFT) => Some(PREV),
            (false, VK_NEXT | VK_RIGHT) => Some(NEXT),
            (false, VK_HOME) => Some(FIRST),
            (false, VK_END) => Some(LAST),
            _ => None,
        }
    }
    fn process(&mut self) {
        let mut msg = MSG::default();
        loop {
            while let Some(mark) = VIEW.with_borrow_mut(|v| v.take_new_mark()) {
                self.marks.push(mark);
                self.update_marks();
            }
            if let Some((page, area)) = VIEW.with_borrow_mut(|v| v.take_signature())
                && let Some(session) = &mut self.signing
                && session.drawing()
            {
                session.drawn = Some((page, area));
                self.sync_signature();
                self.controls();
                self.status();
            }
            // Native file and certificate dialogs have nested message loops.
            // They may consume a wakeup, but never the actual worker event.
            while let Ok(event) = self.receiver.try_recv() {
                self.event(event);
            }
            if unsafe { GetMessageW(&mut msg, null_mut(), 0, 0) } <= 0 {
                break;
            }
            if msg.message == WM_KEYDOWN {
                // Hand navigation and dismissal to the native color list.
                let picker_key = (msg.hwnd == self.ui.color || msg.hwnd == self.ui.position)
                    && (unsafe { SendMessageW(msg.hwnd, CB_GETDROPPEDSTATE, 0, 0) != 0 }
                        || matches!(
                            msg.wParam as u16,
                            VK_LEFT | VK_RIGHT | VK_HOME | VK_END | VK_PRIOR | VK_NEXT
                        ));
                if msg.hwnd == self.ui.page_input {
                    if msg.wParam == VK_RETURN as usize {
                        self.jump_to_input();
                        continue;
                    }
                    if msg.wParam == VK_ESCAPE as usize {
                        self.jump_input(false);
                        unsafe {
                            SetFocus(self.hwnd);
                        }
                        continue;
                    }
                } else if !picker_key && let Some(id) = self.shortcut(msg.wParam) {
                    action(self.hwnd, id);
                    continue;
                }
                if msg.wParam == VK_TAB as usize
                    && unsafe { IsDialogMessageW(self.hwnd, &msg) } != 0
                {
                    continue;
                }
            }
            match msg.message {
                ACTION if msg.hwnd == self.hwnd => {
                    if let Err(e) = self.command(msg.wParam) {
                        notice(self.hwnd, &e, true);
                    }
                }
                READY if msg.hwnd == self.hwnd => {}
                WHEEL if msg.hwnd == self.hwnd => {
                    self.wheel(msg.wParam, msg.lParam == WM_MOUSEHWHEEL as isize)
                }
                DROPPED if msg.hwnd == self.hwnd => {
                    let drop = msg.wParam as HDROP;
                    let len = unsafe { DragQueryFileW(drop, 0, null_mut(), 0) };
                    let mut name = vec![0; len as usize + 1];
                    let n = unsafe { DragQueryFileW(drop, 0, name.as_mut_ptr(), len + 1) };
                    unsafe {
                        DragFinish(drop);
                    }
                    if n > 0 {
                        self.open(PathBuf::from(OsString::from_wide(&name[..n as usize])));
                    }
                }
                WM_TIMER if msg.hwnd == self.hwnd && msg.wParam == RENDER_TIMER => {
                    unsafe {
                        KillTimer(self.hwnd, RENDER_TIMER);
                    }
                    self.render();
                }
                _ => unsafe {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                },
            }
        }
    }
}

pub fn run() {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let com = unsafe { CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32) };
    let result = (|| {
        if !register(WINDOW, Some(window_proc), unsafe {
            GetSysColorBrush(COLOR_WINDOW)
        }) || !register(CANVAS, Some(canvas::procedure), null_mut())
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_CONTROLPARENT,
                wide(WINDOW).as_ptr(),
                wide("SumiPDF").as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                1000,
                780,
                null_mut(),
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            )
        };
        if hwnd.is_null() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let mut app = App::new(hwnd)?;
        unsafe {
            DragAcceptFiles(hwnd, 1);
            ShowWindow(hwnd, SW_SHOW);
            UpdateWindow(hwnd);
        }
        if let Some(path) = std::env::args_os().nth(1) {
            app.open(path.into());
        }
        app.process();
        Ok(())
    })();
    if let Err(e) = result {
        notice(null_mut(), &e, true);
    }
    if com >= 0 {
        unsafe {
            CoUninitialize();
        }
    }
}
