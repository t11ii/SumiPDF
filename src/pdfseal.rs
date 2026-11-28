//! Create a PDF approval signature in an incremental revision.
//! The callback signs exactly the two slices declared by /ByteRange.
mod fields;
mod permissions;

use crate::appearance::{self, Artwork};
use lopdf::{Dictionary, Document, IncrementalDocument, Object, ObjectId, StringFormat, Writer};
use lopdf::{decode_text_string, dictionary};
use memchr::memmem;
use permissions::Permissions;
use std::collections::HashSet;
use time::OffsetDateTime;

const CMS_CAPACITY: usize = 32 * 1024;
const ENCRYPTED: &str = "暗号化されたPDFは編集できません。";
const ALREADY_SIGNED: &str = "署名や証明で保護された PDF には注釈を追加できません。署名を追加するには「電子署名…」を選んでください。";
const PLACEHOLDER_ERROR: &str = "PDFの署名領域を特定できませんでした。";
const TOO_LARGE: &str = "署名済みPDFが4 GiBに達する文書には対応していません。";

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Field(String),
    /// One-based page and rectangle on the rendered crop, measured from its top left.
    Area {
        page: u32,
        rect: [f32; 4],
    },
}

pub fn inspect(input: &[u8]) -> Result<serde_json::Value, String> {
    let (doc, permissions) = signing_document(input)?;
    let (_, form) = acroform(&doc)?;
    fields::inspect(&doc, &array(&doc, &form, b"Fields")?, &permissions)
}

pub fn locate(input: &[u8], target: &Target) -> Result<(u32, [f32; 4]), String> {
    let (doc, permissions) = signing_document(input)?;
    let (_, form) = acroform(&doc)?;
    let placement = fields::resolve(&doc, &array(&doc, &form, b"Fields")?, target, &permissions)?;
    Ok((placement.page, placement.unit))
}

pub(crate) fn pdf_error(error: impl std::fmt::Display) -> String {
    format!("PDFの構造を読み書きできませんでした。\n{error}")
}

pub fn sign_pdf(
    input: Vec<u8>,
    signer: impl FnOnce(&[u8], &[u8]) -> Result<Vec<u8>, String>,
) -> Result<Vec<u8>, String> {
    sign(input, None, signer)
}

struct Visible<'a> {
    target: &'a Target,
    name: &'a str,
    artwork: Artwork<'a>,
}

pub fn sign_visible(
    input: Vec<u8>,
    target: &Target,
    name: &str,
    artwork: Artwork<'_>,
    signer: impl FnOnce(&[u8], &[u8]) -> Result<Vec<u8>, String>,
) -> Result<Vec<u8>, String> {
    sign(
        input,
        Some(Visible {
            target,
            name,
            artwork,
        }),
        signer,
    )
}

fn sign(
    input: Vec<u8>,
    visible: Option<Visible<'_>>,
    signer: impl FnOnce(&[u8], &[u8]) -> Result<Vec<u8>, String>,
) -> Result<Vec<u8>, String> {
    let original_len = input.len();
    // lopdf serializes cross-reference offsets into u32 fields.
    if original_len.saturating_add(CMS_CAPACITY * 2) > u32::MAX as usize {
        return Err(TOO_LARGE.into());
    }
    let (mut output, signature_body) = prepare_revision(input, visible)?;
    if output.len() > u32::MAX as usize {
        return Err(TOO_LARGE.into());
    }
    // Only patch our own serialized dictionary in the appended revision. Data in
    // the original PDF may contain the same PDF keywords and arbitrary streams.
    let mut matches = memmem::find_iter(&output[original_len..], &signature_body);
    let index = original_len + matches.next().ok_or(PLACEHOLDER_ERROR)?;
    if matches.next().is_some() {
        return Err(PLACEHOLDER_ERROR.into());
    }

    let after_key = |key: &[u8]| {
        signature_body
            .windows(key.len())
            .position(|s| s == key)
            .map(|pos| index + pos + key.len())
            .ok_or(PLACEHOLDER_ERROR)
    };
    let contents = after_key(b"/Contents<")? - 1;
    let after_contents = contents + CMS_CAPACITY * 2 + 2;
    let range_start = after_key(b"/ByteRange[")?;
    let range_len = output[range_start..]
        .iter()
        .position(|&b| b == b']')
        .ok_or(PLACEHOLDER_ERROR)?;
    let range_text = format!(
        "0 {contents} {after_contents} {}",
        output.len() - after_contents
    );
    if range_text.len() > range_len || output[after_contents - 1] != b'>' {
        return Err(PLACEHOLDER_ERROR.into());
    }
    let range = &mut output[range_start..range_start + range_len];
    range.fill(b' ');
    range[..range_text.len()].copy_from_slice(range_text.as_bytes());

    let cms = signer(&output[..contents], &output[after_contents..])?;
    if cms.is_empty() || cms.len() > CMS_CAPACITY {
        return Err(format!(
            "証明書を含む署名データが空、または上限（{CMS_CAPACITY}バイト）を超えています。"
        ));
    }
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let (pairs, _) = output[contents + 1..after_contents - 1].as_chunks_mut::<2>();
    for (dest, byte) in pairs.iter_mut().zip(cms) {
        dest[0] = HEX[(byte >> 4) as usize];
        dest[1] = HEX[(byte & 15) as usize];
    }
    Ok(output)
}

