use super::copy;
use crate::highlight::{Mark, UnitRect};
use crate::{wincrypto, winpdf};
use lopdf::{Document, Object, Stream, dictionary};
use std::fs;

fn source_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages = doc.new_object_id();
    let content = doc.add_object(Stream::new(
        dictionary! {},
        b"1 0 0 rg 0 0 36 36 re f".to_vec(),
    ));
    let page = doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(), 0.into(), 72.into(), 72.into()],
        "Contents" => content, "Resources" => dictionary! {},
    });
    doc.set_object(
        pages,
        dictionary! {
            "Type" => "Pages", "Kids" => vec![Object::Reference(page)], "Count" => 1,
        },
    );
    let root = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    doc.trailer.set("Root", root);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    bytes
}

fn check_saved_signature(bytes: &[u8]) {
    let doc = Document::load_mem(bytes).unwrap();
    let sig = doc
        .objects
        .values()
        .filter_map(|obj| obj.as_dict().ok())
        .find(|dict| dict.has_type(b"Sig"))
        .unwrap();
    let range: Vec<_> = sig
        .get(b"ByteRange")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|v| usize::try_from(v.as_i64().unwrap()).unwrap())
        .collect();
    let [start, left, right, tail] = range[..] else {
        panic!("unexpected ByteRange")
    };
    assert_eq!(start, 0);
    assert_eq!(right + tail, bytes.len());
    assert_eq!((bytes[left], bytes[right - 1]), (b'<', b'>'));

    // /Contents reserves space for a certificate chain. Give CryptoAPI exactly the
    // DER SEQUENCE, excluding only PDF padding (a real CMS can end in a zero byte).
    let contents = sig.get(b"Contents").unwrap().as_str().unwrap();
    assert_eq!(contents[0], 0x30);
    let length_octet = usize::from(contents[1]);
    let cms_len = if length_octet < 128 {
        2 + length_octet
    } else {
        let octets = length_octet & 0x7f;
        assert!((1..=4).contains(&octets));
        let length = contents[2..2 + octets]
            .iter()
            .fold(0usize, |n, &b| n * 256 + usize::from(b));
        2 + octets + length
    };
    assert!(contents[cms_len..].iter().all(|&b| b == 0));
    let cms = &contents[..cms_len];
    wincrypto::verify_detached(cms, &bytes[..left], &bytes[right..]).unwrap();
    let mut changed = bytes[right..].to_vec();
    changed[0] ^= 1;
    assert!(wincrypto::verify_detached(cms, &bytes[..left], &changed).is_err());
}

#[test]
fn windows_signs_the_viewed_pdf_and_preserves_existing_files() {
    let _apartment = winpdf::initialize().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("原本.pdf");
    let target = dir.path().join("署名済み.pdf");
    let taken = dir.path().join("existing.pdf");
    let original = source_pdf();
    fs::write(&source_path, &original).unwrap();
    fs::write(&taken, b"existing destination").unwrap();

    let viewed = winpdf::Document::open_file(&source_path).unwrap();
    let first_page = viewed.render_fit(0, (128, 128), 1.0).unwrap();
    let source = viewed.signing_source().unwrap();
    wincrypto::tests::with_test_certificate(|cert| {
        // As in the GUI, the rendering apartment keeps its document open while
        // another thread acquires the stream and creates the signed file.
        std::thread::scope(|scope| {
            let (source_path, target, taken) = (&source_path, &target, &taken);
            scope
                .spawn(move || {
                    copy(&source, &[], Some(&cert), target).unwrap();
                    assert!(copy(&source, &[], Some(&cert), taken).is_err());
                    assert!(copy(&source, &[], Some(&cert), source_path).is_err());
                })
                .join()
                .unwrap();
        });
    });

    let signed = fs::read(&target).unwrap();
    assert!(signed.starts_with(&original));
    assert_eq!(fs::read(&source_path).unwrap(), original);
    assert_eq!(fs::read(&taken).unwrap(), b"existing destination");
    check_saved_signature(&signed);
    let reopened = winpdf::Document::open_file(&target).unwrap();
    assert_eq!(reopened.page_count(), viewed.page_count());
    let signed_page = reopened.render_fit(0, (128, 128), 1.0).unwrap();
    assert_eq!(&first_page.pixels[..], &signed_page.pixels[..]);
}

#[test]
fn windows_reopens_visible_highlights_in_unsigned_and_signed_copies() {
    let _apartment = winpdf::initialize().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let original = source_pdf();
    let input = dir.path().join("元の文書.pdf");
    let annotated = dir.path().join("蛍光ペン.pdf");
    let signed = dir.path().join("蛍光ペンと署名.pdf");
    fs::write(&input, &original).unwrap();
    let doc = winpdf::Document::open_file(&input).unwrap();
    let source = doc.signing_source().unwrap();
    let marks = [Mark {
        page: 0,
        rects: vec![UnitRect {
            left: 0.5,
            top: 0.125,
            right: 0.875,
            bottom: 0.375,
        }],
    }];
    copy(&source, &marks, None, &annotated).unwrap();
    wincrypto::tests::with_test_certificate(|cert| {
        copy(&source, &marks, Some(&cert), &signed).unwrap()
    });
    assert_eq!(fs::read(&input).unwrap(), original);

    let pixel = |image: &winpdf::Bitmap, x: usize, y: usize| -> [u8; 3] {
        let p = 4 * (y * image.width as usize + x);
        image.pixels[p..p + 3].try_into().unwrap()
    };
    let paper = doc.render_fit(0, (128, 128), 1.0).unwrap();
    assert_eq!(pixel(&paper, 96, 32), [255; 3]);
    for path in [&annotated, &signed] {
        let bytes = fs::read(path).unwrap();
        assert!(bytes.starts_with(&original));
        let parsed = Document::load_mem(&bytes).unwrap();
        let annots = parsed
            .get_page_annotations(parsed.page_iter().next().unwrap())
            .unwrap();
        let marks: Vec<_> = annots
            .iter()
            .filter(|a| a.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Highlight"))
            .collect();
        assert_eq!(marks.len(), 1);
        assert_eq!(
            marks[0]
                .get(b"QuadPoints")
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            8
        );
        assert!(
            marks[0]
                .get_deref(b"AP", &parsed)
                .unwrap()
                .as_dict()
                .unwrap()
                .get_deref(b"N", &parsed)
                .unwrap()
                .as_stream()
                .is_ok()
        );
        if path == &signed {
            check_saved_signature(&bytes);
        } else {
            assert!(
                !parsed
                    .objects
                    .values()
                    .any(|o| o.as_dict().is_ok_and(|d| d.has_type(b"Sig")))
            );
        }

        let reopened = winpdf::Document::open_file(path).unwrap();
        let image = reopened.render_fit(0, (128, 128), 1.0).unwrap();
        assert_eq!((image.width, image.height), (128, 128));
        let [blue, green, red] = pixel(&image, 96, 32);
        assert!(
            red >= 235 && green >= 235 && blue < 220,
            "Windows rendered the marked paper as BGRA=[{blue},{green},{red},…] in {path:?}"
        );
        assert_eq!(pixel(&image, 96, 60), pixel(&paper, 96, 60));
        assert_eq!(pixel(&image, 32, 96), pixel(&paper, 32, 96));
    }
}
