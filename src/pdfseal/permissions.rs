//! Permission checks for an incremental approval signature. Each signed revision is
//! read as it stood at signing time. A later replacement of a lock dictionary or a
//! form field cannot grant permissions that a previous signer withheld.
use super::{acroform, array, contains_signature, fields, pdf_error};
use lopdf::{
    Dictionary, Document, Object, ObjectId, content::Content, decode_text_string, xref::XrefEntry,
};
use memchr::memmem;
use std::collections::{HashMap, HashSet};

const INVALID: &str = "既存の署名の対象範囲または署名制限を確認できないため、追署できません。";
const LOCKED: &str = "既存の署名による制限のため、署名できる欄がありません。";

type Fields = HashMap<String, ObjectId>;

#[derive(Default)]
pub(super) struct Permissions {
    signed_fields: HashMap<ObjectId, (ObjectId, u32)>, // signature -> owning field and original offset
    eligible: Option<Fields>,
}

impl Permissions {
    pub(super) fn read(doc: &Document, input: &[u8]) -> Result<Self, String> {
        let mut result = Self::default();
        // The current xref can omit an earlier signature entirely. Read previous
        // revision footers even if the latest form appears to be unsigned.
        let mut revisions = HashSet::new();
        for pos in memmem::find_iter(input, b"%%EOF") {
            let Some(xref) = footer_xref(&input[..pos]) else {
                continue;
            };
            // Linearized files can contain a preliminary startxref of zero.
            if xref == 0 {
                continue;
            }
            let end = pos + 5;
            let next = end + input[end..].iter().take_while(|&&b| pdf_space(b)).count();
            if next == input.len() {
                revisions.insert(xref);
                // A linearized base revision has two xref sections. lopdf
                // exposes the chain length but strips /Prev while merging it.
                if xref != doc.xref_start
                    || doc.xref_section_count
                        > revisions.len() + usize::from(doc.linearization.is_some())
                {
                    return Err(INVALID.into());
                }
                result.read_revision(doc, &input[..next], end)?;
                return Ok(result);
            }
            // An uncompressed embedded PDF has its own startxref/%%EOF. Its
            // offsets are relative to the attachment, not to the outer file.
            if xref >= pos || revisions.contains(&xref) || !at_xref(&input[xref..pos]) {
                continue;
            }
            if revisions.len() >= 128 {
                return Err(INVALID.into());
            }
            let previous = Document::load_mem(&input[..next]).map_err(|_| INVALID)?;
            if previous.xref_recovery.is_some() || previous.xref_start != xref {
                return Err(INVALID.into());
            }
            revisions.insert(xref);
            result.read_revision(&previous, &input[..next], end)?;
        }
        Err(INVALID.into())
    }

    pub(super) fn signed(&self) -> bool {
        !self.signed_fields.is_empty()
    }

    pub(super) fn allows_new_field(&self) -> bool {
        self.eligible.is_none()
    }

    pub(super) fn allows_field(&self, qualified_name: &str) -> bool {
        self.eligible
            .as_ref()
            .is_none_or(|fields| fields.contains_key(qualified_name))
    }

