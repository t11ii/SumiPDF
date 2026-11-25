//! Select text using independently measured word locations, then inspect the
//! annotated page using the same external renderer a recipient could use.
use crate::{
    highlight::{Mark, annotate},
    textselect::TextDocument,
};
use lopdf::{Document, Stream, dictionary};
use std::{fs, path::Path, process::Command};

fn fixture() -> Vec<u8> {
    let mut pdf = Document::with_version("1.7");
    let pages = pdf.new_object_id();
    let font =
        pdf.add_object(dictionary! { "Type"=>"Font", "Subtype"=>"Type1", "BaseFont"=>"Courier" });
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 12 Tf 42 154 Td (ALPHA BRAVO) Tj 0 -30 Td (CHARLIE DELTA) Tj ET".to_vec(),
    ));
    let page = pdf.add_object(dictionary! {
        "Type"=>"Page", "Parent"=>pages, "Contents"=>content,
        "Resources"=>dictionary! { "Font"=>dictionary! { "F1"=>font } },
        "MediaBox"=>vec![0.into(),0.into(),300.into(),220.into()],
    });
    pdf.set_object(
        pages,
        dictionary! { "Type"=>"Pages", "Count"=>1, "Kids"=>vec![page.into()] },
    );
    let root = pdf.add_object(dictionary! { "Type"=>"Catalog", "Pages"=>pages });
    pdf.trailer.set("Root", root);
    let mut out = Vec::new();
    pdf.save_to(&mut out).unwrap();
    out
}

fn external(dir: &Path, program: &str, args: &[&str]) -> Vec<u8> {
    let output = Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{program}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn word(xml: &str, label: &str) -> [f32; 4] {
    let line = xml
        .lines()
        .find(|line| line.contains(&format!(">{label}</word>")))
        .unwrap();
    ["xMin", "yMin", "xMax", "yMax"].map(|attr| {
        line.split_once(&format!("{attr}=\""))
            .unwrap()
            .1
            .split('"')
            .next()
            .unwrap()
            .parse::<f32>()
            .unwrap()
    })
}

#[test]
#[ignore = "requires Poppler pdftotext and pdftoppm"]
fn dragging_across_two_lines_highlights_only_the_selected_words() {
    let dir = tempfile::tempdir().unwrap();
    let source = fixture();
    fs::write(dir.path().join("original.pdf"), &source).unwrap();
    let xml = String::from_utf8(external(
        dir.path(),
        "pdftotext",
        &["-bbox", "original.pdf", "-"],
    ))
    .unwrap();
    let first = word(&xml, "BRAVO");
    let last = word(&xml, "CHARLIE");
    let start = ((first[0] + 1.0) / 300.0, (first[1] + first[3]) / 440.0);
    let end = ((last[2] - 1.0) / 300.0, (last[1] + last[3]) / 440.0);
    let layout = TextDocument::load(&source).unwrap().page(0).unwrap();
    let selection = layout.range(layout.hit(start.0, start.1).unwrap(), end.0, end.1);
    assert_eq!(selection.len(), 2);
    assert_eq!(
        selection,
        layout.range(layout.hit(end.0, end.1).unwrap(), start.0, start.1)
    );
    for (run, bounds) in selection.iter().zip([first, last]) {
        let rect = run.bounds;
        assert!(
            (rect.left * 300.0 - bounds[0]).abs() < 1.0,
            "{rect:?} vs {bounds:?}"
        );
        assert!(
            (rect.right * 300.0 - bounds[2]).abs() < 1.0,
            "{rect:?} vs {bounds:?}"
        );
    }
    let pdf = annotate(
        source,
        &[Mark {
            page: 0,
            style: Default::default(),
            runs: selection,
        }],
    )
    .unwrap();
    fs::write(dir.path().join("marked.pdf"), pdf).unwrap();
    let render = |file: &str| {
        external(
            dir.path(),
            "pdftoppm",
            &["-r", "72", "-singlefile", file, "page"],
        );
        let ppm = fs::read(dir.path().join("page.ppm")).unwrap();
        ppm.strip_prefix(b"P6\n300 220\n255\n").unwrap().to_vec()
    };
    let before = render("original.pdf");
    let after = render("marked.pdf");
    for (label, selected) in [
        ("ALPHA", false),
        ("BRAVO", true),
        ("CHARLIE", true),
        ("DELTA", false),
    ] {
        let [x0, y0, x1, y1] = word(&xml, label);
        let mut checked = 0;
        for y in y0.ceil() as usize..y1.floor() as usize {
            for x in x0.ceil() as usize..x1.floor() as usize {
                let i = (y * 300 + x) * 3;
                let source = &before[i..i + 3];
                let result = &after[i..i + 3];
                if selected {
                    // A Multiply highlight tints the white paper and gray antialias
                    // pixels proportionally, while leaving black letters legible.
                    assert!(
                        result[0].abs_diff(source[0]) <= 1 && result[1].abs_diff(source[1]) <= 1
                    );
                    let blue = (source[2] as f32 * 0.55).round() as u8;
                    assert!(
                        result[2].abs_diff(blue) <= 3,
                        "{label} at ({x},{y}): {source:?} -> {result:?}"
                    );
                } else {
                    assert_eq!(source, result, "{label} at ({x},{y})");
                }
                checked += 1;
            }
        }
        assert!(checked > 100);
    }
}
