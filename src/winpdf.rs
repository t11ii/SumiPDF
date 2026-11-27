//! Native PDF rendering and source access on background MTA threads.

use std::{marker::PhantomData, path::Path, rc::Rc};

use windows::{
    Data::Pdf::{PdfDocument, PdfPage, PdfPageRenderOptions},
    Foundation::Size,
    Graphics::Imaging::{
        BitmapAlphaMode, BitmapDecoder, BitmapEncoder, BitmapPixelFormat, BitmapTransform,
        ColorManagementMode, ExifOrientationMode,
    },
    Storage::{
        FileAccessMode, StorageOpenOptions,
        Streams::{
            FileOpenDisposition, FileRandomAccessStream, IRandomAccessStream,
            InMemoryRandomAccessStream,
        },
    },
    Win32::{
        Foundation::{E_INVALIDARG, E_OUTOFMEMORY, E_UNEXPECTED},
        System::{
            Com::IStream,
            WinRT::{
                CreateStreamOverRandomAccessStream, RO_INIT_MULTITHREADED, RoInitialize,
                RoUninitialize,
            },
        },
    },
    core::{AgileReference, Array, Error, HSTRING, Result as WinResult},
};

use crate::raster_size::{MAX_PIXELS, bounded_bitmap_size, bounded_decoded_size};

/// WinRT initialization and shutdown must occur on the same thread.
pub struct Apartment(PhantomData<Rc<()>>);

pub fn initialize() -> Result<Apartment, String> {
    unsafe { RoInitialize(RO_INIT_MULTITHREADED) }
        .map_err(|e| format!("Windows の PDF 描画を初期化できませんでした。\n{e}"))?;
    Ok(Apartment(PhantomData))
}

impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { RoUninitialize() }
    }
}

pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    /// Tightly packed BGRA, starting at the top left.
    pub pixels: Array<u8>,
}

pub struct Frame {
    pub bitmap: Bitmap,
    pub ratio: f64,
    pub limited_to: Option<u64>,
}

// DetachPixelData returns owned CoTaskMem containing just bytes. No COM object or
// apartment-bound state moves with it; CoTaskMemFree may run on the UI thread.
unsafe impl Send for Bitmap {}

pub struct Document {
    pdf: PdfDocument,
    // Keep the backing stream available for pages that Windows reads on demand.
    stream: IRandomAccessStream,
    count: u32,
}

/// Share the renderer's exact backing stream; reopening a pathname for signing could
/// resolve to a different file. Acquire document bytes only when asked to sign.
#[derive(Clone)]
pub struct SigningSource(AgileReference<IRandomAccessStream>);

impl SigningSource {
    pub fn read_bytes(&self) -> Result<Vec<u8>, String> {
        let _apartment = initialize()?;
        self.read_stream(None)
            .map_err(|e| format!("編集中の PDF を読み込めません。\n{e}"))
    }

    pub fn read_for_search(&self) -> Result<Vec<u8>, String> {
        let _apartment = initialize()?;
        self.read_stream(Some(256 * 1024 * 1024))
            .map_err(|e| e.message().to_string())
    }

    fn read_stream(&self, search_limit: Option<u64>) -> WinResult<Vec<u8>> {
        let stream = self.0.resolve()?.CloneStream()?;
        stream.Seek(0)?;
        let size = stream.Size()?;
        if search_limit.is_some_and(|limit| size > limit) {
            return Err(Error::new(
                E_INVALIDARG,
                "Find supports PDFs up to 256 MiB.",
            ));
        }
        let length = u32::try_from(size)
            .map_err(|_| Error::new(E_INVALIDARG, "4 GiB 以上の PDF は編集できません。"))?
            as usize;
        let reader: IStream = unsafe { CreateStreamOverRandomAccessStream(&stream)? };
        let mut bytes = Vec::<u8>::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| Error::new(E_OUTOFMEMORY, "PDF の編集に必要なメモリを確保できません。"))?;

        while bytes.len() < length {
            let requested = (length - bytes.len()).min(1024 * 1024) as u32;
            let mut read = 0;
            // SAFETY: capacity covers the complete source. Read initializes at most
            // requested bytes in the spare area and reports how many were written.
            unsafe {
                reader
                    .Read(
                        bytes.spare_capacity_mut().as_mut_ptr().cast(),
                        requested,
                        Some(&mut read),
                    )
                    .ok()?;
                if read == 0 || read > requested {
                    return Err(Error::new(E_UNEXPECTED, "PDF の読み込みが中断されました。"));
                }
                bytes.set_len(bytes.len() + read as usize);
            }
        }
        if stream.Size()? != length as u64 {
            return Err(Error::new(
                E_UNEXPECTED,
                "読み込み中に PDF のサイズが変わりました。",
            ));
        }
        Ok(bytes)
    }
}

