use super::*;
use lopdf::{EncryptionState, EncryptionVersion, Permissions, Stream, xref::XrefType};
use std::{fs, path::Path, process::Command};

fn document(with_form: bool) -> Document {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let content = doc.add_object(Stream::new(
        dictionary! {},
        b"0.3 0.55 0.8 rg 20 30 80 60 re f".to_vec(),
    ));
    let mut page = dictionary! {
        "Type" => "Page", "Parent" => pages_id, "Contents" => content,
        "MediaBox" => vec![0.into(), 0.into(), 120.into(), 120.into()],
        "Resources" => dictionary!{},
    };
    let mut root = dictionary! { "Type" => "Catalog", "Pages" => pages_id };
    if with_form {
        let old = doc.add_object(dictionary! {
            "Type" => "Annot", "Subtype" => "Widget", "FT" => "Tx",
            "T" => Object::string_literal("Signature1"), "V" => Object::string_literal("kept"),
            "Rect" => vec![Object::Integer(0); 4],
        });
        let fields = doc.add_object(Object::Array(vec![old.into()]));
        let annotations = doc.add_object(Object::Array(vec![old.into()]));
        let form = doc.add_object(dictionary! { "Fields" => fields });
        root.set("AcroForm", form);
        page.set("Annots", annotations);
    }
    let page_id = doc.add_object(page);
    doc.set_object(
        pages_id,
        dictionary! { "Type" => "Pages", "Count" => 1, "Kids" => vec![page_id.into()] },
    );
    let root_id = doc.add_object(root);
    doc.trailer.set("Root", root_id);
    doc
}

fn serialize(mut doc: Document, modern: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    if modern {
        doc.save_modern(&mut bytes).unwrap();
    } else {
        doc.reference_table.cross_reference_type = XrefType::CrossReferenceTable;
        doc.save_to(&mut bytes).unwrap();
    }
    bytes
}

// Two pages, an indirect fields array, a field inheriting /FT from its parent,
// and a separate widget on the second page. Preserve the intervening objects.
fn nested_signature() -> (Document, ObjectId, ObjectId, ObjectId) {
    let mut doc = document(true);
    let first = doc.page_iter().next().unwrap();
    let parent = doc
        .get_dictionary(first)
        .unwrap()
        .get(b"Parent")
        .unwrap()
        .as_reference()
        .unwrap();
    let second = doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => parent,
        "MediaBox" => vec![0.into(), 0.into(), 500.into(), 400.into()],
        "CropBox" => vec![20.into(), 40.into(), 420.into(), 360.into()],
        "Rotate" => 90, "Resources" => dictionary!{},
    });
    let root_field = doc.new_object_id();
    let field = doc.new_object_id();
    let widget = doc.add_object(dictionary! {
        "Subtype" => "Widget", "Type" => "Annot", "Parent" => field,
        "Rect" => vec![120.into(), 120.into(), 220.into(), 280.into()], "F" => 4, "P" => second,
        "Contents" => Object::string_literal("existing widget"),
    });
    doc.set_object(
        root_field,
        dictionary! {
            "FT" => "Sig", "T" => Object::string_literal("Approvals"), "Kids" => vec![field.into()]
        },
    );
    doc.set_object(field, dictionary! {
        "Parent" => root_field, "T" => Object::string_literal("Reviewer"), "Kids" => vec![widget.into()]
    });
    let pages = doc.get_dictionary_mut(parent).unwrap();
    pages.set("Kids", vec![first.into(), second.into()]);
    pages.set("Count", 2);
    doc.get_dictionary_mut(second)
        .unwrap()
        .set("Annots", vec![widget.into()]);
    let (form, _) = acroform(&doc).unwrap();
    let fields = doc
        .get_dictionary(form.unwrap())
        .unwrap()
        .get(b"Fields")
        .unwrap()
        .as_reference()
        .unwrap();
    doc.get_object_mut(fields)
        .unwrap()
        .as_array_mut()
        .unwrap()
        .push(root_field.into());
    (doc, root_field, field, widget)
}