fn load_document(input: &[u8]) -> Result<Document, String> {
    let doc = Document::load_mem(input).map_err(|e| match e {
        lopdf::Error::InvalidPassword | lopdf::Error::UnsupportedSecurityHandler(_) => {
            ENCRYPTED.into()
        }
        _ => pdf_error(e),
    })?;
    check_editable_source(input, &doc)?;
    Ok(doc)
}

fn check_editable_source(input: &[u8], doc: &Document) -> Result<(), String> {
    if doc.was_encrypted() || doc.trailer.has(b"Encrypt") {
        return Err(ENCRYPTED.into());
    }
    if !input.starts_with(b"%PDF-") || doc.xref_recovery.is_some() || doc.xref_start == 0 {
        return Err("PDFの相互参照が不完全なため、元の内容を保って編集できません。".into());
    }
    Ok(())
}

/// Annotation changes are still limited to unsigned documents.
pub(crate) fn editable_document(input: &[u8]) -> Result<Document, String> {
    let doc = load_document(input)?;
    check_unsigned_for_annotations(&doc)?;
    Ok(doc)
}

pub(crate) fn allows_annotations(input: &[u8], doc: &Document) -> bool {
    check_editable_source(input, doc).is_ok() && check_unsigned_for_annotations(doc).is_ok()
}

fn check_unsigned_for_annotations(doc: &Document) -> Result<(), String> {
    if doc.catalog().map_err(pdf_error)?.has(b"Perms")
        || doc.objects.values().any(|o| contains_signature(doc, o))
    {
        return Err(ALREADY_SIGNED.into());
    }
    Ok(())
}

fn signing_document(input: &[u8]) -> Result<(Document, Permissions), String> {
    let doc = load_document(input)?;
    let permissions = Permissions::read(&doc, input)?;
    if !permissions.allows_new_field() && needs_transparency_upgrade(&doc)? {
        return Err("この PDF は署名の外観を表示するために文書のバージョン変更が必要ですが、既存の署名がその変更を制限しています。".into());
    }
    Ok((doc, permissions))
}

fn acroform(doc: &Document) -> Result<(Option<ObjectId>, Dictionary), String> {
    let (id, form) = match doc.catalog().map_err(pdf_error)?.get(b"AcroForm").ok() {
        None | Some(Object::Null) => (None, Dictionary::new()),
        Some(object) => {
            let (id, form) = doc.dereference(object).map_err(pdf_error)?;
            (id, form.as_dict().map_err(pdf_error)?.clone())
        }
    };
    if form.has(b"XFA") {
        return Err("XFA形式のフォームへの署名には対応していません。".into());
    }
    Ok((id, form))
}

/// Blend mode and image soft masks in annotation appearances require PDF 1.4.
pub(crate) fn require_transparency(revision: &mut IncrementalDocument) -> Result<(), String> {
    let original = revision.get_prev_documents();
    if !needs_transparency_upgrade(original)? {
        return Ok(());
    }
    let root = original
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .map_err(pdf_error)?;
    revision
        .opt_clone_object_to_new_document(root)
        .map_err(pdf_error)?;
    revision
        .new_document
        .get_dictionary_mut(root)
        .map_err(pdf_error)?
        .set("Version", "1.4");
    Ok(())
}

fn needs_transparency_upgrade(doc: &Document) -> Result<bool, String> {
    if !matches!(doc.version.as_str(), "1.0" | "1.1" | "1.2" | "1.3") {
        return Ok(false);
    }
    let catalog = doc.catalog().map_err(pdf_error)?;
    if !catalog.has(b"Version") {
        return Ok(true);
    }
    let version = catalog
        .get_deref(b"Version", doc)
        .and_then(Object::as_name)
        .map_err(pdf_error)?;
    Ok(matches!(version, b"1.0" | b"1.1" | b"1.2" | b"1.3"))
}

