use crate::textselect::{TextDocument, TextLayout};
use crate::winpdf::{self, Bitmap, Document, SigningSource};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

use super::READY;

pub enum Job {
    Open {
        revision: u64,
        path: PathBuf,
    },
    Render {
        revision: u64,
        ticket: u64,
        page: u32,
        area: (i32, i32),
        zoom: f64,
    },
}

pub struct OpenedDocument {
    pub pages: u32,
    pub source: Result<SigningSource, String>,
}

pub enum Event {
    Opened(u64, Result<OpenedDocument, String>),
    Painted(u64, Result<Bitmap, String>),
    Saved {
        signed: bool,
        result: Result<PathBuf, String>,
    },
    Layout {
        revision: u64,
        page: u32,
        result: Result<TextLayout, String>,
    },
}

pub enum TextJob {
    Release,
    Page {
        revision: u64,
        page: u32,
        source: SigningSource,
    },
}

// Parsing text must never block page painting. Only the highlight tool starts
// this worker. Release the parsed document when the user turns the tool off.
pub fn text_pages(hwnd: usize, jobs: Receiver<TextJob>, events: Sender<Event>) {
    let mut cache: Option<(u64, TextDocument)> = None;
    while let Ok(first) = jobs.recv() {
        let TextJob::Page {
            revision,
            page,
            source,
        } = jobs.try_iter().last().unwrap_or(first)
        else {
            cache = None;
            continue;
        };
        let result = (|| {
            if cache.as_ref().is_none_or(|(id, _)| *id != revision) {
                cache = None;
                let document = TextDocument::load(&source.read_bytes()?)?;
                cache = Some((revision, document));
            }
            cache.as_ref().unwrap().1.page(page)
        })();
        report(
            hwnd,
            &events,
            Event::Layout {
                revision,
                page,
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

pub fn run(hwnd: usize, jobs: Receiver<Job>, events: Sender<Event>) {
    // Delay WinRT activation and loading its DLLs until a PDF is actually opened.
    let apartment = winpdf::initialize();
    let mut active: Option<(u64, Document)> = None;
    while let Ok(first) = jobs.recv() {
        // The UI requests renders only after Opened. During fast navigation or
        // resizing the most recent request supersedes all pending work.
        let job = jobs.try_iter().last().unwrap_or(first);
        match job {
            Job::Open { revision, path } => {
                active = None;
                let opened = (|| {
                    apartment.as_ref().map_err(Clone::clone)?;
                    let doc = Document::open_file(&path)?;
                    let opened = OpenedDocument {
                        pages: doc.page_count(),
                        source: doc.signing_source(),
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
            } => {
                let Some((current, doc)) = &active else {
                    continue;
                };
                if *current != revision {
                    continue;
                }
                let result = doc.render_fit(page, area, zoom);
                report(hwnd, &events, Event::Painted(ticket, result));
            }
        }
    }
}