fn shape(object: &Object) -> Vec<f32> {
    object
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o.as_float().unwrap())
        .collect()
}

#[test]
fn signs_only_the_selected_nested_field_and_its_separate_widget() {
    let (mut doc, root_field, field_id, widget_id) = nested_signature();
    doc.get_dictionary_mut(field_id)
        .unwrap()
        .set("TU", lopdf::text_string("品質保証責任者"));
    let (form_id, form) = acroform(&doc).unwrap();
    let original_fields = form.get(b"Fields").unwrap().clone();
    let second = doc.page_iter().nth(1).unwrap();
    let original = serialize(doc, true);
    let inventory = inspect(&original).unwrap();
    assert_eq!(inventory["pages"], 2);
    let list = inventory["fields"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["name"], "Approvals.Reviewer");
    assert_eq!(list[0]["tooltip"], "品質保証責任者");
    assert_eq!(list[0]["page"], 2);
    let target = Target::Field(list[0]["id"].as_str().unwrap().to_owned());
    assert_eq!(
        locate(&original, &target).unwrap(),
        (2, [0.25, 0.25, 0.75, 0.5])
    );
    let signed = sign_visible(original.clone(), &target, "Test Signer", None, |_, _| {
        Ok(vec![0x30])
    })
    .unwrap();
    assert!(signed.starts_with(&original));
    let parsed = Document::load_mem(&signed).unwrap();
    let root = parsed.get_dictionary(root_field).unwrap();
    let field = parsed.get_dictionary(field_id).unwrap();
    let widget = parsed.get_dictionary(widget_id).unwrap();
    assert!(!root.has(b"V") && !field.has(b"AP") && !widget.has(b"V"));
    assert!(
        field
            .get_deref(b"V", &parsed)
            .unwrap()
            .as_dict()
            .unwrap()
            .has_type(b"Sig")
    );
    assert_eq!(
        parsed
            .get_dictionary(form_id.unwrap())
            .unwrap()
            .get(b"Fields")
            .unwrap(),
        &original_fields
    );
    assert_eq!(
        array(&parsed, parsed.get_dictionary(second).unwrap(), b"Annots").unwrap(),
        [widget_id.into()]
    );
    assert_eq!(
        widget.get(b"Contents").unwrap().as_str().unwrap(),
        b"existing widget"
    );
    let ap = widget
        .get(b"AP")
        .unwrap()
        .as_dict()
        .unwrap()
        .get_deref(b"N", &parsed)
        .unwrap()
        .as_stream()
        .unwrap();
    assert_eq!(shape(ap.dict.get(b"BBox").unwrap()), [0., 0., 160., 100.]);
    assert_eq!(
        shape(ap.dict.get(b"Matrix").unwrap()),
        [0., 1., -1., 0., 100., 0.]
    );
}

