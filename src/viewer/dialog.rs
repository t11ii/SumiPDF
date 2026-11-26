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
    SignedCopy(&'a Path),
    HighlightedCopy(&'a Path),
}

pub fn choose(hwnd: HWND, purpose: Purpose<'_>) -> Result<Option<PathBuf>, String> {
    let mut buffer = vec![0u16; 32768];
    let save = matches!(
        purpose,
        Purpose::SignedCopy(_) | Purpose::HighlightedCopy(_)
    );
    let title = wide(match purpose {
        Purpose::SignedCopy(_) => "署名済みPDFを新しいファイルに保存",
        Purpose::HighlightedCopy(_) => "注釈付きPDFを新しいファイルに保存",
        Purpose::Open => "PDFを開く",
        Purpose::Watermark => "電子署名の背景に使う PNG 画像を選択",
    });
    let (filter, ext) = if matches!(purpose, Purpose::Watermark) {
        (wide("PNG 画像 (*.png)\0*.png\0"), wide("png"))
    } else {
        (wide("PDF ファイル (*.pdf)\0*.pdf\0"), wide("pdf"))
    };
    let mut directory = Vec::new();
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
                Ok(None)
            } else {
                Err(format!(
                    "ファイル選択ダイアログを開けません (0x{code:08X})。"
                ))
            };
        }
        let len = buffer.iter().position(|&u| u == 0).unwrap_or(buffer.len());
        let chosen = PathBuf::from(OsString::from_wide(&buffer[..len]));
        if save && chosen.exists() {
            super::notice(
                hwnd,
                "この名前のファイルは既にあります。別の名前で保存してください。",
                false,
            );
            continue;
        }
        return Ok(Some(chosen));
    }
}
