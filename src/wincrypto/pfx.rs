//! Load a PKCS#12 identity into a temporary Windows store for this signing session.
use super::{
    Candidate, CertificateSelection, CertificateSource, SigningCertificate, Store, last_error,
    picker,
};
use std::{fs::File, io::Read, path::Path, ptr::null};
use windows_sys::{
    Win32::{
        Foundation::{ERROR_INVALID_PASSWORD, GetLastError, HWND, LPARAM, WPARAM},
        Security::Cryptography::*,
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::*,
    },
    core::w,
};
use zeroize::Zeroizing;

const MAX_PFX_BYTES: u64 = 32 * 1024 * 1024;

impl SigningCertificate {
    pub(crate) fn pick_pfx(
        owner: HWND,
        path: &Path,
    ) -> Result<Option<CertificateSelection>, String> {
        let mut file = File::open(path).map_err(|e| format!("Could not open the PFX file. {e}"))?;
        let len = file.metadata().map_err(|e| e.to_string())?.len();
        if !(1..=MAX_PFX_BYTES).contains(&len) {
            return Err("The PFX file is empty or larger than 32 MiB.".into());
        }
        let mut bytes = Zeroizing::new(vec![0; len as usize]);
        file.read_exact(&mut bytes)
            .map_err(|e| format!("Could not read the PFX file. {e}"))?;
        if file.read(&mut [0]).map_err(|e| e.to_string())? != 0 {
            return Err("The PFX file size changed while reading it. Open the file again.".into());
        }
        if unsafe { PFXIsPFXBlob(&blob(&mut bytes)?) } == 0 {
            return Err("Choose a PFX file in PKCS#12 format.".into());
        }
        let candidates = {
            let Some(password) = ask_password(owner)? else {
                return Ok(None);
            };
            import(&mut bytes, &password)?
        };
        drop(bytes);
        let index = match candidates.len() {
            1 => 0,
            _ => match picker::select(
                owner,
                &candidates,
                w!("Choose a signing certificate from the PFX file"),
            )? {
                Some(index) => index,
                None => return Ok(None),
            },
        };
        let cert = candidates
            .into_iter()
            .nth(index)
            .expect("picker validated index")
            .certificate;
        Ok(Some(CertificateSelection {
            name: cert.display_name()?,
            source: CertificateSource::Pfx(cert),
        }))
    }
}

fn blob(bytes: &mut [u8]) -> Result<CRYPT_INTEGER_BLOB, String> {
    Ok(CRYPT_INTEGER_BLOB {
        cbData: bytes
            .len()
            .try_into()
            .map_err(|_| "The PFX file is too large.")?,
        pbData: bytes.as_mut_ptr(),
    })
}

pub(super) fn import(bytes: &mut [u8], password: &[u16]) -> Result<Vec<Candidate>, String> {
    if password.contains(&0) {
        return Err("The password cannot contain a null character.".into());
    }
    let data = blob(bytes)?;
    let mut terminated = Zeroizing::new(vec![0u16; password.len() + 1]);
    terminated[..password.len()].copy_from_slice(password);
    let flags = PKCS12_NO_PERSIST_KEY | PKCS12_ALWAYS_CNG_KSP;
    let mut store = Store(unsafe { PFXImportCertStore(&data, terminated.as_ptr(), flags) });
    // PKCS#12 distinguishes a null password from an empty password.
    if store.0.is_null()
        && password.is_empty()
        && unsafe { GetLastError() } == ERROR_INVALID_PASSWORD
    {
        store = Store(unsafe { PFXImportCertStore(&data, null(), flags) });
    }
    if store.0.is_null() {
        return Err(last_error(
            "Could not open the PFX file. Check the password and file format.",
        ));
    }
    let mut candidates = SigningCertificate::in_store(&store, "PFX")?;
    if candidates.is_empty() {
        return Err(
            "The PFX file contains no valid document-signing certificate with a private key."
                .into(),
        );
    }
    for candidate in &mut candidates {
        candidate.certificate.1 = Some(Store(unsafe { CertDuplicateStore(store.0) }));
    }
    Ok(candidates)
}

fn ask_password(owner: HWND) -> Result<Option<Zeroizing<Vec<u16>>>, String> {
    let mut password = Zeroizing::new(Vec::new());
    // Dialog 203 owns the masked edit control. The caller's buffer outlives the modal dialog.
    let result = unsafe {
        DialogBoxParamW(
            GetModuleHandleW(null()),
            203usize as *const u16,
            owner,
            Some(password_proc),
            (&raw mut password) as LPARAM,
        )
    };
    match result {
        1 => Ok(Some(password)),
        0 => Ok(None),
        _ => Err(last_error("Could not open the PFX password dialog.")),
    }
}

unsafe extern "system" fn password_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> isize {
    unsafe {
        match msg {
            WM_INITDIALOG => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, lp);
                SetWindowTextW(hwnd, w!("PFX password"));
                SetDlgItemTextW(
                    hwnd,
                    1000,
                    w!("Enter the password, or leave it blank if none was set."),
                );
                SetDlgItemTextW(hwnd, IDCANCEL, w!("Cancel"));
                1
            }
            WM_COMMAND if (wp & 0xffff) as i32 == IDOK => {
                let edit = GetDlgItem(hwnd, 1001);
                let len = GetWindowTextLengthW(edit);
                let password =
                    &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Zeroizing<Vec<u16>>);
                password.resize(len as usize + 1, 0);
                let copied = GetWindowTextW(edit, password.as_mut_ptr(), len + 1);
                SetWindowTextW(edit, w!(""));
                password.truncate(copied as usize);
                EndDialog(hwnd, if copied == len { 1 } else { -2 });
                1
            }
            WM_CLOSE | WM_COMMAND if msg == WM_CLOSE || (wp & 0xffff) as i32 == IDCANCEL => {
                SetDlgItemTextW(hwnd, 1001, w!(""));
                EndDialog(hwnd, 0);
                1
            }
            _ => 0,
        }
    }
}