#[test]
fn merged_fields_update_in_place_and_restricted_fields_are_never_offered() {
    let (mut doc, parent, field, widget) = nested_signature();
    let page = doc.page_iter().nth(1).unwrap();
    let annotation = doc.get_dictionary(widget).unwrap().clone();
    let merged = doc.get_dictionary_mut(field).unwrap();
    merged.remove(b"Kids");
    for key in [b"Type".as_slice(), b"Subtype", b"Rect", b"F", b"P"] {
        merged.set(key, annotation.get(key).unwrap().clone());
    }
    let null = doc.add_object(Object::Null);
    let merged = doc.get_dictionary_mut(field).unwrap();
    merged.set("FT", "Sig");
    merged.set("V", null);
    merged.set("F", 132); // Printable and Locked; filling preserves widget properties.
    doc.get_dictionary_mut(page)
        .unwrap()
        .set("Annots", vec![field.into()]);
    let source = serialize(doc.clone(), false);
    let listing = inspect(&source).unwrap();
    let target = Target::Field(listing["fields"][0]["id"].as_str().unwrap().to_owned());
    let signed = sign_visible(source, &target, "Reviewer", None, |_, _| Ok(vec![1])).unwrap();
    let parsed = Document::load_mem(&signed).unwrap();
    let merged = parsed.get_dictionary(field).unwrap();
    assert!(merged.has(b"V") && merged.has(b"AP"));
    assert_eq!(merged.get(b"F").unwrap().as_i64().unwrap(), 132);
    assert_eq!(
        merged.get(b"Rect").unwrap(),
        annotation.get(b"Rect").unwrap()
    );
    for (id, key, value) in [
        (parent, "Ff", Object::Integer(1)),
        (parent, "Lock", dictionary! {}.into()),
        (field, "SV", dictionary! {}.into()),
        (field, "F", Object::Integer(32)),
        (field, "F", Object::Integer(512)),
        (field, "MK", dictionary! { "R" => 90 }.into()),
        (field, "P", doc.page_iter().next().unwrap().into()),
        (field, "Rect", vec![Object::Integer(0); 4].into()),
    ] {
        let mut restricted = doc.clone();
        restricted.get_dictionary_mut(id).unwrap().set(key, value);
        let bytes = serialize(restricted, false);
        assert!(
            inspect(&bytes).unwrap()["fields"]
                .as_array()
                .unwrap()
                .is_empty(),
            "{key}"
        );
        assert!(locate(&bytes, &target).is_err());
        assert!(sign_visible(bytes, &target, "Reviewer", None, |_, _| panic!()).is_err());
    }
}

#[test]
fn new_visible_fields_follow_the_rendered_crop_at_all_quarter_turns() {
    let unit = [0.125, 0.25, 0.625, 0.75];
    for (rotation, rect, matrix, bbox) in [
        (0, [70., 120., 270., 280.], None, [0., 0., 200., 160.]),
        (
            90,
            [120., 80., 320., 240.],
            Some([0., 1., -1., 0., 200., 0.]),
            [0., 0., 160., 200.],
        ),
        (
            180,
            [170., 120., 370., 280.],
            Some([-1., 0., 0., -1., 200., 160.]),
            [0., 0., 200., 160.],
        ),
        (
            270,
            [120., 160., 320., 320.],
            Some([0., -1., 1., 0., 0., 160.]),
            [0., 0., 160., 200.],
        ),
    ] {
        let (mut doc, ..) = nested_signature();
        let page = doc.page_iter().nth(1).unwrap();
        let parent = doc
            .get_dictionary(page)
            .unwrap()
            .get(b"Parent")
            .unwrap()
            .as_reference()
            .unwrap();
        doc.get_dictionary_mut(page).unwrap().remove(b"Rotate");
        doc.get_dictionary_mut(parent)
            .unwrap()
            .set("Rotate", rotation);
        let original = serialize(doc, false);
        let target = Target::Area {
            page: 2,
            rect: unit,
        };
        assert_eq!(locate(&original, &target).unwrap(), (2, unit));
        let signed =
            sign_visible(original, &target, "Test Signer", None, |_, _| Ok(vec![1])).unwrap();
        let parsed = Document::load_mem(&signed).unwrap();
        let annots = array(&parsed, parsed.get_dictionary(page).unwrap(), b"Annots").unwrap();
        assert_eq!(annots.len(), 2);
        let widget = parsed
            .dereference(annots.last().unwrap())
            .unwrap()
            .1
            .as_dict()
            .unwrap();
        assert_eq!(shape(widget.get(b"Rect").unwrap()), rect);
        let ap = widget
            .get(b"AP")
            .unwrap()
            .as_dict()
            .unwrap()
            .get_deref(b"N", &parsed)
            .unwrap()
            .as_stream()
            .unwrap();
        assert_eq!(shape(ap.dict.get(b"BBox").unwrap()), bbox);
        if let Some(matrix) = matrix {
            assert_eq!(shape(ap.dict.get(b"Matrix").unwrap()), matrix);
        } else if let Ok(identity) = ap.dict.get(b"Matrix") {
            assert_eq!(shape(identity), [1., 0., 0., 1., 0., 0.]);
        }
    }
}