    fn read_revision(&mut self, doc: &Document, bytes: &[u8], eof: usize) -> Result<(), String> {
        let catalog = doc.catalog().map_err(pdf_error)?;
        let certification = match catalog.get(b"Perms").ok() {
            None => None,
            Some(value) => {
                let perms = doc
                    .dereference(value)
                    .and_then(|(_, o)| o.as_dict())
                    .map_err(|_| INVALID)?;
                if perms.len() != 1 {
                    return Err(INVALID.into());
                }
                Some(
                    perms
                        .get(b"DocMDP")
                        .and_then(Object::as_reference)
                        .map_err(|_| INVALID)?,
                )
            }
        };
        let marked: HashSet<_> = doc
            .objects
            .iter()
            .filter(|(_, o)| contains_signature(doc, o))
            .map(|(&id, _)| id)
            .collect();
        // A child can inherit /FT /Sig, including when its own /V is dangling.
        // Inspect such form trees even if no signature dictionary was recovered.
        if marked.is_empty()
            && !self.signed()
            && certification.is_none()
            && !doc.objects.values().any(|o| {
                o.as_dict().is_ok_and(|d| {
                    d.get_deref(b"FT", doc).and_then(Object::as_name).ok() == Some(b"Sig")
                })
            })
        {
            return Ok(());
        }

        let (_, form) = acroform(doc)?;
        let mut inventory = Signatures::default();
        for root in array(doc, &form, b"Fields")? {
            inventory.visit(doc, &root, false, &mut Vec::new(), 0)?;
        }
        if !marked
            .iter()
            .all(|id| inventory.owners.contains_key(id) || inventory.field_ids.contains(id))
            || self
                .signed_fields
                .keys()
                .any(|id| !inventory.owners.contains_key(id))
            || certification.is_some_and(|id| !inventory.owners.contains_key(&id))
        {
            return Err(INVALID.into());
        }

        let mut rules = inventory.locks;
        let mut restricted = false;
        for (&signature, &owner) in &inventory.owners {
            let dict = doc.get_dictionary(signature).map_err(|_| INVALID)?;
            let kind = dict
                .get(b"Type")
                .ok()
                .map(Object::as_name)
                .transpose()
                .map_err(|_| INVALID)?;
            let subfilter = dict
                .get(b"SubFilter")
                .ok()
                .map(Object::as_name)
                .transpose()
                .map_err(|_| INVALID)?;
            let timestamp = kind == Some(b"DocTimeStamp");
            if !matches!(kind, None | Some(b"Sig" | b"DocTimeStamp"))
                || timestamp != (subfilter == Some(b"ETSI.RFC3161"))
                || timestamp && (dict.has(b"Reference") || certification == Some(signature))
            {
                return Err(INVALID.into());
            }

            let Some(XrefEntry::Normal { offset, generation }) =
                doc.reference_table.get(signature.0)
            else {
                return Err(INVALID.into());
            };
            if *generation != signature.1 {
                return Err(INVALID.into());
            }
            match self.signed_fields.get(&signature) {
                Some(previous) if *previous == (owner, *offset) => {}
                Some(_) => return Err(INVALID.into()),
                None => {
                    check_coverage(dict, bytes, *offset as usize, eof)?;
                    self.signed_fields.insert(signature, (owner, *offset));
                }
            }

            let mut doc_mdp = false;
            if dict.has(b"Reference") {
                let refs = dict
                    .get_deref(b"Reference", doc)
                    .and_then(Object::as_array)
                    .map_err(|_| INVALID)?;
                if refs.is_empty() {
                    return Err(INVALID.into());
                }
                for item in refs {
                    let reference = doc
                        .dereference(item)
                        .and_then(|(_, o)| o.as_dict())
                        .map_err(|_| INVALID)?;
                    if let Ok(data) = reference.get(b"Data")
                        && data.as_reference().map_err(|_| INVALID)?
                            != doc
                                .trailer
                                .get(b"Root")
                                .and_then(Object::as_reference)
                                .map_err(|_| INVALID)?
                    {
                        return Err(INVALID.into());
                    }
                    let params = reference
                        .get_deref(b"TransformParams", doc)
                        .and_then(Object::as_dict)
                        .map_err(|_| INVALID)?;
                    match reference
                        .get_deref(b"TransformMethod", doc)
                        .and_then(Object::as_name)
                        .map_err(|_| INVALID)?
                    {
                        b"DocMDP" => {
                            if doc_mdp {
                                return Err(INVALID.into());
                            }
                            doc_mdp = true;
                            restricted = true;
                            check_level(doc, params)?;
                        }
                        b"FieldMDP" if reference.has(b"Data") => {
                            rules.push(Rule::read(doc, params)?)
                        }
                        _ => return Err(INVALID.into()),
                    }
                }
            }
            if doc_mdp != (certification == Some(signature)) {
                return Err(INVALID.into());
            }
        }
        if restricted || rules.iter().any(Rule::restricts) || self.eligible.is_some() {
            let mut candidates = fields::eligible_fields(doc)?;
            candidates.retain(|name, id| {
                rules.iter().all(|rule| rule.allows(name))
                    && self
                        .eligible
                        .as_ref()
                        .is_none_or(|earlier| earlier.get(name) == Some(id))
            });
            if candidates.is_empty() {
                return Err(LOCKED.into());
            }
            self.eligible = Some(candidates);
        }
        Ok(())
    }
}

