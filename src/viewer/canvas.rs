//! The UI thread owns surfaces intersecting the viewport and its immediate
//! neighbors. Reading and editing share the same Win32 painting path.
use super::{dips, rect, wide};
use crate::continuous::{Layout, Position};
use crate::highlight::{
    HIGHLIGHT_OPACITY, Mark, MarkColor, MarkKind, MarkStyle, TextRun, UnitRect,
};
use crate::raster_size::{FIT_PIXELS, fitted_page_size, view_pixel_limit};
use crate::textselect::TextLayout;
use crate::winpdf::{Bitmap, Frame};
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
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
    Signature((f32, f32)),
    Highlight {
        origin: (f32, f32),
        glyph: Option<usize>,
    },
    Text(TextDrag),
}

#[derive(Clone, Copy)]
enum TextDragPhase {
    Starting,
    Loading { released: bool },
    Selecting(usize),
}

#[derive(Clone, Copy)]
struct TextDrag {
    page: u32,
    area: PageArea,
    origin: (i32, i32),
    point: (i32, i32),
    phase: TextDragPhase,
}

impl TextDrag {
    fn delta(self) -> (i32, i32) {
        (self.point.0 - self.origin.0, self.point.1 - self.origin.1)
    }

    fn moved(self, minimum: i32) -> bool {
        let (dx, dy) = self.delta();
        dx.abs().max(dy.abs()) >= minimum
    }
}

struct Selection {
    page: u32,
    runs: Vec<TextRun>,
}

#[derive(Clone, Copy, Default)]
struct PageArea {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

pub(super) type Quality = ((i32, i32), u64);

struct CachedPage {
    quality: Quality,
    limited_to: Option<u64>,
    image: Result<Bitmap, String>,
}

struct Reading {
    layout: Layout,
    frames: BTreeMap<u32, CachedPage>,
}

#[derive(Clone, Copy, Default)]
pub struct ViewPosition {
    anchor: Option<Position>,
    pan: (i32, i32),
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
    pub(super) matches: super::find::Matches,
    pub(super) find_reveal: Option<(u32, UnitRect)>,
    pub bitmap: Option<Bitmap>,
    reading: Option<Reading>,
    resume: Option<Position>,
    single_quality: Option<Quality>,
    single_ratio: Option<f64>,
    pub visible_range: Option<(u32, u32)>,
    pub text: String,
    pub empty: bool,
    pub zoom: f64,
    offset: (i32, i32),
    drag: Option<Drag>,
    capture_window: HWND,
    tool: Option<MarkStyle>,
    page: u32,
    text_layout: Option<TextLayout>,
    text_selection_enabled: bool,
    selection_layout: Option<(u32, TextLayout)>,
    selection: Option<Selection>,
    marks: Vec<Mark>,
    preview: Vec<TextRun>,
    completed: VecDeque<Mark>,
    draw_signature: bool,
    signature: Option<UnitRect>,
    signature_preview: Option<UnitRect>,
    placed_signature: Option<(u32, UnitRect)>,
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

/// Reuse pixels already painted at their previous positions. A horizontal pan
/// can move different-width pages by different amounts, so repaint it normally.
pub fn scroll_reading(hwnd: HWND, dx: f64, dy: f64) -> bool {
    let Some((shift, update_layout)) = VIEW.with_borrow_mut(|v| {
        let r = v.reading.as_mut()?;
        let old = r.layout.visible();
        if !r.layout.scroll(dx, dy) {
            return None;
        }
        let new = r.layout.visible();
        let shift = if dx == 0.0 {
            old.first()
                .and_then(|p| new.iter().find(|q| q.page == p.page).map(|q| q.y - p.y))
                .or_else(|| {
                    new.first()
                        .and_then(|q| old.iter().find(|p| p.page == q.page).map(|p| q.y - p.y))
                })
        } else {
            None
        };
        let range = new.first().zip(new.last()).map(|(a, b)| (a.page, b.page));
        Some((
            shift,
            range != v.visible_range || r.layout.position.page != v.page,
        ))
    }) else {
        return false;
    };
    match shift {
        Some(0) => {}
        Some(pixels) if pixels.unsigned_abs() < rect(hwnd).bottom as u32 => unsafe {
            if ScrollWindowEx(
                hwnd,
                0,
                pixels,
                null(),
                null(),
                null_mut(),
                null_mut(),
                SW_INVALIDATE,
            ) == 0
            {
                redraw(hwnd);
            }
        },
        _ => redraw(hwnd),
    }
    update_layout
}

impl Canvas {
    pub fn reset(&mut self, empty: bool, text: &str) {
        self.bitmap = None;
        self.reading = None;
        self.single_quality = None;
        self.single_ratio = None;
        self.resume = None;
        self.visible_range = None;
        self.offset = (0, 0);
        self.cancel_drag();
        self.marks.clear();
        self.text_layout = None;
        self.selection = None;
        self.selection_layout = None;
        self.empty = empty;
        self.text = text.into();
    }

    pub fn center(&mut self) {
        self.offset = (0, 0);
    }

    pub fn position(&self) -> ViewPosition {
        ViewPosition {
            anchor: self
                .reading
                .as_ref()
                .map(|r| r.layout.position)
                .or(self.resume),
            pan: self.offset,
        }
    }

