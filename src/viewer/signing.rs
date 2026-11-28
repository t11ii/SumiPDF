use super::{
    App, Controls, Event, SIGN_CANCEL, SIGN_CERTIFICATE, SIGN_PFX, SIGN_POSITION, SIGN_SAVE, VIEW,
    WATERMARK, canvas, create_child, dialog, notice, save, theme, toolbar, wide, worker,
};
use crate::{
    highlight::UnitRect,
    pdfseal,
    preferences::{self, Identity, PfxIdentity},
    wincrypto::{CertificateSelection, SigningCertificate},
};
use std::{
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr::null,
    sync::Arc,
};
use windows_sys::Win32::{
    Foundation::{HWND, RECT},
    Graphics::Gdi::HFONT,
    UI::{
        Input::KeyboardAndMouse::{EnableWindow, SetFocus},
        Shell::ShellExecuteW,
        WindowsAndMessaging::*,
    },
};

#[derive(Clone, Copy)]
pub(super) struct Bar {
    pub(super) position: HWND,
    pub(super) watermark: HWND,
    pub(super) certificate: HWND,
    pub(super) pfx: HWND,
    save: HWND,
    cancel: HWND,
}

impl Bar {
    pub(super) fn new(
        hwnd: HWND,
        after: HWND,
        toolbar: &mut toolbar::Toolbar,
    ) -> Result<Self, String> {
        toolbar.build_row(
            after,
            &[
                SIGN_POSITION,
                WATERMARK,
                SIGN_CERTIFICATE,
                SIGN_PFX,
                SIGN_SAVE,
                SIGN_CANCEL,
            ],
            |toolbar| {
                let position = create_child(
                    hwnd,
                    "COMBOBOX",
                    "",
                    SIGN_POSITION,
                    WS_TABSTOP | CBS_DROPDOWNLIST as u32,
                )?;
                theme::style_control(position)?;
                Ok(Self {
                    position,
                    watermark: toolbar.hidden_button(WATERMARK, toolbar::DEFAULT_WATERMARK)?,
                    certificate: toolbar.hidden_button(SIGN_CERTIFICATE, toolbar::CERTIFICATE)?,
                    pfx: toolbar.hidden_button(SIGN_PFX, toolbar::PFX)?,
                    save: toolbar.hidden_button(SIGN_SAVE, toolbar::CONFIRM)?,
                    cancel: toolbar.hidden_button(SIGN_CANCEL, toolbar::CANCEL)?,
                })
            },
        )
    }

    pub(super) fn show(self, visible: bool) {
        for control in [
            self.position,
            self.watermark,
            self.certificate,
            self.pfx,
            self.save,
            self.cancel,
        ] {
            unsafe { ShowWindow(control, if visible { SW_SHOW } else { SW_HIDE }) };
        }
    }

    pub(super) fn font(self, font: HFONT) {
        unsafe { SendMessageW(self.position, WM_SETFONT, font as usize, 1) };
    }

    fn reset_positions(self) {
        unsafe { SendMessageW(self.position, CB_RESETCONTENT, 0, 0) };
    }
}

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
        let invalid =
            || "Could not read the signature fields or signing permissions in this PDF.".to_owned();
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

    fn populate(&self, control: HWND, selected: Option<usize>) -> Result<(), String> {
        for position in &self.positions {
            let label = match position {
                Position::Draw => "Draw signature box".to_owned(),
                Position::Field(f) => format!("Page {}: {}", f.page + 1, f.name),
                Position::Invisible => "Invisible signature".to_owned(),
            };
            let index =
                unsafe { SendMessageW(control, CB_ADDSTRING, 0, wide(&label).as_ptr() as isize) };
            if index < 0 {
                return Err("Could not display the signature positions.".into());
            }
        }
        if let Some(index) = selected {
            unsafe {
                SendMessageW(control, CB_SETCURSEL, index, 0);
            }
        }
        Ok(())
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
    path: PathBuf,
    bytes: Option<Arc<[u8]>>,
}

#[derive(Default)]
pub(super) struct Session {
    choices: Option<Choices>,
    selected: Option<usize>,
    pub drawn: Option<(u32, UnitRect)>,
    watermark: Option<Watermark>,
    certificate: Option<CertificateSelection>,
    remembered_pfx: Option<PfxIdentity>,
}

