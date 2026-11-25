//! The UI thread owns the current page surface. Panning and painting use the
//! last bitmap immediately, including while a higher-resolution render runs.
use super::{dips, rect, wide};
use crate::highlight::{Mark, UnitRect};
use crate::textselect::TextLayout;
use crate::winpdf::Bitmap;
use std::{
    cell::RefCell,
    collections::VecDeque,
    mem::size_of,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
    Graphics::Gdi::*,
    UI::{
        Input::KeyboardAndMouse::{GetCapture, ReleaseCapture, SetCapture, SetFocus},
        WindowsAndMessaging::*,
    },
};

const RELEASE_IDLE_CAPTURE: u32 = WM_APP + 41;

#[derive(Clone, Copy)]
enum Drag {
    Pan((i32, i32)),
    Highlight {
        origin: (f32, f32),
        glyph: Option<usize>,
    },
}

#[derive(Clone, Copy, Default)]
struct PageArea {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl PageArea {
    fn contains(self, (x, y): (i32, i32)) -> bool {
        (self.x..self.x + self.w).contains(&x) && (self.y..self.y + self.h).contains(&y)
    }

    fn normalized(self, (x, y): (i32, i32)) -> (f32, f32) {
        (
            ((x - self.x) as f32 / self.w as f32).clamp(0.0, 1.0),
            ((y - self.y) as f32 / self.h as f32).clamp(0.0, 1.0),
        )
    }

    fn pixels(self, rect: &UnitRect) -> RECT {
        RECT {
            left: self.x + (rect.left * self.w as f32).floor() as i32,
            top: self.y + (rect.top * self.h as f32).floor() as i32,
            right: self.x + (rect.right * self.w as f32).ceil() as i32,
            bottom: self.y + (rect.bottom * self.h as f32).ceil() as i32,
        }
    }
}

#[derive(Default)]
pub struct Canvas {
    pub bitmap: Option<Bitmap>,
    pub text: String,
    pub empty: bool,
    pub zoom: f64,
    offset: (i32, i32),
    drag: Option<Drag>,
    capture_window: HWND,
    highlight_tool: bool,
    text_layout: Option<TextLayout>,
    marks: Vec<UnitRect>,
    preview: Vec<UnitRect>,
    completed: VecDeque<Vec<UnitRect>>,
    pub font: HFONT,
    pub headline: HFONT,
}

thread_local! { pub static VIEW: RefCell<Canvas> = RefCell::new(Canvas { empty: true, zoom: 1.0, ..Default::default() }); }

pub fn redraw(hwnd: HWND) {
    unsafe {
        InvalidateRect(hwnd, null(), 0);
    }
}

pub fn area(hwnd: HWND) -> (i32, i32) {
    let r = rect(hwnd);
    let margin = dips(hwnd, 18) * 2;
    ((r.right - margin).max(1), (r.bottom - margin).max(1))
}

impl Canvas {
    pub fn reset(&mut self, empty: bool, text: &str) {
        self.bitmap = None;
        self.offset = (0, 0);
        self.cancel_drag();
        self.marks.clear();
        self.text_layout = None;
        self.empty = empty;
        self.text = text.into();
    }

    pub fn center(&mut self) {
        self.offset = (0, 0);
    }

    pub fn set_tool(&mut self, active: bool) {
        if self.highlight_tool != active {
            self.cancel_drag();
            self.highlight_tool = active;
        }
    }

    pub fn set_marks(&mut self, marks: &[Mark], page: u32) {
        self.marks.clear();
        for mark in marks.iter().filter(|mark| mark.page == page) {
            self.marks.extend_from_slice(&mark.rects);
        }
    }

    pub fn set_text_layout(&mut self, layout: Option<TextLayout>) {
        if matches!(self.drag, Some(Drag::Highlight { glyph: Some(_), .. })) {
            self.cancel_drag();
        }
        self.text_layout = layout;
    }

    pub fn take_new_mark(&mut self) -> Option<Vec<UnitRect>> {
        self.completed.pop_front()
    }

    fn cancel_drag(&mut self) {
        if self.drag.take().is_some() {
            // ReleaseCapture synchronously sends WM_CAPTURECHANGED. Run it
            // outside the current mutable borrow of the UI-thread canvas.
            unsafe {
                PostMessageW(self.capture_window, RELEASE_IDLE_CAPTURE, 0, 0);
            }
        }
        if !self.preview.is_empty() {
            redraw(self.capture_window);
        }
        self.preview.clear();
    }