fn pdf_space(b: u8) -> bool {
    matches!(b, 0 | 9 | 10 | 12 | 13 | 32)
}

fn footer_xref(prefix: &[u8]) -> Option<usize> {
    let tail = &prefix[prefix.len().saturating_sub(40)..];
    let keyword = memmem::rfind(tail, b"startxref")?;
    let number = std::str::from_utf8(&tail[keyword + 9..]).ok()?.trim_ascii();
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    number.parse().ok()
}

fn at_xref(data: &[u8]) -> bool {
    if data
        .strip_prefix(b"xref")
        .and_then(|s| s.first())
        .is_some_and(|&b| pdf_space(b))
    {
        return true;
    }
    // Xref stream: an indirect-object header followed by a /Type /XRef dictionary.
    let Some(header) = memmem::find(&data[..data.len().min(64)], b"obj") else {
        return false;
    };
    let mut numbers = data[..header]
        .split(|&b| pdf_space(b))
        .filter(|s| !s.is_empty());
    if !(0..2).all(|_| {
        numbers
            .next()
            .is_some_and(|s| s.iter().all(u8::is_ascii_digit))
    }) || numbers.next().is_some()
    {
        return false;
    }
    Content::decode_iter(&data[header + 3..])
        .next()
        .and_then(Result::ok)
        .is_some_and(|op| {
            op.operator == "stream"
                && matches!(op.operands.as_slice(), [Object::Dictionary(d)] if d.has_type(b"XRef"))
        })
}

// Confirm that the only gap is the *top-level Contents of this indirect signature*.
// Parsing the same dictionary with that token replaced by null avoids guessing which
// /Contents key a byte search found in nested dictionaries or literal strings.
fn check_coverage(
    dict: &Dictionary,
    bytes: &[u8],
    offset: usize,
    eof: usize,
) -> Result<(), String> {
    let range = dict
        .get(b"ByteRange")
        .and_then(Object::as_array)
        .map_err(|_| INVALID)?;
    let [a, b, c, d] = range.as_slice() else {
        return Err(INVALID.into());
    };
    let integer = |o: &Object| {
        o.as_i64()
            .ok()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or(INVALID)
    };
    let (start, first, second, len) = (integer(a)?, integer(b)?, integer(c)?, integer(d)?);
    let covered = second.checked_add(len).ok_or(INVALID)?;
    if start != 0 || !(eof..=bytes.len()).contains(&covered) || offset >= first || first >= second {
        return Err(INVALID.into());
    }
    let gap = bytes.get(first..second).ok_or(INVALID)?;
    let hex = gap
        .strip_prefix(b"<")
        .and_then(|s| s.strip_suffix(b">"))
        .ok_or(INVALID)?;
    if !hex.iter().all(|&b| b.is_ascii_hexdigit() || pdf_space(b))
        || !dict
            .get(b"Contents")
            .and_then(Object::as_str)
            .is_ok_and(|v| v.iter().any(|&b| b != 0))
    {
        return Err(INVALID.into());
    }

    let before = &bytes[offset..first];
    let begin = memmem::find(before, b"<<").ok_or(INVALID)?;
    let after = &bytes[second..covered];
    let end = memmem::find(after, b"endobj").ok_or(INVALID)? + 6;
    let mut patched = before[begin..].to_vec();
    patched.extend_from_slice(b" null ");
    patched.extend_from_slice(&after[..end]);
    let op = Content::decode_iter(&patched)
        .next()
        .ok_or(INVALID)?
        .map_err(|_| INVALID)?;
    let operands: [Object; 1] = op.operands.try_into().map_err(|_| INVALID)?;
    let [Object::Dictionary(mut parsed)] = operands else {
        return Err(INVALID.into());
    };
    if op.operator != "endobj" || parsed.remove(b"Contents") != Some(Object::Null) {
        return Err(INVALID.into());
    }
    let mut expected = dict.clone();
    expected.remove(b"Contents");
    if parsed != expected {
        return Err(INVALID.into());
    }
    Ok(())
}

