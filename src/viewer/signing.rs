use super::{App, Controls, Event, VIEW, canvas, dialog, notice, save, wide, worker};
use crate::{
    highlight::UnitRect,
    pdfseal,
    wincrypto::{CertificateSelection, SigningCertificate},
};
use std::{os::windows::ffi::OsStrExt, path::Path, ptr::null, sync::Arc};
use windows_sys::Win32::{
    Foundation::{HWND, RECT},
    UI::{Input::KeyboardAndMouse::SetFocus, Shell::ShellExecuteW, WindowsAndMessaging::*},
};

struct Field {
    id: String,
    name: String,
    page: u32, // zero-based, same as the canvas
    area: UnitRect,
}

pub(super) struct Choices {
    positions: Vec<Position>,
    already_signed: bool,
}

enum Position {
    Draw,
    Field(Field),
    Invisible,
}

impl Choices {
    fn from_inspection(info: &serde_json::Value) -> Result<Self, String> {
        let invalid = || "PDF の署名欄と署名の制限を読み取れませんでした。".to_owned();
        let new_field = info["allowsNewField"].as_bool().ok_or_else(invalid)?;
        let already_signed = info["alreadySigned"].as_bool().ok_or_else(invalid)?;
        let mut positions = Vec::new();
        if new_field {
            positions.push(Position::Draw);
        }
        for field in info["fields"].as_array().ok_or_else(invalid)? {
            let [left, top, right, bottom]: [f32; 4] =
                serde_json::from_value(field["rect"].clone()).map_err(|_| invalid())?;
            positions.push(Position::Field(Field {
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
            }));
        }
        if new_field {
            positions.push(Position::Invisible);
        }
        Ok(Self {
            positions,
            already_signed,
        })
    }

    fn preferred(&self, page: u32) -> Option<usize> {
        self.positions
            .iter()
            .position(|p| matches!(p, Position::Field(f) if f.page == page))
            .or_else(|| {
                self.positions
                    .iter()
                    .position(|p| matches!(p, Position::Field(_)))
            })
            .or_else(|| (!self.positions.is_empty()).then_some(0))
    }
}

struct Watermark {
    name: String,
    bytes: Arc<[u8]>,
}

#[derive(Default)]
pub(super) struct Session {
    choices: Option<Choices>,
    selected: Option<usize>,
    pub drawn: Option<(u32, UnitRect)>,
    watermark: Option<Watermark>,
    pub(super) certificate: Option<CertificateSelection>,
}

impl Session {
    pub fn has_positions(&self) -> bool {
        self.choices
            .as_ref()
            .is_some_and(|c| !c.positions.is_empty())
    }

    fn position(&self) -> Option<&Position> {
        self.choices.as_ref()?.positions.get(self.selected?)
    }

    pub fn drawing(&self) -> bool {
        matches!(self.position(), Some(Position::Draw))
    }

    pub fn can_pick_watermark(&self) -> bool {
        matches!(self.position(), Some(Position::Draw | Position::Field(_)))
    }

    fn placement(&self) -> Option<(u32, UnitRect)> {
        match self.position()? {
            Position::Draw => self.drawn,
            Position::Field(field) => Some((field.page, field.area)),
            Position::Invisible => None,
        }
    }

    pub fn appearance(&self) -> Result<Option<save::Appearance>, String> {
        let target = match self.position().ok_or("署名位置を選び直してください。")? {
            Position::Draw => {
                let (page, r) = self
                    .drawn
                    .ok_or("署名を表示する位置をドラッグしてください。")?;
                pdfseal::Target::Area {
                    page: page + 1,
                    rect: [r.left, r.top, r.right, r.bottom],
                }
            }
            Position::Field(field) => pdfseal::Target::Field(field.id.clone()),
            Position::Invisible => return Ok(None),
        };
        Ok(Some(save::Appearance {
            target,
            watermark: self.watermark.as_ref().map(|w| w.bytes.clone()),
        }))
    }

    pub fn instructions(&self, current_page: u32) -> String {
        if self.choices.is_none() {
            return "空の署名欄を確認しています…".into();
        }
        if !self.has_positions() {
            return "この PDF には追加署名に使える空の署名欄がありません。".into();
        }
        let position = self.position_instructions(current_page);
        match &self.certificate {
            Some(cert) => format!("署名者：{}　·　{position}", cert.name),
            None => format!("{position}　·　鍵のボタンで証明書を選べます。"),
        }
    }

    fn position_instructions(&self, current_page: u32) -> String {
        if self.position().is_none() {
            return "署名位置を選び直してください。".into();
        }
        if matches!(self.position(), Some(Position::Invisible)) {
            return "ページに表示しない電子署名を作成します。".into();
        }
        match self.placement() {
            Some((page, _)) if page != current_page => format!(
                "署名の位置は {} ページです。そのページを開いて確認するか、署名位置を選び直してください。",
                page + 1
            ),
            Some(_) if self.drawing() => {
                "青枠の位置に署名します。ドラッグで描き直すこともできます。".into()
            }
            Some(_) => "青枠が選択した署名欄です。".into(),
            None => "署名を表示するページで、枠の角から反対側の角までドラッグしてください。".into(),
        }
    }
}

