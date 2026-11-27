//! Modeless document search. The UI acknowledges each scanned page before the
//! worker proceeds, so results cannot accumulate while a native dialog is open.
use super::*;
use crate::{
    highlight::UnitRect,
    textselect::{SearchPageCache, TextDocument},
    winpdf::SigningSource,
};
use std::{
    sync::{Arc, Condvar, Mutex},
    thread::JoinHandle,
    time::{Duration, Instant},
};
use windows_sys::Win32::UI::Controls::{EM_SETLIMITTEXT, EM_SETSEL};

pub(super) const OPEN_FIND: usize = 160;
pub(super) const EDIT: usize = 161;
pub(super) const PREVIOUS: usize = 162;
pub(super) const NEXT: usize = 163;
pub(super) const CLOSE: usize = 164;
const STATUS: usize = 165;
const LABEL: usize = 166;
const COLD_DELAY: Duration = Duration::from_millis(160);
const MAX_QUERY: usize = 256;
const MAX_HITS: usize = 5_000;
const MAX_RECTS: usize = 32_768;
const SS_CENTERIMAGE: u32 = 0x0200;
const SS_ENDELLIPSIS: u32 = 0x4000;

pub(super) struct Bar {
    pub(super) button: HWND,
    pub(super) edit: HWND,
    status: HWND,
    controls: [HWND; 6],
}

impl Bar {
    pub(super) fn new(hwnd: HWND, toolbar: &mut toolbar::Toolbar) -> Result<Self, String> {
        let button = toolbar.button(OPEN_FIND, toolbar::FIND)?;
        let label = child(hwnd, "STATIC", "&Find", LABEL, SS_CENTERIMAGE)?;
        let edit = child(
            hwnd,
            "EDIT",
            "",
            EDIT,
            WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL as u32,
        )?;
        let prev = toolbar.button(PREVIOUS, toolbar::PREVIOUS_MATCH)?;
        let next = toolbar.button(NEXT, toolbar::NEXT_MATCH)?;
        let close = toolbar.button(CLOSE, toolbar::CLOSE_FIND)?;
        let status = child(hwnd, "STATIC", "", STATUS, SS_CENTERIMAGE | SS_ENDELLIPSIS)?;
        theme::style_control(edit)?;
        let bar = Self {
            button,
            edit,
            status,
            controls: [label, edit, prev, next, close, status],
        };
        unsafe {
            SendMessageW(edit, EM_SETLIMITTEXT, MAX_QUERY, 0);
        }
        bar.show(false);
        Ok(bar)
    }

    fn show(&self, visible: bool) {
        for hwnd in self.controls {
            unsafe {
                ShowWindow(hwnd, if visible { SW_SHOW } else { SW_HIDE });
            }
        }
    }

    pub(super) fn font(&self, font: HFONT) {
        for hwnd in [self.controls[0], self.edit, self.status] {
            unsafe {
                SendMessageW(hwnd, WM_SETFONT, font as usize, 1);
            }
        }
    }

    pub(super) fn layout(
        hwnd: HWND,
        top: i32,
        place: &mut impl FnMut(HWND, i32, i32, i32, i32),
    ) -> i32 {
        let control = |id| unsafe { GetDlgItem(hwnd, id as i32) };
        if (unsafe { GetWindowLongW(control(EDIT), GWL_STYLE) } as u32 & WS_VISIBLE) == 0 {
            return 0;
        }
        let s = |n| dips(hwnd, n);
        let row = s(top + 3);
        let width = rect(hwnd).right;
        let edit_width = ((width - s(220)) / 2).clamp(s(82), s(300));
        let buttons = s(48) + edit_width;
        place(control(LABEL), s(10), row, s(34), s(26));
        place(control(EDIT), s(44), row, edit_width, s(26));
        place(control(PREVIOUS), buttons, row, s(32), s(26));
        place(control(NEXT), buttons + s(35), row, s(32), s(26));
        place(control(CLOSE), buttons + s(70), row, s(32), s(26));
        let status_x = buttons + s(109);
        place(
            control(STATUS),
            status_x,
            row,
            (width - status_x - s(4)).max(1),
            s(26),
        );
        35
    }

    fn query(&self) -> String {
        let mut text = [0; MAX_QUERY + 1];
        let n = unsafe { GetWindowTextW(self.edit, text.as_mut_ptr(), text.len() as i32) };
        String::from_utf16_lossy(&text[..n as usize])
    }
}