impl Session {
    fn restore(hwnd: HWND) -> Self {
        let mut session = Self::default();
        match preferences::load_watermark() {
            Ok(path) => session.watermark = path.map(|path| Watermark { path, bytes: None }),
            Err(e) => notice(hwnd, &e, true),
        }
        match preferences::load_identity() {
            Ok(Some(Identity::Store { name, thumbprint })) => {
                session.certificate = Some(CertificateSelection::personal(name, thumbprint));
            }
            Ok(Some(Identity::Pfx(record))) => session.remembered_pfx = Some(record),
            Ok(None) => {}
            Err(e) => notice(hwnd, &e, true),
        }
        session
    }

    pub(super) fn clear_certificate(&mut self) {
        self.certificate = None;
        self.remembered_pfx = None;
    }

    pub(super) fn resolve_certificate(
        &mut self,
        hwnd: HWND,
    ) -> Result<Option<SigningCertificate>, String> {
        if self.certificate.is_none() {
            if let Some(mut saved) = self.remembered_pfx.clone() {
                let latest = preferences::load_identity()?;
                let current = matches!(&latest, Some(Identity::Pfx(other)) if other.path == saved.path && other.file_hash == saved.file_hash && other.thumbprint == saved.thumbprint);
                saved.protected_password = match latest {
                    Some(Identity::Pfx(other)) if current => other.protected_password,
                    _ => None,
                };
                let Some((choice, record)) =
                    SigningCertificate::pick_pfx_for_gui(hwnd, &saved.path, Some(&saved))?
                else {
                    return Ok(None);
                };
                if current && record.protected_password != saved.protected_password {
                    remember_choice(hwnd, &Identity::Pfx(record.clone()));
                }
                self.remembered_pfx = Some(record);
                self.certificate = Some(choice);
            } else if let Some(choice) = SigningCertificate::pick(hwnd)? {
                self.use_store(hwnd, choice)?;
            }
        }
        match &self.certificate {
            Some(choice) => choice.resolve(hwnd),
            None => Ok(None),
        }
    }

    fn use_store(&mut self, hwnd: HWND, choice: CertificateSelection) -> Result<(), String> {
        let identity = Identity::Store {
            name: choice.name.clone(),
            thumbprint: choice.thumbprint()?,
        };
        self.certificate = Some(choice);
        self.remembered_pfx = None;
        remember_choice(hwnd, &identity);
        Ok(())
    }
    pub(super) fn loaded(&self) -> bool {
        self.choices.is_some()
    }

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
        let target = match self.position().ok_or("Choose a signature position.")? {
            Position::Draw => {
                let (page, r) = self
                    .drawn
                    .ok_or("Drag a box where the signature should appear.")?;
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
            watermark: self
                .watermark
                .as_ref()
                .map(|w| match &w.bytes {
                    Some(bytes) => Ok(bytes.clone()),
                    None => watermark_bytes(&w.path),
                })
                .transpose()?,
        }))
    }

    pub fn instructions(&self, current_page: u32) -> String {
        if self.choices.is_none() {
            return "Looking for empty signature fields…".into();
        }
        if !self.has_positions() {
            return "This PDF has no empty field available for another signature.".into();
        }
        let position = self.position_instructions(current_page);
        match self
            .certificate
            .as_ref()
            .map(|c| &c.name)
            .or_else(|| self.remembered_pfx.as_ref().map(|p| &p.name))
        {
            Some(name) => format!("Signer: {name}  ·  {position}"),
            None => {
                format!("{position}  ·  Choose a certificate from the Windows store or a PFX file.")
            }
        }
    }

    fn position_instructions(&self, current_page: u32) -> String {
        if self.position().is_none() {
            return "Choose a signature position.".into();
        }
        if matches!(self.position(), Some(Position::Invisible)) {
            return "The signature will be invisible.".into();
        }
        match self.placement() {
            Some((page, _)) if page != current_page => format!(
                "The signature is on page {}. Open that page or choose a new position.",
                page + 1
            ),
            Some(_) if self.drawing() => {
                "The blue box shows the signature position. Drag to redraw it.".into()
            }
            Some(_) => "The blue box shows the selected signature field.".into(),
            None => "Drag from corner to corner to place the signature.".into(),
        }
    }
}

impl App {
    pub(super) fn can_sign_here(&self) -> bool {
        let (Some(session), Some(bar)) = (&self.signing, self.ui.signing) else {
            return false;
        };
        let index = unsafe { SendMessageW(bar.position, CB_GETCURSEL, 0, 0) };
        session.selected == usize::try_from(index).ok()
            && (matches!(session.position(), Some(Position::Invisible))
                || (session.placement().is_some_and(|(p, _)| p == self.page)
                    && VIEW.with_borrow(|v| v.bitmap.is_some())))
    }