    pub fn restore_position(&mut self, position: ViewPosition) {
        self.resume = position.anchor;
        self.offset = position.pan;
    }

    pub fn prepare(&mut self, hwnd: HWND, page: u32, count: u32, reading: bool) -> u32 {
        if reading && self.reading.is_none() {
            let anchor = self
                .resume
                .take()
                .filter(|pos| pos.page == page)
                .unwrap_or_else(|| Position::at(page));
            let mut layout = Layout::new(count, anchor);
            layout.configure(area(hwnd), dips(hwnd, 18), dips(hwnd, 18), self.zoom);
            let mut frames = BTreeMap::new();
            if let (Some(image), Some(quality)) = (self.bitmap.take(), self.single_quality.take()) {
                if let Some(ratio) = self.single_ratio.take() {
                    layout.measured(page, ratio);
                }
                frames.insert(
                    page,
                    CachedPage {
                        quality,
                        limited_to: None,
                        image: Ok(image),
                    },
                );
            }
            self.reading = Some(Reading { layout, frames });
            self.center();
            self.cancel_drag();
            redraw(hwnd);
        } else if !reading && let Some(mut old) = self.reading.take() {
            self.clear_text_selection(hwnd);
            self.selection_layout = None;
            self.resume = Some(old.layout.position);
            self.text = "Loading page…".into();
            if let Some(cached) = old.frames.remove(&page)
                && let Ok(image) = cached.image
            {
                self.single_ratio = Some(old.layout.aspect_ratio(page));
                self.bitmap = Some(image);
                self.single_quality = cached.limited_to.is_none().then_some(cached.quality);
            }
            self.cancel_drag();
            redraw(hwnd);
        }
        let Some(r) = &mut self.reading else {
            self.visible_range = None;
            self.page = page;
            return page;
        };
        r.layout
            .configure(area(hwnd), dips(hwnd, 18), dips(hwnd, 18), self.zoom);
        let visible = r.layout.visible();
        self.visible_range = visible
            .first()
            .zip(visible.last())
            .map(|(first, last)| (first.page, last.page));
        self.page = r.layout.position.page;
        self.page
    }

    pub fn jump(&mut self, page: u32) {
        if let Some(r) = &mut self.reading {
            r.layout.jump(page);
        }
    }

    pub(super) fn reveal_find(&mut self, hwnd: HWND) -> bool {
        let Some((index, hit)) = self.find_reveal else {
            return false;
        };
        let placement = if let Some(reading) = &self.reading {
            // Wait for the measured dimensions before revealing a match on a
            // landscape or unusually tall page. Estimates can put it offscreen.
            if !reading.frames.get(&index).is_some_and(|p| p.image.is_ok()) {
                return false;
            }
            let Some(p) = reading
                .layout
                .visible()
                .into_iter()
                .find(|p| p.page == index)
            else {
                return false;
            };
            PageArea {
                x: p.x,
                y: p.y,
                w: p.w,
                h: p.h,
            }
        } else {
            if self.page != index || self.bitmap.is_none() {
                return false;
            }
            self.layout(hwnd)
        };
        self.find_reveal = None;
        let target = placement.pixels(&hit);
        let viewport = rect(hwnd);
        let margin = dips(hwnd, 12);
        let offset = |start: i32, end: i32, extent: i32| {
            if start >= margin && end <= extent - margin {
                0
            } else if end - start > extent - 2 * margin {
                margin - start
            } else {
                (extent - (end - start)) / 2 - start
            }
        };
        let dx = offset(target.left, target.right, viewport.right);
        let dy = offset(target.top, target.bottom, viewport.bottom);
        if let Some(reading) = &mut self.reading {
            reading.layout.reveal_offset(index, dx.into(), (-dy).into())
        } else {
            self.offset.0 += dx;
            self.offset.1 += dy;
            self.layout(hwnd);
            dx != 0 || dy != 0
        }
    }

    pub fn next_render(&mut self, page: u32, area: (i32, i32)) -> Option<(u32, Option<u64>)> {
        let quality = (area, self.zoom.to_bits());
        let Some(r) = &mut self.reading else {
            return (self.single_quality != Some(quality)).then_some((page, None));
        };
        let wanted = r.layout.wanted();
        r.frames.retain(|index, _| wanted.contains(index));
        const FIT_CACHE_PIXELS: u64 = 12 * 1024 * 1024;
        const PREVIEW_PIXELS: u64 = 1024 * 1024;
        let lead_limit = view_pixel_limit(self.zoom);
        let cache_limit = FIT_CACHE_PIXELS + lead_limit - FIT_PIXELS;
        let visible = r.layout.visible();
        let on_screen = |index| visible.iter().any(|p| p.page == index);
        let pixels = |f: &CachedPage| {
            f.image
                .as_ref()
                .map_or(0, |img| img.pixels.len() as u64 / 4)
        };
        let mut used: u64 = r.frames.values().map(pixels).sum();
        if used > cache_limit {
            // Keep the current image until its replacement arrives after zooming out.
            r.frames
                .retain(|index, f| *index == page || f.quality == quality);
            used = r.frames.values().map(pixels).sum();
        }
        // Account for replacement before dispatch. The worker also enforces
        // the budget if the native decoder produces an unexpectedly large BMP.
        for &next in &wanted {
            let old = r.frames.get(&next);
            let old_pixels = old.map_or(0, pixels);
            let available =
                |used: u64| cache_limit.saturating_sub(used - old_pixels + wanted.len() as u64);
            let limit = if !on_screen(next) {
                PREVIEW_PIXELS
            } else if next == page {
                lead_limit
            } else {
                FIT_PIXELS
            };
            if old.is_some_and(|f| {
                f.quality == quality && f.limited_to.is_none_or(|cap| cap >= limit)
            }) {
                continue;
            }
            let mut budget = available(used).min(limit);
            // Reclaim oversized neighbors before reducing the lead's resolution.
            if next == page && budget < limit {
                let mut recovered = 0;
                r.frames.retain(|index, f| {
                    let n = pixels(f);
                    if *index != page
                        && ((!on_screen(*index) && n > PREVIEW_PIXELS) || n > FIT_PIXELS)
                    {
                        recovered += n;
                        false
                    } else {
                        true
                    }
                });
                used -= recovered;
                budget = available(used).min(limit);
            }
            if budget > 0
                && !r.frames.get(&next).is_some_and(|f| {
                    f.quality == quality && f.limited_to.is_none_or(|cap| budget <= cap + cap / 4)
                })
            {
                return Some((next, Some(budget)));
            }
        }
        None
    }