#[derive(Default)]
pub(super) struct Matches {
    pub(super) hits: Vec<Hit>,
    pub(super) active: usize,
}
pub(super) struct Hit {
    pub(super) page: u32,
    pub(super) boxes: Vec<UnitRect>,
}

#[derive(Default)]
pub(super) struct State {
    pub(super) open: bool,
    epoch: u64,
    query: String,
    scanning: bool,
    limited: bool,
    skipped: usize,
    pending_step: Option<PendingStep>,
    worker: Option<SearchWorker>,
}

enum PendingStep {
    Next,
    Last,
}

#[derive(Default)]
struct Pending {
    job: Option<Job>,
    // Closing Find must release its data even when another job replaces Release.
    release_cache: bool,
}

#[derive(Default)]
struct Mailbox {
    pending: Mutex<Pending>,
    changed: Condvar,
}

impl Mailbox {
    fn post(&self, job: Job) {
        let mut pending = self.pending.lock().unwrap();
        pending.release_cache |= matches!(job, Job::Release);
        pending.job = Some(job);
        self.changed.notify_one();
    }

    fn receive(&self) -> (Job, bool) {
        let pending = self.pending.lock().unwrap();
        let mut ready = self
            .changed
            .wait_while(pending, |p| p.job.is_none())
            .unwrap();
        (
            ready.job.take().unwrap(),
            std::mem::take(&mut ready.release_cache),
        )
    }

    // A newer edit interrupts the debounce. Never hold this lock while decoding
    // or matching a page. A warm page has no deadline and can be read immediately.
    fn superseded(&self, defer_until: Option<Instant>) -> bool {
        let mut pending = self.pending.lock().unwrap();
        if let Some(delay) = defer_until.and_then(|t| t.checked_duration_since(Instant::now())) {
            pending = self
                .changed
                .wait_timeout_while(pending, delay, |p| p.job.is_none())
                .unwrap()
                .0;
        }
        pending.job.is_some()
    }
}

struct SearchWorker {
    mailbox: Arc<Mailbox>,
    thread: JoinHandle<()>,
}

impl SearchWorker {
    fn new(hwnd: usize, events: Sender<Event>) -> Result<Self, String> {
        let mailbox = Arc::new(Mailbox::default());
        let reader = Arc::clone(&mailbox);
        let thread = std::thread::Builder::new()
            .name("pdf-find".into())
            .spawn(move || run(hwnd, &reader, events))
            .map_err(|e| e.to_string())?;
        Ok(Self { mailbox, thread })
    }

    fn send(&self, job: Job) -> Result<(), String> {
        if self.thread.is_finished() {
            return Err("Search service stopped.".into());
        }
        self.mailbox.post(job);
        Ok(())
    }
}

impl Drop for SearchWorker {
    fn drop(&mut self) {
        self.mailbox.post(Job::Stop);
    }
}

enum Job {
    Start {
        epoch: u64,
        revision: u64,
        source: SigningSource,
        query: String,
        cold_after: Instant,
    },
    Next(u64),
    Cancel,
    Release,
    Stop,
}

pub(super) struct Page {
    index: u32,
    hits: Vec<Vec<UnitRect>>,
    skipped: bool,
    last: bool,
    limited: bool,
}

struct Scan {
    epoch: u64,
    query: String,
    page: u32,
    hits: usize,
    boxes: usize,
    cold_after: Instant,
}

struct SearchDocument {
    revision: u64,
    doc: TextDocument,
    pages: SearchPageCache,
}

impl Scan {
    fn next(&mut self, document: &mut SearchDocument) -> Page {
        let result = document.pages.search(&document.doc, self.page, &self.query);
        let skipped = result.is_err();
        let mut hits = Vec::new();
        let mut limited = false;
        for rects in result.unwrap_or_default() {
            if rects.is_empty() {
                continue;
            }
            if self.hits == MAX_HITS || rects.len() > MAX_RECTS - self.boxes {
                limited = true;
                break;
            }
            self.hits += 1;
            self.boxes += rects.len();
            hits.push(rects);
        }
        let index = self.page;
        self.page += 1;
        Page {
            index,
            hits,
            skipped,
            last: limited || self.page as usize >= document.doc.page_count(),
            limited,
        }
    }
}

