//! Modal identity list shared by the viewer and interactive CLI.
use super::Candidate;
use std::ptr::{null, null_mut};
use windows_sys::{
    Win32::{
        Foundation::{HWND, LPARAM, WPARAM},
        Security::Cryptography::{CERT_STORE_CERTIFICATE_CONTEXT, UI::CryptUIDlgViewContext},
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::*,
    },
    core::w,
};

// IDs shared with assets/app.rc.
const DIALOG: usize = 202;
const PROMPT: i32 = 1000;
const IDENTITIES: i32 = 1001;
const DETAILS: i32 = 1002;

struct Contents<'a> {
    candidates: &'a [Candidate],
    labels: Vec<Vec<u16>>,
    title: *const u16,
}

pub(super) fn select(
    owner: HWND,
    candidates: &[Candidate],
    title: *const u16,
) -> Result<Option<usize>, String> {
    let mut labels = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let name = candidate.certificate.display_name()?;
        let label = format!("{name}   ({})", candidate.store);
        labels.push(label.encode_utf16().chain([0]).collect());
    }
    let contents = Contents {
        candidates,
        labels,
        title,
    };
    // DialogBoxParamW is synchronous. All context references outlive the window.
    let result = unsafe {
        DialogBoxParamW(
            GetModuleHandleW(null()),
            DIALOG as *const u16,
            owner,
            Some(dialog_proc),
            (&raw const contents) as LPARAM,
        )
    };
    match result {
        0 => Ok(None),
        n if n > 0 && n as usize <= candidates.len() => Ok(Some(n as usize - 1)),
        _ => Err(super::last_error("Could not open the certificate picker.")),
    }
}

unsafe extern "system" fn dialog_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> isize {
    unsafe {
        match msg {
            WM_INITDIALOG => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, lp);
                let contents = &*(lp as *const Contents);
                SetWindowTextW(hwnd, contents.title);
                SetDlgItemTextW(
                    hwnd,
                    PROMPT,
                    w!("Select a signing certificate by name and store."),
                );
                SetDlgItemTextW(hwnd, DETAILS, w!("View details"));
                SetDlgItemTextW(hwnd, IDOK, w!("Select"));
                SetDlgItemTextW(hwnd, IDCANCEL, w!("Cancel"));
                let list = GetDlgItem(hwnd, IDENTITIES);
                for label in &contents.labels {
                    if SendMessageW(list, LB_ADDSTRING, 0, label.as_ptr() as LPARAM) < 0 {
                        EndDialog(hwnd, -2);
                        return 1;
                    }
                }
                SendMessageW(list, LB_SETCURSEL, 0, 0);
                1
            }
            WM_CLOSE => {
                EndDialog(hwnd, 0);
                1
            }
            WM_COMMAND => {
                let id = (wp & 0xffff) as i32;
                if id == IDCANCEL {
                    EndDialog(hwnd, 0);
                    return 1;
                }
                let accept = id == IDOK || (id == IDENTITIES && wp >> 16 == LBN_DBLCLK as usize);
                if !accept && id != DETAILS {
                    return 0;
                }
                let index = SendDlgItemMessageW(hwnd, IDENTITIES, LB_GETCURSEL, 0, 0);
                let data = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Contents;
                if let Some(cert) = data.as_ref().and_then(|d| d.candidates.get(index as usize)) {
                    if accept {
                        EndDialog(hwnd, index + 1);
                    } else {
                        CryptUIDlgViewContext(
                            CERT_STORE_CERTIFICATE_CONTEXT,
                            cert.certificate.0.cast(),
                            hwnd,
                            null(),
                            0,
                            null_mut(),
                        );
                    }
                }
                1
            }
            _ => 0,
        }
    }
}
