//! Detached CMS signatures with a key held by a Windows CSP or KSP.

use std::collections::HashSet;
use std::ffi::CStr;
use std::io;
use std::mem::size_of;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{
    CRYPT_E_NOT_FOUND, ERROR_ACCESS_DENIED, ERROR_CANCELLED, ERROR_NO_MORE_FILES, FILETIME,
    GetLastError, HWND, NTE_BAD_KEYSET, NTE_NOT_FOUND, NTE_PERM, NTE_USER_CANCELLED,
    SCARD_W_CANCELLED_BY_USER,
};
use windows_sys::Win32::Security::Cryptography::UI::CryptUIDlgSelectCertificateFromStore;
use windows_sys::Win32::Security::Cryptography::*;
use windows_sys::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows_sys::core::w;

const ENCODING: u32 = X509_ASN_ENCODING | PKCS_7_ASN_ENCODING;

fn last_error(action: &str) -> String {
    format!("{action}\n{}", io::Error::last_os_error())
}

struct Store(HCERTSTORE);

impl Drop for Store {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CertCloseStore(self.0, 0) };
        }
    }
}

pub(crate) struct SigningCertificate(*mut CERT_CONTEXT);

// SAFETY: CryptoAPI owns this thread-independent, reference-counted context. Each
// instance holds exactly one reference. Moving it transfers that reference and the
// cached provider handle, and our API never shares either between signing threads.
unsafe impl Send for SigningCertificate {}

impl Drop for SigningCertificate {
    fn drop(&mut self) {
        unsafe { CertFreeCertificateContext(self.0) };
    }
}

struct Candidate {
    certificate: SigningCertificate,
    hash: [u8; 20],
    store: &'static str,
}

pub(crate) struct CertificateSelection {
    pub name: String,
    pub hash: [u8; 20],
}

impl SigningCertificate {
    // Enumerate only personal stores and leave all private keys with their providers.
    fn candidates() -> Result<Vec<Candidate>, String> {
        let mut found = Vec::new();
        let mut opened = false;
        let mut errors = Vec::new();
        for (scope, store) in [
            (CERT_SYSTEM_STORE_CURRENT_USER, r"CurrentUser\My"),
            (CERT_SYSTEM_STORE_LOCAL_MACHINE, r"LocalMachine\My"),
        ] {
            let personal = Store(unsafe {
                CertOpenStore(
                    CERT_STORE_PROV_SYSTEM_W,
                    0,
                    0,
                    scope
                        | CERT_STORE_READONLY_FLAG
                        | CERT_STORE_OPEN_EXISTING_FLAG
                        | CERT_STORE_DEFER_CLOSE_UNTIL_LAST_FREE_FLAG,
                    w!("MY").cast(),
                )
            });
            if personal.0.is_null() {
                errors.push(format!("{store}: {}", io::Error::last_os_error()));
                continue;
            }
            opened = true;
            let mut current = null_mut();
            loop {
                current = unsafe { CertEnumCertificatesInStore(personal.0, current) };
                if current.is_null() {
                    let code = unsafe { GetLastError() };
                    if code != CRYPT_E_NOT_FOUND as u32 && code != ERROR_NO_MORE_FILES {
                        return Err(last_error("証明書を一覧表示できません。"));
                    }
                    break;
                }
                if permits_signing(current) {
                    // Enumeration consumes the original reference on the next call.
                    let certificate = Self(unsafe { CertDuplicateCertificateContext(current) });
                    let hash = match certificate.thumbprint() {
                        Ok(hash) => hash,
                        Err(e) => {
                            unsafe {
                                CertFreeCertificateContext(current);
                            }
                            return Err(e);
                        }
                    };
                    found.push(Candidate {
                        certificate,
                        hash,
                        store,
                    });
                }
            }
        }
        if !opened {
            return Err(format!(
                "Windows の個人証明書ストアを開けません。\n{}",
                errors.join("\n")
            ));
        }
        Ok(found)
    }