fn run(hwnd: usize, mailbox: &Mailbox, events: Sender<Event>) {
    let mut cache: Option<SearchDocument> = None;
    let mut scan: Option<Scan> = None;
    loop {
        let (job, release) = mailbox.receive();
        if release {
            cache = None;
        }
        match job {
            Job::Stop => return,
            Job::Release | Job::Cancel => {
                scan = None;
                continue;
            }
            Job::Start {
                epoch,
                revision,
                source,
                query,
                cold_after,
            } => {
                if cache.as_ref().is_none_or(|d| d.revision != revision) {
                    cache = None;
                    if mailbox.superseded(Some(cold_after)) {
                        continue;
                    }
                    match source
                        .read_for_search()
                        .and_then(|bytes| TextDocument::load_for_search(&bytes))
                    {
                        Ok(doc) => {
                            cache = Some(SearchDocument {
                                revision,
                                doc,
                                pages: SearchPageCache::default(),
                            })
                        }
                        Err(error) => {
                            scan = None;
                            worker::report(hwnd, &events, Event::Find(epoch, Err(error)));
                            continue;
                        }
                    }
                }
                scan = Some(Scan {
                    epoch,
                    query,
                    page: 0,
                    hits: 0,
                    boxes: 0,
                    cold_after,
                });
            }
            Job::Next(epoch) if scan.as_ref().is_some_and(|s| s.epoch == epoch) => {}
            Job::Next(_) => continue,
        }
        if let (Some(document), Some(current)) = (&mut cache, &mut scan) {
            let deadline = (!document.pages.contains(current.page)).then_some(current.cold_after);
            if mailbox.superseded(deadline) {
                continue;
            }
            let page = current.next(document);
            let last = page.last;
            worker::report(hwnd, &events, Event::Find(current.epoch, Ok(page)));
            if last {
                scan = None;
            }
        }
    }
}

impl App {
    pub(super) fn open_find(&mut self) {
        if self.page_count() == 0 {
            return;
        }
        self.jump_input(false);
        if !self.find.open {
            self.find.open = true;
            self.ui.find.show(true);
            Controls::layout(self.hwnd);
            self.render();
        }
        unsafe {
            SetFocus(self.ui.find.edit);
            SendMessageW(self.ui.find.edit, EM_SETSEL, 0, -1);
        }
    }

    pub(super) fn close_find(&mut self) {
        if !self.find.open {
            return;
        }
        self.find.open = false;
        self.find.epoch += 1;
        self.find.query.clear();
        self.find.scanning = false;
        if let Some(worker) = &self.find.worker {
            let _ = worker.send(Job::Release);
        }
        unsafe {
            if self.ui.find.controls.contains(&GetFocus()) {
                SetFocus(self.ui.canvas);
            }
        }
        self.ui.find.show(false);
        set_text(self.ui.find.edit, "");
        self.clear_find_hits();
        Controls::layout(self.hwnd);
    }

    fn clear_find_hits(&self) {
        VIEW.with_borrow_mut(|v| {
            v.matches = Matches::default();
            v.find_reveal = None;
        });
        set_text(self.ui.find.status, "");
        canvas::redraw(self.ui.canvas);
    }

    pub(super) fn update_find(&mut self) {
        if !self.find.open {
            return;
        }
        let query = self.ui.find.query();
        if self.find.query == query {
            return;
        }
        self.find.epoch += 1;
        self.find.query = query;
        self.find.scanning = !self.find.query.trim().is_empty();
        self.find.limited = false;
        self.find.skipped = 0;
        self.find.pending_step = None;
        self.clear_find_hits();
        if self.find.scanning {
            set_text(self.ui.find.status, "Searching…");
            if let Err(error) = self.start_find() {
                self.find_failed(&error);
            }
        } else if let Some(worker) = &self.find.worker {
            let _ = worker.send(Job::Cancel);
        }
    }

    fn start_find(&mut self) -> Result<(), String> {
        let Some(doc) = &self.document else {
            return Ok(());
        };
        let source = doc.source.as_ref().map_err(Clone::clone)?.clone();
        if self.find.worker.is_none() {
            self.find.worker = Some(SearchWorker::new(self.hwnd as usize, self.events.clone())?);
        }
        self.find.worker.as_ref().unwrap().send(Job::Start {
            epoch: self.find.epoch,
            revision: self.revision,
            source,
            query: self.find.query.clone(),
            cold_after: Instant::now() + COLD_DELAY,
        })
    }

    pub(super) fn find_failed(&mut self, error: &str) {
        self.find.scanning = false;
        set_text(self.ui.find.status, "Search unavailable");
        set_text(self.ui.status, error);
    }