fn prepare_revision(
    input: Vec<u8>,
    visible: Option<Visible<'_>>,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let (doc, permissions) = signing_document(&input)?;
    let (existing_form_id, mut form) = acroform(&doc)?;
    let mut fields = array(&doc, &form, b"Fields")?;
    let placement = visible
        .as_ref()
        .map(|v| fields::resolve(&doc, &fields, v.target, &permissions).map(|p| (v, p)))
        .transpose()?;
    if placement.is_none() && !permissions.allows_new_field() {
        return Err(fields::EXISTING_ONLY.into());
    }
    let flags = form
        .get(b"SigFlags")
        .ok()
        .map(|o| doc.dereference(o).and_then(|(_, o)| o.as_i64()))
        .transpose()
        .map_err(pdf_error)?
        .unwrap_or(0);

    let original_len = input.len();
    let mut revision = IncrementalDocument::create_from(input, doc);
    let now = OffsetDateTime::now_utc();
    let date = format!(
        "D:{:04}{:02}{:02}{:02}{:02}{:02}Z",
        now.year(),
        now.month() as u8,
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    );
    let signature = Object::Dictionary(dictionary! {
        "Type" => "Sig",
        "Filter" => "Adobe.PPKLite",
        "SubFilter" => "adbe.pkcs7.detached",
        "M" => Object::string_literal(date),
        "ByteRange" => vec![Object::Integer(i64::MAX); 4],
        "Contents" => Object::String(vec![0; CMS_CAPACITY], StringFormat::Hexadecimal),
    });
    let mut signature_body = Vec::new();
    Writer::write_object(&mut signature_body, &signature).map_err(pdf_error)?;
    let signature_id = revision
        .new_document
        .try_add_object(signature)
        .map_err(pdf_error)?;
    let new_widget = if let Some((visible, p)) = placement {
        let appearance = visible_appearance(&mut revision.new_document, &p, visible, now)?;
        if let Some((field, widget)) = p.existing {
            fill_field(&mut revision, field, widget, signature_id, appearance)?;
            None
        } else {
            Some(append_widget(
                &mut revision,
                &fields,
                p.page_id,
                p.rect.map(Object::Real).to_vec(),
                signature_id,
                Some(appearance),
            )?)
        }
    } else {
        let page = revision
            .get_prev_documents()
            .page_iter()
            .next()
            .ok_or("PDFにページがありません。")?;
        Some(append_widget(
            &mut revision,
            &fields,
            page,
            vec![Object::Integer(0); 4],
            signature_id,
            None,
        )?)
    };
    if let Some(id) = new_widget {
        fields.push(id.into());
        form.set("Fields", fields);
    }
    // Filling a prepared field in a signed document should change only the
    // field, its widget appearance and the new signature objects.
    if !permissions.signed() || new_widget.is_some() {
        form.set("SigFlags", flags | 3);
        if let Some(id) = existing_form_id {
            revision.new_document.set_object(id, form);
        } else {
            let root_id = revision
                .get_prev_documents()
                .trailer
                .get(b"Root")
                .and_then(Object::as_reference)
                .map_err(pdf_error)?;
            revision
                .opt_clone_object_to_new_document(root_id)
                .map_err(pdf_error)?;
            let update = &mut revision.new_document;
            // Keep a direct AcroForm direct when updating an already-signed catalog.
            let value = if permissions.signed() {
                Object::Dictionary(form)
            } else {
                update.try_add_object(form).map_err(pdf_error)?.into()
            };
            update
                .get_dictionary_mut(root_id)
                .map_err(pdf_error)?
                .set("AcroForm", value);
        }
    }
    if visible.is_some() {
        require_transparency(&mut revision)?;
    }

    // Leave headroom for the changed form, page and xref to avoid a
    // second file-sized allocation on ordinary documents.
    let mut output = Vec::new();
    output
        .try_reserve_exact(original_len + signature_body.len() + 8192)
        .map_err(|_| "署名に必要なメモリを確保できません。")?;
    revision.save_to(&mut output).map_err(pdf_error)?;
    Ok((output, signature_body))
}

fn fill_field(
    revision: &mut IncrementalDocument,
    field: ObjectId,
    widget: ObjectId,
    signature: ObjectId,
    appearance: ObjectId,
) -> Result<(), String> {
    revision
        .opt_clone_object_to_new_document(field)
        .map_err(pdf_error)?;
    revision
        .opt_clone_object_to_new_document(widget)
        .map_err(pdf_error)?;
    let update = &mut revision.new_document;
    update
        .get_dictionary_mut(field)
        .map_err(pdf_error)?
        .set("V", signature);
    let annot = update.get_dictionary_mut(widget).map_err(pdf_error)?;
    annot.remove(b"AS");
    annot.set("AP", dictionary! { "N" => appearance });
    Ok(())
}