#[test]
fn ambiguous_widgets_and_duplicate_terminal_names_cannot_be_selected() {
    let (doc, parent, field, widget) = nested_signature();
    let page = doc.page_iter().nth(1).unwrap();
    let bytes = serialize(doc.clone(), false);
    let listing = inspect(&bytes).unwrap();
    let id = Target::Field(listing["fields"][0]["id"].as_str().unwrap().into());
    for shared_terminal in [true, false] {
        let mut doc = doc.clone();
        let mut second = doc.get_dictionary(widget).unwrap().clone();
        let second_id = doc.new_object_id();
        if shared_terminal {
            doc.get_dictionary_mut(field)
                .unwrap()
                .set("Kids", vec![widget.into(), second_id.into()]);
        } else {
            second.set("Parent", parent);
            second.set("T", Object::string_literal("Reviewer"));
            doc.get_dictionary_mut(parent)
                .unwrap()
                .set("Kids", vec![field.into(), second_id.into()]);
        }
        doc.set_object(second_id, second);
        doc.get_dictionary_mut(page)
            .unwrap()
            .set("Annots", vec![widget.into(), second_id.into()]);
        let bytes = serialize(doc, false);
        assert_eq!(
            inspect(&bytes).unwrap()["fields"].as_array().unwrap().len(),
            0
        );
        assert!(sign_visible(bytes, &id, "Test", None, |_, _| panic!()).is_err());
    }
}

