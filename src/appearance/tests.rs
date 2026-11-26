use super::*;
use lopdf::content::Content;

fn png(color: png::ColorType, bits: png::BitDepth, data: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, 2, 1);
    encoder.set_color(color);
    encoder.set_depth(bits);
    if color == png::ColorType::Indexed {
        encoder.set_palette(vec![199, 31, 21, 24, 96, 160]);
        encoder.set_trns(vec![0, 140]);
    }
    {
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(data).unwrap();
    }
    bytes
}

#[test]
fn png_color_and_alpha_are_independent() {
    use png::BitDepth::{Eight, One, Sixteen};
    use png::ColorType::*;
    for (kind, bits, data, samples, alpha) in [
        (
            Indexed,
            One,
            &b"\x40"[..],
            &b"\xc7\x1f\x15\x18\x60\xa0"[..],
            Some(vec![0, 140]),
        ),
        (
            GrayscaleAlpha,
            Eight,
            b"\x01\x20\x02\x00",
            b"\x01\x02",
            Some(vec![32, 0]),
        ),
        (
            Rgba,
            Eight,
            b"\x01\x02\x03\x20\x04\x05\x06\x40",
            b"\x01\x02\x03\x04\x05\x06",
            Some(vec![32, 64]),
        ),
        (
            Rgb,
            Eight,
            b"\x00\x08\x10\x40\x60\xff",
            b"\x00\x08\x10\x40\x60\xff",
            None,
        ),
        (Grayscale, Sixteen, b"\x88\x00\xff\x00", b"\x88\xff", None),
        (Grayscale, One, b"\x40", b"\x00\xff", None),
    ] {
        let pixels = watermark::decode(&png(kind, bits, data)).unwrap();
        assert_eq!(pixels.samples, samples);
        assert_eq!(pixels.alpha, alpha);
    }
}

#[test]
fn reject_invalid_truncated_and_oversize_images() {
    assert!(validate_watermark(b"not a PNG").is_err());
    assert!(validate_watermark(&vec![0; MAX_WATERMARK_BYTES + 1]).is_err());
    let mut bytes = Vec::new();
    png::Encoder::new(&mut bytes, 4097, 1)
        .write_header()
        .unwrap()
        .finish()
        .unwrap();
    assert!(validate_watermark(&bytes).unwrap_err().contains("4096"));
    let mut bytes = png(png::ColorType::Grayscale, png::BitDepth::Eight, &[0, 255]);
    bytes.truncate(bytes.len() - 10);
    assert!(validate_watermark(&bytes).is_err());
}

#[test]
fn appearance_contains_only_encoded_user_text_and_orders_art_behind_the_signer() {
    let mut doc = Document::with_version("1.7");
    let name = "A) Tj ET (B\\C%";
    let id = make(&mut doc, 315.0, 85.0, name, "D:20261126140305Z", None).unwrap();
    let form = doc.get_object(id).unwrap().as_stream().unwrap();
    let ops = Content::decode(&form.content).unwrap().operations;
    let strings: Vec<_> = ops
        .iter()
        .filter(|o| o.operator == "Tj")
        .map(|o| o.operands[0].as_str().unwrap())
        .collect();
    assert_eq!(
        strings,
        [
            name.as_bytes(),
            name.as_bytes(),
            b"Digitally signed by",
            b"Date: 2026-11-26 14:03:05 UTC"
        ]
    );
    assert!(
        ops.iter().position(|op| op.operator == "gs")
            < ops.iter().position(|op| op.operator == "Tj")
    );
    assert_eq!(
        form.dict.get(b"BBox").unwrap().as_array().unwrap()[2]
            .as_float()
            .unwrap(),
        315.0
    );
}

#[cfg(windows)]
#[test]
fn native_unicode_name_is_painted_and_embedded_once() {
    let mask = windows_text::render("飯田 岳夫 · José").unwrap();
    assert!(mask.width > mask.height && mask.coverage.contains(&255) && mask.coverage.contains(&0));
    let mut doc = Document::with_version("1.7");
    let id = make(
        &mut doc,
        315.0,
        85.0,
        "飯田 岳夫 · José",
        "D:20261126140305Z",
        None,
    )
    .unwrap();
    let form = doc.get_object(id).unwrap().as_stream().unwrap();
    let ops = Content::decode(&form.content).unwrap().operations;
    assert_eq!(
        ops.iter()
            .filter(|o| o.operator == "Do" && o.operands[0].as_name().unwrap() == b"Signer")
            .count(),
        2
    );
}

#[test]
fn embedded_name_keeps_transparency_and_unicode_text_in_both_columns() {
    let mut doc = Document::with_version("1.7");
    let mask = NameMask {
        width: 3,
        height: 2,
        coverage: vec![255, 0, 92, 0, 255, 0],
    };
    let logo = png(png::ColorType::Indexed, png::BitDepth::One, &[0x40]);
    let name = "飯田 岳夫 · José";
    let id = draw(
        &mut doc,
        300.0,
        75.0,
        name,
        Some(mask),
        "2026-11-26 05:18:09 UTC",
        Some(&logo),
    )
    .unwrap();
    let form = doc.get_object(id).unwrap().as_stream().unwrap();
    let resources = form.dict.get(b"Resources").unwrap().as_dict().unwrap();
    let images = resources.get(b"XObject").unwrap().as_dict().unwrap();
    for key in [b"Signer".as_slice(), b"Watermark".as_slice()] {
        let image = images.get_deref(key, &doc).unwrap().as_stream().unwrap();
        assert!(
            image
                .dict
                .get_deref(b"SMask", &doc)
                .unwrap()
                .as_stream()
                .is_ok()
        );
    }
    let ops = Content::decode(&form.content).unwrap().operations;
    let painted: Vec<_> = ops
        .iter()
        .filter(|o| o.operator == "Do")
        .map(|o| o.operands[0].as_name().unwrap())
        .collect();
    assert_eq!(painted, [b"Watermark".as_slice(), b"Signer", b"Signer"]);
    let actual: Vec<_> = ops
        .iter()
        .filter(|o| o.operator == "BDC")
        .map(|op| {
            let tag = op.operands[1].as_dict().unwrap();
            lopdf::decode_text_string(tag.get(b"ActualText").unwrap()).unwrap()
        })
        .collect();
    assert_eq!(actual, [name, name]);
}