    fn layout(&mut self, hwnd: HWND) -> PageArea {
        let r = rect(hwnd);
        let (w, h) = area(hwnd);
        let Some(image) = &self.bitmap else {
            return PageArea::default();
        };
        let fit = (w as f64 / f64::from(image.width)).min(h as f64 / f64::from(image.height));
        let width = (f64::from(image.width) * fit * self.zoom).round().max(1.0) as i32;
        let height = (f64::from(image.height) * fit * self.zoom).round().max(1.0) as i32;
        let max_x = ((width - w + 1) / 2).max(0);
        let max_y = ((height - h + 1) / 2).max(0);
        self.offset.0 = self.offset.0.clamp(-max_x, max_x);
        self.offset.1 = self.offset.1.clamp(-max_y, max_y);
        PageArea {
            x: (r.right - width) / 2 + self.offset.0,
            y: (r.bottom - height) / 2 + self.offset.1,
            w: width,
            h: height,
        }
    }

    fn start_drag(&mut self, hwnd: HWND, point: (i32, i32)) -> bool {
        let page = self.layout(hwnd);
        self.drag = if self.highlight_tool && page.contains(point) {
            let origin = page.normalized(point);
            let glyph = self
                .text_layout
                .as_ref()
                .and_then(|text| text.hit(origin.0, origin.1));
            Some(Drag::Highlight { origin, glyph })
        } else if !self.highlight_tool && self.bitmap.is_some() && self.zoom > 1.0 {
            Some(Drag::Pan(point))
        } else {
            None
        };
        self.capture_window = hwnd;
        self.preview.clear();
        self.drag.is_some()
    }

    fn update_preview(&mut self, hwnd: HWND, point: (i32, i32)) {
        let Some(Drag::Highlight { origin, glyph }) = self.drag else {
            return;
        };
        let page = self.layout(hwnd);
        if page.w <= 0 || page.h <= 0 {
            return;
        }
        let (x, y) = page.normalized(point);
        let dx = (x - origin.0).abs() * page.w as f32;
        let dy = (y - origin.1).abs() * page.h as f32;
        let threshold = dips(hwnd, 3) as f32;
        let new = if dx.max(dy) < threshold {
            Vec::new()
        } else if let (Some(text), Some(anchor)) = (&self.text_layout, glyph) {
            text.range(anchor, x, y)
        } else if dx >= threshold && dy >= threshold {
            vec![UnitRect {
                left: x.min(origin.0),
                top: y.min(origin.1),
                right: x.max(origin.0),
                bottom: y.max(origin.1),
            }]
        } else {
            Vec::new()
        };
        if self.preview != new {
            for r in self.preview.iter().chain(&new) {
                unsafe {
                    InvalidateRect(hwnd, &page.pixels(r), 0);
                }
            }
            self.preview = new;
        }
    }
}

pub fn pan(hwnd: HWND, dx: i32, dy: i32) -> bool {
    VIEW.with_borrow_mut(|v| {
        let old = v.offset;
        v.offset.0 = v.offset.0.saturating_add(dx);
        v.offset.1 = v.offset.1.saturating_add(dy);
        v.layout(hwnd);
        let changed = old != v.offset;
        if changed {
            redraw(hwnd);
        }
        changed
    })
}

fn fill(dc: HDC, r: &RECT, color: u32) {
    unsafe {
        let brush = GetStockObject(DC_BRUSH) as HBRUSH;
        SetDCBrushColor(dc, color);
        FillRect(dc, r, brush);
    }
}

fn line(dc: HDC, font: HFONT, color: u32, bounds: RECT, label: &str) {
    let mut bounds = bounds;
    unsafe {
        let old = SelectObject(dc, font as _);
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, color);
        DrawTextW(
            dc,
            wide(label).as_ptr(),
            -1,
            &mut bounds,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
        SelectObject(dc, old);
    }
}

fn paint_highlights(dc: HDC, clip: RECT, page: PageArea, state: &Canvas) {
    let mut visible = state.marks.iter().chain(&state.preview).filter_map(|mark| {
        let p = page.pixels(mark);
        let r = RECT {
            left: p.left.max(clip.left),
            top: p.top.max(clip.top),
            right: p.right.min(clip.right),
            bottom: p.bottom.min(clip.bottom),
        };
        (r.left < r.right && r.top < r.bottom).then_some(r)
    });
    let Some(first) = visible.next() else { return };

    // Match PDF Multiply with yellow at 45% opacity. MERGECOPY removes
    // blue from the page pixels, then AlphaBlend mixes those with the page,
    // preserving dark text. A bounded tile handles even a full-page mark.
    const WIDTH: usize = 512;
    const HEIGHT: usize = 128;
    unsafe {
        let tile = CreateCompatibleDC(dc);
        if tile.is_null() {
            return;
        }
        let bitmap = CreateCompatibleBitmap(dc, WIDTH as i32, HEIGHT as i32);
        if !bitmap.is_null() {
            let original = SelectObject(tile, bitmap);
            SelectObject(tile, GetStockObject(DC_BRUSH));
            SetDCBrushColor(tile, 0x0000ffff);
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                SourceConstantAlpha: 115,
                ..Default::default()
            };
            for r in std::iter::once(first).chain(visible) {
                for y in (r.top..r.bottom).step_by(HEIGHT) {
                    for x in (r.left..r.right).step_by(WIDTH) {
                        let w = (r.right - x).min(WIDTH as i32);
                        let h = (r.bottom - y).min(HEIGHT as i32);
                        if BitBlt(tile, 0, 0, w, h, dc, x, y, MERGECOPY) != 0 {
                            GdiAlphaBlend(dc, x, y, w, h, tile, 0, 0, w, h, blend);
                        }
                    }
                }
            }
            SelectObject(tile, original);
            DeleteObject(bitmap);
        }
        DeleteDC(tile);
    }
}

