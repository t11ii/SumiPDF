use super::{App, Controls, Event, VIEW, canvas, dialog, notice, save, set_text, wide, worker};
use crate::{highlight::UnitRect, pdfseal};
use std::{os::windows::ffi::OsStrExt, path::Path, ptr::null, sync::Arc};
use windows_sys::Win32::{
    Foundation::{HWND, RECT},
    UI::{Input::KeyboardAndMouse::SetFocus, Shell::ShellExecuteW, WindowsAndMessaging::*},
};

pub(super) struct Field {
    id: String,
    name: String,
    page: u32, // zero-based, same as the canvas
    area: UnitRect,
}

fn inspect_fields(bytes: &[u8]) -> Result<Vec<Field>, String> {
    let info = pdfseal::inspect(bytes)?;
    let invalid = || "PDF の署名欄の位置を読み取れませんでした。".to_owned();
    info["fields"]
        .as_array()
        .ok_or_else(invalid)?
        .iter()
        .map(|field| {
            let [left, top, right, bottom]: [f32; 4] =
                serde_json::from_value(field["rect"].clone()).map_err(|_| invalid())?;
            Ok(Field {
                id: field["id"].as_str().ok_or_else(invalid)?.to_string(),
                name: field["name"].as_str().ok_or_else(invalid)?.to_string(),
                page: field["page"]
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .and_then(|n| n.checked_sub(1))
                    .ok_or_else(invalid)?,
                area: UnitRect {
                    left,
                    top,
                    right,
                    bottom,
                },
            })
        })
        .collect()
}

#[derive(Clone, Copy, Default, PartialEq)]
enum Position {
    #[default]
    Draw,
    Field(usize),
    Invisible,
}

struct Watermark {
    name: String,
    bytes: Arc<[u8]>,
}

#[derive(Default)]
pub(super) struct Session {
    pub ready: bool,
    fields: Vec<Field>,
    position: Position,
    pub drawn: Option<(u32, UnitRect)>,
    watermark: Option<Watermark>,
}

impl Session {
    pub fn drawing(&self) -> bool {
        self.ready && self.position == Position::Draw
    }

    pub fn invisible(&self) -> bool {
        self.position == Position::Invisible
    }

    fn placement(&self) -> Option<(u32, UnitRect)> {
        match self.position {
            Position::Draw => self.drawn,
            Position::Field(index) => self.fields.get(index).map(|f| (f.page, f.area)),
            Position::Invisible => None,
        }
    }

    pub fn appearance(&self) -> Option<save::Appearance> {
        let target = match self.position {
            Position::Draw => {
                let (page, r) = self.drawn?;
                pdfseal::Target::Area {
                    page: page + 1,
                    rect: [r.left, r.top, r.right, r.bottom],
                }
            }
            Position::Field(index) => pdfseal::Target::Field(self.fields.get(index)?.id.clone()),
            Position::Invisible => return None,
        };
        Some(save::Appearance {
            target,
            watermark: self.watermark.as_ref().map(|w| w.bytes.clone()),
        })
    }

    pub fn instructions(&self, current_page: u32) -> String {
        if !self.ready {
            return "空の署名欄を確認しています…".into();
        }
        if self.invisible() {
            return "ページに表示しない電子署名を作成します。「署名して保存」で証明書を選択します。".into();
        }
        match self.placement() {
            Some((page, _)) if page != current_page => format!(
                "署名の位置は {} ページです。そのページを開いて確認するか、署名位置を選び直してください。", page + 1
            ),
            Some(_) if self.drawing() =>
                "青枠の位置に署名します。ドラッグで描き直すか、「署名して保存」で証明書を選択してください。".into(),
            Some(_) => "青枠が選択した署名欄です。「署名して保存」で証明書を選択してください。".into(),
            None => "署名を表示するページで、枠の角から反対側の角までドラッグしてください。".into(),
        }
    }
}

impl App {
    pub(super) fn can_sign_here(&self) -> bool {
        self.signing.as_ref().is_some_and(|s| {
            s.ready
                && (s.invisible()
                    || (s.placement().is_some_and(|(p, _)| p == self.page)
                        && VIEW.with_borrow(|v| v.bitmap.is_some())))
        })
    }

    pub(super) fn sync_signature(&self) {
        let session = self.signing.as_ref();
        let draw = !self.writing && session.is_some_and(Session::drawing);
        let area = session
            .and_then(Session::placement)
            .filter(|(page, _)| *page == self.page)
            .map(|(_, r)| r);
        VIEW.with_borrow_mut(|v| v.set_signature(draw, area));
        if session.is_some() {
            canvas::redraw(self.ui.canvas);
        }
    }

    pub(super) fn begin_signing(&mut self) -> Result<(), String> {
        if self.signing.is_some() || self.writing {
            return Ok(());
        }
        let Some(document) = &self.document else {
            return Ok(());
        };
        if self.checking_fields != Some(self.revision) {
            let source = document.source.as_ref().map_err(Clone::clone)?.clone();
            let revision = self.revision;
            let events = self.events.clone();
            let hwnd = self.hwnd as usize;
            std::thread::Builder::new()
                .name("pdf-signature-fields".into())
                .spawn(move || {
                    let result = source.read_bytes().and_then(|bytes| inspect_fields(&bytes));
                    worker::report(hwnd, &events, Event::SignatureFields { revision, result });
                })
                .map_err(|e| e.to_string())?;
            self.checking_fields = Some(revision);
        }
        self.tool = None;
        self.request_text()?;
        self.signing = Some(Session::default());
        self.ui.show_signing(true);
        set_text(self.ui.watermark, "透かし：標準 ▾");
        Controls::layout(self.hwnd);
        self.controls();
        self.status();
        self.render();
        Ok(())
    }