#[test]
fn a_transparent_signature_upgrades_an_older_catalog_and_keeps_its_new_form() {
    let mut doc = document(false);
    doc.version = "1.3".into();
    let target = Target::Area {
        page: 1,
        rect: [0.0, 0.0, 0.8, 0.3],
    };
    let bytes = sign_visible(serialize(doc, false), &target, "Test", None, |_, _| {
        Ok(vec![1])
    })
    .unwrap();
    let parsed = Document::load_mem(&bytes).unwrap();
    assert_eq!(
        parsed
            .catalog()
            .unwrap()
            .get(b"Version")
            .unwrap()
            .as_name()
            .unwrap(),
        b"1.4"
    );
    assert_eq!(
        array(&parsed, &acroform(&parsed).unwrap().1, b"Fields")
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn new_field_name_accounts_for_anonymous_form_groups() {
    let (mut doc, parent, child, _) = nested_signature();
    doc.get_dictionary_mut(parent).unwrap().remove(b"T");
    doc.get_dictionary_mut(child)
        .unwrap()
        .set("T", Object::string_literal("Signature2"));
    let (_, form) = acroform(&doc).unwrap();
    assert_eq!(
        next_field_name(&doc, &array(&doc, &form, b"Fields").unwrap()).unwrap(),
        "Signature3"
    );
}

#[test]
fn refuses_invalid_area_before_calling_the_provider() {
    let bytes = serialize(document(false), false);
    for (page, rect) in [
        (0, [0., 0., 1., 1.]),
        (2, [0., 0., 1., 1.]),
        (1, [0., 0., 1., f32::NAN]),
        (1, [0., 0., 1.1, 1.]),
        (1, [0., 0.2, 0.8, 0.2]),
        (1, [1., 1., 0., 0.]),
    ] {
        let target = Target::Area { page, rect };
        assert!(locate(&bytes, &target).is_err());
        assert!(sign_visible(bytes.clone(), &target, "Test", None, |_, _| panic!()).is_err());
    }
}

#[test]
fn covers_every_byte_except_contents_and_preserves_forms_and_pages() {
    for modern in [false, true] {
        for existing_form in [false, true] {
            let original = serialize(document(existing_form), modern);
            let mut parts = (Vec::new(), Vec::new());
            let signed = sign_pdf(original.clone(), |a, b| {
                parts = (a.to_vec(), b.to_vec());
                Ok(vec![0x30, 0xad])
            })
            .unwrap();
            assert!(signed.starts_with(&original));
            let result = Document::load_mem(&signed).unwrap();
            assert!(result.xref_recovery.is_none());
            assert_eq!(result.revision_count(), 2);
            let form = result
                .catalog()
                .unwrap()
                .get_deref(b"AcroForm", &result)
                .unwrap()
                .as_dict()
                .unwrap();
            let fields = array(&result, form, b"Fields").unwrap();
            assert_eq!(fields.len(), 1 + usize::from(existing_form));
            assert_eq!(form.get(b"SigFlags").unwrap().as_i64().unwrap(), 3);
            let page = result.page_iter().next().unwrap();
            let annots = array(&result, result.get_dictionary(page).unwrap(), b"Annots").unwrap();
            assert_eq!(annots, fields);
            if existing_form {
                let old = result.dereference(&fields[0]).unwrap().1.as_dict().unwrap();
                assert_eq!(old.get(b"V").unwrap().as_str().unwrap(), b"kept");
            }
            let field = result
                .dereference(fields.last().unwrap())
                .unwrap()
                .1
                .as_dict()
                .unwrap();
            let expected = if existing_form {
                "Signature2"
            } else {
                "Signature1"
            };
            assert_eq!(
                field.get(b"T").unwrap().as_str().unwrap(),
                expected.as_bytes()
            );
            assert_eq!(field.get(b"P").unwrap().as_reference().unwrap(), page);
            let signature = field.get_deref(b"V", &result).unwrap().as_dict().unwrap();
            let data = signature.get(b"Contents").unwrap().as_str().unwrap();
            assert_eq!(data.len(), CMS_CAPACITY);
            assert_eq!(&data[..2], &[0x30, 0xad]);
            assert!(data[2..].iter().all(|&x| x == 0));
            let range: Vec<usize> = signature
                .get(b"ByteRange")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|o| o.as_i64().unwrap() as usize)
                .collect();
            assert_eq!(
                range,
                [
                    0,
                    parts.0.len(),
                    signed.len() - parts.1.len(),
                    parts.1.len()
                ]
            );
            assert_eq!(&signed[..range[1]], &parts.0);
            assert_eq!(&signed[range[2]..], &parts.1);
            assert!(signed[range[1]..range[2]].starts_with(b"<30AD"));
            assert_eq!(signed[range[2] - 1], b'>');
            assert_eq!(range[2] - range[1], 2 + 2 * CMS_CAPACITY);
        }
    }
}

#[test]
fn refuses_malformed_signature_ranges_encryption_and_bad_containers_before_signing() {
    let original = serialize(document(false), false);
    let mut signed = sign_pdf(original, |_, _| Ok(vec![1])).unwrap();
    let marker = b"/ByteRange[";
    let start = memmem::find(&signed, marker).unwrap() + marker.len();
    signed[start] = b'1'; // The signature no longer covers the start of the PDF.
    assert!(sign_pdf(signed, |_, _| panic!()).is_err());
    assert!(sign_pdf(b"not a PDF".to_vec(), |_, _| panic!()).is_err());

    for password in ["", "reader"] {
        let mut doc = document(false);
        doc.trailer
            .set("ID", vec![Object::string_literal("0123456789ABCDEF"); 2]);
        let encryption = EncryptionState::try_from(EncryptionVersion::V2 {
            document: &doc,
            owner_password: "owner",
            user_password: password,
            key_length: 128,
            permissions: Permissions::PRINTABLE,
        })
        .unwrap();
        doc.encrypt(&encryption).unwrap();
        let encrypted = serialize(doc, false);
        assert_eq!(sign_pdf(encrypted, |_, _| panic!()).unwrap_err(), ENCRYPTED);
    }
}

#[test]
fn rejects_empty_or_oversized_cms_and_propagates_provider_error() {
    let original = serialize(document(false), false);
    assert!(sign_pdf(original.clone(), |_, _| Ok(Vec::new())).is_err());
    assert!(sign_pdf(original.clone(), |_, _| Ok(vec![0; CMS_CAPACITY + 1])).is_err());
    assert_eq!(
        sign_pdf(original, |_, _| Err("cancelled".into())),
        Err("cancelled".into())
    );
}

fn run(cwd: &Path, tool: &str, args: &[&str]) -> String {
    let result = Command::new(tool)
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{tool} {args:?}\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8_lossy(&result.stdout).into_owned()
}

fn openssl_signer<'a>(
    dir: &'a Path,
    name: &str,
) -> impl Fn(&[u8], &[u8]) -> Result<Vec<u8>, String> + 'a {
    let key = format!("{name}-key.pem");
    let cert = format!("{name}-cert.pem");
    run(
        dir,
        "openssl",
        &[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            &key,
            "-out",
            &cert,
            "-days",
            "1",
            "-subj",
            &format!("/CN={name}"),
        ],
    );
    move |a, b| {
        fs::write(dir.join("content.bin"), [a, b].concat()).unwrap();
        run(
            dir,
            "openssl",
            &[
                "cms",
                "-sign",
                "-binary",
                "-md",
                "sha256",
                "-signer",
                &cert,
                "-inkey",
                &key,
                "-in",
                "content.bin",
                "-outform",
                "DER",
                "-out",
                "signature.der",
            ],
        );
        Ok(fs::read(dir.join("signature.der")).unwrap())
    }
}