fn paint(hwnd: HWND, state: &mut Canvas) {
    let mut ps = PAINTSTRUCT::default();
    unsafe {
        let dc = BeginPaint(hwnd, &mut ps);
        let bounds = rect(hwnd);
        let page = state.layout(hwnd);
        let PageArea { x, y, w, h } = page;
        if let Some(img) = &state.bitmap {
            // Paint only the margins. Skipping a full-window erase prevents
            // a background flash and avoids a screen-sized backbuffer.
            let saved = SaveDC(dc);
            ExcludeClipRect(dc, x, y, x + w, y + h);
            fill(dc, &bounds, 0x00e9e9e6);
            fill(
                dc,
                &RECT {
                    left: x - 1,
                    top: y - 1,
                    right: x + w + 1,
                    bottom: y + h + 1,
                },
                0x00c8c6c1,
            );
            RestoreDC(dc, saved);
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: img.width as i32,
                    biHeight: -(img.height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..Default::default()
                },
                ..Default::default()
            };
            SetStretchBltMode(dc, COLORONCOLOR);
            StretchDIBits(
                dc,
                x,
                y,
                w,
                h,
                0,
                0,
                img.width as i32,
                img.height as i32,
                img.pixels.as_ptr().cast(),
                &info,
                DIB_RGB_COLORS,
                SRCCOPY,
            );
            paint_highlights(dc, ps.rcPaint, page, state);
        } else {
            fill(dc, &bounds, 0x00f7f7f4);
            let cy = bounds.bottom / 2;
            let step = dips(hwnd, 37);
            if state.empty {
                line(
                    dc,
                    state.headline,
                    0x00342c23,
                    RECT {
                        top: cy - step,
                        bottom: cy + 2,
                        ..bounds
                    },
                    "PDF を開く",
                );
                line(
                    dc,
                    state.font,
                    0x006a625a,
                    RECT {
                        top: cy + 2,
                        bottom: cy + step,
                        ..bounds
                    },
                    "ここにファイルをドロップ  /  Ctrl + O",
                );
            } else {
                line(dc, state.font, 0x006a625a, bounds, &state.text);
            }
        }
        EndPaint(hwnd, &ps);
    }
}

