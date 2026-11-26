//! Normal appearance of a visible PDF approval signature, in upright display coordinates.
use crate::pdfseal::pdf_error;
use crate::textselect::HELVETICA_WIDTHS;
use lopdf::{Document, Object, ObjectId, Stream, dictionary};
use std::fmt::Write;

mod watermark;
#[cfg(windows)]
mod windows_text;

pub use watermark::{MAX_WATERMARK_BYTES, validate_watermark};

struct NameMask {
    width: u32,
    height: u32,
    coverage: Vec<u8>,
}

pub fn make(
    doc: &mut Document,
    width: f32,
    height: f32,
    name: &str,
    date: &str,
    watermark: Option<&[u8]>,
) -> Result<ObjectId, String> {
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return Err("署名欄の幅または高さが不正です。".into());
    }
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() || name.encode_utf16().count() > 1024 || name.chars().any(char::is_control) {
        return Err("証明書の表示名が空、長すぎる、または制御文字が含まれています。".into());
    }
    let date = display_date(date)?;
    let mask = if name.is_ascii() {
        None
    } else {
        #[cfg(windows)]
        {
            Some(windows_text::render(&name)?)
        }
        #[cfg(not(windows))]
        {
            return Err("証明書の表示名の描画にはWindowsのフォントが必要です。".into());
        }
    };
    draw(doc, width, height, &name, mask, &date, watermark)
}

fn display_date(date: &str) -> Result<String, String> {
    if let Some(digits) = date.strip_prefix("D:").and_then(|s| s.strip_suffix('Z'))
        && digits.len() == 14
        && digits.bytes().all(|b| b.is_ascii_digit())
    {
        return Ok(format!(
            "{}-{}-{} {}:{}:{} UTC",
            &digits[..4],
            &digits[4..6],
            &digits[6..8],
            &digits[8..10],
            &digits[10..12],
            &digits[12..14]
        ));
    }
    if date.is_empty() || date.len() > 96 || !date.bytes().all(|b| (32..=126).contains(&b)) {
        return Err("署名日時の表記が不正です。".into());
    }
    Ok(date.to_owned())
}