    pub(crate) fn pick(owner: HWND) -> Result<Option<CertificateSelection>, String> {
        let candidates = Self::candidates()?;
        if candidates.is_empty() {
            return Err("Windows の「現在のユーザー」と「ローカル コンピューター」の読み取り可能な個人ストアに、有効な秘密鍵付きの文書署名用証明書がありません。IC カードを使う場合はカードを挿入してから再検索してください。".into());
        }
        let picker = Store(unsafe {
            CertOpenStore(
                CERT_STORE_PROV_MEMORY,
                0,
                0,
                CERT_STORE_DEFER_CLOSE_UNTIL_LAST_FREE_FLAG,
                null(),
            )
        });
        if picker.0.is_null() {
            return Err(last_error("証明書を選択できません。"));
        }
        let mut linked = HashSet::new();
        for candidate in &candidates {
            if !linked.insert(candidate.hash) {
                continue;
            }
            if unsafe {
                CertAddCertificateLinkToStore(
                    picker.0,
                    candidate.certificate.0,
                    CERT_STORE_ADD_ALWAYS,
                    null_mut(),
                )
            } == 0
            {
                return Err(last_error("署名用証明書を一覧に追加できません。"));
            }
        }
        let selected = unsafe {
            CryptUIDlgSelectCertificateFromStore(
                picker.0,
                owner,
                w!("Windows 証明書ストアから選ぶ"),
                w!(
                    "現在のユーザーとローカル コンピューターの個人ストアから、この PDF の署名者を選択してください。"
                ),
                0,
                0,
                null(),
            )
        };
        if selected.is_null() {
            return Ok(None);
        }
        let cert = Self(selected);
        Ok(Some(CertificateSelection {
            name: cert.display_name()?,
            hash: cert.thumbprint()?,
        }))
    }

    pub(crate) fn choose(owner: HWND) -> Result<Option<Self>, String> {
        match Self::pick(owner)? {
            Some(choice) => Self::by_thumbprint(choice.hash, owner),
            None => Ok(None),
        }
    }

    pub(crate) fn display_name(&self) -> Result<String, String> {
        let read = |buf: *mut u16, len| unsafe {
            CertGetNameStringW(self.0, CERT_NAME_SIMPLE_DISPLAY_TYPE, 0, null(), buf, len)
        };
        let size = read(null_mut(), 0);
        if size <= 1 {
            return Err("証明書に署名者名がありません。".into());
        }
        let mut name = vec![0; size as usize];
        if read(name.as_mut_ptr(), size) != size {
            return Err(last_error("証明書の署名者名を読み込めません。"));
        }
        String::from_utf16(&name[..size as usize - 1]).map_err(|e| e.to_string())
    }

    fn thumbprint(&self) -> Result<[u8; 20], String> {
        let mut hash = [0u8; 20];
        let mut size = hash.len() as u32;
        if unsafe {
            CertGetCertificateContextProperty(
                self.0,
                CERT_SHA1_HASH_PROP_ID,
                hash.as_mut_ptr().cast(),
                &mut size,
            )
        } == 0
            || size != 20
        {
            return Err(last_error("証明書の拇印を読み込めません。"));
        }
        Ok(hash)
    }

    pub(crate) fn list() -> Result<Vec<serde_json::Value>, String> {
        let mut seen = HashSet::new();
        let mut result = Vec::new();
        for candidate in Self::candidates()? {
            if seen.insert(candidate.hash) {
                let fingerprint: String =
                    candidate.hash.iter().map(|b| format!("{b:02X}")).collect();
                result.push(
                    serde_json::json!({"name":candidate.certificate.display_name()?,
                            "thumbprint":fingerprint, "store":candidate.store}),
                );
            }
        }
        Ok(result)
    }

    pub(crate) fn by_thumbprint(hash: [u8; 20], owner: HWND) -> Result<Option<Self>, String> {
        let mut matches = Self::candidates()?
            .into_iter()
            .filter(|c| c.hash == hash)
            .peekable();
        while let Some(candidate) = matches.next() {
            match candidate.certificate.acquire(owner) {
                Ok(selected) => return Ok(selected),
                Err(e) => {
                    // The same identity may have an inaccessible key in one store.
                    // Never retry a cancellation or PIN failure against a second provider.
                    if matches.peek().is_some()
                        && matches!(e.raw_os_error(),
                        Some(code) if [ERROR_ACCESS_DENIED, NTE_BAD_KEYSET as u32,
                                      NTE_NOT_FOUND as u32, NTE_PERM as u32].contains(&(code as u32)))
                    {
                        continue;
                    }
                    return Err(format!(
                        "{} の秘密鍵を使用できません。IC カードの接続や鍵へのアクセスを確認してください。\n{e}",
                        candidate.store
                    ));
                }
            }
        }
        Err("選択した署名用証明書が Windows の個人ストアに見つかりません。証明書を選び直してください。".into())
    }

