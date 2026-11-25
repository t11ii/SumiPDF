use crate::{
    highlight::{self, Mark},
    pdfseal,
    wincrypto::SigningCertificate,
};
use std::{io::Write, path::Path};

#[cfg(test)]
#[path = "save_tests.rs"]
mod tests;

pub fn copy(
    source: &crate::winpdf::SigningSource,
    marks: &[Mark],
    cert: Option<&SigningCertificate>,
    destination: &Path,
) -> Result<(), String> {
    let mut bytes = source.read_bytes()?;
    if !marks.is_empty() {
        bytes = highlight::annotate(bytes, marks)?;
    }
    if let Some(cert) = cert {
        bytes = pdfseal::sign_pdf(bytes, |a, b| cert.sign_detached(a, b))?;
    }
    let folder = destination
        .parent()
        .ok_or("保存先フォルダーを取得できません。")?;
    let mut tmp = tempfile::NamedTempFile::new_in(folder)
        .map_err(|e| format!("一時ファイルを作成できません。\n{e}"))?;
    tmp.write_all(&bytes)
        .and_then(|_| tmp.as_file().sync_all())
        .map_err(|e| format!("PDFを書き込めません。\n{e}"))?;
    // An intervening writer, a hard link, or a case-insensitive alias can
    // never turn this save-as operation into an overwrite of another file.
    tmp.persist_noclobber(destination)
        .map_err(|e| format!("新しい名前で保存できません。\n{}", e.error))?;
    Ok(())
}
