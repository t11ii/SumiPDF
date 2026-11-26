//! Let Windows shape a complete name, including fallback fonts, before embedding its ink.
use super::NameMask;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Graphics::GdiPlus as gdip;

#[derive(Default)]
struct Graphics {
    token: usize,
    bitmap: *mut gdip::GpBitmap,
    dc: *mut gdip::GpGraphics,
    family: *mut gdip::GpFontFamily,
    font: *mut gdip::GpFont,
    brush: *mut gdip::GpSolidFill,
}

impl Drop for Graphics {
    fn drop(&mut self) {
        unsafe {
            if !self.brush.is_null() {
                gdip::GdipDeleteBrush(self.brush.cast());
            }
            if !self.font.is_null() {
                gdip::GdipDeleteFont(self.font);
            }
            if !self.family.is_null() {
                gdip::GdipDeleteFontFamily(self.family);
            }
            if !self.dc.is_null() {
                gdip::GdipDeleteGraphics(self.dc);
            }
            if !self.bitmap.is_null() {
                gdip::GdipDisposeImage(self.bitmap.cast());
            }
            if self.token != 0 {
                gdip::GdiplusShutdown(self.token);
            }
        }
    }
}

fn check(status: gdip::Status) -> Result<(), String> {
    if status == gdip::Ok {
        Ok(())
    } else {
        Err(format!(
            "証明書の氏名を描画できませんでした（GDI+ {status}）。"
        ))
    }
}

pub(super) fn render(name: &str) -> Result<NameMask, String> {
    let text: Vec<u16> = name.encode_utf16().collect();
    let em = (6656.0 / text.len() as f32).clamp(8.0, 120.0);
    let width = ((text.len() + 3) as f32 * em * 1.8).ceil() as usize;
    let height = (em * 3.5 + 8.0).ceil() as usize;
    let mut buffer = vec![0u8; width * height * 4];
    // Declared after the pixel memory and dropped before it can be read or freed.
    let mut g = Graphics::default();
    let startup = gdip::GdiplusStartupInput {
        GdiplusVersion: 1,
        SuppressExternalCodecs: 1,
        ..Default::default()
    };
    unsafe {
        check(gdip::GdiplusStartup(&mut g.token, &startup, null_mut()))?;
        // PixelFormat32bppARGB from GdiplusPixelFormats.h.
        check(gdip::GdipCreateBitmapFromScan0(
            width as i32,
            height as i32,
            (width * 4) as i32,
            0x0026_200a,
            buffer.as_mut_ptr(),
            &mut g.bitmap,
        ))?;
        check(gdip::GdipGetImageGraphicsContext(
            g.bitmap.cast(),
            &mut g.dc,
        ))?;
        check(gdip::GdipGraphicsClear(g.dc, 0xff00_0000))?;
        check(gdip::GdipSetTextRenderingHint(
            g.dc,
            gdip::TextRenderingHintAntiAliasGridFit,
        ))?;

        let jp = name.chars().any(|c| matches!(c as u32, 0x3000..=0x30ff | 0x3400..=0x9fff | 0xf900..=0xfaff | 0xff66..=0xff9f | 0x20000..=0x323af));
        let preferred = if jp { "Yu Gothic UI" } else { "Segoe UI" };
        let face: Vec<u16> = preferred.encode_utf16().chain(Some(0)).collect();
        if gdip::GdipCreateFontFamilyFromName(face.as_ptr(), null_mut(), &mut g.family) != gdip::Ok
        {
            check(gdip::GdipGetGenericFontFamilySansSerif(&mut g.family))?;
        }
        check(gdip::GdipCreateFont(
            g.family,
            em,
            gdip::FontStyleRegular,
            gdip::UnitPixel,
            &mut g.font,
        ))?;
        check(gdip::GdipCreateSolidFill(0xffff_ffff, &mut g.brush))?;
        let origin = gdip::RectF {
            X: 2.0,
            Y: 2.0,
            ..Default::default()
        };
        // A zero-sized layout rectangle is GDI+'s point overload (no wrapping).
        let mut bounds = gdip::RectF::default();
        check(gdip::GdipMeasureString(
            g.dc,
            text.as_ptr(),
            text.len() as i32,
            g.font,
            &origin,
            null(),
            &mut bounds,
            null_mut(),
            null_mut(),
        ))?;
        if bounds.X + bounds.Width > width as f32 || bounds.Y + bounds.Height > height as f32 {
            return Err("証明書の表示名が描画できる範囲を超えています。".into());
        }
        check(gdip::GdipDrawString(
            g.dc,
            text.as_ptr(),
            text.len() as i32,
            g.font,
            &origin,
            null(),
            g.brush.cast(),
        ))?;
    }
    drop(g);

    // Trim the line to visible glyph ink so its center and width remain stable
    // across Windows language packs, font versions and complex-script metrics.
    let (mut left, mut top, mut right, mut bottom) = (width, height, 0, 0);
    let (pixels, _) = buffer.as_chunks::<4>();
    for (i, pixel) in pixels.iter().enumerate() {
        if pixel[1] != 0 {
            let (x, y) = (i % width, i / width);
            left = left.min(x);
            right = right.max(x + 1);
            top = top.min(y);
            bottom = bottom.max(y + 1);
        }
    }
    if left >= right || top >= bottom {
        return Err("証明書の表示名に描画できる文字がありません。".into());
    }
    let mut coverage = Vec::with_capacity((right - left) * (bottom - top));
    for row in pixels.chunks_exact(width).take(bottom).skip(top) {
        coverage.extend(row[left..right].iter().map(|px| px[1]));
    }
    Ok(NameMask {
        width: (right - left) as u32,
        height: (bottom - top) as u32,
        coverage,
    })
}