    pub fn surface_message(&self, reading: bool) -> Option<String> {
        let pending = || "Loading page…".to_owned();
        if reading {
            let Some(r) = &self.reading else {
                return Some(pending());
            };
            for page in r.layout.visible() {
                match r.frames.get(&page.page).map(|f| &f.image) {
                    Some(Ok(_)) => {}
                    Some(Err(error)) => return Some(error.clone()),
                    None => return Some(pending()),
                }
            }
            None
        } else {
            self.bitmap.is_none().then(|| {
                if self.single_quality.is_some() {
                    self.text.clone()
                } else {
                    pending()
                }
            })
        }
    }

    pub fn finish_render(
        &mut self,
        page: u32,
        quality: Quality,
        reading: bool,
        result: Result<Frame, String>,
    ) -> bool {
        if reading != self.reading.is_some() {
            return false;
        }
        if let Some(r) = &mut self.reading {
            if !r.layout.wanted().contains(&page) {
                return false;
            }
            let before = r.layout.visible();
            if let Ok(frame) = &result {
                r.layout.measured(page, frame.ratio);
            }
            let limited_to = result.as_ref().ok().and_then(|f| f.limited_to);
            let image = result.map(|frame| frame.bitmap);
            r.frames.insert(
                page,
                CachedPage {
                    quality,
                    limited_to,
                    image,
                },
            );
            return before.iter().any(|p| p.page == page) || before != r.layout.visible();
        } else if self.page == page {
            self.single_quality = Some(quality);
            match result {
                Ok(frame) => {
                    self.single_ratio = Some(frame.ratio);
                    self.bitmap = Some(frame.bitmap);
                }
                Err(e) => {
                    self.bitmap = None;
                    self.text = e;
                }
            }
            return true;
        }
        false
    }

    pub fn set_tool(&mut self, tool: Option<MarkStyle>) {
        if self.tool != tool {
            self.cancel_drag();
            self.tool = tool;
        }
    }

    pub fn set_signature(&mut self, draw: bool, area: Option<UnitRect>) {
        if self.draw_signature != draw || self.signature != area {
            self.cancel_drag();
            self.draw_signature = draw;
            self.signature = area;
        }
    }

    pub fn take_signature(&mut self) -> Option<(u32, UnitRect)> {
        self.placed_signature.take()
    }

    pub fn set_marks(&mut self, marks: &[Mark], page: u32) {
        self.page = page;
        self.marks.clear();
        self.marks.extend_from_slice(marks);
    }

    pub fn set_text_layout(&mut self, layout: Option<TextLayout>) {
        if matches!(self.drag, Some(Drag::Highlight { glyph: Some(_), .. })) {
            self.cancel_drag();
        }
        self.text_layout = layout;
    }

    pub(super) fn enable_text_selection(&mut self, hwnd: HWND, enabled: bool) {
        if self.text_selection_enabled && !enabled {
            self.clear_text_selection(hwnd);
            self.selection_layout = None;
        }
        self.text_selection_enabled = enabled;
    }

    pub(super) fn clear_text_selection(&mut self, hwnd: HWND) {
        if matches!(self.drag, Some(Drag::Text(_))) {
            self.cancel_drag();
        }
        if self.selection.take().is_some() {
            redraw(hwnd);
        }
    }

    pub(super) fn has_selection(&self) -> bool {
        self.drag.is_none() && self.selection.is_some()
    }

    pub(super) fn selection_active(&self) -> bool {
        self.selection.is_some() || matches!(self.drag, Some(Drag::Text(_)))
    }

    pub(super) fn selection_allows_annotations(&self) -> bool {
        self.selection.as_ref().is_some_and(|selection| {
            self.selection_layout
                .as_ref()
                .is_some_and(|(page, layout)| *page == selection.page && layout.allows_annotations)
        })
    }

    pub(super) fn take_selection(&mut self, style: MarkStyle) -> Option<Mark> {
        if !self.has_selection() || !self.selection_allows_annotations() {
            return None;
        }
        let Selection { page, runs } = self.selection.take()?;
        Some(Mark { page, style, runs })
    }

