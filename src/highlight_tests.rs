use super::*;
use std::{fs, process::Command};

fn fixture(crop_right: i32) -> Vec<u8> {
    let mut doc = Document::with_version("1.3");
    doc.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceTable;
    let root_pages = doc.new_object_id();
    let font =
        doc.add_object(dictionary! {"Type"=>"Font", "Subtype"=>"Type1", "BaseFont"=>"Helvetica"});
    let content = doc.add_object(Stream::new(
        dictionary! {},
        b"0.4 g -50 -50 500 500 re f\n0 g BT /F1 12 Tf 80 90 Td (Hello markup) Tj ET".to_vec(),
    ));
    let existing = doc.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Link", "Rect" => vec![0.into();4],
        "Border" => vec![0.into();3],
    });
    let existing_annots = doc.add_object(Object::Array(vec![existing.into()]));
    let media = doc.add_object(Object::Array(vec![
        (-10).into(),
        10.into(),
        190.into(),
        170.into(),
    ]));
    let crop = doc.add_object(Object::Array(vec![
        10.into(),
        30.into(),
        crop_right.into(),
        130.into(),
    ]));
    let mut groups = Vec::new();
    for angle in [0, 90, 180, -90] {
        let group = doc.new_object_id();
        let page = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => group, "Annots" => existing_annots, "Contents" => content,
        });
        doc.set_object(group, dictionary! {
            "Type"=>"Pages", "Parent" => root_pages, "Kids" => vec![Object::Reference(page)], "Count"=>1,
            "CropBox" => crop, "Rotate" => angle,
        });
        groups.push(Object::Reference(group));
    }
    doc.set_object(
        root_pages,
        dictionary! {
            "Type" => "Pages", "Count" => 4, "Kids" => groups, "MediaBox" => media,
            "Resources" => dictionary! {"Font" => dictionary! {"F1" => font } },
        },
    );
    let root = doc.add_object(dictionary! {"Type" => "Catalog", "Pages" => root_pages});
    doc.trailer.set("Root", root);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    bytes
}

fn marks(page: u32) -> Mark {
    Mark {
        page,
        rects: vec![
            UnitRect {
                left: 0.1,
                top: 0.18,
                right: 0.65,
                bottom: 0.3,
            },
            UnitRect {
                left: 0.12,
                top: 0.55,
                right: 0.52,
                bottom: 0.67,
            },
        ],
    }
}

#[test]
fn retains_original_annotations_and_native_multiline_highlights_through_signing() {
    let original = fixture(190);
    let marks = [marks(1), marks(1), marks(3)];
    let marked = annotate(original.clone(), &marks).unwrap();
    assert!(marked.starts_with(&original));
    let doc = Document::load_mem(&marked).unwrap();
    assert!(doc.xref_recovery.is_none());
    assert_eq!(doc.revision_count(), 2);
    assert_eq!(
        doc.catalog()
            .unwrap()
            .get(b"Version")
            .unwrap()
            .as_name()
            .unwrap(),
        b"1.4"
    );
    for (page, count) in doc.page_iter().zip([0, 2, 0, 1]) {
        let annots = doc.get_page_annotations(page).unwrap();
        assert_eq!(annots.len(), 1 + count);
        assert_eq!(
            annots[0].get(b"Subtype").unwrap().as_name().unwrap(),
            b"Link"
        );
        for highlight in &annots[1..] {
            assert_eq!(
                highlight.get(b"Subtype").unwrap().as_name().unwrap(),
                b"Highlight"
            );
            assert_eq!(
                highlight
                    .get(b"QuadPoints")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .len(),
                16
            );
            assert!(
                highlight
                    .get(b"AP")
                    .unwrap()
                    .as_dict()
                    .unwrap()
                    .get_deref(b"N", &doc)
                    .unwrap()
                    .as_stream()
                    .is_ok()
            );
        }
    }
    let signed = crate::pdfseal::sign_pdf(marked.clone(), |_, _| Ok(vec![1])).unwrap();
    assert!(signed.starts_with(&marked));
    let signed_doc = Document::load_mem(&signed).unwrap();
    let page = signed_doc.page_iter().nth(1).unwrap();
    assert_eq!(signed_doc.get_page_annotations(page).unwrap().len(), 3);
    assert!(annotate(signed, &marks).is_err());
}