    pub(super) fn end_signing(&mut self) {
        if self.signing.take().is_none() {
            return;
        }
        unsafe {
            SendMessageW(self.ui.position, CB_RESETCONTENT, 0, 0);
        }
        VIEW.with_borrow_mut(|v| v.set_signature(false, None));
        self.ui.show_signing(false);
        Controls::layout(self.hwnd);
        self.controls();
        self.status();
        self.render();
        unsafe {
            SetFocus(self.hwnd);
        }
    }

    pub(super) fn show_fields(&mut self, fields: Vec<Field>) -> Result<(), String> {
        let Some(session) = &mut self.signing else {
            return Ok(());
        };
        let preferred = fields
            .iter()
            .position(|f| f.page == self.page)
            .or_else(|| (!fields.is_empty()).then_some(0));
        let add = |label: &str| -> Result<(), String> {
            let index = unsafe {
                SendMessageW(
                    self.ui.position,
                    CB_ADDSTRING,
                    0,
                    wide(label).as_ptr() as isize,
                )
            };
            if index < 0 {
                Err("署名位置の選択肢を表示できませんでした。".into())
            } else {
                Ok(())
            }
        };
        add("署名位置をドラッグして指定")?;
        for field in &fields {
            add(&format!(
                "{} ページの署名欄：{}",
                field.page + 1,
                field.name
            ))?;
        }
        add("ページに表示しない署名")?;
        session.fields = fields;
        session.ready = true;
        unsafe {
            SendMessageW(
                self.ui.position,
                CB_SETCURSEL,
                preferred.map_or(0, |i| i + 1),
                0,
            );
        }
        self.choose_position();
        unsafe {
            SetFocus(self.ui.position);
        }
        Ok(())
    }

    pub(super) fn choose_position(&mut self) {
        let Some(session) = &mut self.signing else {
            return;
        };
        if !session.ready {
            return;
        }
        let index = unsafe { SendMessageW(self.ui.position, CB_GETCURSEL, 0, 0) };
        if index < 0 {
            return;
        }
        session.position = if index == 0 {
            Position::Draw
        } else if index as usize <= session.fields.len() {
            Position::Field(index as usize - 1)
        } else {
            Position::Invisible
        };
        if let Some((page, _)) = session.placement() {
            self.page_to(page);
            self.scale(1.0);
        }
        self.sync_signature();
        self.controls();
        self.status();
    }

    pub(super) fn choose_watermark(&mut self) -> Result<(), String> {
        let Some(session) = &mut self.signing else {
            return Ok(());
        };
        if !session.ready || session.invisible() {
            return Ok(());
        }
        let name = session.watermark.as_ref().map(|w| w.name.as_str());
        match watermark_menu(self.hwnd, self.ui.watermark, name)? {
            1 => session.watermark = None,
            2 => {
                let Some(path) = dialog::choose(self.hwnd, dialog::Purpose::Watermark)? else {
                    return Ok(());
                };
                session.watermark = Some(load_watermark(&path)?);
            }
            3 => return open_designer(self.hwnd),
            _ => return Ok(()),
        }
        set_text(
            self.ui.watermark,
            if session.watermark.is_some() {
                "透かし：指定画像 ▾"
            } else {
                "透かし：標準 ▾"
            },
        );
        Ok(())
    }
}

fn load_watermark(path: &Path) -> Result<Watermark, String> {
    let bytes = crate::read_watermark(path)?;
    Ok(Watermark {
        name: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into(),
        bytes: bytes.into(),
    })
}

fn watermark_menu(owner: HWND, button: HWND, name: Option<&str>) -> Result<i32, String> {
    let menu = unsafe { CreatePopupMenu() };
    if menu.is_null() {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let custom = match name {
        Some(filename) => format!("PNG 画像を変更…（{}）", filename.replace('&', "&&")),
        None => "PNG 画像を選択…".into(),
    };
    let check = |value: bool| if value { MF_CHECKED } else { MF_UNCHECKED };
    let mut bounds = RECT::default();
    let result = unsafe {
        AppendMenuW(
            menu,
            MF_STRING | check(name.is_none()),
            1,
            wide("標準の透かし（用紙模様）").as_ptr(),
        );
        AppendMenuW(
            menu,
            MF_STRING | check(name.is_some()),
            2,
            wide(&custom).as_ptr(),
        );
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        AppendMenuW(menu, MF_STRING, 3, wide("透かし画像を作成…").as_ptr());
        GetWindowRect(button, &mut bounds);
        let choice = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY,
            bounds.left,
            bounds.bottom,
            0,
            owner,
            null(),
        );
        DestroyMenu(menu);
        choice
    };
    Ok(result)
}

fn open_designer(owner: HWND) -> Result<(), String> {
    let path = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .with_file_name("watermarkDesigner.html");
    if !path.is_file() {
        notice(
            owner,
            "配布 ZIP の watermarkDesigner.html を SumiPDF.exe と同じフォルダーに置いてください。作成済みの PNG は「PNG 画像を選択」で使用できます。",
            false,
        );
        return Ok(());
    }
    let file: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let result = unsafe {
        ShellExecuteW(
            owner,
            wide("open").as_ptr(),
            file.as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        )
    };
    if result as isize <= 32 {
        return Err("透かし画像の作成画面をブラウザーで開けませんでした。".into());
    }
    Ok(())
}