    pub(super) fn pending_text_page(&self) -> Option<u32> {
        match self.drag {
            Some(Drag::Text(TextDrag {
                page,
                phase: TextDragPhase::Loading { .. },
                ..
            })) => Some(page),
            _ => None,
        }
    }

    pub(super) fn finish_reading_text(
        &mut self,
        hwnd: HWND,
        page: u32,
        layout: TextLayout,
    ) -> Option<(i32, i32)> {
        let Some(Drag::Text(mut drag)) = self.drag else {
            return None;
        };
        let TextDragPhase::Loading { released } = drag.phase else {
            return None;
        };
        if drag.page != page {
            return None;
        }
        let (x, y) = drag.area.normalized(drag.origin);
        let glyph = layout.hit(x, y);
        self.selection_layout = Some((page, layout));
        if let Some(glyph) = glyph {
            self.update_selection(hwnd, drag, glyph);
            drag.phase = TextDragPhase::Selecting(glyph);
            self.drag = (!released).then_some(Drag::Text(drag));
            None
        } else {
            self.drag = (!released).then_some(Drag::Pan(drag.point));
            Some(drag.delta())
        }
    }

    pub fn take_new_mark(&mut self) -> Option<Mark> {
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
        let had_signature_preview = self.signature_preview.take().is_some();
        if !self.preview.is_empty() || had_signature_preview {
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
        let ratio = self
            .single_ratio
            .unwrap_or_else(|| f64::from(image.width) / f64::from(image.height));
        let (width, height) = fitted_page_size((w, h), ratio, self.zoom);
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
        if self.selection.is_some() {
            self.clear_text_selection(hwnd);
            super::action(unsafe { GetParent(hwnd) }, super::SELECTION_CHANGED);
        }
        let page = self.layout(hwnd);
        self.drag = if let Some(reading) = &self.reading {
            Some(self.start_reading_drag(reading, point))
        } else if self.draw_signature && page.contains(point) {
            Some(Drag::Signature(page.normalized(point)))
        } else if self.tool.is_some() && page.contains(point) {
            let origin = page.normalized(point);
            let glyph = self
                .text_layout
                .as_ref()
                .and_then(|text| text.hit(origin.0, origin.1));
            Some(Drag::Highlight { origin, glyph })
        } else if self.tool.is_none() && self.bitmap.is_some() && self.zoom > 1.0 {
            Some(Drag::Pan(point))
        } else {
            None
        };
        self.capture_window = hwnd;
        self.preview.clear();
        self.drag.is_some()
    }

    fn start_reading_drag(&self, reading: &Reading, point: (i32, i32)) -> Drag {
        if !self.text_selection_enabled {
            return Drag::Pan(point);
        }
        let Some((page, area)) = reading.layout.visible().into_iter().find_map(|p| {
            let area = PageArea {
                x: p.x,
                y: p.y,
                w: p.w,
                h: p.h,
            };
            (area.contains(point) && reading.frames.get(&p.page).is_some_and(|f| f.image.is_ok()))
                .then_some((p.page, area))
        }) else {
            return Drag::Pan(point);
        };
        let phase =
            if let Some((_, layout)) = self.selection_layout.as_ref().filter(|(p, _)| *p == page) {
                let (x, y) = area.normalized(point);
                let Some(glyph) = layout.hit(x, y) else {
                    return Drag::Pan(point);
                };
                TextDragPhase::Selecting(glyph)
            } else {
                TextDragPhase::Starting
            };
        Drag::Text(TextDrag {
            page,
            area,
            origin: point,
            point,
            phase,
        })
    }

    fn update_selection(&mut self, hwnd: HWND, drag: TextDrag, glyph: usize) {
        let (_, layout) = self.selection_layout.as_ref().unwrap();
        let (x, y) = drag.area.normalized(drag.point);
        let runs = if drag.moved(dips(hwnd, 3)) {
            layout.range(glyph, x, y)
        } else {
            Vec::new()
        };
        let previous = self
            .selection
            .as_ref()
            .map_or(&[][..], |s| s.runs.as_slice());
        if previous == runs {
            return;
        }
        for run in previous.iter().chain(&runs) {
            unsafe { InvalidateRect(hwnd, &drag.area.pixels(&run.ink(MarkKind::Highlight)), 0) };
        }
        self.selection = (!runs.is_empty()).then_some(Selection {
            page: drag.page,
            runs,
        });
    }

    fn update_preview(&mut self, hwnd: HWND, point: (i32, i32)) {
        if let Some(Drag::Text(mut drag)) = self.drag {
            if matches!(drag.phase, TextDragPhase::Loading { released: true }) {
                return;
            }
            drag.point = point;
            match drag.phase {
                TextDragPhase::Starting if drag.moved(dips(hwnd, 3)) => {
                    drag.phase = TextDragPhase::Loading { released: false };
                    super::action(unsafe { GetParent(hwnd) }, super::LOAD_SELECTION_TEXT);
                }
                TextDragPhase::Selecting(glyph) => self.update_selection(hwnd, drag, glyph),
                _ => {}
            }
            self.drag = Some(Drag::Text(drag));
            return;
        }
        if let Some(Drag::Signature((x0, y0))) = self.drag {
            let page = self.layout(hwnd);
            if page.w <= 0 || page.h <= 0 {
                return;
            }
            let (x, y) = page.normalized(point);
            let minimum = dips(hwnd, 3) as f32;
            let area = ((x - x0).abs() * page.w as f32 >= minimum
                && (y - y0).abs() * page.h as f32 >= minimum)
                .then_some(UnitRect {
                    left: x.min(x0),
                    top: y.min(y0),
                    right: x.max(x0),
                    bottom: y.max(y0),
                });
            if self.signature_preview != area {
                for bounds in [
                    self.signature_preview.or(self.signature),
                    area.or(self.signature),
                ]
                .into_iter()
                .flatten()
                {
                    unsafe { InvalidateRect(hwnd, &page.pixels(&bounds), 0) };
                }
                self.signature_preview = area;
            }
            return;
        }
        let Some(Drag::Highlight { origin, glyph }) = self.drag else {
            return;
        };
        let Some(style) = self.tool else { return };
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
        } else if dx >= threshold && (style.kind == MarkKind::Underline || dy >= threshold) {
            let (top, bottom) = if style.kind == MarkKind::Underline {
                // A horizontal drag on an image needs a nonempty text quad
                // for PDF readers. Place its lower edge at the pointer.
                let bottom = origin.1.max(0.0015);
                ((bottom - 0.015).max(0.0), bottom)
            } else {
                (y.min(origin.1), y.max(origin.1))
            };
            vec![TextRun::horizontal(UnitRect {
                left: x.min(origin.0),
                right: x.max(origin.0),
                top,
                bottom,
            })]
        } else {
            Vec::new()
        };
        if self.preview != new {
            for run in self.preview.iter().chain(&new) {
                unsafe {
                    InvalidateRect(hwnd, &page.pixels(&run.ink(style.kind)), 0);
                }
            }
            self.preview = new;
        }
    }