fn render_page(dir: &Path, pdf: &str, page: u32) -> Vec<u8> {
    let page = page.to_string();
    run(
        dir,
        "pdftoppm",
        &[
            "-f",
            &page,
            "-l",
            &page,
            "-singlefile",
            "-r",
            "72",
            pdf,
            "page",
        ],
    );
    fs::read(dir.join("page.ppm")).unwrap()
}

fn tampered_report(dir: &Path, pdf: &str) -> String {
    let check = Command::new("pdfsig")
        .current_dir(dir)
        .args(["-nssdir", "sql:.", "-nocert", pdf])
        .output()
        .unwrap();
    String::from_utf8(check.stdout).unwrap()
}

fn signature_report<'a>(report: &'a str, signer: &str) -> &'a str {
    report
        .split("Signature #")
        .skip(1)
        .find(|s| s.contains(signer))
        .unwrap_or_else(|| panic!("Missing {signer} in {report}"))
}

#[test]
#[ignore = "requires openssl, NSS certutil and pk12util, pdfsig and pdftoppm"]
fn verify_with_openssl_and_poppler() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    run(dir, "certutil", &["-N", "-d", "sql:.", "--empty-password"]);
    let alice = openssl_signer(dir, "SumiPDF Alice");
    let bob = openssl_signer(dir, "SumiPDF Bob");
    let original = serialize(document(false), false);
    fs::write(dir.join("original.pdf"), &original).unwrap();
    let signed = sign_pdf(original, &alice).unwrap();
    fs::write(dir.join("signed.pdf"), &signed).unwrap();
    let report = run(
        dir,
        "pdfsig",
        &["-nssdir", "sql:.", "-nocert", "signed.pdf"],
    );
    assert!(report.contains("Signature is Valid."), "{report}");
    assert!(report.contains("Total document signed"), "{report}");
    assert!(report.contains("SumiPDF Alice"), "{report}");
    assert_eq!(
        render_page(dir, "original.pdf", 1),
        render_page(dir, "signed.pdf", 1)
    );
    let (doc, _, existing_field, existing_widget) = nested_signature();
    let unsigned = serialize(doc, true);
    fs::write(dir.join("unsigned.pdf"), &unsigned).unwrap();
    // Let an independent PDF writer produce the first approval in the prepared field.
    run(
        dir,
        "openssl",
        &[
            "pkcs12",
            "-export",
            "-in",
            "SumiPDF Alice-cert.pem",
            "-inkey",
            "SumiPDF Alice-key.pem",
            "-name",
            "alice",
            "-passout",
            "pass:test",
            "-out",
            "alice.p12",
        ],
    );
    run(
        dir,
        "pk12util",
        &["-d", "sql:.", "-i", "alice.p12", "-W", "test"],
    );
    run(
        dir,
        "pdfsig",
        &[
            "-nssdir",
            "sql:.",
            "-nick",
            "alice",
            "-sign",
            "Approvals.Reviewer",
            "unsigned.pdf",
            "external.pdf",
        ],
    );
    let fields = inspect(&unsigned).unwrap();
    let existing = Target::Field(fields["fields"][0]["id"].as_str().unwrap().into());
    let area = Target::Area {
        page: 1,
        rect: [0.1, 0.1, 0.9, 0.3],
    };
    for (first_target, second_target) in [(&existing, &area), (&area, &existing)] {
        let first = if first_target == &existing {
            fs::read(dir.join("external.pdf")).unwrap()
        } else {
            sign_visible(
                unsigned.clone(),
                first_target,
                "SumiPDF Alice",
                None,
                &alice,
            )
            .unwrap()
        };
        assert!(first.starts_with(&unsigned));
        let inventory = inspect(&first).unwrap();
        let available = inventory["fields"].as_array().unwrap();
        let before = Document::load_mem(&first).unwrap();
        let (first_field, first_widget, first_name, second_name) = if first_target == &existing {
            assert!(available.is_empty());
            (
                existing_field,
                existing_widget,
                "Approvals.Reviewer",
                "Signature2",
            )
        } else {
            assert_eq!(available, fields["fields"].as_array().unwrap());
            let roots = array(&before, &acroform(&before).unwrap().1, b"Fields").unwrap();
            let added = roots.last().unwrap().as_reference().unwrap();
            (added, added, "Signature2", "Approvals.Reviewer")
        };
        let first_id = Target::Field(format!("field-{}-{}", first_field.0, first_field.1));
        assert!(
            sign_visible(
                first.clone(),
                &first_id,
                "SumiPDF Bob",
                None,
                |_, _| panic!()
            )
            .is_err()
        );

        let both = sign_visible(first.clone(), second_target, "SumiPDF Bob", None, &bob).unwrap();
        assert!(both.starts_with(&first));
        let after = Document::load_mem(&both).unwrap();
        assert!(after.xref_recovery.is_none());
        assert_eq!(after.revision_count(), before.revision_count() + 1);
        let sig = before
            .get_dictionary(first_field)
            .unwrap()
            .get(b"V")
            .unwrap()
            .as_reference()
            .unwrap();
        for object in [first_field, first_widget, sig] {
            assert_eq!(
                after.get_object(object).unwrap(),
                before.get_object(object).unwrap()
            );
        }
        fs::write(dir.join("first.pdf"), &first).unwrap();
        fs::write(dir.join("both.pdf"), &both).unwrap();
        // Each visible signature changes only its own page, in either signing order.
        let first_page = locate(&unsigned, first_target).unwrap().0;
        for page in [1, 2] {
            let original_pixels = render_page(dir, "unsigned.pdf", page);
            let first_pixels = render_page(dir, "first.pdf", page);
            let both_pixels = render_page(dir, "both.pdf", page);
            assert_eq!(original_pixels != first_pixels, page == first_page);
            assert_eq!(first_pixels != both_pixels, page != first_page);
        }
        let report = run(dir, "pdfsig", &["-nssdir", "sql:.", "-nocert", "both.pdf"]);
        for (signer, field, total) in [
            ("SumiPDF Alice", first_name, false),
            ("SumiPDF Bob", second_name, true),
        ] {
            let block = signature_report(&report, signer);
            assert!(
                block.contains(field) && block.contains("Signature is Valid."),
                "{report}"
            );
            assert_eq!(block.contains("Total document signed"), total, "{report}");
        }
        // Change the second revision only. The first signature should still verify.
        let mut altered = both;
        let date = memmem::find(&altered[first.len()..], b"D:").unwrap() + first.len() + 2;
        altered[date] = b'1'; // change the year without changing offsets
        fs::write(dir.join("altered.pdf"), altered).unwrap();
        let report = tampered_report(dir, "altered.pdf");
        assert!(
            signature_report(&report, "SumiPDF Alice").contains("Signature is Valid."),
            "{report}"
        );
        assert!(
            signature_report(&report, "SumiPDF Bob").contains("Digest Mismatch"),
            "{report}"
        );
    }
    // Make one visible drawing coordinate differ, without changing offsets.
    let mut tampered = signed;
    let pos = tampered.windows(5).position(|s| s == b"80 60").unwrap();
    tampered[pos] = b'5';
    fs::write(dir.join("tampered.pdf"), tampered).unwrap();
    let report = tampered_report(dir, "tampered.pdf");
    assert!(report.contains("Digest Mismatch"), "{report}");
}

