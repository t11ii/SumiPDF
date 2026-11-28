use crate::preferences::TabSession;
use crate::textselect::{TextDocument, TextLayout};
use crate::winpdf::{Document, Frame, SigningSource};
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

pub enum Event {
    PreviousTabs(Result<TabSession, String>),
    Find(u64, Result<super::find::Page, String>),
    Opened(u64, Result<OpenedDocument, String>),
    Painted(u64, bool, Result<Frame, String>),
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

struct SelectionDocument {
    revision: u64,
    document: TextDocument,
    annotation_check: Option<Result<(), String>>,
}

// Open the same file in a separate MuPDF context only when text is requested.
pub fn text_pages(hwnd: usize, jobs: Receiver<TextJob>, events: Sender<Event>) {
    let mut cache: Option<SelectionDocument> = None;
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
        let result = (|| {
            if cache.as_ref().is_none_or(|doc| doc.revision != revision) {
                cache = Some(SelectionDocument {
                    revision,
                    document: TextDocument::from_source(&source)?,
                    annotation_check: None,
                });
            }
            let cache = cache.as_mut().unwrap();
            let mut layout = cache.document.page(page)?;
            let annotation_check = cache.annotation_check.get_or_insert_with(|| {
                source
                    .read_bytes()
                    .and_then(|bytes| crate::pdfseal::editable_document(&bytes).map(|_| ()))
            });
            if matches!(purpose, TextPurpose::Markup) {
                annotation_check.as_ref().map_err(Clone::clone)?;
            }
            layout.allows_annotations = annotation_check.is_ok();
            Ok(layout)
        })();
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

fn run(hwnd: usize, jobs: Receiver<Job>, events: Sender<Event>) {
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
                    let (doc, source) = match job {
                        Job::Open { path, .. } => {
                            let doc = Document::open_file(&path)?;
                            let source = Ok(doc.signing_source());
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
