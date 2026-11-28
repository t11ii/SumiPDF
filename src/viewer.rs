mod canvas;
mod dialog;
mod find;
mod save;
mod shortcuts;
mod signing;
mod tabs;
mod theme;
mod toolbar;
mod worker;

use crate::highlight::{Mark, MarkColor, MarkKind, MarkStyle};
use crate::preferences;
use crate::textselect::TextLayout;
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
    UI::{
        Controls::{DRAWITEMSTRUCT, MEASUREITEMSTRUCT},
        HiDpi::*,
        Input::KeyboardAndMouse::*,
        Shell::*,
        WindowsAndMessaging::*,
    },
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
const TABS: usize = 133;
const PREVIOUS_TAB: usize = 134;
const NEXT_TAB: usize = 135;
const CLOSE_TAB: usize = 136;
const SELECT_TAB: usize = 137;
const SCROLLED: usize = 138;
const LOAD_SELECTION_TEXT: usize = 139;
const SELECTION_CHANGED: usize = 140;
const CLEAR_SELECTION: usize = 141;
const SHOW_SHORTCUTS: usize = 144;
const RESTORE_TABS: usize = 0x1fd0;
const FORGET_CERTIFICATE: usize = 0x1fe0;
const RENDER_TIMER: usize = 1;
const SCROLL_TIMER: usize = 2;
const SMOOTH_TIMER: usize = 3;
const ACTION: u32 = WM_APP + 1;
const READY: u32 = WM_APP + 2;
const DROPPED: u32 = WM_APP + 3;
const WHEEL: u32 = WM_APP + 4;
const ZOOM_STEP: f64 = 1.25;
const MIN_ZOOM: f64 = 0.5;
const MAX_ZOOM: f64 = 8.0;
const WINDOW: &str = "SumiPDF.Window";
const APP_TITLE: &str = concat!("SumiPDF v", env!("CARGO_PKG_VERSION"));
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
        WM_SYSCOMMAND if shortcuts::is_menu_command(w) => {
            action(hwnd, SHOW_SHORTCUTS);
            0
        }
        WM_SYSCOMMAND if matches!(w & 0xfff0, FORGET_CERTIFICATE | RESTORE_TABS) => {
            action(hwnd, w & 0xfff0);
            0
        }
        WM_INITMENUPOPUP if (l as usize >> 16) & 0xffff != 0 => {
            shortcuts::check_restore_tabs(hwnd, preferences::restore_tabs_enabled());
            unsafe { DefWindowProcW(hwnd, msg, w, l) }
        }
        WM_MEASUREITEM if w == COLOR && l != 0 => {
            let item = unsafe { &mut *(l as *mut MEASUREITEMSTRUCT) };
            item.itemHeight = dips(hwnd, theme::COLOR_ROW_HEIGHT) as u32;
            1
        }
        WM_DRAWITEM if w == COLOR && l != 0 => {
            let item = unsafe { &*(l as *const DRAWITEMSTRUCT) };
            theme::draw_color_item(item);
            1
        }
        WM_NOTIFY if l != 0 => {
            let header = unsafe { &*(l as *const windows_sys::Win32::UI::Controls::NMHDR) };
            if header.idFrom == TABS
                && header.code == windows_sys::Win32::UI::Controls::TCN_SELCHANGE
            {
                tabs::post_selected(header.hwndFrom, SELECT_TAB);
            }
            unsafe { DefWindowProcW(hwnd, msg, w, l) }
        }
        WM_COMMAND => {
            let id = w & 0xffff;
            if id == find::EDIT {
                if w >> 16 == EN_CHANGE as usize {
                    action(hwnd, id);
                }
            } else if id == PAGE_INPUT {
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
        WM_ERASEBKGND => {
            theme::paint_chrome(hwnd, w as HDC);
            1
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX
            if !theme::palette().high_contrast =>
        {
            let field =
                msg != WM_CTLCOLORSTATIC || unsafe { GetDlgCtrlID(l as HWND) } == PAGE_INPUT as i32;
            theme::control_color(w as HDC, field, unsafe { IsWindowEnabled(l as HWND) != 0 })
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED | WM_SYSCOLORCHANGE => {
            theme::refresh(hwnd);
            unsafe { DefWindowProcW(hwnd, msg, w, l) }
        }
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
    create_child(parent, class, caption, id, style | WS_VISIBLE)
}

fn create_child(
    parent: HWND,
    class: &str,
    caption: &str,
    id: usize,
    style: u32,
) -> Result<HWND, String> {
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            wide(class).as_ptr(),
            wide(caption).as_ptr(),
            WS_CHILD | WS_CLIPSIBLINGS | style,
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
    find_button: HWND,
    find: Option<find::Bar>,
    tab_bar: HWND,
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
    signing: Option<signing::Bar>,
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
            WS_TABSTOP | (CBS_DROPDOWNLIST | CBS_OWNERDRAWFIXED | CBS_HASSTRINGS) as u32,
        )?;
        for color in MarkColor::ALL {
            let label = wide(color.label());
            let index = unsafe { SendMessageW(picker, CB_ADDSTRING, 0, label.as_ptr() as isize) };
            if index < 0 {
                return Err("Could not add a markup color.".into());
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
        let mut ui = Self {
            find_button: toolbar.button(find::OPEN_FIND, toolbar::FIND)?,
            find: None,
            tab_bar: tabs::create(hwnd)?,
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
            signing: None,
            canvas: child(hwnd, CANVAS, "", CANVAS_ID, 0)?,
            status: child(
                hwnd,
                "STATIC",
                "Open a PDF to read or sign it.",
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
        ui.toolbar.color_tip(ui.color)?;
        for control in [ui.page, ui.page_input, ui.color] {
            theme::style_control(control)?;
        }
        unsafe {
            ShowWindow(ui.tab_bar, SW_HIDE);
        }
        Ok(ui)
    }

    fn ensure_find(&mut self, hwnd: HWND) -> Result<&find::Bar, String> {
        if self.find.is_none() {
            let bar = find::Bar::new(hwnd, self.find_button, &mut self.toolbar)?;
            bar.font(self.font);
            self.find = Some(bar);
        }
        Ok(self.find.as_ref().unwrap())
    }

    fn ensure_signing(&mut self, hwnd: HWND) -> Result<signing::Bar, String> {
        if let Some(bar) = self.signing {
            return Ok(bar);
        }
        let bar = signing::Bar::new(hwnd, self.sign, &mut self.toolbar)?;
        bar.font(self.font);
        self.signing = Some(bar);
        Ok(bar)
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
        self.font = make("Yu Gothic UI", 14, 400);
        self.headline = make("Yu Gothic UI", 22, 600);
        self.icon_font = make("Segoe MDL2 Assets", toolbar::ICON_SIZE, 400);
        self.toolbar.set_font(self.icon_font);
        if let Some(bar) = &self.find {
            bar.font(self.font);
        }
        if let Some(bar) = self.signing {
            bar.font(self.font);
        }
        VIEW.with_borrow_mut(|v| {
            v.font = self.font;
            v.headline = self.headline;
        });
        for c in [
            self.page,
            self.page_input,
            self.tab_bar,
            self.color,
            self.status,
        ] {
            unsafe {
                SendMessageW(c, WM_SETFONT, self.font as usize, 1);
            }
        }
        tabs::item_size(self.tab_bar);
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
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96) as i32;
        let s = |n: i32| (n * dpi + 48) / 96;
        let button = toolbar::BUTTON_SIZE;
        let step = toolbar::BUTTON_STEP;
        let row_height = toolbar::ROW_HEIGHT;
        let button_inset = (row_height - button) / 2;
        let gap = step - button;
        let margin = 8;
        let group_gap = 12;

        let prev_x = margin + button + margin;
        let page_x = prev_x + step;
        let page_width = 80;
        let next_x = page_x + page_width + gap;
        let zoom_x = next_x + button + margin;
        let find_x = zoom_x + 3 * step;

        let color_x = 2 * step;
        let color_width = 64;
        let undo_x = color_x + color_width + gap;
        let save_x = undo_x + step;
        let right_group_width = save_x + button + group_gap + button + margin;
        let wrap = bounds.right < s(find_x + button + group_gap + right_group_width);
        let has_tabs = unsafe { GetWindowLongW(control(TABS), GWL_STYLE) } as u32 & WS_VISIBLE != 0;
        let tab_height = if has_tabs { 33 } else { 0 };
        let toolbar_bottom = tab_height + if wrap { 2 * row_height } else { row_height };
        let mut positions = Vec::with_capacity(23);
        let mut place = |w, x, y, width, height| positions.push((w, x, y, width, height));
        if has_tabs {
            place(
                control(TABS),
                s(6),
                s(1),
                (bounds.right - s(12)).max(1),
                s(32),
            );
        }
        let row_y = s(tab_height + button_inset);
        for (id, x, width) in [
            (OPEN, margin, button),
            (PREV, prev_x, button),
            (PAGE, page_x, page_width),
            (NEXT, next_x, button),
            (SMALLER, zoom_x, button),
            (LARGER, zoom_x + step, button),
            (FIT, zoom_x + 2 * step, button),
            (find::OPEN_FIND, find_x, button),
        ] {
            place(control(id), s(x), row_y, s(width), s(button));
        }
        place(
            control(PAGE_INPUT),
            s(page_x),
            s(tab_height + 3),
            s(page_width),
            s(26),
        );
        let markup_left = if wrap {
            s(margin)
        } else {
            bounds.right - s(right_group_width)
        };
        let markup_top = tab_height + if wrap { row_height } else { 0 };
        let markup_y = s(markup_top + button_inset);
        for (id, x) in [
            (HIGHLIGHT, 0),
            (UNDERLINE, step),
            (UNDO, undo_x),
            (SAVE_MARKS, save_x),
        ] {
            place(
                control(id),
                markup_left + s(x),
                markup_y,
                s(button),
                s(button),
            );
        }
        place(
            control(COLOR),
            markup_left + s(color_x),
            s(markup_top + 3),
            s(color_width),
            s(140),
        );
        place(
            control(SIGN),
            bounds.right - s(margin + button),
            row_y,
            s(button),
            s(button),
        );
        let mut top = toolbar_bottom;
        let position = control(SIGN_POSITION);
        if !position.is_null()
            && unsafe { GetWindowLongW(position, GWL_STYLE) } as u32 & WS_VISIBLE != 0
        {
            let width = (bounds.right - s(2 * margin + 5 * step)).min(s(390));
            place(position, s(margin), s(top + 3), width, s(280));
            for (id, x) in [
                (WATERMARK, 0),
                (SIGN_CERTIFICATE, step),
                (SIGN_PFX, 2 * step),
                (SIGN_SAVE, 3 * step),
                (SIGN_CANCEL, 4 * step),
            ] {
                place(
                    control(id),
                    width + s(margin + gap + x),
                    s(top + button_inset),
                    s(button),
                    s(button),
                );
            }
            top += row_height;
        }
        top += find::Bar::layout(hwnd, top, &mut place);
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
        // Move the rows together. Copied pixels can otherwise belong to a button's
        // former neighbour when a resize moves the toolbar between one and two rows.
        unsafe {
            let flags = SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOCOPYBITS;
            let mut batch = BeginDeferWindowPos(positions.len() as i32);
            for &(child, x, y, w, h) in &positions {
                if batch.is_null() {
                    break;
                }
                batch = DeferWindowPos(batch, child, null_mut(), x, y, w, h, flags);
            }
            if batch.is_null() || EndDeferWindowPos(batch) == 0 {
                for (child, x, y, w, h) in positions {
                    SetWindowPos(child, null_mut(), x, y, w, h, flags);
                }
            }
            let toolbar_bounds = RECT {
                bottom: s(top),
                ..bounds
            };
            RedrawWindow(
                hwnd,
                &toolbar_bounds,
                null_mut(),
                RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN,
            );
        }
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

struct WheelMotion {
    start: std::time::Instant,
    distance: f64,
    delivered: f64,
}

impl WheelMotion {
    fn advance(&mut self) -> (f64, bool) {
        let t = (self.start.elapsed().as_secs_f64() / 0.105).min(1.0);
        let position = self.distance * (1.0 - (1.0 - t).powi(3));
        let dy = position - self.delivered;
        self.delivered = position;
        (dy, t >= 1.0)
    }
}

fn client_animations() -> bool {
    let mut enabled = 0i32;
    unsafe {
        SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&raw mut enabled).cast(), 0) != 0
            && enabled != 0
    }
}

struct App {
    find: find::State,
    tabs: Vec<tabs::Tab>,
    active_tab: Option<usize>,
    pending_restore: Option<tabs::SessionRestore>,
    resume_source: Option<crate::winpdf::SigningSource>,
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
    wheel_motion: Option<WheelMotion>,
    requested: Option<(u64, u32, (i32, i32), u64)>,
    first_frame_pending: bool,
    smoothing_in_progress: bool,
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
    fn new(hwnd: HWND, opening_pdf: bool) -> Result<Self, String> {
        let (events, receiver) = mpsc::channel();
        // On a file launch, let the renderer initialize WinRT while the UI
        // constructs its controls. Opening the tab retries a failed spawn.
        let worker = if opening_pdf {
            worker::spawn(hwnd as usize, events.clone()).ok()
        } else {
            None
        };
        let mut ui = Controls::new(hwnd)?;
        ui.fonts(hwnd);
        Ok(Self {
            find: find::State::default(),
            tabs: Vec::new(),
            active_tab: None,
            pending_restore: None,
            resume_source: None,
            hwnd,
            ui,
            worker,
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
            wheel_motion: None,
            requested: None,
            first_frame_pending: false,
            smoothing_in_progress: false,
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
        })
    }
    fn page_count(&self) -> u32 {
        self.document.as_ref().map_or(0, |d| d.pages)
    }
    fn controls(&self) {
        let editing = self.signing.is_none() && !self.writing;
        let annotate = editing && !self.known_signed;
        VIEW.with_borrow_mut(|v| {
            v.enable_text_selection(self.ui.canvas, editing && self.page_count() > 0)
        });
        for (c, enabled) in [
            (self.ui.open, !self.writing),
            (self.ui.tab_bar, !self.writing),
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
            (self.ui.find_button, self.page_count() > 0),
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
        self.enable_signing_controls();
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
        if let Some(index) = self.active_tab {
            tabs::update_label(self.ui.tab_bar, index, &format!("{name}{star}"));
            set_text(self.hwnd, &format!("{APP_TITLE}  —  {name}{star}"));
        } else {
            set_text(self.hwnd, APP_TITLE);
        }
    }
    fn discard_all_ok(&self) -> bool {
        if !self.tabs.iter().any(tabs::Tab::dirty) {
            return self.marks == self.saved_marks || self.confirm_discard(&self.path);
        }
        unsafe {
            MessageBoxW(
                self.hwnd,
                wide("Some tabs have unsaved annotations. Discard the changes and close all tabs?")
                    .as_ptr(),
                wide("SumiPDF").as_ptr(),
                MB_YESNO | MB_DEFBUTTON2 | MB_ICONQUESTION,
            ) == IDYES
        }
    }
    fn confirm_discard(&self, path: &std::path::Path) -> bool {
        unsafe {
            MessageBoxW(
                self.hwnd,
                wide(&format!(
                    "Discard unsaved annotations and close this PDF?\n\n{}",
                    path.display()
                ))
                .as_ptr(),
                wide("SumiPDF").as_ptr(),
                MB_YESNO | MB_DEFBUTTON2 | MB_ICONQUESTION,
            ) == IDYES
        }
    }
    fn status(&self) {
        if self.writing {
            return;
        }
        let reading = self.tool.is_none() && self.signing.is_none();
        let text = if let Some(message) = VIEW.with_borrow(|v| v.surface_message(reading)) {
            message
        } else if let Some(signing) = &self.signing {
            signing.instructions(self.page)
        } else if let Some(kind) = self.tool {
            let name = match kind {
                MarkKind::Highlight => "Highlight",
                MarkKind::Underline => "Underline",
            };
            let help = if !self.text_ready {
                "Loading text positions…"
            } else if self.has_text {
                "Drag across text to select it"
            } else if kind == MarkKind::Underline {
                "Drag horizontally to underline"
            } else {
                "Drag to highlight an area"
            };
            format!(
                "{name} ({})  ·  {help}  ·  Esc to finish",
                self.color.label()
            )
        } else {
            let range = VIEW
                .with_borrow(|v| v.visible_range)
                .unwrap_or((self.page, self.page));
            let pages = if range.0 == range.1 {
                format!("Page {}", range.0 + 1)
            } else {
                format!("Pages {}–{}", range.0 + 1, range.1 + 1)
            };
            let help = VIEW.with_borrow(|v| {
                if !v.has_selection() {
                    "Scroll with the mouse wheel"
                } else if !self.known_signed && v.selection_allows_annotations() {
                    "H to highlight  ·  U to underline  ·  Esc to clear"
                } else {
                    "Esc to clear selection"
                }
            });
            format!(
                "{pages}  ·  {:.0}% of fit size  ·  {help}",
                self.zoom * 100.0
            )
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
        self.load_text(self.page, worker::TextPurpose::Markup)
    }

    fn load_text(&mut self, page: u32, purpose: worker::TextPurpose) -> Result<(), String> {
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
                page,
                purpose,
                source,
            })
            .map_err(|_| "The text selection service has stopped.".to_owned())
    }

    fn finish_selection_text(&mut self, page: u32, layout: TextLayout) {
        let movement =
            VIEW.with_borrow_mut(|v| v.finish_reading_text(self.ui.canvas, page, layout));
        if let Some((dx, dy)) = movement
            && canvas::scroll_reading(self.ui.canvas, f64::from(dx), -f64::from(dy))
        {
            self.render();
        }
        self.status();
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
        self.render();
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
                    &format!("Enter a page number from 1 to {}.", self.page_count()),
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
    fn send(&self, job: Job) {
        if let Some(w) = &self.worker
            && w.send(job).is_err()
        {
            notice(
                self.hwnd,
                "The page renderer has stopped. Restart SumiPDF.",
                true,
            );
        }
    }
    fn render(&mut self) {
        let count = self.page_count();
        if count == 0 {
            return;
        }
        let area = canvas::area(self.ui.canvas);
        let reading = self.tool.is_none() && self.signing.is_none();
        let (lead, range_changed) = VIEW.with_borrow_mut(|v| {
            let before = v.visible_range;
            let lead = v.prepare(self.ui.canvas, self.page, count, reading);
            (lead, before != v.visible_range)
        });
        if self.page != lead {
            self.page = lead;
            self.controls();
        }
        if range_changed {
            self.status();
        }
        if self.requested.is_some() {
            return;
        }
        let Some((page, cache_pixels)) = VIEW.with_borrow_mut(|v| v.next_render(self.page, area))
        else {
            if let Some(quality) = self.smoothing_quality()
                && VIEW.with_borrow(|v| v.has_smoothing(self.page, quality))
            {
                unsafe { SetTimer(self.hwnd, SMOOTH_TIMER, 200, None) };
            }
            return;
        };
        self.requested = Some((self.revision, page, area, self.zoom.to_bits()));
        self.ticket += 1;
        self.send(Job::Render {
            revision: self.revision,
            ticket: self.ticket,
            page,
            area,
            zoom: self.zoom,
            cache_pixels,
        });
    }

    fn smoothing_quality(&self) -> Option<canvas::Quality> {
        if self.smoothing_in_progress
            || self.requested.is_some()
            || self.wheel_motion.is_some()
            || self.writing
            || self.tool.is_some()
            || self.signing.is_some()
            || self.resume_source.is_none()
        {
            return None;
        }
        Some((canvas::area(self.ui.canvas), self.zoom.to_bits()))
    }

    fn smooth_visible_page(&mut self) {
        if let Some(quality) = self.smoothing_quality()
            && let Some(source) = self.resume_source.clone()
            && let Some(smoothing) = VIEW.with_borrow_mut(|v| v.take_smoothing(self.page, quality))
        {
            self.smoothing_in_progress = worker::smooth(
                self.hwnd as usize,
                self.events.clone(),
                source,
                worker::SmoothingRequest {
                    revision: self.revision,
                    page: self.page,
                    quality,
                    smoothing,
                },
            )
            .is_ok();
        }
    }

    fn page_to(&mut self, page: u32) {
        self.stop_motion();
        if page >= self.page_count() {
            return;
        }
        if self.tool.is_none() && self.signing.is_none() {
            VIEW.with_borrow_mut(|v| {
                v.clear_text_selection(self.ui.canvas);
                v.jump(page);
            });
            self.page = page;
            self.controls();
            canvas::redraw(self.ui.canvas);
            self.render();
            return;
        }
        if self.page == page {
            return;
        }
        self.page = page;
        self.reset_canvas("Loading page…");
        self.sync_signature();
        if self.tool.is_some()
            && let Err(e) = self.request_text()
        {
            self.text_failed(&e);
        }
        if !self.writing {
            set_text(self.ui.status, "Loading page…");
        }
        self.controls();
        self.render();
    }
    fn scale(&mut self, zoom: f64) {
        self.stop_motion();
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
        self.status();
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
            let session = self
                .signing
                .as_mut()
                .ok_or("Choose a signature position.")?;
            let appearance = session.appearance()?;
            let certificate = session.resolve_certificate(self.hwnd)?;
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
        let review_page = signed.then_some(self.page);
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
                worker::report(
                    hwnd,
                    &events,
                    Event::Saved {
                        review_page,
                        result,
                    },
                );
            })
            .map_err(|e| e.to_string())?;
        self.writing = true;
        self.sync_canvas_tool();
        self.sync_signature();
        set_text(
            self.ui.status,
            if signed {
                "Signing PDF…"
            } else {
                "Saving annotations…"
            },
        );
        self.controls();
        Ok(())
    }
    fn event(&mut self, event: Event) {
        match event {
            Event::PreviousTabs(result) if self.pending_restore.is_some() => {
                self.pending_restore = Some(tabs::SessionRestore::Ready(result));
            }
            Event::Find(epoch, result) => self.find_page(epoch, result),
            Event::Opened(id, result) if id == self.revision => match result {
                Ok(document) => {
                    if matches!(self.pending_restore, Some(tabs::SessionRestore::Opening)) {
                        self.pending_restore = None;
                    }
                    self.page = self.page.min(document.pages.saturating_sub(1));
                    self.resume_source = document.source.as_ref().ok().cloned();
                    self.document = Some(document);
                    if self.tool.is_some()
                        && let Err(error) = self.request_text()
                    {
                        self.text_failed(&error);
                    }
                    if self
                        .signing
                        .as_ref()
                        .is_some_and(|session| !session.loaded())
                    {
                        self.signing = None;
                        if let Err(error) = self.begin_signing() {
                            if let Some(bar) = self.ui.signing {
                                bar.show(false);
                            }
                            Controls::layout(self.hwnd);
                            notice(self.hwnd, &error, true);
                        }
                    }
                    self.controls();
                    self.render();
                }
                Err(_) if matches!(self.pending_restore, Some(tabs::SessionRestore::Opening)) => {
                    if let Err(error) = self.skip_failed_restore_tab() {
                        self.pending_restore = None;
                        set_text(self.ui.status, &error);
                    }
                }
                Err(e) => {
                    self.reset_canvas("Could not open this PDF.");
                    set_text(self.ui.status, "Could not load PDF");
                    notice(self.hwnd, &format!("{e}\n\n{}", self.path.display()), true);
                }
            },
            Event::Painted(ticket, reading, result) if ticket == self.ticket => {
                if let Some((revision, page, area, zoom)) = self.requested.take()
                    && revision == self.revision
                {
                    let first_frame =
                        self.first_frame_pending && page == self.page && result.is_ok();
                    let repaint = VIEW.with_borrow_mut(|v| {
                        let repaint = v.finish_render(page, (area, zoom), reading, result);
                        v.reveal_find(self.ui.canvas) || repaint
                    });
                    if repaint {
                        canvas::redraw(self.ui.canvas);
                        if first_frame {
                            self.first_frame_pending = false;
                            // Present the first page before queuing the next render.
                            unsafe { UpdateWindow(self.ui.canvas) };
                        }
                    }
                    self.render();
                    let visible = VIEW.with_borrow(|v| {
                        v.visible_range
                            .is_some_and(|(first, last)| (first..=last).contains(&page))
                    });
                    if page == self.page || visible {
                        self.status();
                        if self.signing.is_some() {
                            self.controls();
                        }
                    }
                }
            }
            Event::Smoothed(request, image) => {
                self.smoothing_in_progress = false;
                if request.revision == self.revision
                    && request.quality == (canvas::area(self.ui.canvas), self.zoom.to_bits())
                    && let Some(image) = image
                    && VIEW.with_borrow_mut(|v| {
                        v.finish_smoothing(request.page, request.quality, image)
                    })
                {
                    canvas::redraw(self.ui.canvas);
                }
                self.render();
            }
            Event::Layout {
                revision,
                page,
                purpose: worker::TextPurpose::Markup,
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
            Event::Layout {
                revision,
                page,
                purpose: worker::TextPurpose::Selection,
                result,
            } if revision == self.revision => {
                self.finish_selection_text(page, result.unwrap_or_default());
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
            Event::Saved {
                review_page,
                result,
            } => {
                self.writing = false;
                match result {
                    Ok(path) => {
                        self.saved_marks.clone_from(&self.marks);
                        self.title();
                        if let Some(page) = review_page {
                            self.end_signing();
                            if let Err(error) = self.open_signed_copy(&path, page) {
                                let message = format!(
                                    "Saved the signed PDF.\n{}\n\nCould not open it automatically. {error}",
                                    path.display()
                                );
                                set_text(self.ui.status, &format!("Saved  ·  {}", path.display()));
                                notice(self.hwnd, &message, true);
                            }
                        } else {
                            self.sync_canvas_tool();
                            self.controls();
                            let message = format!("Saved the annotated PDF.\n{}", path.display());
                            set_text(self.ui.status, &format!("Saved  ·  {}", path.display()));
                            notice(self.hwnd, &message, false);
                        }
                    }
                    Err(e) => {
                        self.sync_canvas_tool();
                        self.sync_signature();
                        self.controls();
                        set_text(self.ui.status, "Could not save the PDF.");
                        notice(self.hwnd, &e, true);
                    }
                }
            }
            _ => {}
        }
    }
    fn command(&mut self, id: usize, tab_button: HWND) -> Result<(), String> {
        if !matches!(id, LAYOUT | SCROLLED) {
            self.stop_motion();
        }
        match id {
            find::PREVIOUS | find::NEXT if self.find.open => self.step_find(id == find::PREVIOUS),
            find::OPEN_FIND | find::PREVIOUS | find::NEXT => self.open_find()?,
            find::EDIT => self.update_find(),
            find::CLOSE => {
                self.close_find();
                self.render();
            }
            SHOW_SHORTCUTS => shortcuts::show(self.hwnd)?,
            RESTORE_TABS => {
                let enabled = !preferences::restore_tabs_enabled();
                preferences::set_restore_tabs_enabled(enabled)?;
                shortcuts::check_restore_tabs(self.hwnd, enabled);
                if !enabled && !matches!(self.pending_restore, Some(tabs::SessionRestore::Opening))
                {
                    self.pending_restore = None;
                }
            }
            FORGET_CERTIFICATE if !self.writing => self.forget_certificate()?,
            OPEN if !self.writing => {
                self.open_tabs(dialog::open_pdfs(self.hwnd)?)?;
            }
            PREVIOUS_TAB => self.cycle_tab(-1)?,
            NEXT_TAB => self.cycle_tab(1)?,
            CLOSE_TAB | SELECT_TAB => {
                // Resolve the captured HWND after any earlier queued edits to the tab list.
                let target = if tab_button.is_null() {
                    tabs::selected(self.ui.tab_bar)
                } else {
                    self.tabs
                        .iter()
                        .position(|tab| tab.close_button == tab_button)
                };
                if let Some(index) = target {
                    if id == CLOSE_TAB {
                        if self.active_tab == Some(index) {
                            self.pending_restore = None;
                        }
                        self.close_tab(index)?
                    } else {
                        self.switch_tab(index)?
                    }
                }
            }
            SCROLLED => self.render(),
            LOAD_SELECTION_TEXT => {
                if let Some(page) = VIEW.with_borrow(canvas::Canvas::pending_text_page)
                    && self
                        .load_text(page, worker::TextPurpose::Selection)
                        .is_err()
                {
                    self.finish_selection_text(page, TextLayout::default());
                }
            }
            SELECTION_CHANGED => self.status(),
            CLEAR_SELECTION => {
                VIEW.with_borrow_mut(|v| v.clear_text_selection(self.ui.canvas));
                self.status();
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
            WATERMARK if !self.writing => self.choose_appearance()?,
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
                let style = MarkStyle {
                    kind,
                    color: self.color,
                };
                if VIEW.with_borrow(canvas::Canvas::has_selection) {
                    if let Some(mark) = VIEW.with_borrow_mut(|v| v.take_selection(style)) {
                        self.marks.push(mark);
                        self.update_marks();
                    }
                    return Ok(());
                }
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
                self.render();
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
                notice(self.hwnd, "Please wait for the save to finish.", false)
            }
            CLOSE if self.discard_all_ok() => {
                if let Err(error) = self.remember_tabs() {
                    notice(
                        self.hwnd,
                        &format!("Could not remember the open tabs. {error}"),
                        true,
                    );
                }
                self.ui.toolbar.clear_names();
                unsafe { DestroyWindow(self.hwnd) };
            }
            _ => {}
        }
        Ok(())
    }
    fn stop_motion(&mut self) {
        if self.wheel_motion.take().is_some() {
            unsafe {
                KillTimer(self.hwnd, SCROLL_TIMER);
            }
        }
    }

    fn motion_tick(&mut self) {
        let Some(motion) = self.wheel_motion.as_mut() else {
            return;
        };
        if unsafe { GetCapture() } == self.ui.canvas {
            self.stop_motion();
            return;
        }
        let (dy, done) = motion.advance();
        if done {
            self.stop_motion();
        }
        if canvas::scroll_reading(self.ui.canvas, 0.0, dy) {
            self.render();
        }
    }

    fn wheel(&mut self, w: WPARAM, horizontal: bool) {
        if self.page_count() == 0 {
            return;
        }
        let delta = ((w >> 16) as i16) as i32;
        let ctrl = w & 8 != 0;
        if ctrl {
            if !horizontal {
                self.scale(
                    self.zoom * ZOOM_STEP.powf((f64::from(delta) / 120.0).clamp(-16.0, 16.0)),
                );
            }
            return;
        }
        if self.tool.is_none() && self.signing.is_none() {
            let distance = f64::from(delta) * f64::from(dips(self.ui.canvas, 72)) / 120.0;
            let (dx, dy) = if horizontal {
                (-distance, 0.0)
            } else if w & 4 != 0 {
                (distance, 0.0)
            } else {
                (0.0, -distance)
            };
            let carry = self
                .wheel_motion
                .as_ref()
                .map_or(0.0, |m| m.distance - m.delivered);
            let carry = if dy != 0.0 && carry.signum() == dy.signum() {
                carry
            } else {
                0.0
            };
            if dy != 0.0 && delta % 120 == 0 && client_animations() {
                let distance = dy + carry;
                let immediate = distance * 0.20;
                self.wheel_motion = Some(WheelMotion {
                    start: std::time::Instant::now(),
                    distance: distance - immediate,
                    delivered: 0.0,
                });
                let timed = unsafe { SetTimer(self.hwnd, SCROLL_TIMER, 15, None) } != 0;
                if !timed {
                    self.stop_motion();
                }
                if canvas::scroll_reading(
                    self.ui.canvas,
                    0.0,
                    if timed { immediate } else { distance },
                ) {
                    self.render();
                }
            } else {
                self.stop_motion();
                if canvas::scroll_reading(self.ui.canvas, dx, dy + carry) {
                    self.render();
                }
            }
            return;
        }
        if self.zoom > 1.0 {
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
            self.page_to(
                (i64::from(self.page) - i64::from(ticks)).clamp(0, i64::from(self.page_count() - 1))
                    as u32,
            );
        }
    }
    fn shortcut(&self, key: usize) -> Option<usize> {
        let ctrl = unsafe { GetKeyState(VK_CONTROL as i32) } < 0;
        let alt = unsafe { GetKeyState(VK_MENU as i32) } < 0;
        if alt {
            return None;
        }
        match (ctrl, key as u16) {
            (true, 0x46) => Some(find::OPEN_FIND),
            (false, VK_F3) => Some(if unsafe { GetKeyState(VK_SHIFT as i32) } < 0 {
                find::PREVIOUS
            } else {
                find::NEXT
            }),
            (false, VK_ESCAPE) if self.find.open => Some(find::CLOSE),
            (false, VK_ESCAPE) if VIEW.with_borrow(canvas::Canvas::selection_active) => {
                Some(CLEAR_SELECTION)
            }
            (true, VK_TAB) if unsafe { GetKeyState(VK_SHIFT as i32) } < 0 => Some(PREVIOUS_TAB),
            (true, VK_TAB) => Some(NEXT_TAB),
            (true, 0x57) => Some(CLOSE_TAB),
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
            // Process an already queued file drop or Open command before
            // applying the background session load to an empty window.
            if matches!(self.pending_restore, Some(tabs::SessionRestore::Ready(_)))
                && unsafe { PeekMessageW(&mut msg, null_mut(), 0, 0, PM_NOREMOVE) } == 0
                && let Some(tabs::SessionRestore::Ready(result)) = self.pending_restore.take()
                && let Err(error) = result.and_then(|session| self.restore_tab_session(session))
            {
                set_text(
                    self.ui.status,
                    &format!("Could not restore the previous tabs. {error}"),
                );
            }
            if unsafe { GetMessageW(&mut msg, null_mut(), 0, 0) } <= 0 {
                break;
            }
            let in_find = self
                .ui
                .find
                .as_ref()
                .is_some_and(|bar| msg.hwnd == bar.edit);
            if msg.message == WM_KEYDOWN {
                if in_find && self.find.open && msg.wParam == VK_RETURN as usize {
                    self.step_find(unsafe { GetKeyState(VK_SHIFT as i32) } < 0);
                    continue;
                }
                // Hand navigation and dismissal to the native color list.
                let picker_key = (msg.hwnd == self.ui.color
                    || self.ui.signing.is_some_and(|bar| msg.hwnd == bar.position))
                    && (unsafe { SendMessageW(msg.hwnd, CB_GETDROPPEDSTATE, 0, 0) != 0 }
                        || matches!(
                            msg.wParam as u16,
                            VK_LEFT | VK_RIGHT | VK_HOME | VK_END | VK_PRIOR | VK_NEXT
                        ));
                let tab_arrow =
                    msg.hwnd == self.ui.tab_bar && matches!(msg.wParam as u16, VK_LEFT | VK_RIGHT);
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
                }
                if !picker_key
                    && !tab_arrow
                    && let Some(id) = self.shortcut(msg.wParam)
                    && (!in_find
                        || matches!(
                            id,
                            find::OPEN_FIND
                                | find::NEXT
                                | find::PREVIOUS
                                | find::CLOSE
                                | CLOSE_TAB
                                | NEXT_TAB
                                | PREVIOUS_TAB
                                | OPEN
                        ))
                    && (msg.hwnd != self.ui.page_input
                        || matches!(
                            id,
                            find::OPEN_FIND | CLOSE_TAB | NEXT_TAB | PREVIOUS_TAB | OPEN
                        ))
                {
                    if id == CLOSE_TAB {
                        tabs::post_selected(self.ui.tab_bar, id);
                    } else {
                        action(self.hwnd, id);
                    }
                    continue;
                }
                if msg.wParam == VK_TAB as usize
                    && unsafe { IsDialogMessageW(self.hwnd, &msg) } != 0
                {
                    continue;
                }
            }
            match msg.message {
                // TranslateMessage uses the active keyboard layout to produce '?'.
                WM_CHAR if msg.wParam == usize::from(b'?') && !in_find => {
                    if msg.lParam & (1 << 30) == 0 {
                        action(self.hwnd, SHOW_SHORTCUTS);
                    }
                }
                ACTION if msg.hwnd == self.hwnd => {
                    if let Err(e) = self.command(msg.wParam, msg.lParam as HWND) {
                        notice(self.hwnd, &e, true);
                    }
                    if msg.wParam == CLOSE && unsafe { IsWindow(self.hwnd) } == 0 {
                        return;
                    }
                }
                READY if msg.hwnd == self.hwnd => {}
                WHEEL if msg.hwnd == self.hwnd => {
                    self.wheel(msg.wParam, msg.lParam == WM_MOUSEHWHEEL as isize)
                }
                WM_TIMER if msg.hwnd == self.hwnd && msg.wParam == SCROLL_TIMER => {
                    self.motion_tick()
                }
                DROPPED if msg.hwnd == self.hwnd => {
                    let drop = msg.wParam as HDROP;
                    let count = unsafe { DragQueryFileW(drop, u32::MAX, null_mut(), 0) };
                    let mut paths = Vec::new();
                    for i in 0..count {
                        let len = unsafe { DragQueryFileW(drop, i, null_mut(), 0) };
                        let mut name = vec![0; len as usize + 1];
                        let n = unsafe { DragQueryFileW(drop, i, name.as_mut_ptr(), len + 1) };
                        if n > 0 {
                            paths.push(PathBuf::from(OsString::from_wide(&name[..n as usize])));
                        }
                    }
                    unsafe {
                        DragFinish(drop);
                    }
                    if let Err(e) = self.open_tabs(paths) {
                        notice(self.hwnd, &e, true);
                    }
                }
                WM_TIMER if msg.hwnd == self.hwnd && msg.wParam == RENDER_TIMER => {
                    unsafe {
                        KillTimer(self.hwnd, RENDER_TIMER);
                    }
                    self.render();
                }
                WM_TIMER if msg.hwnd == self.hwnd && msg.wParam == SMOOTH_TIMER => {
                    unsafe { KillTimer(self.hwnd, SMOOTH_TIMER) };
                    self.smooth_visible_page();
                }
                _ => unsafe {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                },
            }
        }
    }
}

pub fn run(files: Vec<OsString>) {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let com = unsafe { CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32) };
    let result = (|| {
        if !register(WINDOW, Some(window_proc), null_mut())
            || !register(CANVAS, Some(canvas::procedure), null_mut())
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_CONTROLPARENT,
                wide(WINDOW).as_ptr(),
                wide(APP_TITLE).as_ptr(),
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
        theme::refresh(hwnd);
        let restore_tabs = preferences::restore_tabs_enabled();
        shortcuts::add_to_window_menu(hwnd, restore_tabs);
        let start_with_files = !files.is_empty();
        let mut app = App::new(hwnd, start_with_files)?;
        let opened = app.open_tabs(files.into_iter().map(PathBuf::from));
        if app.active_tab.is_none() {
            Controls::layout(hwnd);
            app.controls();
        }
        unsafe {
            DragAcceptFiles(hwnd, 1);
            ShowWindow(hwnd, SW_SHOW);
            UpdateWindow(hwnd);
        }
        if let Err(e) = opened {
            notice(hwnd, &e, true);
        }
        if !start_with_files
            && restore_tabs
            && let Err(error) = app.read_previous_tabs()
        {
            set_text(
                app.ui.status,
                &format!("Could not restore the previous tabs. {error}"),
            );
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
