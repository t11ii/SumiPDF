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

const MAX_EDGE: u32 = 8192;
const MAX_PIXELS: u64 = 8 * 1024 * 1024;

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
        self.read_stream()
            .map_err(|e| format!("編集中の PDF を読み込めません。\n{e}"))
    }

    fn read_stream(&self) -> WinResult<Vec<u8>> {
        let stream = self.0.resolve()?.CloneStream()?;
        stream.Seek(0)?;
        let length = u32::try_from(stream.Size()?)
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
        let page = Page(self.pdf.GetPage(index).map_err(|e| e.to_string())?);
        let size = checked_size(&page.0)?;
        let fit = (f64::from(area.0) / f64::from(size.Width))
            .min(f64::from(area.1) / f64::from(size.Height));
        let width = (f64::from(size.Width) * fit * zoom).round().max(1.0) as u32;
        let (w, h) = bitmap_size(size, width);
        rasterize(&page.0, w, h).map_err(|e| format!("ページを表示できませんでした。\n{e}"))
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

fn bitmap_size(size: Size, requested_width: u32) -> (u32, u32) {
    let width = f64::from(requested_width.clamp(1, MAX_EDGE));
    let height = width * f64::from(size.Height) / f64::from(size.Width);
    let scale = 1.0_f64
        .min(f64::from(MAX_EDGE) / height)
        .min((MAX_PIXELS as f64 / (width * height)).sqrt());
    // Round down to stay within both caps. Even a very slender page gets one pixel.
    (
        (width * scale).floor().max(1.0) as u32,
        (height * scale).floor().max(1.0) as u32,
    )
}

fn rasterize(page: &PdfPage, width: u32, height: u32) -> WinResult<Bitmap> {
    let stream = InMemoryRandomAccessStream::new()?;
    let options = PdfPageRenderOptions::new()?;
    options.SetDestinationWidth(width)?;
    options.SetDestinationHeight(height)?;
    options.SetBitmapEncoderId(BitmapEncoder::BmpEncoderId()?)?;
    page.RenderWithOptionsToStreamAsync(&stream, &options)?
        .join()?;
    stream.Seek(0)?;
    let decoder = BitmapDecoder::CreateAsync(&stream)?.join()?;
    let (actual_width, actual_height) = (decoder.PixelWidth()?, decoder.PixelHeight()?);
    if actual_width != width || actual_height != height {
        return Err(Error::new(
            E_UNEXPECTED,
            "Windows が予期しない寸法のビットマップを返しました。",
        ));
    }
    let data = decoder
        .GetPixelDataTransformedAsync(
            BitmapPixelFormat::Bgra8,
            BitmapAlphaMode::Ignore,
            &BitmapTransform::new()?,
            ExifOrientationMode::IgnoreExifOrientation,
            ColorManagementMode::DoNotColorManage,
        )?
        .join()?
        .DetachPixelData()?;
    if data.len() != (u64::from(width) * u64::from(height) * 4) as usize {
        return Err(Error::new(
            E_UNEXPECTED,
            "Windows が不完全なビットマップを返しました。",
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
    fn bounds_large_and_extremely_tall_pages_before_rendering() {
        for ratio in [0.0001, 0.1, 1.0, 1.5, 100.0, 1.0e30] {
            let (w, h) = bitmap_size(
                Size {
                    Width: 1.0,
                    Height: ratio,
                },
                u32::MAX,
            );
            assert!((1..=MAX_EDGE).contains(&w) && (1..=MAX_EDGE).contains(&h));
            assert!(u64::from(w) * u64::from(h) <= MAX_PIXELS);
        }
    }

    #[test]
    fn windows_renders_a_red_lower_left_quadrant_into_top_down_bgra() {
        let _apartment = initialize().unwrap();
        let content = "1 0 0 rg 0 0 36 36 re f\n";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 72 72] /Resources << >> /Contents 4 0 R >>".to_owned(),
            format!("<< /Length {} >>\nstream\n{content}endstream", content.len()),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut xref = String::from("xref\n0 5\n0000000000 65535 f \n");
        for (i, object) in objects.iter().enumerate() {
            xref.push_str(&format!("{:010} 00000 n \n", pdf.len()));
            writeln!(pdf, "{} 0 obj\n{object}\nendobj", i + 1).unwrap();
        }
        let offset = pdf.len();
        writeln!(
            pdf,
            "{xref}trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{offset}\n%%EOF"
        )
        .unwrap();
        let path = std::env::temp_dir().join(format!("winPdfTest-文書-{}.pdf", std::process::id()));
        std::fs::write(&path, &pdf).unwrap();
        let document = Document::open_file(&path).unwrap();
        assert_eq!(document.page_count(), 1);
        let image = document.render_fit(0, (160, 128), 1.0).unwrap();
        assert_eq!((image.width, image.height), (128, 128));
        let pixel = |x, y| &image.pixels[(y * 128 + x) * 4..(y * 128 + x) * 4 + 3];
        assert_eq!(pixel(32usize, 96usize), &[0, 0, 255]);
        assert_eq!(pixel(32usize, 32usize), &[255, 255, 255]);
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