fn draw(
    doc: &mut Document,
    width: f32,
    height: f32,
    name: &str,
    mask: Option<NameMask>,
    date: &str,
    watermark: Option<&[u8]>,
) -> Result<ObjectId, String> {
    let mut objects = lopdf::Dictionary::new();
    let mut content = String::from("q /Faint gs\n");
    let graphic_ratio = if let Some(bytes) = watermark {
        let png = watermark::decode(bytes)?;
        let ratio = png.width as f32 / png.height as f32;
        objects.set("Watermark", png.embed(doc)?);
        ratio
    } else {
        1.0
    };
    let graphic_h = (height * 0.9).min(width * 0.72 / graphic_ratio);
    let graphic_w = graphic_h * graphic_ratio;
    let gx = (width - graphic_w) / 2.0;
    let gy = (height - graphic_h) / 2.0;
    writeln!(
        content,
        "{graphic_w:.4} 0 0 {graphic_h:.4} {gx:.4} {gy:.4} cm"
    )
    .unwrap();
    if watermark.is_some() {
        content.push_str("/Watermark Do\n");
    } else {
        // Two overlapping folded sheets. Coordinates stay in a unit square so a
        // custom image and the built-in mark share the same placement rules.
        content.push_str(
            "0.12 0.40 0.57 RG 0.028 w 1 j 1 J\n\
            0.17 0.23 m 0.17 0.82 l 0.55 0.94 l 0.72 0.80 l 0.72 0.67 l S\n\
            0.55 0.94 m 0.55 0.76 l 0.72 0.80 l S\n\
            0.29 0.08 m 0.29 0.66 l 0.69 0.77 l 0.86 0.62 l 0.86 0.24 l h S\n\
            0.69 0.77 m 0.69 0.58 l 0.86 0.62 l S\n",
        );
    }
    content.push_str("Q\n");

    // A tall dragged rectangle keeps its text centered, a very wide one keeps
    // its font size proportional to height. All text fits its own column.
    let band = height.min(width / 2.8);
    let mid = height / 2.0;
    let padding = (band * 0.12).min(width * 0.045);
    let right = width * 0.54;
    let left_w = right - 2.0 * padding;
    let right_w = width - right - padding;
    let positions = [
        (padding, mid, left_w, band * 0.40),
        (right, mid, right_w, band * 0.17),
    ];
    content.push_str("0.09 0.12 0.15 rg\n");
    if let Some(mask) = mask {
        let ratio = mask.width as f32 / mask.height as f32;
        // The bitmap is cropped to its ink. A separate soft mask preserves
        // anti-aliasing on transparent fields, even above page artwork.
        let alpha = image(
            doc,
            mask.width,
            mask.height,
            "DeviceGray",
            mask.coverage,
            None,
        )?;
        let ink = vec![25; mask.width as usize * mask.height as usize];
        objects.set(
            "Signer",
            image(doc, mask.width, mask.height, "DeviceGray", ink, Some(alpha))?,
        );
        let utf16 = [0xfe, 0xff]
            .into_iter()
            .chain(name.encode_utf16().flat_map(u16::to_be_bytes));
        let actual = hex(utf16);
        for (x, y, max_w, em) in positions {
            let h = (em * 0.86).min(max_w / ratio);
            let w = h * ratio;
            let bottom = y - h / 2.0;
            // Text extractors need a text-showing operation to associate with
            // ActualText. Its invisible advance covers the painted name exactly.
            writeln!(
                content,
                "q {w:.4} 0 0 {h:.4} {x:.4} {bottom:.4} cm /Signer Do"
            )
            .unwrap();
            writeln!(content, "/Span << /ActualText <{actual}> >> BDC BT /F1 1 Tf 3 Tr 1.2 0 0 1 0 0.1 Tm (M) Tj ET EMC Q").unwrap();
        }
    } else {
        for (x, y, max_w, em) in positions {
            text_line(&mut content, name, x, y, max_w, em);
        }
    }
    content.push_str("0.21 0.25 0.29 rg\n");
    text_line(
        &mut content,
        "Digitally signed by",
        right,
        mid + band * 0.215,
        right_w,
        band * 0.13,
    );
    text_line(
        &mut content,
        &format!("Date: {date}"),
        right,
        mid - band * 0.215,
        right_w,
        band * 0.115,
    );

    let resources = dictionary! {
        "Font" => dictionary! {"F1" => dictionary! {"Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica"}},
        "ExtGState" => dictionary! {"Faint" => dictionary! {"Type" => "ExtGState", "ca" => 0.15, "CA" => 0.15}},
        "XObject" => objects,
    };
    let form = Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Form", "FormType" => 1,
            "BBox" => vec![0.into(), 0.into(), Object::from(width), Object::from(height)],
            "Resources" => resources,
        },
        content.into_bytes(),
    );
    doc.try_add_object(form).map_err(pdf_error)
}

fn text_line(content: &mut String, text: &str, x: f32, center_y: f32, max_w: f32, em: f32) {
    let units = text
        .bytes()
        .map(|b| u32::from(HELVETICA_WIDTHS[(b - b' ') as usize]))
        .sum::<u32>();
    let size = em.min(max_w * 1000.0 / units.max(1) as f32);
    let baseline = center_y - size * 0.31;
    let data = hex(text.bytes());
    writeln!(
        content,
        "BT /F1 {size:.4} Tf 1 0 0 1 {x:.4} {baseline:.4} Tm <{data}> Tj ET"
    )
    .unwrap();
}

fn hex(bytes: impl Iterator<Item = u8>) -> String {
    let mut encoded = String::new();
    for b in bytes {
        write!(encoded, "{b:02X}").unwrap();
    }
    encoded
}

fn image(
    doc: &mut Document,
    width: u32,
    height: u32,
    space: &str,
    data: Vec<u8>,
    alpha: Option<ObjectId>,
) -> Result<ObjectId, String> {
    let mut dict = dictionary! {
        "Type" => "XObject", "Subtype" => "Image", "Width" => width, "Height" => height,
        "ColorSpace" => space, "BitsPerComponent" => 8, "Interpolate" => true,
    };
    if let Some(alpha) = alpha {
        dict.set("SMask", alpha);
    }
    let mut stream = Stream::new(dict, data);
    stream.compress().map_err(pdf_error)?;
    doc.try_add_object(stream).map_err(pdf_error)
}

#[cfg(test)]
mod tests;
