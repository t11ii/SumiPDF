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
fn refuses_existing_signatures_encryption_and_bad_containers_before_signing() {
    let original = serialize(document(false), false);
    let signed = sign_pdf(original, |_, _| Ok(vec![1])).unwrap();
    assert_eq!(
        sign_pdf(signed, |_, _| panic!()).unwrap_err(),
        ALREADY_SIGNED
    );
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

#[test]
#[ignore = "requires openssl, NSS certutil, pdfsig and pdftoppm"]
fn verify_with_openssl_and_poppler() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    run(dir, "certutil", &["-N", "-d", "sql:.", "--empty-password"]);
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
            "key.pem",
            "-out",
            "cert.pem",
            "-days",
            "1",
            "-subj",
            "/CN=SumiPDF Test",
        ],
    );
    let original = serialize(document(false), false);
    fs::write(dir.join("original.pdf"), &original).unwrap();
    let signed = sign_pdf(original, |a, b| {
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
                "cert.pem",
                "-inkey",
                "key.pem",
                "-in",
                "content.bin",
                "-outform",
                "DER",
                "-out",
                "signature.der",
            ],
        );
        Ok(fs::read(dir.join("signature.der")).unwrap())
    })
    .unwrap();
    fs::write(dir.join("signed.pdf"), &signed).unwrap();
    let report = run(
        dir,
        "pdfsig",
        &["-nssdir", "sql:.", "-nocert", "signed.pdf"],
    );
    assert!(report.contains("Signature is Valid."), "{report}");
    assert!(report.contains("Total document signed"), "{report}");
    assert!(report.contains("SumiPDF Test"), "{report}");
    for stem in ["original", "signed"] {
        run(
            dir,
            "pdftoppm",
            &["-r", "72", "-singlefile", &format!("{stem}.pdf"), stem],
        );
    }
    assert_eq!(
        fs::read(dir.join("original.ppm")).unwrap(),
        fs::read(dir.join("signed.ppm")).unwrap()
    );
    // Make one visible drawing coordinate differ, without changing offsets.
    let mut tampered = signed;
    let pos = tampered.windows(5).position(|s| s == b"80 60").unwrap();
    tampered[pos] = b'5';
    fs::write(dir.join("tampered.pdf"), tampered).unwrap();
    let check = Command::new("pdfsig")
        .current_dir(dir)
        .args(["-nssdir", "sql:.", "-nocert", "tampered.pdf"])
        .output()
        .unwrap();
    let report = String::from_utf8_lossy(&check.stdout);
    assert!(report.contains("Digest Mismatch"), "{report}");
}