#[test]
#[ignore = "requires pdftoppm"]
fn renders_the_same_upright_stamp_at_all_page_rotations() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let target = Target::Area {
        page: 1,
        rect: [0.125, 0.25, 0.875, 0.5],
    };
    let mut reference: Option<Vec<[u8; 3]>> = None;
    for rotation in [0, 90, 180, 270] {
        let mut doc = document(false);
        let page_id = doc.page_iter().next().unwrap();
        let page = doc.get_dictionary_mut(page_id).unwrap();
        page.remove(b"Contents");
        page.set("MediaBox", vec![0.into(), 0.into(), 400.into(), 400.into()]);
        page.set(
            "CropBox",
            vec![30.into(), 40.into(), 350.into(), 360.into()],
        );
        page.set("Rotate", rotation);
        let result = sign_visible(
            serialize(doc, false),
            &target,
            "Test Signer",
            None,
            |_, _| Ok(vec![1]),
        )
        .unwrap();
        fs::write(dir.join("test.pdf"), result).unwrap();
        run(
            dir,
            "pdftoppm",
            &["-cropbox", "-r", "72", "-singlefile", "test.pdf", "render"],
        );
        let ppm = fs::read(dir.join("render.ppm")).unwrap();
        let (pixels, tail) = ppm
            .strip_prefix(b"P6\n320 320\n255\n")
            .unwrap()
            .as_chunks::<3>();
        assert!(tail.is_empty());
        let mut ink = 0;
        for (i, rgb) in pixels.iter().enumerate() {
            if rgb.iter().any(|&channel| channel < 230) {
                assert!(
                    (40..280).contains(&(i % 320)) && (80..160).contains(&(i / 320)),
                    "stamp escaped target at rotation {rotation}"
                );
                ink += 1;
            }
        }
        assert!(ink > 100, "missing visible stamp at rotation {rotation}");
        if let Some(first) = &reference {
            // Different seconds on the on-stamp clock can change the last digits.
            let changed = pixels
                .iter()
                .zip(first)
                .filter(|(a, b)| a.iter().zip(b.iter()).any(|(v, w)| v.abs_diff(*w) > 20))
                .count();
            assert!(
                changed <= ink / 10,
                "{changed} different pixels at rotation {rotation}"
            );
        } else {
            reference = Some(pixels.to_vec());
        }
    }
}
