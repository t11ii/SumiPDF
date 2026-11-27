//! Load a PKCS#12 identity into a temporary Windows store for this signing session.
use super::{
    Candidate, CertificateSelection, CertificateSource, SigningCertificate, Store, last_error,
    picker,
};
use crate::preferences::PfxIdentity;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    path::Path,
    ptr::{null, null_mut},
    slice,
};
use windows_sys::{
    Win32::{
        Foundation::{ERROR_INVALID_PASSWORD, GetLastError, HWND, LPARAM, LocalFree, WPARAM},
        Security::Cryptography::*,
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Controls::{BST_CHECKED, BST_UNCHECKED, CheckDlgButton, IsDlgButtonChecked},
            WindowsAndMessaging::*,
        },
    },
    core::w,
};
use zeroize::{Zeroize, Zeroizing};

const MAX_PFX_BYTES: u64 = 32 * 1024 * 1024;
const REMEMBER_PASSWORD: i32 = 1003;
type Password = Zeroizing<Vec<u16>>;
type ImportedCandidates = (Vec<Candidate>, Option<Password>);

impl SigningCertificate {
    pub(crate) fn pick_pfx(
        owner: HWND,
        path: &Path,
    ) -> Result<Option<CertificateSelection>, String> {
        let mut bytes = read_file(path)?;
        let Some((candidates, _)) = prompt_import(owner, &mut bytes, false)? else {
            return Ok(None);
        };
        drop(bytes);
        select_candidate(owner, candidates, None)?
            .map(selection)
            .transpose()
    }

    pub(crate) fn pick_pfx_for_gui(
        owner: HWND,
        path: &Path,
        preferred: Option<&PfxIdentity>,
    ) -> Result<Option<(CertificateSelection, PfxIdentity)>, String> {
        let mut bytes = read_file(path).map_err(|e| {
            if preferred.is_some() {
                format!("{e} Choose the PFX file again.")
            } else {
                e
            }
        })?;
        let file_hash: [u8; 32] = Sha256::digest(&*bytes).into();
        if preferred.is_some_and(|saved| saved.path != path || saved.file_hash != file_hash) {
            return Err("The saved PFX file has changed. Choose the PFX file again.".into());
        }
        let restored = preferred.and_then(|saved| {
            let ciphertext = saved.protected_password.as_ref()?;
            let password = unprotect_password(ciphertext, &file_hash, &saved.thumbprint).ok()?;
            Some((import(&mut bytes, &password).ok()?, ciphertext))
        });
        let (candidates, previous_blob, entered_password) = match restored {
            Some((candidates, ciphertext)) => (candidates, Some(ciphertext), None),
            None => {
                let Some((candidates, password)) = prompt_import(owner, &mut bytes, true)? else {
                    return Ok(None);
                };
                (candidates, None, password)
            }
        };
        drop(bytes);
        let Some(candidate) = select_candidate(owner, candidates, preferred)? else {
            return Ok(None);
        };
        let thumbprint = candidate.hash;
        let selected = selection(candidate)?;
        let protected_password = match entered_password {
            Some(password) => Some(protect_password(&password, &file_hash, &thumbprint)?),
            None => previous_blob.cloned(),
        };
        let record = PfxIdentity {
            path: path.to_path_buf(),
            name: selected.name.clone(),
            thumbprint,
            file_hash,
            protected_password,
        };
        Ok(Some((selected, record)))
    }
}

fn read_file(path: &Path) -> Result<Zeroizing<Vec<u8>>, String> {
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
    Ok(bytes)
}

fn prompt_import(
    owner: HWND,
    bytes: &mut [u8],
    allow_remember: bool,
) -> Result<Option<ImportedCandidates>, String> {
    if unsafe { PFXIsPFXBlob(&blob(bytes)?) } == 0 {
        return Err("Choose a PFX file in PKCS#12 format.".into());
    }
    let Some(answer) = ask_password(owner, allow_remember)? else {
        return Ok(None);
    };
    let candidates = import(bytes, &answer.password)?;
    Ok(Some((
        candidates,
        answer.remember.then_some(answer.password),
    )))
}