    fn acquire(self, owner: HWND) -> Result<Option<Self>, io::Error> {
        // Acquire/cache only the selected key. CACHE_FLAG makes the cert own the resulting
        // handle. The parent HWND lets the provider display its PIN prompt over the viewer.
        let mut key = 0;
        let mut key_spec = 0;
        let mut caller_frees = 0;
        if unsafe {
            CryptAcquireCertificatePrivateKey(
                self.0,
                CRYPT_ACQUIRE_CACHE_FLAG
                    | CRYPT_ACQUIRE_ALLOW_NCRYPT_KEY_FLAG
                    | CRYPT_ACQUIRE_NO_HEALING
                    | if owner.is_null() {
                        0
                    } else {
                        CRYPT_ACQUIRE_WINDOW_HANDLE_FLAG
                    },
                if owner.is_null() {
                    null()
                } else {
                    (&raw const owner).cast()
                },
                &mut key,
                &mut key_spec,
                &mut caller_frees,
            )
        } == 0
        {
            let code = unsafe { GetLastError() };
            if [
                ERROR_CANCELLED,
                NTE_USER_CANCELLED as u32,
                SCARD_W_CANCELLED_BY_USER as u32,
            ]
            .contains(&code)
            {
                return Ok(None);
            }
            return Err(io::Error::from_raw_os_error(code as i32));
        }
        Ok(Some(self))
    }

    pub(crate) fn sign_detached(&self, first: &[u8], second: &[u8]) -> Result<Vec<u8>, String> {
        let data = [first.as_ptr(), second.as_ptr()];
        let lengths = range_lengths(first, second)?;

        let chain = LocalChain::for_signer(self);
        let mut certs = vec![self.0];
        if let Some(chain) = chain.as_ref() {
            chain.append_intermediates(&mut certs);
        }

        let mut time = FILETIME::default();
        let mut encoded_time = [0u8; 32];
        let mut time_len = encoded_time.len() as u32;
        unsafe { GetSystemTimeAsFileTime(&mut time) };
        if unsafe {
            CryptEncodeObject(
                X509_ASN_ENCODING,
                X509_CHOICE_OF_TIME,
                (&raw const time).cast(),
                encoded_time.as_mut_ptr(),
                &mut time_len,
            )
        } == 0
        {
            return Err(last_error("署名日時を記録できません。"));
        }
        let mut time_blob = CRYPT_INTEGER_BLOB {
            cbData: time_len,
            pbData: encoded_time.as_mut_ptr(),
        };
        // With any authenticated attribute supplied, Windows adds contentType and messageDigest.
        let mut time_attr = CRYPT_ATTRIBUTE {
            pszObjId: szOID_RSA_signingTime.cast_mut(),
            cValue: 1,
            rgValue: &mut time_blob,
        };
        let params = CRYPT_SIGN_MESSAGE_PARA {
            cbSize: size_of::<CRYPT_SIGN_MESSAGE_PARA>() as u32,
            dwMsgEncodingType: ENCODING,
            pSigningCert: self.0,
            HashAlgorithm: CRYPT_ALGORITHM_IDENTIFIER {
                pszObjId: szOID_NIST_sha256.cast_mut(),
                ..Default::default()
            },
            cMsgCert: certs.len() as u32,
            rgpMsgCert: certs.as_mut_ptr(),
            cAuthAttr: 1,
            rgAuthAttr: &mut time_attr,
            ..Default::default()
        };
        let mut cms_len = 0;
        if unsafe {
            CryptSignMessage(
                &params,
                1,
                2,
                data.as_ptr(),
                lengths.as_ptr(),
                null_mut(),
                &mut cms_len,
            )
        } == 0
        {
            return Err(last_error("署名情報を作成できません。"));
        }
        let mut cms = vec![0; cms_len as usize];
        if unsafe {
            CryptSignMessage(
                &params,
                1,
                2,
                data.as_ptr(),
                lengths.as_ptr(),
                cms.as_mut_ptr(),
                &mut cms_len,
            )
        } == 0
        {
            return Err(last_error("電子署名を作成できません。"));
        }
        cms.truncate(cms_len as usize);
        verify_detached(&cms, first, second)?;
        Ok(cms)
    }
}