    fn finish_drag(&mut self, hwnd: HWND, point: (i32, i32)) -> bool {
        self.update_preview(hwnd, point);
        if let Some(Drag::Text(mut drag)) = self.drag.take() {
            // The first selection on a page can finish before text is ready.
            if matches!(drag.phase, TextDragPhase::Loading { .. }) {
                drag.phase = TextDragPhase::Loading { released: true };
                self.drag = Some(Drag::Text(drag));
            }
            super::action(unsafe { GetParent(hwnd) }, super::SELECTION_CHANGED);
            return false;
        }
        if let Some(area) = self.signature_preview.take() {
            self.placed_signature = Some((self.page, area));
            self.signature = Some(area);
            redraw(hwnd);
            return true;
        }
        if self.preview.is_empty() {
            return false;
        }
        let mark = Mark {
            page: self.page,
            style: self.tool.expect("active markup drag"),
            runs: std::mem::take(&mut self.preview),
        };
        self.marks.push(mark.clone());
        self.completed.push_back(mark);
        true
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

fn paint_highlights(dc: HDC, clip: RECT, page: PageArea, index: u32, state: &Canvas) {
    paint_find(dc, clip, page, index, &state.matches);
    let committed = state
        .marks
        .iter()
        .filter(|m| m.page == index)
        .map(|m| (m.style, m.runs.as_slice()));
    let preview = state
        .tool
        .filter(|_| state.reading.is_none())
        .map(|s| (s, state.preview.as_slice()));
    let selection = state
        .selection
        .as_ref()
        .filter(|s| s.page == index)
        .map(|s| {
            (
                MarkStyle {
                    kind: MarkKind::Highlight,
                    color: MarkColor::Blue,
                },
                s.runs.as_slice(),
            )
        });
    let mut visible = committed
        .chain(preview)
        .chain(selection)
        .flat_map(|(style, runs)| runs.iter().map(move |run| (style, run)))
        .filter_map(|(style, run)| {
            let p = page.pixels(&run.ink(style.kind));
            let r = RECT {
                left: p.left.max(clip.left),
                top: p.top.max(clip.top),
                right: p.right.min(clip.right),
                bottom: p.bottom.min(clip.bottom),
            };
            (r.left < r.right && r.top < r.bottom).then_some((r, style))
        });
    let Some(first) = visible.next() else { return };

    // Each palette channel is 0 or 255, so MERGECOPY's bitwise AND is
    // exactly Multiply. AlphaBlend then applies the PDF highlight opacity.
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
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                SourceConstantAlpha: (HIGHLIGHT_OPACITY * 255.0).round() as u8,
                ..Default::default()
            };
            for (r, style) in std::iter::once(first).chain(visible) {
                let [red, green, blue] = style.color.rgb();
                let color = u32::from_le_bytes([red, green, blue, 0]);
                if style.kind == MarkKind::Underline {
                    fill(dc, &r, color);
                    continue;
                }
                SetDCBrushColor(tile, color);
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

fn paint_find(dc: HDC, clip: RECT, page: PageArea, index: u32, matches: &super::find::Matches) {
    let start = matches.hits.partition_point(|hit| hit.page < index);
    let hits = &matches.hits[start..];
    if hits.first().is_none_or(|hit| hit.page != index) {
        return;
    }
    unsafe {
        let saved = SaveDC(dc);
        let brush = GetStockObject(DC_BRUSH) as HBRUSH;
        SelectObject(dc, brush);
        for (i, hit) in hits.iter().take_while(|hit| hit.page == index).enumerate() {
            let active = start + i == matches.active;
            for bounds in &hit.boxes {
                let r = page.pixels(bounds);
                if r.right <= clip.left
                    || r.left >= clip.right
                    || r.bottom <= clip.top
                    || r.top >= clip.bottom
                {
                    continue;
                }
                // Multiply the page pixels by a pale tint, preserving black lettering.
                SetDCBrushColor(dc, if active { 0x0052a9ff } else { 0x0098f1ff });
                BitBlt(
                    dc,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    dc,
                    r.left,
                    r.top,
                    MERGECOPY,
                );
                if active {
                    SetDCBrushColor(dc, 0x002562a9);
                    FrameRect(dc, &r, brush);
                }
            }
        }
        RestoreDC(dc, saved);
    }
}

fn draw_bitmap(dc: HDC, page: PageArea, img: &Bitmap) {
    let PageArea { x, y, w, h } = page;
    unsafe {
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
        if (w, h) != (img.width as i32, img.height as i32) {
            // Preserve fine lines while showing the last frame at a new size.
            SetStretchBltMode(dc, HALFTONE);
            SetBrushOrgEx(dc, x, y, null_mut());
        }
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
    }
}

fn draw_reading(dc: HDC, bounds: RECT, clip: RECT, state: &Canvas, reading: &Reading) {
    let palette = super::theme::palette();
    let pages = reading.layout.visible();
    unsafe {
        let saved = SaveDC(dc);
        for p in &pages {
            ExcludeClipRect(dc, p.x, p.y, p.x + p.w, p.y + p.h);
        }
        fill(dc, &bounds, palette.canvas);
        RestoreDC(dc, saved);
        let brush = GetStockObject(DC_BRUSH) as HBRUSH;
        for p in pages {
            if p.y + p.h < clip.top || p.y > clip.bottom {
                continue;
            }
            let page = PageArea {
                x: p.x,
                y: p.y,
                w: p.w,
                h: p.h,
            };
            let mut paper = RECT {
                left: p.x,
                top: p.y,
                right: p.x + p.w,
                bottom: p.y + p.h,
            };
            let cached = reading.frames.get(&p.page).map(|f| &f.image);
            if let Some(Ok(image)) = cached {
                draw_bitmap(dc, page, image);
                paint_highlights(dc, clip, page, p.page, state);
            } else {
                fill(dc, &paper, 0x00ffffff);
                if let Some(Err(message)) = cached {
                    line(dc, state.font, 0x00505050, paper, message);
                }
            }
            InflateRect(&mut paper, 1, 1);
            SetDCBrushColor(dc, palette.edge);
            FrameRect(dc, &paper, brush);
        }
    }
}

fn draw(hwnd: HWND, dc: HDC, clip: RECT, state: &mut Canvas) {
    let bounds = rect(hwnd);
    let palette = super::theme::palette();
    if let Some(reading) = &state.reading {
        draw_reading(dc, bounds, clip, state, reading);
        return;
    }
    unsafe {
        let page = state.layout(hwnd);
        let PageArea { x, y, w, h } = page;
        if let Some(img) = &state.bitmap {
            let saved = SaveDC(dc);
            ExcludeClipRect(dc, x, y, x + w, y + h);
            fill(dc, &bounds, palette.canvas);
            fill(
                dc,
                &RECT {
                    left: x - 1,
                    top: y - 1,
                    right: x + w + 1,
                    bottom: y + h + 1,
                },
                palette.edge,
            );
            RestoreDC(dc, saved);
            draw_bitmap(dc, page, img);
            paint_highlights(dc, clip, page, state.page, state);
            if let Some(area) = state.signature_preview.or(state.signature) {
                let mut bounds = page.pixels(&area);
                let brush = GetStockObject(DC_BRUSH) as HBRUSH;
                SetDCBrushColor(dc, 0x00ab5419);
                FrameRect(dc, &bounds, brush);
                InflateRect(&mut bounds, -1, -1);
                SetDCBrushColor(dc, 0x00ffffff);
                FrameRect(dc, &bounds, brush);
                InflateRect(&mut bounds, -1, -1);
                SetDCBrushColor(dc, 0x00ab5419);
                FrameRect(dc, &bounds, brush);
                if bounds.right - bounds.left > dips(hwnd, 70)
                    && bounds.bottom - bounds.top > dips(hwnd, 20)
                {
                    line(dc, state.font, 0x00ab5419, bounds, "Signature position");
                }
            }
        } else {
            fill(dc, &bounds, palette.canvas);
            let cy = bounds.bottom / 2;
            let step = dips(hwnd, 37);
            if state.empty {
                line(
                    dc,
                    state.headline,
                    palette.text,
                    RECT {
                        top: cy - step,
                        bottom: cy + 2,
                        ..bounds
                    },
                    "Open a PDF",
                );
                line(
                    dc,
                    state.font,
                    palette.muted,
                    RECT {
                        top: cy + 2,
                        bottom: cy + step,
                        ..bounds
                    },
                    "Drop a file here or press Ctrl+O",
                );
            } else {
                line(dc, state.font, palette.muted, bounds, &state.text);
            }
        }
    }
}

pub unsafe extern "system" fn procedure(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let point = ((l as u16 as i16) as i32, ((l >> 16) as i16) as i32);
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = unsafe { BeginPaint(hwnd, &mut ps) };
            VIEW.with_borrow_mut(|v| draw(hwnd, dc, ps.rcPaint, v));
            unsafe { EndPaint(hwnd, &ps) };
            0
        }
        WM_PRINTCLIENT if w != 0 => {
            VIEW.with_borrow_mut(|v| draw(hwnd, w as HDC, rect(hwnd), v));
            0
        }
        WM_ERASEBKGND => 1,
        WM_SETCURSOR
            if (l as u16) == HTCLIENT as u16
                && VIEW.with_borrow(|v| {
                    matches!(
                        v.drag,
                        Some(Drag::Text(TextDrag {
                            phase: TextDragPhase::Selecting(_),
                            ..
                        }))
                    )
                }) =>
        {
            unsafe { SetCursor(LoadCursorW(null_mut(), IDC_IBEAM)) };
            1
        }
        WM_SETCURSOR
            if (l as u16) == HTCLIENT as u16
                && VIEW.with_borrow(|v| {
                    (v.draw_signature || v.tool.is_some()) && v.bitmap.is_some()
                }) =>
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
                Some(Drag::Highlight { .. } | Drag::Signature(_) | Drag::Text(_)) => {
                    v.update_preview(hwnd, point);
                    None
                }
                None => None,
            });
            if let Some((x, y)) = previous {
                let (dx, dy) = (point.0 - x, point.1 - y);
                if VIEW.with_borrow(|v| v.reading.is_some()) {
                    if scroll_reading(hwnd, f64::from(dx), -f64::from(dy)) {
                        super::action(unsafe { GetParent(hwnd) }, super::SCROLLED);
                    }
                } else {
                    pan(hwnd, dx, dy);
                }
            }
            0
        }
        WM_LBUTTONUP => {
            let marked = VIEW.with_borrow_mut(|v| v.finish_drag(hwnd, point));
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
                // Releasing our own capture is normal after recording the
                // endpoint of a drag while its page's text is still loading.
                if msg == WM_CAPTURECHANGED
                    && matches!(
                        v.drag,
                        Some(Drag::Text(TextDrag {
                            phase: TextDragPhase::Loading { released: true },
                            ..
                        }))
                    )
                {
                    return false;
                }
                let selected =
                    matches!(v.drag.take(), Some(Drag::Text(_))) && v.selection.take().is_some();
                let visible =
                    v.signature_preview.take().is_some() || !v.preview.is_empty() || selected;
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
    fn high_zoom_gives_the_reading_position_more_pixels_than_its_prefetch() {
        let area = (600, 440);
        let mut layout = Layout::new(3, Position::at(1));
        layout.configure(area, 18, 18, 4.0);
        let mut view = Canvas {
            zoom: 4.0,
            reading: Some(Reading {
                layout,
                frames: BTreeMap::new(),
            }),
            ..Default::default()
        };
        assert_eq!(view.next_render(1, area), Some((1, Some(32 * 1024 * 1024))));
        view.reading.as_mut().unwrap().frames.insert(
            1,
            CachedPage {
                quality: (area, view.zoom.to_bits()),
                limited_to: None,
                image: Ok(Bitmap {
                    width: 1,
                    height: 1,
                    pixels: vec![255; 4],
                }),
            },
        );
        assert_eq!(view.next_render(1, area), Some((2, Some(1024 * 1024))));
        view.zoom = 1.0;
        view.reading
            .as_mut()
            .unwrap()
            .layout
            .configure(area, 18, 18, 1.0);
        assert_eq!(view.next_render(1, area), Some((1, Some(8 * 1024 * 1024))));
    }

    #[test]
    fn a_drag_released_before_text_load_can_be_highlighted_on_its_page() {
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                wide("STATIC").as_ptr(),
                null(),
                WS_POPUP,
                0,
                0,
                636,
                476,
                null_mut(),
                null_mut(),
                null_mut(),
                null(),
            )
        };
        assert!(!hwnd.is_null());
        let mut layout = Layout::new(2, Position::at(1));
        layout.configure((600, 440), 18, 18, 1.0);
        layout.measured(1, 300.0 / 220.0);
        let page = layout.visible().into_iter().find(|p| p.page == 1).unwrap();
        let mut state = Canvas {
            reading: Some(Reading {
                layout,
                frames: BTreeMap::from([(
                    1,
                    CachedPage {
                        quality: ((600, 440), 1.0_f64.to_bits()),
                        limited_to: None,
                        image: Ok(Bitmap {
                            width: 1,
                            height: 1,
                            pixels: vec![255; 4],
                        }),
                    },
                )]),
            }),
            text_selection_enabled: true,
            ..Default::default()
        };
        let text =
            crate::textselect::TextDocument::selection(&crate::highlight_roundtrip::fixture(), 0)
                .unwrap();
        assert!(state.start_drag(hwnd, (page.x + 172, page.y + 122)));
        assert!(!state.finish_drag(hwnd, (page.x + 182, page.y + 182)));
        assert!(!state.has_selection());
        assert!(state.finish_reading_text(hwnd, 1, text).is_none());
        assert!(state.has_selection());
        assert!(state.marks.is_empty());
        let mark = state.take_selection(MarkStyle::default()).unwrap();
        assert_eq!(mark.page, 1);
        assert_eq!(mark.runs.len(), 2);
        assert!(!state.has_selection());