    pub(super) fn find_page(&mut self, epoch: u64, result: Result<Page, String>) {
        if epoch != self.find.epoch || !self.find.open || !self.find.scanning {
            return;
        }
        if self.find.query != self.ui.find.query() {
            self.update_find();
            return;
        }
        let page = match result {
            Ok(page) => page,
            Err(e) => {
                self.find_failed(&e);
                return;
            }
        };
        self.find.skipped += usize::from(page.skipped);
        self.find.limited = page.limited;
        self.find.scanning = !page.last;
        let added_hits = !page.hits.is_empty();
        let visible = VIEW.with_borrow(|v| {
            v.visible_range
                .map_or(self.page == page.index, |(first, last)| {
                    (first..=last).contains(&page.index)
                })
        });
        let reveal = VIEW.with_borrow_mut(|view| {
            let matches = &mut view.matches;
            let before = matches.hits.len();
            matches.hits.extend(page.hits.into_iter().map(|boxes| Hit {
                page: page.index,
                boxes,
            }));
            let added = matches.hits.len() > before;
            match self.find.pending_step {
                Some(PendingStep::Next) if added || page.last => {
                    matches.active = if added { before } else { 0 };
                }
                Some(PendingStep::Last) if page.last => {
                    matches.active = matches.hits.len().saturating_sub(1);
                }
                _ => return added && before == 0,
            }
            self.find.pending_step = None;
            !matches.hits.is_empty()
        });
        if reveal {
            self.reveal_find_match();
        } else if added_hits && visible {
            canvas::redraw(self.ui.canvas);
        }
        if added_hits || page.last {
            self.find_status();
        }
        if !page.last
            && self
                .find
                .worker
                .as_ref()
                .is_none_or(|worker| worker.send(Job::Next(epoch)).is_err())
        {
            self.find_failed("Search service stopped.");
        }
    }

    fn find_status(&self) {
        let (i, n) = VIEW.with_borrow(|v| (v.matches.active + 1, v.matches.hits.len()));
        let mut text = if n > 0 {
            format!("{i} of {n}")
        } else if self.find.scanning {
            "Searching…".into()
        } else {
            "No matches".into()
        };
        if n > 0 && self.find.scanning {
            text.push_str(" …");
        }
        if self.find.limited {
            text.push_str(" (limit reached)");
        }
        if !self.find.scanning && self.find.skipped > 0 {
            text.push_str(&format!(" · {} pages skipped", self.find.skipped));
        }
        set_text(self.ui.find.status, &text);
    }

    pub(super) fn step_find(&mut self, previous: bool) {
        if !self.find.open {
            self.open_find();
            return;
        }
        let advanced = VIEW.with_borrow_mut(|v| {
            let m = &mut v.matches;
            if previous && m.active == 0 && self.find.scanning {
                self.find.pending_step = Some(PendingStep::Last);
                return false;
            }
            if m.hits.is_empty() {
                return false;
            }
            if !previous && m.active + 1 == m.hits.len() && self.find.scanning {
                self.find.pending_step = Some(PendingStep::Next);
                return false;
            }
            self.find.pending_step = None;
            m.active = if previous {
                (m.active + m.hits.len() - 1) % m.hits.len()
            } else {
                (m.active + 1) % m.hits.len()
            };
            true
        });
        if advanced {
            self.reveal_find_match();
            self.find_status();
        }
    }

    fn reveal_find_match(&mut self) {
        self.stop_motion();
        let hit = VIEW.with_borrow(|v| {
            v.matches
                .hits
                .get(v.matches.active)
                .map(|h| (h.page, h.boxes[0]))
        });
        if let Some((page, bounds)) = hit {
            let on_screen = VIEW.with_borrow(|v| {
                v.visible_range.map_or(self.page == page, |(start, end)| {
                    (start..=end).contains(&page)
                })
            });
            if !on_screen {
                self.page_to(page);
            }
            VIEW.with_borrow_mut(|v| {
                v.find_reveal = Some((page, bounds));
                v.reveal_find(self.ui.canvas);
            });
            canvas::redraw(self.ui.canvas);
            self.render();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacing_a_release_still_discards_the_cache_once() {
        let mailbox = Mailbox::default();
        mailbox.post(Job::Release);
        mailbox.post(Job::Cancel);
        assert!(mailbox.superseded(Some(Instant::now() + COLD_DELAY)));
        let (job, release) = mailbox.receive();
        assert!(release && matches!(job, Job::Cancel));
        assert!(!mailbox.superseded(None));

        mailbox.post(Job::Next(3));
        let (job, release) = mailbox.receive();
        assert!(!release && matches!(job, Job::Next(3)));
    }
}
