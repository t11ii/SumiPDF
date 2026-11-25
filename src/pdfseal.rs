//! Create an invisible PDF approval signature in an incremental revision.
//! The callback signs exactly the two slices declared by /ByteRange.
use lopdf::{Dictionary, Document, IncrementalDocument, Object, StringFormat, Writer};
use lopdf::{decode_text_string, dictionary};
use memchr::memmem;
use std::collections::HashSet;
use time::OffsetDateTime;

const CMS_CAPACITY: usize = 32 * 1024;
const ENCRYPTED: &str = "暗号化されたPDFは編集できません。";
const ALREADY_SIGNED: &str = "このPDFには既存の署名または証明による制限があります。";
const PLACEHOLDER_ERROR: &str = "PDFの署名領域を特定できませんでした。";
const TOO_LARGE: &str = "署名済みPDFが4 GiBに達する文書には対応していません。";

pub(crate) fn pdf_error(error: impl std::fmt::Display) -> String {
    format!("PDFの構造を読み書きできませんでした。\n{error}")
}

pub fn sign_pdf(
    input: Vec<u8>,
    signer: impl FnOnce(&[u8], &[u8]) -> Result<Vec<u8>, String>,
) -> Result<Vec<u8>, String> {
    let original_len = input.len();
    // lopdf serializes cross-reference offsets into u32 fields.
    if original_len.saturating_add(CMS_CAPACITY * 2) > u32::MAX as usize {
        return Err(TOO_LARGE.into());
    }
    let (mut output, signature_body) = prepare_revision(input)?;
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

/// Load the source for an incremental edit, preserving signature and encryption restrictions.
pub(crate) fn editable_document(input: &[u8]) -> Result<Document, String> {
    let doc = Document::load_mem(input).map_err(|e| match e {
        lopdf::Error::InvalidPassword | lopdf::Error::UnsupportedSecurityHandler(_) => {
            ENCRYPTED.into()
        }
        _ => pdf_error(e),
    })?;
    if doc.was_encrypted() || doc.trailer.has(b"Encrypt") {
        return Err(ENCRYPTED.into());
    }
    if !input.starts_with(b"%PDF-") || doc.xref_recovery.is_some() || doc.xref_start == 0 {
        return Err("PDFの相互参照が不完全なため、元の内容を保って編集できません。".into());
    }
    if doc.catalog().map_err(pdf_error)?.has(b"Perms")
        || doc.objects.values().any(contains_signature)
    {
        return Err(ALREADY_SIGNED.into());
    }
    Ok(doc)
}

fn prepare_revision(input: Vec<u8>) -> Result<(Vec<u8>, Vec<u8>), String> {
    let doc = editable_document(&input)?;
    let root_id = doc
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .map_err(pdf_error)?;
    let mut root = doc.get_dictionary(root_id).map_err(pdf_error)?.clone();
    let page_id = doc.page_iter().next().ok_or("PDFにページがありません。")?;
    let mut page = doc.get_dictionary(page_id).map_err(pdf_error)?.clone();
    let mut annotations = array(&doc, &page, b"Annots")?;
    let (existing_form_id, mut form) = match root.get(b"AcroForm").ok() {
        None | Some(Object::Null) => (None, Dictionary::new()),
        Some(object) => {
            let (id, form) = doc.dereference(object).map_err(pdf_error)?;
            (id, form.as_dict().map_err(pdf_error)?.clone())
        }
    };
    if form.has(b"XFA") {
        return Err("XFA形式のフォームへの署名には対応していません。".into());
    }
    let mut fields = array(&doc, &form, b"Fields")?;
    let field_name = next_field_name(&doc, &fields)?;
    let flags = form
        .get(b"SigFlags")
        .ok()
        .map(|o| doc.dereference(o).and_then(|(_, o)| o.as_i64()))
        .transpose()
        .map_err(pdf_error)?
        .unwrap_or(0);

    let original_len = input.len();
    let mut revision = IncrementalDocument::create_from(input, doc);
    let update = &mut revision.new_document;
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
    let signature_id = update.try_add_object(signature).map_err(pdf_error)?;
    let field_id = update
        .try_add_object(dictionary! {
            "Type" => "Annot", "Subtype" => "Widget", "FT" => "Sig",
            "Rect" => vec![Object::Integer(0); 4], "F" => 132,
            "T" => Object::string_literal(field_name), "V" => signature_id, "P" => page_id,
        })
        .map_err(pdf_error)?;
    annotations.push(field_id.into());
    fields.push(field_id.into());
    page.set("Annots", annotations);
    form.set("Fields", fields);
    form.set("SigFlags", flags | 3);
    update.set_object(page_id, page);
    if let Some(id) = existing_form_id {
        update.set_object(id, form);
    } else {
        let id = update.try_add_object(form).map_err(pdf_error)?;
        root.set("AcroForm", id);
        update.set_object(root_id, root);
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

fn contains_signature(object: &Object) -> bool {
    match object {
        Object::Array(items) => items.iter().any(contains_signature),
        Object::Dictionary(dict) => {
            dict.has_type(b"Sig")
                || dict.has_type(b"DocTimeStamp")
                || (dict.has(b"ByteRange") && dict.has(b"Contents"))
                || (dict.get(b"FT").and_then(Object::as_name).ok() == Some(b"Sig")
                    && dict.get(b"V").is_ok_and(|v| !v.is_null()))
                || dict.iter().any(|(_, child)| contains_signature(child))
        }
        // References are visited when scanning the loaded indirect objects.
        _ => false,
    }
}

fn next_field_name(doc: &Document, fields: &[Object]) -> Result<String, String> {
    let mut names = HashSet::new();
    for field in fields {
        let dict = doc
            .dereference(field)
            .and_then(|(_, o)| o.as_dict())
            .map_err(pdf_error)?;
        if dict.has(b"T") {
            let name = dict
                .get_deref(b"T", doc)
                .and_then(decode_text_string)
                .map_err(pdf_error)?;
            names.insert(name);
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