        // A first drag on a page without any text still pans by the full drag.
        state.selection_layout = None;
        let from = (page.x + 5, page.y + 12);
        assert!(state.start_drag(hwnd, from));
        assert!(!state.finish_drag(hwnd, (from.0 + 8, from.1 + 24)));
        assert_eq!(
            state.finish_reading_text(hwnd, 1, TextLayout::default()),
            Some((8, 24))
        );
        assert!(state.drag.is_none() && !state.has_selection());
        unsafe { DestroyWindow(hwnd) };
    }

    #[test]
    fn continuous_paint_shows_both_page_surfaces_and_their_gutter() {
        let mut layout = Layout::new(2, Position::at(0));
        layout.configure((64, 80), 8, 8, 1.0);
        layout.measured(0, 0.8);
        layout.measured(1, 0.8);
        layout.scroll(0.0, 40.0);
        let surface = |bgra: [u8; 4]| CachedPage {
            quality: ((64, 80), 1.0f64.to_bits()),
            limited_to: None,
            image: Ok(Bitmap {
                width: 1,
                height: 1,
                pixels: bgra.to_vec(),
            }),
        };
        let reading = Reading {
            layout,
            frames: BTreeMap::from([
                (0, surface([0, 0, 255, 255])),
                (1, surface([255, 0, 0, 255])),
            ]),
        };
        let state = Canvas::default();
        let bounds = RECT {
            left: 0,
            top: 0,
            right: 80,
            bottom: 96,
        };
        unsafe {
            let dc = CreateCompatibleDC(null_mut());
            assert!(!dc.is_null());
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: bounds.right,
                    biHeight: -bounds.bottom,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut data = null_mut();
            let dib = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut data, null_mut(), 0);
            assert!(!dib.is_null());
            let original = SelectObject(dc, dib);
            draw_reading(dc, bounds, bounds, &state, &reading);
            let samples = [20, 52, 72].map(|y| GetPixel(dc, 40, y));
            SelectObject(dc, original);
            DeleteObject(dib);
            DeleteDC(dc);
            assert_eq!(
                samples,
                [0xff, super::super::theme::palette().canvas, 0xff0000]
            );
        }
    }

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
    fn colored_markers_preserve_text_and_underlines_only_paint_the_lower_edge() {
        let bounds = UnitRect {
            left: 0.25,
            top: 0.25,
            right: 0.75,
            bottom: 0.75,
        };
        let state = Canvas {
            marks: vec![
                Mark {
                    page: 0,
                    style: MarkStyle::default(),
                    runs: vec![TextRun::horizontal(bounds)],
                },
                Mark {
                    page: 0,
                    style: MarkStyle {
                        kind: MarkKind::Highlight,
                        color: MarkColor::Red,
                    },
                    runs: vec![TextRun::horizontal(UnitRect {
                        left: 0.,
                        top: 0.,
                        right: 0.25,
                        bottom: 0.75,
                    })],
                },
            ],
            tool: Some(MarkStyle {
                kind: MarkKind::Underline,
                color: MarkColor::Blue,
            }),
            preview: vec![TextRun::horizontal(bounds)],
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
                0,
                &state,
            );
            let paper = GetPixel(dc, 2, 2);
            let ink = GetPixel(dc, 3, 3);
            let outside = GetPixel(dc, 7, 0);
            let underline = GetPixel(dc, 3, 5);
            let red_marker = GetPixel(dc, 0, 3);
            fill(dc, &bounds, 0xffffff);
            SetPixelV(dc, 1, 1, 0);
            let hit = |left, right| super::super::find::Hit {
                page: 2,
                boxes: vec![UnitRect {
                    left,
                    right,
                    top: 0.0,
                    bottom: 1.0,
                }],
            };
            let matches = super::super::find::Matches {
                hits: vec![hit(0.0, 0.5), hit(0.5, 1.0)],
                active: 1,
            };
            let page = PageArea {
                x: 0,
                y: 0,
                w: 8,
                h: 8,
            };
            paint_find(dc, bounds, page, 1, &matches);
            let other_page = GetPixel(dc, 2, 3);
            paint_find(dc, bounds, page, 2, &matches);
            let search_colors = [
                GetPixel(dc, 1, 1),
                GetPixel(dc, 2, 3),
                GetPixel(dc, 6, 3),
                GetPixel(dc, 7, 3),
            ];
            SelectObject(dc, original);
            DeleteObject(bitmap);
            DeleteDC(dc);
            assert_eq!(outside, 0x00ffffff);
            assert_eq!(other_page, 0xffffff);
            assert_eq!(search_colors, [0, 0x98f1ff, 0x52a9ff, 0x2562a9]);
            assert_eq!(ink, 0);
            assert_eq!(underline, 0x00ff0000);
            assert_eq!(red_marker & 0xff, 0xff);
            assert!((139..=141).contains(&(red_marker >> 16)));
            assert_eq!(red_marker >> 16, (red_marker >> 8) & 0xff);
            assert_eq!(paper & 0xffff, 0xffff);
            assert!(
                (139..=141).contains(&(paper >> 16)),
                "COLORREF={paper:#08x}"
            );
        }
    }
}
