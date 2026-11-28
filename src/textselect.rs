//! On-demand MuPDF text geometry in display coordinates.
#[cfg(mupdf_backend)]
mod layout;

#[cfg(mupdf_backend)]
pub(crate) use layout::*;
mod standard;
pub(crate) const HELVETICA_WIDTHS: &[u16; 95] = &standard::HELVETICA;
