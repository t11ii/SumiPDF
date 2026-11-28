use super::wide;
use std::{
    ffi::{OsStr, OsString},
    mem::size_of,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    ptr::null,
};
use windows_sys::Win32::{Foundation::HWND, UI::Controls::Dialogs::*};

pub enum Purpose<'a> {
    Open,
    Watermark,
    AcrobatWatermark,
    SignatureImage,
    Pfx,
    SignedCopy(&'a Path),
    HighlightedCopy(&'a Path),
}

pub fn choose(hwnd: HWND, purpose: Purpose<'_>) -> Result<Option<PathBuf>, String> {
    Ok(choose_paths(hwnd, purpose)?.pop())
}

pub fn open_pdfs(hwnd: HWND) -> Result<Vec<PathBuf>, String> {
    choose_paths(hwnd, Purpose::Open)
}

fn choose_paths(hwnd: HWND, purpose: Purpose<'_>) -> Result<Vec<PathBuf>, String> {
    let multiple = matches!(purpose, Purpose::Open);
    let mut buffer = vec![0u16; if multiple { 65536 } else { 32768 }];
    let save = matches!(
        purpose,
        Purpose::SignedCopy(_) | Purpose::HighlightedCopy(_)
    );
    let title = wide(match purpose {
        Purpose::SignedCopy(_) => "Save a signed copy",
        Purpose::HighlightedCopy(_) => "Save an annotated copy",
        Purpose::Pfx => "Open a PFX file for signing",
        Purpose::Open => "Open PDF",
        Purpose::Watermark => "Choose a PNG for the signature background",
        Purpose::AcrobatWatermark => "Choose a single-page Acrobat watermark PDF",
        Purpose::SignatureImage => "Choose a PNG or JPEG signature image",
    });
    let (filter, ext) = if matches!(purpose, Purpose::Pfx) {
        (wide("PKCS#12 (*.pfx, *.p12)\0*.pfx;*.p12\0"), wide("pfx"))
    } else if matches!(purpose, Purpose::Watermark) {
        (wide("PNG images (*.png)\0*.png\0"), wide("png"))
    } else if matches!(purpose, Purpose::SignatureImage) {
        (
            wide("Signature images (*.png, *.jpg, *.jpeg)\0*.png;*.jpg;*.jpeg\0"),
            wide("png"),
        )
    } else {
        (wide("PDF files (*.pdf)\0*.pdf\0"), wide("pdf"))
    };
    let mut directory = Vec::new();
    if matches!(purpose, Purpose::AcrobatWatermark)
        && let Some(folder) =
            crate::preferences::acrobat_security_folder().filter(|folder| folder.is_dir())
    {
        directory.extend(folder.as_os_str().encode_wide().chain([0]));
    }
    if let Purpose::SignedCopy(path) | Purpose::HighlightedCopy(path) = purpose {
        let mut name = path
            .file_stem()
            .unwrap_or_else(|| OsStr::new("document"))
            .to_owned();
        name.push(if matches!(purpose, Purpose::SignedCopy(_)) {
            "-signed.pdf"
        } else {
            "-highlighted.pdf"
        });
        let proposed = name.encode_wide().collect::<Vec<_>>();
        if proposed.len() < buffer.len() {
            buffer[..proposed.len()].copy_from_slice(&proposed);
        }
        if let Some(parent) = path.parent() {
            directory.extend(parent.as_os_str().encode_wide().chain([0]));
        }
    }
    let mut ofn = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: hwnd,
        lpstrFilter: filter.as_ptr(),
        nFilterIndex: 1,
        lpstrFile: buffer.as_mut_ptr(),
        nMaxFile: buffer.len() as u32,
        lpstrDefExt: ext.as_ptr(),
        lpstrTitle: title.as_ptr(),
        lpstrInitialDir: if directory.is_empty() {
            null()
        } else {
            directory.as_ptr()
        },
        Flags: OFN_EXPLORER
            | OFN_PATHMUSTEXIST
            | OFN_NOCHANGEDIR
            | OFN_HIDEREADONLY
            | if multiple { OFN_ALLOWMULTISELECT } else { 0 }
            | if save {
                OFN_NOTESTFILECREATE
            } else {
                OFN_FILEMUSTEXIST
            },
        ..Default::default()
    };
    loop {
        let ok = unsafe {
            if save {
                GetSaveFileNameW(&mut ofn)
            } else {
                GetOpenFileNameW(&mut ofn)
            }
        };
        if ok == 0 {
            let code = unsafe { CommDlgExtendedError() };
            return if code == 0 {
                Ok(Vec::new())
            } else {
                Err(format!("Could not open the file dialog (0x{code:08X})."))
            };
        }
        let len = buffer.iter().position(|&u| u == 0).unwrap_or(buffer.len());
        let chosen = PathBuf::from(OsString::from_wide(&buffer[..len]));
        if multiple && buffer.get(len + 1).is_some_and(|&c| c != 0) {
            return Ok(buffer[len + 1..]
                .split(|&c| c == 0)
                .take_while(|s| !s.is_empty())
                .map(|s| chosen.join(OsString::from_wide(s)))
                .collect());
        }
        if save && chosen.exists() {
            super::notice(
                hwnd,
                "A file with this name already exists. Choose a different name.",
                false,
            );
            continue;
        }
        return Ok(vec![chosen]);
    }
}