impl Document {
    pub fn open_file(path: &Path) -> Result<Self, String> {
        Self::open_stream(path).map_err(|e| {
            if e.code().0 as u32 == 0x8007052B {
                "この PDF を開くにはパスワードが必要です。".to_owned()
            } else {
                format!("PDF を開けませんでした。\n{e}")
            }
        })
    }

    fn open_stream(path: &Path) -> WinResult<Self> {
        let stream = FileRandomAccessStream::OpenWithOptionsAsync(
            &HSTRING::from(path),
            FileAccessMode::Read,
            StorageOpenOptions::AllowOnlyReaders,
            FileOpenDisposition::OpenExisting,
        )?
        .join()?;
        Self::load_stream(stream)
    }

    pub fn from_source(source: &SigningSource) -> Result<Self, String> {
        let stream = source
            .0
            .resolve()
            .and_then(|s| s.CloneStream())
            .map_err(|e| e.to_string())?;
        stream.Seek(0).map_err(|e| e.to_string())?;
        Self::load_stream(stream).map_err(|e| e.to_string())
    }

    fn load_stream(stream: IRandomAccessStream) -> WinResult<Self> {
        let pdf = PdfDocument::LoadFromStreamAsync(&stream)?.join()?;
        let count = pdf.PageCount()?;
        if count == 0 {
            return Err(Error::new(
                E_UNEXPECTED,
                "この PDF にはページがありません。",
            ));
        }
        Ok(Self { pdf, stream, count })
    }

    pub fn page_count(&self) -> u32 {
        self.count
    }

    pub fn signing_source(&self) -> Result<SigningSource, String> {
        AgileReference::new(&self.stream)
            .map(SigningSource)
            .map_err(|e| format!("署名用に PDF を保持できません。\n{e}"))
    }

    pub fn render_fit(&self, index: u32, area: (i32, i32), zoom: f64) -> Result<Bitmap, String> {
        self.render_page(index, area, zoom, None)
            .map(|frame| frame.bitmap)
    }

    pub fn render_page(
        &self,
        index: u32,
        area: (i32, i32),
        zoom: f64,
        cache_pixels: Option<u64>,
    ) -> Result<Frame, String> {
        let page = Page(self.pdf.GetPage(index).map_err(|e| e.to_string())?);
        let size = checked_size(&page.0)?;
        let fit = (f64::from(area.0) / f64::from(size.Width))
            .min(f64::from(area.1) / f64::from(size.Height));
        let width = (f64::from(size.Width) * fit * zoom).round().max(1.0) as u32;
        let budget = cache_pixels.unwrap_or(MAX_PIXELS).clamp(1, MAX_PIXELS);
        let dimensions =
            |cap| bounded_bitmap_size(f64::from(size.Width), f64::from(size.Height), width, cap);
        let (w, h) = dimensions(budget);
        let limited_to =
            (budget < MAX_PIXELS && (w, h) != dimensions(MAX_PIXELS)).then_some(budget);
        let bitmap = rasterize(&page.0, w, h, budget)
            .map_err(|e| format!("{} ページを表示できませんでした。 {e}", index + 1))?;
        Ok(Frame {
            bitmap,
            ratio: f64::from(size.Width) / f64::from(size.Height),
            limited_to,
        })
    }
}

// PdfPage is IClosable. Close it promptly when page navigation or decoding fails.
struct Page(PdfPage);
impl Drop for Page {
    fn drop(&mut self) {
        let _ = self.0.Close();
    }
}

fn checked_size(page: &PdfPage) -> Result<Size, String> {
    let size = page.Size().map_err(|e| e.to_string())?;
    if !size.Width.is_finite()
        || !size.Height.is_finite()
        || size.Width <= 0.0
        || size.Height <= 0.0
    {
        return Err("ページの寸法を読み取れませんでした。".into());
    }
    Ok(size)
}

fn rasterize(page: &PdfPage, width: u32, height: u32, budget: u64) -> WinResult<Bitmap> {
    let stream = InMemoryRandomAccessStream::new()?;
    let options = PdfPageRenderOptions::new()?;
    options.SetDestinationWidth(width)?;
    options.SetDestinationHeight(height)?;
    options.SetBitmapEncoderId(BitmapEncoder::BmpEncoderId()?)?;
    page.RenderWithOptionsToStreamAsync(&stream, &options)?
        .join()?;
    decode_page_bitmap(&stream, (width, height), budget)
}

