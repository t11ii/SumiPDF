use super::*;
use lopdf::content::Content;

const JPEG: &[u8] = include_bytes!("../../tests/fixtures/SignatureImage.jpg");

fn signed_at() -> OffsetDateTime {
    time::Date::from_calendar_date(2026, time::Month::November, 26)
        .unwrap()
        .with_hms(14, 3, 5)
        .unwrap()
        .assume_utc()
}

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
        let pixels = images::decode_png(&png(kind, bits, data)).unwrap();
        assert_eq!(pixels.samples, samples);
        assert_eq!(pixels.alpha, alpha);
    }
}

#[test]
fn reject_invalid_truncated_and_oversize_images() {
    assert!(images::decode_png(b"not a PNG").is_err());
    assert!(images::decode_png(&vec![0; images::MAX_IMAGE_BYTES + 1]).is_err());
    let mut bytes = Vec::new();
    png::Encoder::new(&mut bytes, 4097, 1)
        .write_header()
        .unwrap()
        .finish()
        .unwrap();
    assert!(images::decode_png(&bytes).err().unwrap().contains("4096"));
    let mut bytes = png(png::ColorType::Grayscale, png::BitDepth::Eight, &[0, 255]);
    bytes.truncate(bytes.len() - 10);
    assert!(images::decode_png(&bytes).is_err());
    assert!(images::decode_signature_image(&JPEG[..JPEG.len() / 2]).is_err());
}

#[test]
fn appearance_contains_only_encoded_user_text_and_orders_art_behind_the_signer() {
    let mut doc = Document::with_version("1.7");
    let name = "A) Tj ET (B\\C%";
    let id = make(&mut doc, 315.0, 85.0, name, signed_at(), Artwork::default()).unwrap();
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
            b"Date: 2026-11-26",
            b"Time: 14:03:05 UTC"
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

#[test]
fn png_and_jpeg_stay_in_the_left_half_with_the_signer_and_datetime_on_the_right() {
    let signature_png = png(
        png::ColorType::Rgba,
        png::BitDepth::Eight,
        &[0, 0, 0, 0, 10, 60, 155, 250],
    );
    for (image, size, alpha) in [
        (signature_png.as_slice(), (2, 1), true),
        (JPEG, (16, 8), false),
    ] {
        let mut doc = Document::with_version("1.7");
        let id = make(
            &mut doc,
            300.0,
            75.0,
            "Signer Name",
            signed_at(),
            Artwork {
                watermark: Some(&signature_png),
                signature_image: Some(image),
            },
        )
        .unwrap();
        let form = doc.get_object(id).unwrap().as_stream().unwrap();
        let objects = form
            .dict
            .get(b"Resources")
            .unwrap()
            .as_dict()
            .unwrap()
            .get(b"XObject")
            .unwrap()
            .as_dict()
            .unwrap();
        let graphic = objects
            .get_deref(b"SignatureImage", &doc)
            .unwrap()
            .as_stream()
            .unwrap();
        let dimension = |key: &[u8]| graphic.dict.get(key).unwrap().as_i64().unwrap();
        assert_eq!((dimension(b"Width"), dimension(b"Height")), size);
        assert_eq!(graphic.dict.has(b"SMask"), alpha);
        assert!(objects.has(b"Watermark"));

        let ops = Content::decode(&form.content).unwrap().operations;
        let placement = ops
            .windows(2)
            .find(|ops| {
                ops[0].operator == "cm"
                    && ops[1].operator == "Do"
                    && ops[1].operands[0].as_name().unwrap() == b"SignatureImage"
            })
            .unwrap();
        let coordinate = |i: usize| placement[0].operands[i].as_float().unwrap();
        let (w, h, x, y) = (coordinate(0), coordinate(3), coordinate(4), coordinate(5));
        assert!((w / h - 2.0).abs() < 1e-4);
        assert!(x > 0.0 && x + w < 150.0 && y > 0.0 && y + h < 75.0);

        let lines: Vec<_> = ops
            .windows(2)
            .filter(|ops| ops[0].operator == "Tm" && ops[1].operator == "Tj")
            .map(|ops| {
                assert!(ops[0].operands[4].as_float().unwrap() > 150.0);
                (
                    ops[1].operands[0].as_str().unwrap(),
                    ops[0].operands[5].as_float().unwrap(),
                )
            })
            .collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0].0, b"Signer Name");
        assert_eq!(lines[2].0, b"Date: 2026-11-26");
        assert_eq!(lines[3].0, b"Time: 14:03:05 UTC");
        assert!(lines[1].1 > lines[0].1 && lines[0].1 > lines[2].1 && lines[2].1 > lines[3].1);
    }
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
        signed_at(),
        Artwork::default(),
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
        signed_at(),
        Artwork {
            watermark: Some(&logo),
            ..Artwork::default()
        },
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
