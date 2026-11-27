use super::*;
use lopdf::{Stream, dictionary};

fn sample(rotate: i64) -> Vec<u8> {
    let mut d = Document::with_version("1.7");
    let pages = d.new_object_id();
    let latin =
        d.add_object(dictionary! { "Type"=>"Font", "Subtype"=>"Type1", "BaseFont"=>"Helvetica" });
    let cid=d.add_object(dictionary! { "Type"=>"Font", "Subtype"=>"CIDFontType2", "DW"=>1000, "W"=>vec![1.into(), Object::Array(vec![800.into(),900.into()])] });
    let to_unicode = d.add_object(Stream::new(
        dictionary! {},
        b"/CIDInit /ProcSet findresource begin
12 dict begin begincmap /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def
/CMapName /Adobe-Identity-UCS def /CMapType 2 def
1 begincodespacerange <0000> <ffff> endcodespacerange
1 beginbfrange <0001> <0002> [<6771> <4eac>] endbfrange
endcmap CMapName currentdict /CMap defineresource pop end end"
            .to_vec(),
    ));
    let japanese=d.add_object(dictionary! { "Type"=>"Font","Subtype"=>"Type0","Encoding"=>"Identity-H","DescendantFonts"=>vec![Object::Reference(cid)],"ToUnicode"=>to_unicode });
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
    assert!(doc.search_page(0, "A").is_err());
    assert!(doc.page(1).is_err());
}

#[test]
fn search_maps_split_text_and_japanese_to_rotated_cropped_page_rectangles() {
    for angle in [0, 90, 180, 270] {
        let doc = TextDocument::load_for_search(&sample(angle)).unwrap();
        assert_eq!(doc.page_count(), 1);
        let layout = doc.page(0).unwrap();
        let rects = doc.search_page(0, "aB   cd").unwrap();
        assert_eq!(
            rects,
            [vec![
                union(layout.glyphs[0], layout.glyphs[1]),
                union(layout.glyphs[2], layout.glyphs[3])
            ]]
        );
        assert_eq!(
            doc.search_page(0, "東京").unwrap(),
            [vec![union(layout.glyphs[4], layout.glyphs[5])]]
        );
        assert!(doc.search_page(0, "absent").unwrap().is_empty());
        assert!(doc.search_page(0, "　 ").unwrap().is_empty());
        assert!(doc.search_page(1, "ab").is_err());
    }
}

#[test]
fn search_finds_repeated_words_joined_by_glyph_positioning_in_signed_pdfs() {
    let mut doc = Document::load_mem(&sample(0)).unwrap();
    let page = doc.page_iter().next().unwrap();
    doc.change_page_content(
        page,
        b"BT /F 10 Tf 26 90 Td 12 TL [(Sea) 15 (RCH) -340 (one)] TJ (search one) ' ET".to_vec(),
    )
    .unwrap();
    let mut original = Vec::new();
    doc.save_to(&mut original).unwrap();
    let signed = crate::pdfseal::sign_pdf(original, |_, _| Ok(vec![1; 64])).unwrap();
    assert!(TextDocument::load(&signed).is_err());
    let doc = TextDocument::load_for_search(&signed).unwrap();
    let found = doc.search_page(0, "search one").unwrap();
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].len(), 1);
    assert_eq!(found[1].len(), 1);
    assert!(found[0][0].bottom < found[1][0].top);
}