    pub(super) fn enable_signing_controls(&self) {
        let Some(bar) = self.ui.signing else {
            return;
        };
        let session = self.signing.as_ref();
        let ready = !self.writing && session.is_some_and(Session::has_positions);
        for (control, enabled) in [
            (bar.position, ready),
            (bar.certificate, ready),
            (bar.pfx, ready),
            (
                bar.watermark,
                !self.writing && session.is_some_and(Session::can_pick_watermark),
            ),
            (bar.save, !self.writing && self.can_sign_here()),
            (bar.cancel, !self.writing),
        ] {
            unsafe { EnableWindow(control, i32::from(enabled)) };
        }
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

    pub(super) fn restore_signing_controls(&mut self) -> Result<(), String> {
        let Some(bar) = self.ui.signing else {
            return Ok(());
        };
        bar.reset_positions();
        if let Some(session) = &self.signing {
            self.ui
                .toolbar
                .watermark(bar.watermark, session.watermark.is_some())?;
            if let Some(choices) = &session.choices {
                choices.populate(bar.position, session.selected)?;
            }
        }
        bar.show(self.signing.is_some());
        Ok(())
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
        let bar = self.ui.ensure_signing(self.hwnd)?;
        let session = Session::restore(self.hwnd);
        self.ui
            .toolbar
            .watermark(bar.watermark, session.watermark.is_some())?;
        self.tool = None;
        self.request_text()?;
        self.signing = Some(session);
        bar.show(true);
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
        if let Some(bar) = self.ui.signing {
            bar.reset_positions();
            bar.show(false);
        }
        VIEW.with_borrow_mut(|v| v.set_signature(false, None));
        Controls::layout(self.hwnd);
        self.controls();
        self.status();
        self.render();
        unsafe {
            SetFocus(self.hwnd);
        }
    }

    pub(super) fn show_fields(&mut self, choices: Choices) -> Result<(), String> {
        let (Some(session), Some(bar)) = (&mut self.signing, self.ui.signing) else {
            return Ok(());
        };
        self.known_signed = choices.already_signed;
        if choices.already_signed && !self.marks.is_empty() {
            return Err("This PDF is already signed. Undo all unsaved annotations before adding another signature.".into());
        }
        let preferred = choices.preferred(self.page);
        choices.populate(bar.position, preferred)?;
        session.choices = Some(choices);
        self.choose_position();
        unsafe {
            SetFocus(if preferred.is_some() {
                bar.position
            } else {
                bar.cancel
            });
        }
        Ok(())
    }

    pub(super) fn choose_position(&mut self) {
        let (Some(session), Some(bar)) = (&mut self.signing, self.ui.signing) else {
            return;
        };
        let index = unsafe { SendMessageW(bar.position, CB_GETCURSEL, 0, 0) };
        session.selected = usize::try_from(index).ok();
        if let Some((page, _)) = session.placement() {
            self.page_to(page);
            self.scale(1.0);
        }
        self.sync_signature();
        self.controls();
        self.status();
    }

    pub(super) fn choose_certificate(&mut self, from_pfx: bool) -> Result<(), String> {
        let Some(session) = self.signing.as_mut().filter(|s| s.has_positions()) else {
            return Ok(());
        };
        if from_pfx {
            let Some(path) = dialog::choose(self.hwnd, dialog::Purpose::Pfx)? else {
                return Ok(());
            };
            if let Some((choice, record)) =
                SigningCertificate::pick_pfx_for_gui(self.hwnd, &path, None)?
            {
                remember_choice(self.hwnd, &Identity::Pfx(record.clone()));
                session.remembered_pfx = Some(record);
                session.certificate = Some(choice);
            }
        } else if let Some(choice) = SigningCertificate::pick(self.hwnd)? {
            session.use_store(self.hwnd, choice)?;
        }
        self.status();
        Ok(())
    }

    pub(super) fn forget_certificate(&mut self) -> Result<(), String> {
        preferences::save_identity(None)?;
        if let Some(session) = &mut self.signing {
            session.clear_certificate();
        }
        for tab in &mut self.tabs {
            tab.clear_certificate();
        }
        self.status();
        Ok(())
    }

    pub(super) fn choose_watermark(&mut self) -> Result<(), String> {
        let (Some(session), Some(bar)) = (&mut self.signing, self.ui.signing) else {
            return Ok(());
        };
        if !session.can_pick_watermark() {
            return Ok(());
        }
        let path = session.watermark.as_ref().map(|w| w.path.as_path());
        match watermark_menu(self.hwnd, bar.watermark, path)? {
            1 => session.watermark = None,
            choice @ (2 | 4) => {
                let purpose = if choice == 4 {
                    dialog::Purpose::AcrobatWatermark
                } else {
                    dialog::Purpose::Watermark
                };
                let Some(path) = dialog::choose(self.hwnd, purpose)? else {
                    return Ok(());
                };
                session.watermark = Some(Watermark {
                    bytes: Some(watermark_bytes(&path)?),
                    path,
                });
            }
            3 => return open_designer(self.hwnd),
            _ => return Ok(()),
        }
        if let Err(e) =
            preferences::save_watermark(session.watermark.as_ref().map(|w| w.path.as_path()))
        {
            notice(self.hwnd, &e, true);
        }
        self.ui
            .toolbar
            .watermark(bar.watermark, session.watermark.is_some())?;
        Ok(())
    }
}

fn remember_choice(hwnd: HWND, identity: &Identity) {
    if let Err(e) = preferences::save_identity(Some(identity)) {
        notice(hwnd, &e, true);
    }
}

fn pdf_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

fn watermark_bytes(path: &Path) -> Result<Arc<[u8]>, String> {
    let result = if pdf_path(path) {
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    if path.metadata().map_err(|e| e.to_string())?.len() > 32 * 1024 * 1024 {
                        return Err("Choose a watermark PDF no larger than 32 MiB.".into());
                    }
                    let _apartment = crate::winpdf::initialize()?;
                    let pdf = crate::winpdf::Document::open_file(path)?;
                    if pdf.page_count() != 1 {
                        return Err("Choose a single-page watermark PDF.".into());
                    }
                    let bitmap = pdf.render_fit(0, (1024, 1024), 1.0)?;
                    crate::cli::page_png(bitmap.width, bitmap.height, &bitmap.pixels, None)
                })
                .join()
        })
        .map_err(|_| "Could not render the watermark PDF.".to_owned())?
    } else {
        crate::read_watermark(path)
    };
    result.map(Into::into).map_err(|e| format!("Could not load the watermark from {}. Choose another watermark in the signature toolbar.\n{e}", path.display()))
}