pub unsafe extern "system" fn procedure(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let point = ((l as u16 as i16) as i32, ((l >> 16) as i16) as i32);
    match msg {
        WM_PAINT => {
            VIEW.with_borrow_mut(|v| paint(hwnd, v));
            0
        }
        WM_ERASEBKGND => 1,
        WM_SETCURSOR
            if (l as u16) == HTCLIENT as u16
                && VIEW.with_borrow(|v| v.highlight_tool && v.bitmap.is_some()) =>
        {
            unsafe {
                SetCursor(LoadCursorW(null_mut(), IDC_CROSS));
            }
            1
        }
        WM_LBUTTONDOWN => {
            if VIEW.with_borrow_mut(|v| v.start_drag(hwnd, point)) {
                unsafe {
                    SetFocus(hwnd);
                    SetCapture(hwnd);
                }
            }
            0
        }
        WM_MOUSEMOVE => {
            let previous = VIEW.with_borrow_mut(|v| match &mut v.drag {
                Some(Drag::Pan(p)) => Some(std::mem::replace(p, point)),
                Some(Drag::Highlight { .. }) => {
                    v.update_preview(hwnd, point);
                    None
                }
                None => None,
            });
            if let Some((x, y)) = previous {
                pan(hwnd, point.0 - x, point.1 - y);
            }
            0
        }
        WM_LBUTTONUP => {
            let marked = VIEW.with_borrow_mut(|v| {
                v.update_preview(hwnd, point);
                v.drag = None;
                if v.preview.is_empty() {
                    return false;
                }
                let new = std::mem::take(&mut v.preview);
                v.marks.extend_from_slice(&new);
                v.completed.push_back(new);
                true
            });
            unsafe {
                if GetCapture() == hwnd {
                    ReleaseCapture();
                }
                if marked {
                    PostMessageW(GetParent(hwnd), super::READY, 0, 0);
                }
            }
            0
        }
        RELEASE_IDLE_CAPTURE => {
            if VIEW.with_borrow(|v| v.drag.is_none()) && unsafe { GetCapture() } == hwnd {
                unsafe {
                    ReleaseCapture();
                }
            }
            0
        }
        WM_CAPTURECHANGED | WM_CANCELMODE => {
            let preview = VIEW.with_borrow_mut(|v| {
                v.drag = None;
                let visible = !v.preview.is_empty();
                v.preview.clear();
                visible
            });
            if preview {
                redraw(hwnd);
            }
            if msg == WM_CANCELMODE && unsafe { GetCapture() } == hwnd {
                unsafe {
                    ReleaseCapture();
                }
            }
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_points_remain_normalized_when_panned_and_zoomed() {
        let page = PageArea {
            x: -200,
            y: -80,
            w: 800,
            h: 400,
        };
        assert!(page.contains((0, 20)));
        assert!(!page.contains((600, 20)));
        assert_eq!(page.normalized((0, 20)), (0.25, 0.25));
        assert_eq!(page.normalized((-600, 999)), (0.0, 1.0));
        let converted = page.pixels(&UnitRect {
            left: 0.25,
            top: 0.25,
            right: 0.75,
            bottom: 0.5,
        });
        assert_eq!(
            (
                converted.left,
                converted.top,
                converted.right,
                converted.bottom
            ),
            (0, 20, 400, 120)
        );
    }

    #[test]
    fn yellow_preview_multiplies_paper_and_preserves_black_text() {
        let state = Canvas {
            marks: vec![UnitRect {
                left: 0.25,
                top: 0.25,
                right: 0.75,
                bottom: 0.75,
            }],
            ..Default::default()
        };
        unsafe {
            let dc = CreateCompatibleDC(null_mut());
            assert!(!dc.is_null());
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: 8,
                    biHeight: -8,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut data = null_mut();
            let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut data, null_mut(), 0);
            assert!(!bitmap.is_null());
            let original = SelectObject(dc, bitmap);
            let bounds = RECT {
                left: 0,
                top: 0,
                right: 8,
                bottom: 8,
            };
            fill(dc, &bounds, 0x00ffffff);
            SetPixelV(dc, 3, 3, 0);
            paint_highlights(
                dc,
                bounds,
                PageArea {
                    x: 0,
                    y: 0,
                    w: 8,
                    h: 8,
                },
                &state,
            );
            let paper = GetPixel(dc, 2, 2);
            let ink = GetPixel(dc, 3, 3);
            let outside = GetPixel(dc, 7, 0);
            SelectObject(dc, original);
            DeleteObject(bitmap);
            DeleteDC(dc);
            assert_eq!(outside, 0x00ffffff);
            assert_eq!(ink, 0);
            assert_eq!(paper & 0xffff, 0xffff);
            assert!(
                (139..=141).contains(&(paper >> 16)),
                "COLORREF={paper:#08x}"
            );
        }
    }
}
