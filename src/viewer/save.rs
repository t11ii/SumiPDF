use crate::{
    appearance::Artwork,
    highlight::{self, Mark},
    pdfseal,
    wincrypto::SigningCertificate,
};
use std::{path::Path, sync::Arc};

pub(super) struct Appearance {
    pub target: pdfseal::Target,
    pub watermark: Option<Arc<[u8]>>,
    pub signature_image: Option<Arc<[u8]>>,
}

#[cfg(test)]
#[path = "save_tests.rs"]
mod tests;

pub(super) fn copy(
    source: &crate::winpdf::SigningSource,
    marks: &[Mark],
    signature: Option<(&SigningCertificate, Option<&Appearance>)>,
    destination: &Path,
) -> Result<(), String> {
    let mut bytes = source.read_bytes()?;
    if !marks.is_empty() {
        bytes = highlight::annotate(bytes, marks)?;
    }
    if let Some((cert, appearance)) = signature {
        bytes = match appearance {
            Some(a) => pdfseal::sign_visible(
                bytes,
                &a.target,
                &cert.display_name()?,
                Artwork {
                    watermark: a.watermark.as_deref(),
                    signature_image: a.signature_image.as_deref(),
                },
                |left, right| cert.sign_detached(left, right),
            )?,
            None => pdfseal::sign_pdf(bytes, |left, right| cert.sign_detached(left, right))?,
        };
    }
    crate::write_new(destination, &bytes)
}