impl App {
    pub(super) fn can_sign_here(&self) -> bool {
        self.signing.as_ref().is_some_and(|s| {
            let index = unsafe { SendMessageW(self.ui.position, CB_GETCURSEL, 0, 0) };
            s.selected == usize::try_from(index).ok()
                && (matches!(s.position(), Some(Position::Invisible))
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
                    let result = source
                        .read_bytes()
                        .and_then(|bytes| Choices::from_inspection(&pdfseal::inspect(&bytes)?));
                    worker::report(hwnd, &events, Event::SignatureFields { revision, result });
                })
                .map_err(|e| e.to_string())?;
            self.checking_fields = Some(revision);
        }
        self.tool = None;
        self.request_text()?;
        self.signing = Some(Session::default());
        self.ui.show_signing(true);
        self.ui.toolbar.watermark(self.ui.watermark, false)?;
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

    pub(super) fn show_fields(&mut self, choices: Choices) -> Result<(), String> {
        let Some(session) = &mut self.signing else {
            return Ok(());
        };
        self.known_signed = choices.already_signed;
        if choices.already_signed && !self.marks.is_empty() {
            return Err("署名済みの PDF には注釈を追加できません。「元に戻す」で注釈をすべて取り消してから署名してください。".into());
        }
        let preferred = choices.preferred(self.page);
        for position in &choices.positions {
            let label = match position {
                Position::Draw => "署名位置をドラッグして指定".to_owned(),
                Position::Field(f) => format!("{} ページの署名欄：{}", f.page + 1, f.name),
                Position::Invisible => "ページに表示しない署名".to_owned(),
            };
            let index = unsafe {
                SendMessageW(
                    self.ui.position,
                    CB_ADDSTRING,
                    0,
                    wide(&label).as_ptr() as isize,
                )
            };
            if index < 0 {
                return Err("署名位置の選択肢を表示できませんでした。".into());
            }
        }
        session.choices = Some(choices);
        if let Some(index) = preferred {
            unsafe {
                SendMessageW(self.ui.position, CB_SETCURSEL, index, 0);
            }
        }
        self.choose_position();
        unsafe {
            SetFocus(if preferred.is_some() {
                self.ui.position
            } else {
                self.ui.sign_cancel
            });
        }
        Ok(())
    }

    pub(super) fn choose_position(&mut self) {
        let Some(session) = &mut self.signing else {
            return;
        };
        let index = unsafe { SendMessageW(self.ui.position, CB_GETCURSEL, 0, 0) };
        session.selected = usize::try_from(index).ok();
        if let Some((page, _)) = session.placement() {
            self.page_to(page);
            self.scale(1.0);
        }
        self.sync_signature();
        self.controls();
        self.status();
    }

    pub(super) fn choose_certificate(&mut self) -> Result<(), String> {
        if let Some(session) = &mut self.signing
            && session.has_positions()
            && let Some(choice) = SigningCertificate::pick(self.hwnd)?
        {
            session.certificate = Some(choice);
            self.status();
        }
        Ok(())
    }

    pub(super) fn choose_watermark(&mut self) -> Result<(), String> {
        let Some(session) = &mut self.signing else {
            return Ok(());
        };
        if !session.can_pick_watermark() {
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
        self.ui
            .toolbar
            .watermark(self.ui.watermark, session.watermark.is_some())?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn restricted_picker_only_resolves_permitted_fields() {
        let choices = Choices::from_inspection(&json!({
            "alreadySigned": true, "allowsNewField": false,
            "fields": [{"id":"field-8-0", "name":"Approval", "page":2, "rect":[0.1,0.2,0.5,0.4]}]
        }))
        .unwrap();
        let mut session = Session {
            selected: choices.preferred(0),
            choices: Some(choices),
            ..Session::default()
        };
        assert_eq!(session.selected, Some(0));
        assert!(!session.drawing());
        assert_eq!(session.placement().unwrap().0, 1);
        assert_eq!(
            session.appearance().unwrap().unwrap().target,
            pdfseal::Target::Field("field-8-0".into())
        );
        session.certificate = Some(CertificateSelection {
            name: "山田 太郎".into(),
            hash: [1; 20],
        });
        let status = session.instructions(1);
        assert!(status.starts_with("署名者：山田 太郎"));
        assert!(status.contains("青枠が選択した署名欄"));

        session.selected = Some(1);
        assert!(session.appearance().is_err());
        assert!(!session.can_pick_watermark());
    }

    #[test]
    fn restricted_document_without_a_field_explains_why_signing_is_unavailable() {
        let choices = Choices::from_inspection(&json!({
            "alreadySigned": true, "allowsNewField": false, "fields": []
        }))
        .unwrap();
        let session = Session {
            selected: choices.preferred(0),
            choices: Some(choices),
            ..Session::default()
        };
        assert!(!session.has_positions());
        assert!(
            session
                .instructions(0)
                .contains("使える空の署名欄がありません")
        );
        assert!(session.appearance().is_err());
    }
}