fn select_candidate(
    owner: HWND,
    candidates: Vec<Candidate>,
    preferred: Option<&PfxIdentity>,
) -> Result<Option<Candidate>, String> {
    if let Some(saved) = preferred {
        return candidates
            .into_iter()
            .find(|c| c.hash == saved.thumbprint)
            .map(Some)
            .ok_or_else(|| {
                "The saved certificate is unavailable in this PFX file. Choose the PFX file again."
                    .into()
            });
    }
    let index = if candidates.len() == 1 {
        0
    } else {
        let Some(index) = picker::select(
            owner,
            &candidates,
            w!("Choose a signing certificate from the PFX file"),
        )?
        else {
            return Ok(None);
        };
        index
    };
    Ok(candidates.into_iter().nth(index))
}

fn selection(candidate: Candidate) -> Result<CertificateSelection, String> {
    Ok(CertificateSelection {
        name: candidate.certificate.display_name()?,
        source: CertificateSource::Pfx(candidate.certificate),
    })
}

// CryptoAPI declares pbData mutable even for its read-only input blobs.
fn blob(bytes: &[u8]) -> Result<CRYPT_INTEGER_BLOB, String> {
    Ok(CRYPT_INTEGER_BLOB {
        cbData: bytes
            .len()
            .try_into()
            .map_err(|_| "The input is too large for Windows cryptography.")?,
        pbData: bytes.as_ptr().cast_mut(),
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

// DPAPI authenticates this entropy along with the password. A protected value
// copied to a different file or a different signer in the same PFX cannot unlock it.
fn entropy(file_hash: &[u8; 32], thumbprint: &[u8; 20]) -> [u8; 52] {
    let mut value = [0; 52];
    value[..32].copy_from_slice(file_hash);
    value[32..].copy_from_slice(thumbprint);
    value
}

struct DpapiOutput(CRYPT_INTEGER_BLOB);

impl DpapiOutput {
    fn bytes(&self) -> &[u8] {
        if self.0.pbData.is_null() {
            return &[];
        }
        // Successful DPAPI calls allocate cbData bytes with LocalAlloc.
        unsafe { slice::from_raw_parts(self.0.pbData, self.0.cbData as usize) }
    }
}

impl Drop for DpapiOutput {
    fn drop(&mut self) {
        if !self.0.pbData.is_null() {
            unsafe {
                slice::from_raw_parts_mut(self.0.pbData, self.0.cbData as usize).zeroize();
                LocalFree(self.0.pbData.cast());
            }
        }
    }
}

fn protect_password(
    password: &[u16],
    file_hash: &[u8; 32],
    thumbprint: &[u8; 20],
) -> Result<Vec<u8>, String> {
    // A terminator gives DPAPI nonempty input even for a passwordless PFX.
    let mut encoded = Zeroizing::new(Vec::with_capacity((password.len() + 1) * 2));
    for unit in password {
        encoded.extend_from_slice(&unit.to_le_bytes());
    }
    encoded.extend_from_slice(&[0, 0]);
    let binding = entropy(file_hash, thumbprint);
    let mut out = DpapiOutput(CRYPT_INTEGER_BLOB::default());
    if unsafe {
        CryptProtectData(
            &blob(&encoded)?,
            null(),
            &blob(&binding)?,
            null(),
            null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out.0,
        )
    } == 0
    {
        return Err(last_error(
            "Windows could not remember the PFX password for this user.",
        ));
    }
    Ok(out.bytes().to_vec())
}

fn unprotect_password(
    ciphertext: &[u8],
    file_hash: &[u8; 32],
    thumbprint: &[u8; 20],
) -> Result<Password, String> {
    let binding = entropy(file_hash, thumbprint);
    let mut out = DpapiOutput(CRYPT_INTEGER_BLOB::default());
    if unsafe {
        CryptUnprotectData(
            &blob(ciphertext)?,
            null_mut(),
            &blob(&binding)?,
            null(),
            null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out.0,
        )
    } == 0
    {
        return Err(last_error(
            "Windows could not unlock the remembered PFX password.",
        ));
    }
    let invalid = "Invalid remembered PFX password.";
    let bytes = out.bytes().strip_suffix(&[0, 0]).ok_or(invalid)?;
    if !bytes.len().is_multiple_of(2) {
        return Err(invalid.into());
    }
    let mut password = Zeroizing::new(Vec::with_capacity(bytes.len() / 2));
    for &pair in bytes.as_chunks::<2>().0 {
        let unit = u16::from_le_bytes(pair);
        if unit == 0 {
            return Err(invalid.into());
        }
        password.push(unit);
    }
    Ok(password)
}

struct PasswordDialog {
    password: Password,
    allow_remember: bool,
    remember: bool,
}

fn ask_password(owner: HWND, allow_remember: bool) -> Result<Option<PasswordDialog>, String> {
    let mut state = PasswordDialog {
        password: Zeroizing::new(Vec::new()),
        allow_remember,
        remember: false,
    };
    // Dialog 203 owns the masked edit control. The caller's buffer outlives the modal dialog.
    let result = unsafe {
        DialogBoxParamW(
            GetModuleHandleW(null()),
            203usize as *const u16,
            owner,
            Some(password_proc),
            (&raw mut state) as LPARAM,
        )
    };
    match result {
        1 => Ok(Some(state)),
        0 => Ok(None),
        _ => Err(last_error("Could not open the PFX password dialog.")),
    }
}

unsafe extern "system" fn password_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> isize {
    unsafe {
        match msg {
            WM_INITDIALOG => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, lp);
                let state = &*(lp as *const PasswordDialog);
                CheckDlgButton(hwnd, REMEMBER_PASSWORD, BST_UNCHECKED);
                if !state.allow_remember {
                    ShowWindow(GetDlgItem(hwnd, REMEMBER_PASSWORD), SW_HIDE);
                }
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
                let state = &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut PasswordDialog);
                state.remember = state.allow_remember
                    && IsDlgButtonChecked(hwnd, REMEMBER_PASSWORD) == BST_CHECKED;
                let password = &mut state.password;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpapi_round_trip_authenticates_ciphertext_file_and_signer() {
        let hash = [0x5c; 32];
        let thumbprint = [0xba; 20];
        for input in ["", "試験🔑"] {
            let password = Zeroizing::new(input.encode_utf16().collect::<Vec<_>>());
            let mut ciphertext = protect_password(&password, &hash, &thumbprint).unwrap();
            assert!(unprotect_password(&ciphertext, &hash, &thumbprint).unwrap() == password);
            assert!(unprotect_password(&ciphertext, &[0; 32], &thumbprint).is_err());
            assert!(unprotect_password(&ciphertext, &hash, &[0; 20]).is_err());
            ciphertext[0] ^= 0xff;
            assert!(unprotect_password(&ciphertext, &hash, &thumbprint).is_err());
        }
    }

    #[test]
    fn restored_pfx_requires_the_saved_file_and_certificate() {
        let mut bytes = super::super::tests::test_pfx_bytes();
        let password = Zeroizing::new("unit-test".encode_utf16().collect::<Vec<_>>());
        let thumbprint = import(&mut bytes, &password).unwrap()[0].hash;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("temporary.pfx");
        std::fs::write(&path, &*bytes).unwrap();
        let hash = Sha256::digest(&*bytes).into();
        let mut saved = PfxIdentity {
            path: path.clone(),
            name: String::new(),
            thumbprint,
            file_hash: hash,
            protected_password: Some(protect_password(&password, &hash, &thumbprint).unwrap()),
        };
        let (selected, restored) =
            SigningCertificate::pick_pfx_for_gui(null_mut(), &path, Some(&saved))
                .unwrap()
                .unwrap();
        assert_eq!(selected.thumbprint().unwrap(), thumbprint);
        assert_eq!(selected.name, "Temporary PDF Signing Test");
        assert_eq!(restored.protected_password, saved.protected_password);

        let other = dir.path().join("copy.pfx");
        std::fs::write(&other, &*bytes).unwrap();
        assert!(SigningCertificate::pick_pfx_for_gui(null_mut(), &other, Some(&saved)).is_err());
        saved.thumbprint[0] ^= 0xff;
        saved.protected_password =
            Some(protect_password(&password, &hash, &saved.thumbprint).unwrap());
        assert!(SigningCertificate::pick_pfx_for_gui(null_mut(), &path, Some(&saved)).is_err());
        std::fs::write(&path, b"replaced").unwrap();
        assert!(SigningCertificate::pick_pfx_for_gui(null_mut(), &path, Some(&saved)).is_err());
    }
}