fn watermark_menu(owner: HWND, button: HWND, path: Option<&Path>) -> Result<i32, String> {
    let menu = unsafe { CreatePopupMenu() };
    if menu.is_null() {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let label = |base: &str, selected: Option<&Path>| match selected.and_then(Path::file_name) {
        Some(filename) => format!("{base} ({})", filename.to_string_lossy().replace('&', "&&")),
        None => base.to_owned(),
    };
    let png = path.filter(|p| !pdf_path(p));
    let pdf = path.filter(|p| pdf_path(p));
    let check = |value: bool| if value { MF_CHECKED } else { MF_UNCHECKED };
    let mut bounds = RECT::default();
    let result = unsafe {
        AppendMenuW(
            menu,
            MF_STRING | check(path.is_none()),
            1,
            wide("&Default watermark (paper pattern)").as_ptr(),
        );
        AppendMenuW(
            menu,
            MF_STRING | check(png.is_some()),
            2,
            wide(&label("Choose &PNG…", png)).as_ptr(),
        );
        AppendMenuW(
            menu,
            MF_STRING | check(pdf.is_some()),
            4,
            wide(&label("Choose &Acrobat watermark PDF…", pdf)).as_ptr(),
        );
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        AppendMenuW(menu, MF_STRING, 3, wide("Create watermark…").as_ptr());
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
            "Place watermarkDesigner.html from the ZIP in the same folder as SumiPDF.exe. You can also choose an existing PNG from the watermark menu.",
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
        return Err("Could not open the watermark designer in your browser.".into());
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
        session.certificate = Some(CertificateSelection::personal("山田 太郎".into(), [1; 20]));
        let status = session.instructions(1);
        assert!(status.starts_with("Signer: 山田 太郎"));
        assert!(status.contains("The blue box shows the selected signature field"));

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
        assert!(session.instructions(0).contains("no empty field available"));
        assert!(session.appearance().is_err());
    }
}