fn append_widget(
    revision: &mut IncrementalDocument,
    fields: &[Object],
    page_id: ObjectId,
    rect: Vec<Object>,
    signature: ObjectId,
    appearance: Option<ObjectId>,
) -> Result<ObjectId, String> {
    let doc = revision.get_prev_documents();
    let field_name = next_field_name(doc, fields)?;
    let mut page = doc.get_dictionary(page_id).map_err(pdf_error)?.clone();
    let mut annotations = array(doc, &page, b"Annots")?;
    let mut widget = dictionary! {
        "Type" => "Annot", "Subtype" => "Widget", "FT" => "Sig",
        "Rect" => rect, "F" => 132, "P" => page_id,
        "T" => Object::string_literal(field_name), "V" => signature,
    };
    if let Some(id) = appearance {
        widget.set("AP", dictionary! { "N" => id });
    }
    let update = &mut revision.new_document;
    let field_id = update.try_add_object(widget).map_err(pdf_error)?;
    annotations.push(field_id.into());
    page.set("Annots", annotations);
    update.set_object(page_id, page);
    Ok(field_id)
}

fn visible_appearance(
    update: &mut Document,
    p: &fields::Placement,
    visible: &Visible<'_>,
    now: OffsetDateTime,
) -> Result<ObjectId, String> {
    let [left, bottom, right, top] = p.rect;
    let (w, h) = (right - left, top - bottom);
    let (width, height) = if p.rotation % 180 == 0 {
        (w, h)
    } else {
        (h, w)
    };
    let id = appearance::make(update, width, height, visible.name, now, visible.artwork)?;
    // The viewer rotates the whole page clockwise. Rotate local appearance
    // coordinates back to default user space so the stamp stays upright.
    let matrix = match p.rotation {
        90 => [0., 1., -1., 0., w, 0.],
        180 => [-1., 0., 0., -1., w, h],
        270 => [0., -1., 1., 0., 0., h],
        _ => return Ok(id),
    };
    update
        .get_object_mut(id)
        .and_then(Object::as_stream_mut)
        .map_err(pdf_error)?
        .dict
        .set("Matrix", matrix.map(Object::Real).to_vec());
    Ok(id)
}

pub(crate) fn array(doc: &Document, dict: &Dictionary, key: &[u8]) -> Result<Vec<Object>, String> {
    match dict.get(key).ok() {
        None | Some(Object::Null) => Ok(Vec::new()),
        Some(value) => doc
            .dereference(value)
            .and_then(|(_, o)| o.as_array())
            .cloned()
            .map_err(pdf_error),
    }
}

fn contains_signature(doc: &Document, object: &Object) -> bool {
    match object {
        Object::Array(items) => items.iter().any(|o| contains_signature(doc, o)),
        Object::Dictionary(dict) => {
            dict.has_type(b"Sig")
                || dict.has_type(b"DocTimeStamp")
                || (dict.has(b"ByteRange") && dict.has(b"Contents"))
                || (dict.get_deref(b"FT", doc).and_then(Object::as_name).ok() == Some(b"Sig")
                    && dict.has(b"V")
                    && !dict.get_deref(b"V", doc).is_ok_and(Object::is_null))
                || dict.iter().any(|(_, child)| contains_signature(doc, child))
        }
        // References are visited when scanning the loaded indirect objects.
        _ => false,
    }
}

fn next_field_name(doc: &Document, fields: &[Object]) -> Result<String, String> {
    let mut names = HashSet::new();
    let mut seen = HashSet::new();
    let mut pending: Vec<_> = fields.iter().collect();
    while let Some(field) = pending.pop() {
        let (id, object) = doc.dereference(field).map_err(pdf_error)?;
        if id.is_some_and(|id| !seen.insert(id)) {
            continue;
        }
        let dict = object.as_dict().map_err(pdf_error)?;
        if dict.has(b"T") {
            let name = dict
                .get_deref(b"T", doc)
                .and_then(decode_text_string)
                .map_err(pdf_error)?;
            if !name.is_empty() {
                names.insert(name);
                continue;
            }
        }
        // Descendants of an unnamed branch are also in the root namespace.
        if dict.has(b"Kids") {
            let kids = dict.get_deref(b"Kids", doc).map_err(pdf_error)?;
            if !kids.is_null() {
                pending.extend(kids.as_array().map_err(pdf_error)?);
            }
        }
    }
    for number in 1..=names.len() + 1 {
        let name = format!("Signature{number}");
        if !names.contains(&name) {
            return Ok(name);
        }
    }
    unreachable!("there are more candidate names than existing fields")
}

#[cfg(test)]
#[path = "pdfseal_tests.rs"]
mod tests;