#[test]
fn maps_both_directions_at_all_rotations_and_rejects_invalid_selections() {
    let source = fixture(220);
    let doc = Document::load_mem(&source).unwrap();
    // The inherited boxes intersect at [10 30 190 130]. Each tuple gives
    // the independently expected PDF position of the visible top-left.
    for (page, corner) in doc
        .page_iter()
        .zip([(10., 130.), (10., 30.), (190., 30.), (190., 130.)])
    {
        let geometry = PageGeometry::from_page(&doc, page).unwrap();
        assert_eq!(geometry.pdf_point(0., 0.), corner);
        let (x, y) = geometry.pdf_point(0.125, 0.75);
        assert_eq!(geometry.to_unit(x, y), (0.125, 0.75));
    }
    let mut bad = marks(0);
    bad.rects[1].right = f32::NAN;
    assert!(annotate(source.clone(), &[bad]).is_err());
    assert!(annotate(source.clone(), &[marks(4)]).is_err());
    assert_eq!(annotate(source.clone(), &[]).unwrap(), source);
}

#[test]
#[ignore = "requires Poppler pdftoppm"]
fn poppler_renders_yellow_only_in_selected_runs_on_each_rotated_crop() {
    let original = fixture(190);
    let selections: Vec<_> = (0..4).map(marks).collect();
    let output = annotate(original.clone(), &selections).unwrap();
    let dir = tempfile::tempdir().unwrap();
    for (name, bytes) in [("source", original), ("marked", output)] {
        let path = dir.path().join(format!("{name}.pdf"));
        fs::write(&path, bytes).unwrap();
        let cmd = Command::new("pdftoppm")
            .current_dir(dir.path())
            .args(["-r", "72", "-cropbox"])
            .arg(&path)
            .arg(name)
            .output()
            .unwrap();
        assert!(
            cmd.status.success(),
            "{}",
            String::from_utf8_lossy(&cmd.stderr)
        );
    }
    for i in 1..=4 {
        let (w, h) = if i % 2 == 1 { (180, 100) } else { (100, 180) };
        let prefix = format!("P6\n{w} {h}\n255\n");
        let original = fs::read(dir.path().join(format!("source-{i}.ppm"))).unwrap();
        let marked = fs::read(dir.path().join(format!("marked-{i}.ppm"))).unwrap();
        let a = original.strip_prefix(prefix.as_bytes()).unwrap_or_else(|| {
            panic!(
                "page {i}: actual header {:?}, expected {prefix:?}",
                String::from_utf8_lossy(&original[..24])
            )
        });
        let b = marked.strip_prefix(prefix.as_bytes()).unwrap();
        for (x, y, selected) in [
            (0.2, 0.24, true),
            (0.3, 0.61, true),
            (0.2, 0.42, false),
            (0.87, 0.25, false),
        ] {
            let offset = ((y * h as f32) as usize * w + (x * w as f32) as usize) * 3;
            let pixel = &b[offset..offset + 3];
            let source = &a[offset..offset + 3];
            if selected {
                assert!(
                    pixel[0].abs_diff(source[0]) <= 1 && pixel[1].abs_diff(source[1]) <= 1,
                    "page {i} {x} {y}: {pixel:?}, {source:?}"
                );
                assert!(
                    pixel[2].abs_diff((source[2] as f32 * 0.55).round() as u8) <= 3,
                    "page {i} {x} {y}: {pixel:?}, {source:?}"
                );
            } else {
                assert_eq!(pixel, source, "page {i} {x} {y}");
            }
        }
    }
}