#[derive(Default)]
struct Signatures {
    owners: HashMap<ObjectId, ObjectId>,
    field_ids: HashSet<ObjectId>,
    seen: HashSet<ObjectId>,
    locks: Vec<Rule>,
}

impl Signatures {
    fn visit<'a>(
        &mut self,
        doc: &'a Document,
        field: &'a Object,
        inherited_sig: bool,
        ancestor_locks: &mut Vec<&'a Dictionary>,
        depth: usize,
    ) -> Result<(), String> {
        let id = field.as_reference().map_err(|_| INVALID)?;
        if depth > 64 || !self.seen.insert(id) {
            return Err(INVALID.into());
        }
        let dict = doc.get_dictionary(id).map_err(|_| INVALID)?;
        let signature_field = match dict.get(b"FT") {
            Ok(_) => {
                dict.get_deref(b"FT", doc)
                    .and_then(Object::as_name)
                    .map_err(|_| INVALID)?
                    == b"Sig"
            }
            Err(_) => inherited_sig,
        };
        let lock_count = ancestor_locks.len();
        if let Ok(value) = dict.get(b"Lock") {
            let lock = doc.dereference(value).map_err(|_| INVALID)?.1;
            if !lock.is_null() {
                ancestor_locks.push(lock.as_dict().map_err(|_| INVALID)?);
            }
        }
        if signature_field && let Ok(value) = dict.get(b"V") {
            let (signature, value) = doc.dereference(value).map_err(|_| INVALID)?;
            if !value.is_null() {
                let signature = signature.ok_or(INVALID)?;
                if self.owners.insert(signature, id).is_some() {
                    return Err(INVALID.into());
                }
                self.field_ids.insert(id);
                for lock in ancestor_locks.iter() {
                    self.locks.push(Rule::read(doc, lock)?);
                }
            }
        }
        if let Ok(kids) = dict.get(b"Kids") {
            let (_, kids) = doc.dereference(kids).map_err(|_| INVALID)?;
            if !kids.is_null() {
                for child in kids.as_array().map_err(|_| INVALID)? {
                    self.visit(doc, child, signature_field, ancestor_locks, depth + 1)?;
                }
            }
        }
        ancestor_locks.truncate(lock_count);
        Ok(())
    }
}

fn check_level(doc: &Document, dict: &Dictionary) -> Result<(), String> {
    if dict.has(b"P") {
        match dict
            .get_deref(b"P", doc)
            .and_then(Object::as_i64)
            .map_err(|_| INVALID)?
        {
            1 => return Err(LOCKED.into()),
            2 | 3 => {}
            _ => return Err(INVALID.into()),
        }
    }
    Ok(())
}

struct Rule {
    exclude: bool,
    has_level: bool,
    names: Vec<String>,
}

