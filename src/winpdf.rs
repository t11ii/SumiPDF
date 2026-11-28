//! MuPDF on background workers. Each document instance is confined to its worker.
use std::{
    ffi::{CStr, c_char, c_int, c_void},
    fs::{File, OpenOptions},
    io::{self, Read},
    path::Path,
    ptr::NonNull,
};

#[cfg(windows)]
use std::os::windows::{
    fs::OpenOptionsExt,
    io::{AsRawHandle, FromRawHandle, IntoRawHandle},
};
#[cfg(windows)]
use std::sync::Arc;
#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{GENERIC_READ, INVALID_HANDLE_VALUE},
    Storage::FileSystem::{FILE_SHARE_READ, ReOpenFile},
};

use crate::{
    highlight::UnitRect,
    raster_size::{FIT_PIXELS, bounded_bitmap_size, fitted_page_size, view_pixel_limit},
};

type ErrorBuffer = [c_char; 512];
type Glyph = unsafe extern "C" fn(*mut c_void, UnitRect, c_int, c_int, c_int) -> c_int;

unsafe extern "C" {
    #[cfg(windows)]
    fn sumi_pdf_open_handle(
        file: *mut c_void,
        count: *mut c_int,
        error: *mut c_char,
    ) -> *mut c_void;
    #[cfg(not(windows))]
    fn sumi_pdf_open_path(
        path: *const c_char,
        count: *mut c_int,
        error: *mut c_char,
    ) -> *mut c_void;
    fn sumi_pdf_drop(doc: *mut c_void);
    fn sumi_pdf_ratio(doc: *mut c_void, page: c_int, ratio: *mut f64, error: *mut c_char) -> c_int;
    fn sumi_pdf_render(
        doc: *mut c_void,
        page: c_int,
        width: c_int,
        height: c_int,
        bgra: *mut u8,
        error: *mut c_char,
    ) -> c_int;
    fn sumi_pdf_text(
        doc: *mut c_void,
        page: c_int,
        output: *mut c_void,
        glyph: Glyph,
        error: *mut c_char,
    ) -> c_int;
}

fn message(error: &ErrorBuffer) -> String {
    // The bridge writes a NUL-terminated message into the zero-initialized buffer.
    unsafe { CStr::from_ptr(error.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    /// Tightly packed BGRA, starting at the top left.
    pub pixels: Vec<u8>,
}

pub struct Frame {
    pub bitmap: Bitmap,
    pub ratio: f64,
    pub limited_to: Option<u64>,
}

/// Pins the exact open file. On Windows the handle denies writes and deletion, and
/// render/search workers reopen the file object by handle without re-resolving the path.
#[derive(Clone)]
pub struct SigningSource {
    #[cfg(windows)]
    file: Arc<File>,
    #[cfg(not(windows))]
    test_path: std::path::PathBuf,
}

impl SigningSource {
    fn open(path: &Path) -> Result<Self, String> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        options.share_mode(FILE_SHARE_READ);
        let _file = options
            .open(path)
            .map_err(|e| format!("PDF を開けませんでした。\n{e}"))?;
        Ok(Self {
            #[cfg(windows)]
            file: Arc::new(_file),
            #[cfg(not(windows))]
            test_path: path.to_path_buf(),
        })
    }

    fn reader(&self) -> io::Result<File> {
        #[cfg(windows)]
        {
            // ReOpenFile keeps the file identity and gives each reader its own cursor.
            let handle =
                unsafe { ReOpenFile(self.file.as_raw_handle(), GENERIC_READ, FILE_SHARE_READ, 0) };
            if handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            Ok(unsafe { File::from_raw_handle(handle) })
        }
        #[cfg(not(windows))]
        File::open(&self.test_path)
    }

    pub fn read_bytes(&self) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        self.reader()
            .and_then(|mut file| file.read_to_end(&mut bytes))
            .map_err(|e| format!("編集中の PDF を読み込めません。\n{e}"))?;
        Ok(bytes)
    }
}

pub struct Document {
    inner: NonNull<c_void>,
    source: SigningSource,
    count: u32,
}

impl Document {
    pub fn open_file(path: &Path) -> Result<Self, String> {
        let source = SigningSource::open(path)?;
        Self::from_source(&source).map_err(|e| format!("PDF を開けませんでした。\n{e}"))
    }