#[test]
fn simple_font_tounicode_takes_precedence_and_preserves_ligatures_surrogate_pairs_and_wrap() {
    let mut doc = Document::load_mem(&sample(0)).unwrap();
    let cmap = doc.add_object(Stream::new(
        dictionary! {},
        b"begincmap
2 beginbfrange <40> <41> <0041> <42> <43> [<00660069> <d840dc00>] endbfrange
2 beginbfchar <44> <6771> <45> <4eac> endbfchar endcmap"
            .to_vec(),
    ));
    let font = doc
        .objects
        .values_mut()
        .filter_map(|o| o.as_dict_mut().ok())
        .find(|d| d.get(b"BaseFont").and_then(Object::as_name).ok() == Some(b"Helvetica"))
        .unwrap();
    font.set("ToUnicode", cmap);
    font.set("Encoding", "WinAnsiEncoding");
    let page = doc.page_iter().next().unwrap();
    doc.change_page_content(
        page,
        b"BT /F 10 Tf 36 90 Td 12 TL (@A) Tj (BCD) Tj (E) ' ET".to_vec(),
    )
    .unwrap();
    let doc = TextDocument::from_document(doc);
    assert_eq!(doc.search_page(0, "ABfi𠀀東京").unwrap()[0].len(), 2);
    assert_eq!(
        doc.search_page(0, "FI").unwrap(),
        [vec![doc.page(0).unwrap().glyphs[2]]]
    );
    assert!(doc.search_page(0, "@A").unwrap().is_empty());
}

#[test]
fn winansi_text_and_following_glyphs_remain_searchable() {
    let mut doc = Document::load_mem(&sample(0)).unwrap();
    let font = doc
        .objects
        .values_mut()
        .filter_map(|o| o.as_dict_mut().ok())
        .find(|d| d.get(b"BaseFont").and_then(Object::as_name).ok() == Some(b"Helvetica"))
        .unwrap();
    font.set("Encoding", "WinAnsiEncoding");
    let page = doc.page_iter().next().unwrap();
    doc.change_page_content(
        page,
        b"BT /F 10 Tf 36 90 Td <436166E92053747261DF65> Tj ET".to_vec(),
    )
    .unwrap();
    let doc = TextDocument::from_document(doc);
    assert_eq!(doc.search_page(0, "CAFÉ STRASSE").unwrap().len(), 1);
}

#[test]
fn rereading_cached_text_preserves_the_original_match_rectangles() {
    for angle in [0, 90, 180, 270] {
        let doc = TextDocument::load_for_search(&sample(angle)).unwrap();
        let mut cache = SearchPageCache::default();
        assert!(!cache.contains(0));
        for query in ["absent", "aB  cd", "東", "東京", "ab", "　"] {
            assert_eq!(
                cache.search(&doc, 0, query).unwrap(),
                doc.search_page(0, query).unwrap()
            );
            assert!(cache.contains(0));
        }
        let bytes = cache.retained_bytes();
        assert!(cache.search(&doc, 1, "ab").is_err());
        assert!(!cache.contains(1));
        assert_eq!(cache.retained_bytes(), bytes);
    }
}

#[test]
fn repeated_full_scans_keep_the_pages_that_fit_and_search_the_rest() {
    let mut doc = TextDocument::load_for_search(&sample(0)).unwrap();
    let page = doc.pages[0];
    doc.pages.push(page);
    let expected = doc.search_page(0, "東京").unwrap();
    assert_eq!(expected.len(), 1);

    let mut probe = SearchPageCache::default();
    probe.search(&doc, 0, "東京").unwrap();
    let limit = probe.retained_bytes();
    for budget in [0, limit - 1, limit] {
        let mut cache = SearchPageCache::with_limit(budget);
        for _ in 0..3 {
            for index in 0..2 {
                assert_eq!(cache.search(&doc, index, "東京").unwrap(), expected);
            }
            assert_eq!(cache.contains(0), budget == limit);
            assert!(!cache.contains(1));
            assert!(cache.retained_bytes() <= budget);
        }
    }

    // An oversized first page must not prevent a subsequent small page from fitting.
    let mut blank = doc.doc.get_dictionary(page).unwrap().clone();
    blank.remove(b"Contents");
    doc.pages.push(doc.doc.add_object(blank));
    let mut blank_only = SearchPageCache::default();
    assert!(blank_only.search(&doc, 2, "東京").unwrap().is_empty());
    let mut cache = SearchPageCache::with_limit(blank_only.retained_bytes());
    assert_eq!(cache.search(&doc, 0, "東京").unwrap(), expected);
    assert!(!cache.contains(0));
    assert!(cache.search(&doc, 2, "東京").unwrap().is_empty());
    assert!(cache.contains(2));
}
