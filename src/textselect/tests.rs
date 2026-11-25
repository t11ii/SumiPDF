use super::*;
use lopdf::{Stream, dictionary};

fn sample(rotate: i64) -> Vec<u8> {
    let mut d = Document::with_version("1.7");
    let pages = d.new_object_id();
    let latin =
        d.add_object(dictionary! { "Type"=>"Font", "Subtype"=>"Type1", "BaseFont"=>"Helvetica" });
    let cid=d.add_object(dictionary! { "Type"=>"Font", "Subtype"=>"CIDFontType2", "DW"=>1000, "W"=>vec![1.into(), Object::Array(vec![800.into(),900.into()])] });
    let japanese=d.add_object(dictionary! { "Type"=>"Font","Subtype"=>"Type0","Encoding"=>"Identity-H","DescendantFonts"=>vec![Object::Reference(cid)] });
    let nested = d.add_object(Stream::new(
        dictionary! {
            "Subtype"=>"Form", "BBox"=>vec![0.into(),0.into(),70.into(),70.into()],
            "Matrix"=>vec![1.into(),0.into(),0.into(),1.into(),35.into(),65.into()],
            "Resources"=>dictionary! { "Font"=>dictionary! { "JP"=>japanese } }
        },
        b"BT /JP 10 Tf <00010002> Tj ET".to_vec(),
    ));
    let resources=d.add_object(dictionary! { "Font"=>dictionary! { "F"=>latin }, "XObject"=>dictionary! { "Form"=>nested } });
    let stream=d.add_object(Stream::new(dictionary!{},b"q 1 0 0 1 4 0 cm BT /F 10 Tf 1 0 0 1 36 90 Tm 12 TL [(A) -100 (B)] TJ (CD) ' ET Q /Form Do".to_vec()));
    let page = d.add_object(dictionary! {"Type"=>"Page","Parent"=>pages,"Contents"=>stream});
    d.set_object(
        pages,
        dictionary! { "Type"=>"Pages","Kids"=>vec![Object::Reference(page)],"Count"=>1,
        "Resources"=>resources,"MediaBox"=>vec![0.into(),0.into(),240.into(),140.into()],
        "CropBox"=>vec![20.into(),20.into(),220.into(),120.into()], "Rotate"=>rotate },
    );
    let root = d.add_object(dictionary! { "Type"=>"Catalog", "Pages"=>pages });
    d.trailer.set("Root", root);
    let mut bytes = Vec::new();
    d.save_to(&mut bytes).unwrap();
    bytes
}

fn near(x: f32, y: f32) {
    assert!((x - y).abs() < 0.0001, "{x} != {y}");
}
fn midpoint(r: UnitRect) -> (f32, f32) {
    ((r.left + r.right) / 2., (r.top + r.bottom) / 2.)
}

#[test]
fn pdf_glyphs_snap_to_forward_and_reverse_lines_and_cid_widths() {
    let doc = TextDocument::load(&sample(0)).unwrap();
    let text = doc.page(0).unwrap();
    assert!(!text.is_empty());
    assert_eq!(text.glyphs.len(), 6);
    near(text.glyphs[0].left, 0.10);
    near(text.glyphs[0].right, (40. + 6.67 - 20.) / 200.);
    near(text.glyphs[1].left, (40. + 6.67 + 1. - 20.) / 200.);
    near(text.glyphs[4].left, (35. - 20.) / 200.);
    near(text.glyphs[4].right, (35. + 8. - 20.) / 200.);
    near(text.glyphs[5].right, (35. + 8. + 9. - 20.) / 200.);
    assert!(text.hit(0.95, 0.95).is_none());
    let (x, y) = midpoint(text.glyphs[1]);
    let start = text.hit(x, y).unwrap();
    let (u, v) = midpoint(text.glyphs[3]);
    assert_eq!(
        text.range(start, u, v),
        [text.glyphs[1], union(text.glyphs[2], text.glyphs[3])].map(TextRun::horizontal)
    );
    assert_eq!(
        text.range(start, u, v),
        text.range(text.hit(u, v).unwrap(), x, y)
    );
}
#[test]
fn rotated_pages_select_the_same_cropped_glyphs() {
    let normal = TextDocument::load(&sample(0)).unwrap().page(0).unwrap();
    let original = normal.glyphs[0];
    for (angle, direction, want) in [
        (90, Direction::Down, (1. - original.bottom, original.left)),
        (
            180,
            Direction::Left,
            (1. - original.right, 1. - original.bottom),
        ),
        (270, Direction::Up, (original.top, 1. - original.right)),
    ] {
        let text = TextDocument::load(&sample(angle)).unwrap().page(0).unwrap();
        let first = text.glyphs[0];
        near(first.left, want.0);
        near(first.top, want.1);
        let (x, y) = midpoint(first);
        let (u, v) = midpoint(text.glyphs[1]);
        assert_eq!(
            text.range(text.hit(x, y).unwrap(), u, v),
            [TextRun {
                bounds: union(first, text.glyphs[1]),
                direction
            }]
        );
    }
}
#[test]
fn image_only_page_has_no_text_hit() {
    let mut d = Document::load_mem(&sample(0)).unwrap();
    let id = d.page_iter().next().unwrap();
    d.change_page_content(id, b"0.3 g 30 45 28 22 re f".to_vec())
        .unwrap();
    let mut bytes = Vec::new();
    d.save_to(&mut bytes).unwrap();
    let layout = TextDocument::load(&bytes).unwrap().page(0).unwrap();
    assert!(layout.is_empty());
    assert!(layout.hit(0.1, 0.4).is_none());
}

#[test]
fn unsupported_page_content_falls_back_but_a_bad_page_number_is_reported() {
    let mut d = Document::load_mem(&sample(0)).unwrap();
    let id = d.page_iter().next().unwrap();
    let mut bytes = b"BT /F 10 Tf 1 0 0 1 36 90 Tm (A) Tj ET ".to_vec();
    bytes.extend(b"q ".repeat(257));
    d.change_page_content(id, bytes).unwrap();
    let mut source = Vec::new();
    d.save_to(&mut source).unwrap();
    let doc = TextDocument::load(&source).unwrap();
    assert!(doc.page(0).unwrap().is_empty());
    assert!(doc.page(1).is_err());
}