    pub fn from_source(source: &SigningSource) -> Result<Self, String> {
        let mut count = 0;
        let mut error: ErrorBuffer = [0; 512];
        #[cfg(windows)]
        let ptr = {
            let file = source.reader().map_err(|e| e.to_string())?;
            // The bridge takes ownership of the reopened handle, including on error.
            unsafe { sumi_pdf_open_handle(file.into_raw_handle(), &mut count, error.as_mut_ptr()) }
        };
        #[cfg(not(windows))]
        let ptr = {
            use std::{ffi::CString, os::unix::ffi::OsStrExt};
            let path =
                CString::new(source.test_path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
            unsafe { sumi_pdf_open_path(path.as_ptr(), &mut count, error.as_mut_ptr()) }
        };
        Ok(Self {
            inner: NonNull::new(ptr).ok_or_else(|| message(&error))?,
            source: source.clone(),
            count: count as u32,
        })
    }

    pub fn page_count(&self) -> u32 {
        self.count
    }

    pub fn signing_source(&self) -> SigningSource {
        self.source.clone()
    }

    pub fn render_fit(&self, index: u32, area: (i32, i32), zoom: f64) -> Result<Bitmap, String> {
        self.render_page(index, area, zoom, Some(FIT_PIXELS))
            .map(|frame| frame.bitmap)
    }

    pub fn render_page(
        &self,
        index: u32,
        area: (i32, i32),
        zoom: f64,
        cache_pixels: Option<u64>,
    ) -> Result<Frame, String> {
        let page = self.page_index(index)?;
        let mut ratio = 0.0;
        self.call(|doc, error| unsafe { sumi_pdf_ratio(doc, page, &mut ratio, error) })?;
        let (width, height) = fitted_page_size(area, ratio, zoom);
        let requested = (width as u32, height as u32);
        let limit = view_pixel_limit(zoom);
        let budget = cache_pixels.unwrap_or(limit).clamp(1, limit);
        let output = bounded_bitmap_size(requested, budget);
        let fallback = bounded_bitmap_size(requested, budget.min(FIT_PIXELS));
        let bitmap = match self.rasterize(page, output) {
            Err(_) if fallback != output => self.rasterize(page, fallback),
            rendered => rendered,
        }
        .map_err(|e| format!("{} ページを表示できませんでした。 {e}", index + 1))?;
        let full_size = bounded_bitmap_size(requested, limit);
        let limited_to = (output != full_size).then_some(budget);
        Ok(Frame {
            bitmap,
            ratio,
            limited_to,
        })
    }

    fn rasterize(&self, page: c_int, (width, height): (u32, u32)) -> Result<Bitmap, String> {
        let size = width as usize * height as usize * 4;
        let mut pixels = Vec::new();
        pixels.try_reserve_exact(size).map_err(|e| e.to_string())?;
        pixels.resize(size, 255);
        // The bridge renders into the requested-sized, opaque buffer and releases
        // its view of it before returning.
        self.call(|doc, error| unsafe {
            sumi_pdf_render(
                doc,
                page,
                width as c_int,
                height as c_int,
                pixels.as_mut_ptr(),
                error,
            )
        })?;
        Ok(Bitmap {
            width,
            height,
            pixels,
        })
    }

    pub fn glyphs(
        &self,
        index: u32,
        mut visit: impl FnMut(UnitRect, Option<char>, c_int, crate::highlight::Direction) -> bool,
    ) -> Result<(), String> {
        use crate::highlight::Direction;

        unsafe extern "C" fn glyph<F: FnMut(UnitRect, Option<char>, c_int, Direction) -> bool>(
            visitor: *mut c_void,
            rect: UnitRect,
            unicode: c_int,
            line: c_int,
            direction: c_int,
        ) -> c_int {
            let direction = match direction {
                1 => Direction::Down,
                2 => Direction::Left,
                3 => Direction::Up,
                _ => Direction::Right,
            };
            let ch = char::from_u32(unicode as u32).filter(|&ch| ch != '\0' && ch != '\u{fffd}');
            i32::from(unsafe { &mut *visitor.cast::<F>() }(
                rect, ch, line, direction,
            ))
        }

        fn run<F: FnMut(UnitRect, Option<char>, c_int, Direction) -> bool>(
            doc: &Document,
            index: c_int,
            visitor: &mut F,
        ) -> Result<(), String> {
            // The bridge calls this visitor synchronously and contains MuPDF's
            // exception handling on the C side.
            doc.call(|doc, error| unsafe {
                sumi_pdf_text(
                    doc,
                    index,
                    std::ptr::from_mut(visitor).cast(),
                    glyph::<F>,
                    error,
                )
            })
        }

        run(self, self.page_index(index)?, &mut visit)
    }

    fn page_index(&self, index: u32) -> Result<c_int, String> {
        if index >= self.count {
            return Err("ページ番号が範囲外です。".into());
        }
        Ok(index as c_int)
    }

    fn call(&self, f: impl FnOnce(*mut c_void, *mut c_char) -> c_int) -> Result<(), String> {
        let mut error: ErrorBuffer = [0; 512];
        if f(self.inner.as_ptr(), error.as_mut_ptr()) == 0 {
            return Err(message(&error));
        }
        Ok(())
    }
}

impl Drop for Document {
    fn drop(&mut self) {
        unsafe { sumi_pdf_drop(self.inner.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn renders_graphics_and_antialiased_text_at_fit_and_zoom() {
        let content = "1 0 0 rg 0 0 297.64 420.945 re f\n";
        let text = "BT /F1 36 Tf 64 700 Td (Text-only PDF) Tj ET\n";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595.28 841.89] /Resources << >> /Contents 4 0 R >>".to_owned(),
            format!("<< /Length {} >>\nstream\n{content}endstream", content.len()),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595.28 841.89] /Resources << /Font << /F1 7 0 R >> >> /Contents 6 0 R >>".to_owned(),
            format!("<< /Length {} >>\nstream\n{text}endstream", text.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let size = objects.len() + 1;
        let mut xref = format!("xref\n0 {size}\n0000000000 65535 f \n");
        for (i, object) in objects.iter().enumerate() {
            xref.push_str(&format!("{:010} 00000 n \n", pdf.len()));
            writeln!(pdf, "{} 0 obj\n{object}\nendobj", i + 1).unwrap();
        }
        let offset = pdf.len();
        writeln!(
            pdf,
            "{xref}trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{offset}\n%%EOF"
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("文書.pdf");
        std::fs::write(&path, &pdf).unwrap();
        let document = Document::open_file(&path).unwrap();
        assert_eq!(document.page_count(), 2);
        for zoom in [1.0, 0.5] {
            let image = document.render_fit(0, (721, 850), zoom).unwrap();
            let (w, h) = (image.width as usize, image.height as usize);
            assert!(w > 0 && h > w);
            assert!((w as f64 / h as f64 - 595.28 / 841.89).abs() < 0.01);
            let pixel = |x, y| &image.pixels[(y * w + x) * 4..(y * w + x) * 4 + 3];
            assert_eq!(pixel(w / 4, 3 * h / 4), &[0, 0, 255]);
            assert_eq!(pixel(w / 4, h / 4), &[255, 255, 255]);
            let paper = document.render_fit(1, (721, 850), zoom).unwrap();
            let top_half = &paper.pixels[..(paper.height / 2 * paper.width * 4) as usize];
            assert!(
                top_half
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .filter(|p| p[0] < 100 && p[1] < 100 && p[2] < 100)
                    .count()
                    > 50
            );
        }
        let zoom = 4.0;
        let limit = view_pixel_limit(zoom);
        let detailed = document
            .render_page(1, (900, 1000), zoom, Some(limit))
            .unwrap();
        assert!((detailed.ratio - 595.28 / 841.89).abs() < 0.001);
        assert!(detailed.limited_to.is_none());
        let pixels = u64::from(detailed.bitmap.width) * u64::from(detailed.bitmap.height);
        assert!(
            (FIT_PIXELS + 1..=limit).contains(&pixels),
            "high-zoom bitmap: {pixels} pixels"
        );
        assert!(
            detailed
                .bitmap
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[..3] == [0, 0, 0])
        );
        let source = document.signing_source();
        drop(document);
        let read = std::thread::spawn(move || source.read_bytes())
            .join()
            .unwrap()
            .unwrap();
        assert_eq!(read, pdf);
    }
}