fn decode_page_bitmap(
    stream: &InMemoryRandomAccessStream,
    requested: (u32, u32),
    budget: u64,
) -> WinResult<Bitmap> {
    stream.Seek(0)?;
    let decoder = BitmapDecoder::CreateAsync(stream)?.join()?;
    let actual = (decoder.PixelWidth()?, decoder.PixelHeight()?);
    let dimensions = || {
        format!(
            "要求 {}×{}、実画像 {}×{} px",
            requested.0, requested.1, actual.0, actual.1
        )
    };
    let (width, height) = bounded_decoded_size(requested, actual, budget).ok_or_else(|| {
        Error::new(
            E_UNEXPECTED,
            format!("ページ画像の寸法がゼロです（{}）。", dimensions()),
        )
    })?;
    let decode = || -> WinResult<Array<u8>> {
        let transform = BitmapTransform::new()?;
        if (width, height) != actual {
            transform.SetScaledWidth(width)?;
            transform.SetScaledHeight(height)?;
        }
        decoder
            .GetPixelDataTransformedAsync(
                BitmapPixelFormat::Bgra8,
                BitmapAlphaMode::Ignore,
                &transform,
                ExifOrientationMode::IgnoreExifOrientation,
                ColorManagementMode::DoNotColorManage,
            )?
            .join()?
            .DetachPixelData()
    };
    let data = decode().map_err(|error| {
        Error::new(
            error.code(),
            format!(
                "ページ画像を展開できません（{}）。 {}",
                dimensions(),
                error.message()
            ),
        )
    })?;
    if data.len() != (u64::from(width) * u64::from(height) * 4) as usize {
        return Err(Error::new(
            E_UNEXPECTED,
            format!(
                "ページ画像のデータ長が一致しません（{}、出力 {width}×{height} px、{} バイト）。",
                dimensions(),
                data.len()
            ),
        ));
    }
    Ok(Bitmap {
        width,
        height,
        pixels: data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn reads_the_real_bmp_rows_when_windows_returns_a_different_size() {
        let _apartment = initialize().unwrap();
        // The second BMP exceeds the edge limit but requires only 128 KiB of test pixels.
        for (w, h, requested, expected) in [
            (201, 100, (200, 100), (201, 100)),
            (8193, 4, (4096, 3000), (4096, 1)),
        ] {
            let mut pixels = vec![255; (w * h * 4) as usize];
            for row in pixels.chunks_exact_mut(w as usize * 4) {
                row.as_chunks_mut::<4>().0[..w as usize / 2].fill([0, 0, 255, 255]);
            }
            let stream = InMemoryRandomAccessStream::new().unwrap();
            let encoder =
                BitmapEncoder::CreateAsync(BitmapEncoder::BmpEncoderId().unwrap(), &stream)
                    .unwrap()
                    .join()
                    .unwrap();
            encoder
                .SetPixelData(
                    BitmapPixelFormat::Bgra8,
                    BitmapAlphaMode::Ignore,
                    w,
                    h,
                    96.0,
                    96.0,
                    &pixels,
                )
                .unwrap();
            encoder.FlushAsync().unwrap().join().unwrap();
            drop(encoder);
            let image = decode_page_bitmap(&stream, requested, MAX_PIXELS).unwrap();
            assert_eq!((image.width, image.height), expected);
            let stride = image.width as usize * 4;
            assert_eq!(image.pixels.len(), stride * image.height as usize);
            for row in image.pixels.chunks_exact(stride) {
                assert_eq!(&row[..3], &[0, 0, 255]);
                assert_eq!(&row[stride - 4..stride - 1], &[255, 255, 255]);
            }
        }
    }

    #[test]
    fn windows_renders_unsigned_graphics_and_text_only_a4_pages_at_fit_and_minimum_zoom() {
        let _apartment = initialize().unwrap();
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
        let path = std::env::temp_dir().join(format!("winPdfTest-文書-{}.pdf", std::process::id()));
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
        let source = document.signing_source().unwrap();
        drop(document);
        let read = std::thread::spawn(move || source.read_bytes())
            .join()
            .unwrap()
            .unwrap();
        assert_eq!(read, pdf);
        std::fs::remove_file(path).unwrap();
    }
}