fn range_lengths(first: &[u8], second: &[u8]) -> Result<[u32; 2], String> {
    const TOO_LARGE: &str = "Windows の署名サイズ上限を超えています。";
    Ok([
        u32::try_from(first.len()).map_err(|_| TOO_LARGE)?,
        u32::try_from(second.len()).map_err(|_| TOO_LARGE)?,
    ])
}

pub(crate) fn verify_detached(cms: &[u8], first: &[u8], second: &[u8]) -> Result<(), String> {
    let data = [first.as_ptr(), second.as_ptr()];
    let lengths = range_lengths(first, second)?;
    let params = CRYPT_VERIFY_MESSAGE_PARA {
        cbSize: size_of::<CRYPT_VERIFY_MESSAGE_PARA>() as u32,
        dwMsgAndCertEncodingType: ENCODING,
        ..Default::default()
    };
    if unsafe {
        CryptVerifyDetachedMessageSignature(
            &params,
            0,
            cms.as_ptr(),
            cms.len() as u32,
            2,
            data.as_ptr(),
            lengths.as_ptr(),
            null_mut(),
        )
    } == 0
    {
        return Err(last_error("作成した電子署名を検証できません。"));
    }
    Ok(())
}

fn permits_signing(cert: *const CERT_CONTEXT) -> bool {
    let info = unsafe { (*cert).pCertInfo };
    if unsafe { CertVerifyTimeValidity(null(), info) } != 0 {
        return false;
    }
    let mut usage = [0u8; 2];
    if unsafe { CertGetIntendedKeyUsage(X509_ASN_ENCODING, info, usage.as_mut_ptr(), 2) } != 0 {
        let signing_bits =
            (CERT_DIGITAL_SIGNATURE_KEY_USAGE | CERT_NON_REPUDIATION_KEY_USAGE) as u8;
        if usage[0] & signing_bits == 0 {
            return false;
        }
    } else if unsafe { GetLastError() } != 0 {
        return false; // No KeyUsage extension is unrestricted. A decoding error is not.
    }
    let has_key = [CERT_KEY_PROV_INFO_PROP_ID, CERT_KEY_CONTEXT_PROP_ID]
        .iter()
        .any(|id| {
            let mut size = 0;
            unsafe { CertGetCertificateContextProperty(cert, *id, null_mut(), &mut size) != 0 }
        });
    has_key && permits_document_eku(cert)
}

fn permits_document_eku(cert: *const CERT_CONTEXT) -> bool {
    let mut bytes = 0;
    let mut count = 0;
    if unsafe { CertGetValidUsages(1, &cert, &mut count, null_mut(), &mut bytes) } == 0 {
        return false;
    }
    if count == -1 {
        return true;
    }
    if bytes == 0 {
        return false;
    }
    // Windows writes both an aligned pointer array and its NUL-terminated strings here.
    let slots = (bytes as usize).div_ceil(size_of::<*mut u8>());
    let mut usages = vec![null_mut(); slots];
    if unsafe { CertGetValidUsages(1, &cert, &mut count, usages.as_mut_ptr(), &mut bytes) } == 0 {
        return false;
    }
    if count == -1 {
        return true;
    }
    usages.iter().take(count as usize).any(|oid| {
        if oid.is_null() {
            return false;
        }
        matches!(
            unsafe { CStr::from_ptr((*oid).cast()) }.to_bytes(),
            b"1.3.6.1.5.5.7.3.36" |          // RFC 9336 documentSigning
            b"1.3.6.1.4.1.311.10.3.12" |     // Microsoft document signing
            b"1.2.840.113583.1.1.5" |        // Adobe authentic documents
            b"2.5.29.37.0"
        ) // anyExtendedKeyUsage
    })
}