impl Rule {
    fn read(doc: &Document, dict: &Dictionary) -> Result<Self, String> {
        check_level(doc, dict)?;
        let exclude = match dict
            .get_deref(b"Action", doc)
            .and_then(Object::as_name)
            .map_err(|_| INVALID)?
        {
            b"All" => return Err(LOCKED.into()),
            b"Include" => false,
            b"Exclude" => true,
            _ => return Err(INVALID.into()),
        };
        let fields = dict
            .get_deref(b"Fields", doc)
            .and_then(Object::as_array)
            .map_err(|_| INVALID)?;
        let mut names = Vec::new();
        for field in fields {
            let name = doc
                .dereference(field)
                .and_then(|(_, o)| decode_text_string(o))
                .map_err(|_| INVALID)?;
            if name.is_empty() {
                return Err(INVALID.into());
            }
            names.push(name);
        }
        Ok(Self {
            exclude,
            has_level: dict.has(b"P"),
            names,
        })
    }

    fn restricts(&self) -> bool {
        self.has_level || self.exclude || !self.names.is_empty()
    }

    fn allows(&self, name: &str) -> bool {
        let listed = self
            .names
            .iter()
            .any(|n| name == n || name.strip_prefix(n).is_some_and(|s| s.starts_with('.')));
        self.exclude == listed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{IncrementalDocument, dictionary};
    use std::io::Write;

    // A byte-level fixture also exercises producer formatting different from
    // SumiPDF's serializer. Both the transform and the lock can reference obj 11.
    fn fixture(method: &str, params: &str, owner_lock: bool) -> Vec<u8> {
        let cert = if method == "DocMDP" {
            "/Perms << /DocMDP 9 0 R >>"
        } else {
            ""
        };
        let lock = if owner_lock { "/Lock 11 0 R" } else { "" };
        let reference = if method.is_empty() {
            ""
        } else {
            "/Reference [10 0 R]"
        };
        let objects = [
            format!("<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R {cert} >>"),
            "<< /Type /Pages /Count 1 /Kids [3 0 R] >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Annots [5 0 R 7 0 R 8 0 R] >>"
                .into(),
            "<< /Fields [5 0 R 6 0 R 8 0 R] /SigFlags 3 >>".into(),
            format!("<< /FT /Sig /T (First) /Subtype /Widget /Rect [0 0 10 10] /V 9 0 R {lock} >>"),
            "<< /FT /Sig /T (Team) /Kids [7 0 R] >>".into(),
            "<< /Subtype /Widget /Parent 6 0 R /T (QA) /Rect [20 20 40 40] >>".into(),
            "<< /FT /Sig /Subtype /Widget /T (TeamOther) /Rect [50 50 70 70] >>".into(),
            format!(
                "<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /ETSI.CAdES.detached /ByteRange [0 {}] /Contents <AA BB> /Other <CC DD> {reference} >>",
                " ".repeat(40)
            ),
            format!(
                "<< /Type /SigRef /Data 1 0 R /TransformMethod /{method} /TransformParams 11 0 R >>"
            ),
            format!("<< {params} >>"),
        ];
        let mut buf = b"%PDF-1.7\n".to_vec();
        let mut offsets = vec![];
        for (i, obj) in objects.iter().enumerate() {
            offsets.push(buf.len());
            writeln!(&mut buf, "{} 0 obj\n{obj}\nendobj", i + 1).unwrap();
        }
        let xref = buf.len();
        writeln!(&mut buf, "xref\n0 12\n0000000000 65535 f ").unwrap();
        for off in offsets {
            writeln!(&mut buf, "{off:010} 00000 n ").unwrap();
        }
        writeln!(
            &mut buf,
            "trailer\n<< /Size 12 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF"
        )
        .unwrap();
        set_range(&mut buf, b"<AA BB>");
        buf
    }

    fn set_range(bytes: &mut [u8], token: &[u8]) {
        let start = memmem::find(bytes, token).unwrap();
        let second = start + token.len();
        let text = format!("0 {start} {second} {}", bytes.len() - second);
        let marker = b"/ByteRange [";
        let range = memmem::find(bytes, marker).unwrap() + marker.len();
        let end = range + memmem::find(&bytes[range..], b"]").unwrap();
        bytes[range..end].fill(b' ');
        bytes[range..range + text.len()].copy_from_slice(text.as_bytes());
    }

    fn policy(bytes: &[u8]) -> Result<Permissions, String> {
        Permissions::read(&Document::load_mem(bytes).unwrap(), bytes)
    }

    fn update(bytes: Vec<u8>, id: ObjectId, change: impl FnOnce(&mut Dictionary)) -> Vec<u8> {
        let doc = Document::load_mem(&bytes).unwrap();
        let mut version = IncrementalDocument::create_from(bytes, doc);
        version.opt_clone_object_to_new_document(id).unwrap();
        change(version.new_document.get_dictionary_mut(id).unwrap());
        let mut result = Vec::new();
        version.save_to(&mut result).unwrap();
        result
    }

    #[test]
    fn approvals_and_certification_levels() {
        let normal = fixture("", "", false);
        let current = policy(&normal).unwrap();
        assert!(current.signed() && current.allows_new_field());
        let noop = fixture("FieldMDP", "/Action /Include /Fields []", false);
        assert!(policy(&noop).unwrap().allows_new_field());
        assert!(policy(&fixture("DocMDP", "/P 1", false)).is_err());
        for params in ["", "/P 2", "/P 3"] {
            let current = policy(&fixture("DocMDP", params, false)).unwrap();
            assert!(!current.allows_new_field());
            assert!(current.allows_field("Team.QA"));
            assert!(!current.allows_field("First"));
        }
        assert!(policy(&fixture("Unknown", "", false)).is_err());
    }

    #[test]
    fn inherited_field_names_and_locks_from_both_sources() {
        for (method, owner_lock) in [("FieldMDP", false), ("", true)] {
            for (action, allow_team) in [("Include", false), ("Exclude", true)] {
                let params = format!("/Action /{action} /Fields [(Team)]");
                let bytes = fixture(method, &params, owner_lock);
                let current = policy(&bytes).unwrap();
                assert!(!current.allows_new_field());
                assert_eq!(current.allows_field("Team.QA"), allow_team);
                assert_eq!(current.allows_field("TeamOther"), !allow_team);
                let relaxed = update(bytes, (11, 0), |dict| {
                    *dict = dictionary! { "Action" => "Include", "Fields" => Vec::<Object>::new() }
                });
                let current = policy(&relaxed).unwrap();
                assert!(!current.allows_new_field());
                assert_eq!(current.allows_field("Team.QA"), allow_team);
                assert_eq!(current.allows_field("TeamOther"), !allow_team);
            }
            for params in [
                "/Action /All",
                "/P 1 /Action /Include /Fields []",
                "/Action /Exclude /Fields []",
                "/Action /Include /Fields 99 0 R",
            ] {
                assert!(policy(&fixture(method, params, owner_lock)).is_err());
            }
        }
    }

    #[test]
    fn public_signing_entry_points_enforce_field_permissions_before_invoking_provider() {
        use crate::pdfseal::{Target, inspect, sign_pdf, sign_visible};
        let bytes = fixture("FieldMDP", "/Action /Include /Fields [(Team)]", false);
        let listing = inspect(&bytes).unwrap();
        let rows = listing["fields"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["name"], "TeamOther");
        for target in [
            Target::Area {
                page: 1,
                rect: [0.1, 0.1, 0.3, 0.3],
            },
            Target::Field("field-7-0".into()),
        ] {
            assert!(
                sign_visible(
                    bytes.clone(),
                    &target,
                    "Reviewer",
                    Default::default(),
                    |_, _| panic!("forbidden target reached signer")
                )
                .is_err()
            );
        }
        assert!(
            sign_pdf(bytes.clone(), |_, _| panic!(
                "forbidden new field reached signer"
            ))
            .is_err()
        );
        let target = Target::Field(rows[0]["id"].as_str().unwrap().into());
        let signed = sign_visible(
            bytes.clone(),
            &target,
            "Reviewer",
            Default::default(),
            |_, _| Ok(vec![1]),
        )
        .unwrap();
        assert!(signed.starts_with(&bytes));
    }

    #[test]
    fn changed_field_identity_and_removed_or_rewritten_signatures() {
        let original = fixture("DocMDP", "/P 2", false);
        let renamed = update(original, (7, 0), |dict| {
            dict.set("T", Object::string_literal("Renamed"))
        });
        let current = policy(&renamed).unwrap();
        assert!(!current.allows_field("Team.Renamed") && current.allows_field("TeamOther"));

        let approval = fixture("", "", false);
        let removed = update(approval.clone(), (5, 0), |dict| {
            dict.remove(b"V");
        });
        assert!(policy(&removed).is_err());
        let rewritten = update(approval, (9, 0), |dict| {
            dict.remove(b"Other");
        });
        assert!(policy(&rewritten).is_err());
    }

    #[test]
    fn inherited_signature_field_with_a_dangling_value_is_rejected() {
        let mut doc = Document::load_mem(&fixture("", "", false)).unwrap();
        doc.objects.remove(&(9, 0));
        doc.get_dictionary_mut((5, 0)).unwrap().remove(b"V");
        doc.get_dictionary_mut((7, 0))
            .unwrap()
            .set("V", Object::Reference((999, 0)));
        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();
        assert!(policy(&buf).is_err());
    }

    #[test]
    fn byte_range_must_exclude_only_its_own_contents_and_cover_the_revision() {
        let mut bytes = fixture("", "", false);
        set_range(&mut bytes, b"<CC DD>");
        assert!(policy(&bytes).is_err());
        set_range(&mut bytes, b"<AA BB>");
        let new_end = bytes.len() - 20;
        set_range(&mut bytes[..new_end], b"<AA BB>");
        assert!(policy(&bytes).is_err());
    }

    #[test]
    fn document_timestamp_allows_another_approval() {
        let mut bytes = fixture("", "", false);
        let old = b"/Type /Sig /Filter /Adobe.PPKLite /SubFilter /ETSI.CAdES.detached";
        let pos = memmem::find(&bytes, old).unwrap();
        let new = b"/Type/DocTimeStamp/Filter/Adobe.PPKLite/SubFilter/ETSI.RFC3161";
        bytes[pos..pos + old.len()].fill(b' ');
        bytes[pos..pos + new.len()].copy_from_slice(new);
        let current = policy(&bytes).unwrap();
        assert!(current.signed() && current.allows_new_field());
        let bad = b"/SubFilter/ETSI.invalid";
        let p = memmem::find(&bytes, b"/SubFilter/ETSI.RFC3161").unwrap();
        bytes[p..p + bad.len()].copy_from_slice(bad);
        assert!(policy(&bytes).is_err());
    }

    #[test]
    fn ignores_a_certified_pdf_embedded_in_an_uncompressed_stream() {
        let mut doc = Document::with_version("1.7");
        let pages = doc.new_object_id();
        let page = doc.add_object(dictionary! { "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(),0.into(),100.into(),100.into()] });
        doc.set_object(
            pages,
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        );
        let embedded = doc.add_object(lopdf::Stream::new(
            dictionary! { "Type" => "EmbeddedFile" },
            fixture("DocMDP", "/P 1", false),
        ));
        let root = doc.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => pages, "AF" => vec![embedded.into()] },
        );
        doc.trailer.set("Root", root);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        assert_eq!(memmem::find_iter(&bytes, b"%%EOF").count(), 2);
        assert!(policy(&bytes).unwrap().allows_new_field());
        let later = update(bytes, root, |catalog| catalog.set("Version", "1.7"));
        assert!(policy(&later).unwrap().allows_new_field());
    }
}
