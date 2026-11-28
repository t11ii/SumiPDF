use crate::preferences::TabSession;
use crate::textselect::{TextDocument, TextLayout};
use crate::winpdf::{self, Bitmap, Document, Frame, SigningSource, Smoothing};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

use super::READY;

pub enum Job {
    Open {
        revision: u64,
        path: PathBuf,
    },
    Restore {
        revision: u64,
        source: SigningSource,
    },
    Close,
    Render {
        revision: u64,
        ticket: u64,
        page: u32,
        area: (i32, i32),
        zoom: f64,
        cache_pixels: Option<u64>,
    },
}

pub struct OpenedDocument {
    pub pages: u32,
    pub source: Result<SigningSource, String>,
}

pub(super) struct SmoothingRequest {
    pub revision: u64,
    pub page: u32,
    pub quality: super::canvas::Quality,
    pub smoothing: Smoothing,
}

pub enum Event {
    PreviousTabs(Result<TabSession, String>),
    Find(u64, Result<super::find::Page, String>),
    Opened(u64, Result<OpenedDocument, String>),
    Painted(u64, bool, Result<Frame, String>),
    Smoothed(SmoothingRequest, Option<Bitmap>),
    SignatureFields {
        revision: u64,
        result: Result<super::signing::Choices, String>,
    },
    Saved {
        review_page: Option<u32>,
        result: Result<PathBuf, String>,
    },
    Layout {
        revision: u64,
        page: u32,
        purpose: TextPurpose,
        result: Result<TextLayout, String>,
    },
}

#[derive(Clone, Copy)]
pub enum TextPurpose {
    Markup,
    Selection,
}

pub enum TextJob {
    Release,
    Page {
        revision: u64,
        page: u32,
        purpose: TextPurpose,
        source: SigningSource,
    },
}

// Parse text on demand. The markup tool can reuse the document while active.
// Reading selections keep only their current page's layout on the UI thread.
pub fn text_pages(hwnd: usize, jobs: Receiver<TextJob>, events: Sender<Event>) {
    let mut cache: Option<(u64, TextDocument)> = None;
    while let Ok(first) = jobs.recv() {
        let TextJob::Page {
            revision,
            page,
            purpose,
            source,
        } = jobs.try_iter().last().unwrap_or(first)
        else {
            cache = None;
            continue;
        };
        let result = if matches!(purpose, TextPurpose::Selection) {
            cache = None;
            source
                .read_bytes()
                .and_then(|bytes| TextDocument::selection(&bytes, page))
        } else {
            (|| {
                if cache.as_ref().is_none_or(|(id, _)| *id != revision) {
                    cache = None;
                    let document = TextDocument::load(&source.read_bytes()?)?;
                    cache = Some((revision, document));
                }
                cache.as_ref().unwrap().1.page(page)
            })()
        };
        report(
            hwnd,
            &events,
            Event::Layout {
                revision,
                page,
                purpose,
                result,
            },
        );
    }
}

pub fn report(hwnd: usize, events: &Sender<Event>, event: Event) {
    if events.send(event).is_ok() {
        unsafe { PostMessageW(hwnd as _, READY, 0, 0) };
    }
}

pub(super) fn spawn(hwnd: usize, events: Sender<Event>) -> Result<Sender<Job>, String> {
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name("pdf-render".into())
        .spawn(move || run(hwnd, rx, events))
        .map_err(|e| e.to_string())?;
    Ok(tx)
}

pub(super) fn smooth(
    hwnd: usize,
    events: Sender<Event>,
    source: SigningSource,
    request: SmoothingRequest,
) -> std::io::Result<()> {
    // Use an independent page stream so optional work cannot hold up the open/render queue.
    std::thread::Builder::new()
        .name("pdf-smooth".into())
        .spawn(move || {
            let image = (|| {
                let _apartment = winpdf::initialize().ok()?;
                let doc = Document::from_source(&source).ok()?;
                doc.smooth_page(request.page, request.smoothing).ok()
            })();
            report(hwnd, &events, Event::Smoothed(request, image));
        })?;
    Ok(())
}

fn run(hwnd: usize, jobs: Receiver<Job>, events: Sender<Event>) {
    // Delay WinRT activation and loading its DLLs until a PDF is actually opened.
    let apartment = winpdf::initialize();
    let mut active: Option<(u64, Document)> = None;
    while let Ok(first) = jobs.recv() {
        // A newer activation supersedes queued tab switches. The UI sends a
        // render only after the current revision has successfully opened.
        let job = jobs.try_iter().last().unwrap_or(first);
        match job {
            Job::Close => active = None,
            Job::Open { revision, .. } | Job::Restore { revision, .. } => {
                active = None;
                let opened = (|| {
                    apartment.as_ref().map_err(Clone::clone)?;
                    let (doc, source) = match job {
                        Job::Open { path, .. } => {
                            let doc = Document::open_file(&path)?;
                            let source = doc.signing_source();
                            (doc, source)
                        }
                        Job::Restore { source, .. } => {
                            (Document::from_source(&source)?, Ok(source))
                        }
                        _ => unreachable!(),
                    };
                    let opened = OpenedDocument {
                        pages: doc.page_count(),
                        source,
                    };
                    active = Some((revision, doc));
                    Ok(opened)
                })();
                report(hwnd, &events, Event::Opened(revision, opened));
            }
            Job::Render {
                revision,
                ticket,
                page,
                area,
                zoom,
                cache_pixels,
            } => {
                let Some((current, doc)) = &active else {
                    continue;
                };
                if *current != revision {
                    continue;
                }
                let result = doc.render_page(page, area, zoom, cache_pixels);
                report(
                    hwnd,
                    &events,
                    Event::Painted(ticket, cache_pixels.is_some(), result),
                );
            }
        }
    }
}