struct LocalChain(*mut CERT_CHAIN_CONTEXT);

impl LocalChain {
    fn for_signer(cert: &SigningCertificate) -> Option<Self> {
        let params = CERT_CHAIN_PARA {
            cbSize: size_of::<CERT_CHAIN_PARA>() as u32,
            ..Default::default()
        };
        let mut chain = null_mut();
        let flags = CERT_CHAIN_CACHE_ONLY_URL_RETRIEVAL | CERT_CHAIN_DISABLE_AUTH_ROOT_AUTO_UPDATE;
        let ok = unsafe {
            CertGetCertificateChain(
                null_mut(),
                cert.0,
                null(),
                null_mut(),
                &params,
                flags,
                null(),
                &mut chain,
            )
        };
        (ok != 0).then(|| Self(chain))
    }

    fn append_intermediates(&self, certs: &mut Vec<*mut CERT_CONTEXT>) {
        // Windows returns the signer's chain first, ordered leaf to root.
        let context = unsafe { &*self.0 };
        if context.cChain == 0 {
            return;
        }
        let simple = unsafe { &**context.rgpChain };
        for i in 1..simple.cElement as usize {
            let element = unsafe { &**simple.rgpElement.add(i) };
            if element.TrustStatus.dwInfoStatus & CERT_TRUST_IS_SELF_SIGNED == 0 {
                certs.push(element.pCertContext.cast_mut());
            }
        }
    }
}

impl Drop for LocalChain {
    fn drop(&mut self) {
        unsafe { CertFreeCertificateChain(self.0) };
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::process::Command;

    pub(crate) fn with_test_certificate(test: impl FnOnce(SigningCertificate)) {
        // Generate a disposable RSA identity in memory. No personal store or file is touched.
        let script = r#"
$ErrorActionPreference = 'Stop'
$key = [System.Security.Cryptography.RSACng]::new(2048)
$req = [System.Security.Cryptography.X509Certificates.CertificateRequest]::new(
    'CN=Temporary PDF Signing Test', $key,
    [System.Security.Cryptography.HashAlgorithmName]::SHA256,
    [System.Security.Cryptography.RSASignaturePadding]::Pkcs1)
$cert = $req.CreateSelfSigned([DateTimeOffset]::UtcNow.AddMinutes(-1), [DateTimeOffset]::UtcNow.AddDays(1))
try {
    $pfx = $cert.Export([System.Security.Cryptography.X509Certificates.X509ContentType]::Pfx, 'unit-test')
    [Console]::OpenStandardOutput().Write($pfx, 0, $pfx.Length)
} finally { $cert.Dispose(); $key.Dispose() }
"#;
        let generated = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .output()
            .expect("launch Windows PowerShell");
        assert!(
            generated.status.success(),
            "{}",
            String::from_utf8_lossy(&generated.stderr)
        );
        let mut bytes = generated.stdout;
        let blob = CRYPT_INTEGER_BLOB {
            cbData: bytes.len() as u32,
            pbData: bytes.as_mut_ptr(),
        };
        let store = Store(unsafe {
            PFXImportCertStore(
                &blob,
                w!("unit-test"),
                PKCS12_NO_PERSIST_KEY | PKCS12_ALWAYS_CNG_KSP,
            )
        });
        assert!(!store.0.is_null(), "{}", last_error("PFXImportCertStore"));
        let cert = SigningCertificate(unsafe { CertEnumCertificatesInStore(store.0, null()) });
        assert!(!cert.0.is_null());
        assert!(permits_signing(cert.0));
        test(cert);
    }

    #[test]
    fn windows_cms_detects_a_changed_second_byte_range() {
        with_test_certificate(|cert| {
            let first = b"left";
            let second = b"right";
            let cms = cert.sign_detached(first, second).unwrap();
            assert!(verify_detached(&cms, b"left", b"right").is_ok());
            assert!(verify_detached(&cms, b"leftrigh", b"t").is_ok());
            assert!(verify_detached(&cms, b"left", b"Right").is_err());
        });
    }
}
