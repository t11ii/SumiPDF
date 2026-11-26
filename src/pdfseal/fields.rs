use super::{Permissions, Target, array, pdf_error};
use crate::highlight::PageGeometry;
use lopdf::{Dictionary, Document, Object, ObjectId, decode_text_string};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};

const INVALID_TARGET: &str = "指定された署名欄が見つからないか、この欄には署名できません。";
pub(super) const EXISTING_ONLY: &str =
    "この PDF には署名位置の制限があります。記入が許可された既存の空の署名欄を選んでください。";
const INVALID_RECT: &str = "署名の範囲はページ内にある正の面積の長方形を指定してください。";
const INVALID_TREE: &str = "PDFのフォーム階層に循環、重複、または深すぎる階層があります。";

pub(super) struct Placement {
    pub page: u32,
    pub page_id: ObjectId,
    pub unit: [f32; 4],
    pub rect: [f32; 4],
    pub rotation: i64,
    pub existing: Option<(ObjectId, ObjectId)>, // terminal field and its widget
}

fn ordered((x, y): (f32, f32), (u, v): (f32, f32)) -> [f32; 4] {
    [x.min(u), y.min(v), x.max(u), y.max(v)]
}

fn valid_unit([x, y, u, v]: [f32; 4]) -> bool {
    [x, y, u, v].iter().all(|v| (0.0..=1.0).contains(v)) && x < u && y < v
}

struct Candidate {
    name: String,
    tooltip: Option<String>,
    id: String,
    placement: Placement,
}

type PageIndex = HashMap<ObjectId, Option<(u32, ObjectId)>>;

struct Inventory<'a> {
    doc: &'a Document,
    widgets: PageIndex,
    seen: HashSet<ObjectId>,
    names: HashMap<String, usize>,
    candidates: Vec<Candidate>,
}

#[derive(Default)]
struct Attributes {
    name: String,
    signature: bool,
    read_only: bool,
    occupied: bool,
    constrained: bool,
}

fn value<'a>(
    doc: &'a Document,
    dict: &'a Dictionary,
    key: &[u8],
) -> Result<Option<&'a Object>, String> {
    dict.get(key)
        .ok()
        .map(|o| doc.dereference(o).map(|(_, v)| v).map_err(pdf_error))
        .transpose()
        .map(|v| v.filter(|o| !o.is_null()))
}

impl Attributes {
    fn child(&self, doc: &Document, dict: &Dictionary) -> Result<Self, String> {
        let partial = value(doc, dict, b"T")?
            .map(decode_text_string)
            .transpose()
            .map_err(pdf_error)?;
        let mut name = self.name.clone();
        if let Some(part) = partial.filter(|s| !s.is_empty()) {
            if !name.is_empty() {
                name.push('.');
            }
            name.push_str(&part);
        }
        Ok(Self {
            name,
            signature: value(doc, dict, b"FT")?
                .map(Object::as_name)
                .transpose()
                .map_err(pdf_error)?
                .map_or(self.signature, |ft| ft == b"Sig"),
            read_only: self.read_only
                || value(doc, dict, b"Ff")?
                    .map(Object::as_i64)
                    .transpose()
                    .map_err(pdf_error)?
                    .is_some_and(|flags| flags & 1 != 0),
            occupied: self.occupied || value(doc, dict, b"V")?.is_some(),
            constrained: self.constrained
                || value(doc, dict, b"Lock")?.is_some()
                || value(doc, dict, b"SV")?.is_some(),
        })
    }
}

fn is_widget(doc: &Document, dict: &Dictionary) -> Result<bool, String> {
    Ok(value(doc, dict, b"Subtype")?
        .map(Object::as_name)
        .transpose()
        .map_err(pdf_error)?
        == Some(b"Widget"))
}

impl<'a> Inventory<'a> {
    fn load(
        doc: &'a Document,
        pages: &BTreeMap<u32, ObjectId>,
        roots: &[Object],
    ) -> Result<Vec<Candidate>, String> {
        if roots.is_empty() {
            return Ok(Vec::new());
        }
        let mut widgets = PageIndex::new();
        for (&page, &id) in pages {
            for annot in array(doc, doc.get_dictionary(id).map_err(pdf_error)?, b"Annots")? {
                if let Ok(widget) = annot.as_reference() {
                    widgets
                        .entry(widget)
                        .and_modify(|v| *v = None)
                        .or_insert(Some((page, id)));
                }
            }
        }
        let mut inventory = Self {
            doc,
            widgets,
            seen: HashSet::new(),
            names: HashMap::new(),
            candidates: Vec::new(),
        };
        for root in roots {
            if let Ok(id) = root.as_reference() {
                inventory.visit(id, None, &Attributes::default(), 0)?;
            }
        }
        // A fully qualified terminal name identifies one logical field. A malformed
        // tree with duplicate names could make a reader update a different widget.
        inventory
            .candidates
            .retain(|c| inventory.names.get(&c.name) == Some(&1));
        Ok(inventory.candidates)
    }

    fn visit(
        &mut self,
        id: ObjectId,
        parent: Option<ObjectId>,
        inherited: &Attributes,
        depth: u32,
    ) -> Result<(), String> {
        if depth > 64 || !self.seen.insert(id) {
            return Err(INVALID_TREE.into());
        }
        let dict = self.doc.get_dictionary(id).map_err(pdf_error)?;
        if !parent_matches(dict, parent) {
            return Ok(());
        }
        let attrs = inherited.child(self.doc, dict)?;
        let kids = array(self.doc, dict, b"Kids")?;
        if kids.is_empty() {
            *self.names.entry(attrs.name.clone()).or_default() += 1;
            if is_widget(self.doc, dict)? {
                self.add_widget(id, id, &attrs)?;
            }
            return Ok(());
        }
        if is_widget(self.doc, dict)? {
            return Ok(());
        }
        let mut widgets = Vec::new();
        let mut branches = Vec::new();
        for kid in kids {
            // A widget must have an indirect identity to appear in page Annots.
            let Ok(kid_id) = kid.as_reference() else {
                return Ok(());
            };
            let child = self.doc.get_dictionary(kid_id).map_err(pdf_error)?;
            if is_widget(self.doc, child)?
                && !child.has(b"T")
                && !child.has(b"FT")
                && !child.has(b"Kids")
            {
                widgets.push(kid_id);
            } else {
                branches.push(kid_id);
            }
        }
        if widgets.is_empty() {
            for child in branches {
                self.visit(child, Some(id), &attrs, depth + 1)?;
            }
        } else if branches.is_empty() {
            *self.names.entry(attrs.name.clone()).or_default() += 1;
            if let [widget] = widgets[..] {
                if !self.seen.insert(widget) {
                    return Err(INVALID_TREE.into());
                }
                let child = self.doc.get_dictionary(widget).map_err(pdf_error)?;
                if parent_matches(child, Some(id)) {
                    self.add_widget(id, widget, &attrs.child(self.doc, child)?)?;
                }
            }
        }
        Ok(())
    }

    fn add_widget(
        &mut self,
        field: ObjectId,
        widget: ObjectId,
        attrs: &Attributes,
    ) -> Result<(), String> {
        if !attrs.signature || attrs.constrained || attrs.occupied || attrs.read_only {
            return Ok(());
        }
        let Some(&Some((page, page_id))) = self.widgets.get(&widget) else {
            return Ok(());
        };
        let dict = self.doc.get_dictionary(widget).map_err(pdf_error)?;
        if dict.has(b"P") && dict.get(b"P").and_then(Object::as_reference).ok() != Some(page_id) {
            return Ok(());
        }
        let flags = value(self.doc, dict, b"F")?
            .map(Object::as_i64)
            .transpose()
            .map_err(pdf_error)?
            .unwrap_or(0);
        // Locked (128) protects the widget's geometry and flags, while still
        // permitting a form value. Filling it updates only its value and appearance.
        if flags & (1 | 2 | 8 | 16 | 32 | 64 | 256 | 512) != 0 || dict.has(b"OC") {
            return Ok(());
        }
        // A widget's /MK /R is independent of the page's /Rotate. Such
        // widgets have their own orientation even before the document is signed.
        if let Some(mk) = value(self.doc, dict, b"MK")? {
            let mk = mk.as_dict().map_err(pdf_error)?;
            let rotation = value(self.doc, mk, b"R")?
                .map(Object::as_i64)
                .transpose()
                .map_err(pdf_error)?
                .unwrap_or(0);
            if rotation.rem_euclid(360) != 0 {
                return Ok(());
            }
        }
        let Some(raw) = value(self.doc, dict, b"Rect")? else {
            return Ok(());
        };
        let coords = raw.as_array().map_err(pdf_error)?;
        if coords.len() != 4 {
            return Ok(());
        }
        let mut rect = [0.; 4];
        for (dest, obj) in rect.iter_mut().zip(coords) {
            *dest = self
                .doc
                .dereference(obj)
                .and_then(|(_, o)| o.as_float())
                .map_err(pdf_error)?;
        }
        if !rect.iter().all(|v| v.is_finite()) {
            return Ok(());
        }
        let [x, y, u, v] = rect;
        let geometry = PageGeometry::from_page(self.doc, page_id)?;
        let unit = ordered(geometry.to_unit(x, y), geometry.to_unit(u, v));
        if !valid_unit(unit) {
            return Ok(());
        }
        let terminal = self.doc.get_dictionary(field).map_err(pdf_error)?;
        let tooltip = value(self.doc, terminal, b"TU")?
            .map(decode_text_string)
            .transpose()
            .map_err(pdf_error)?
            .filter(|s| !s.trim().is_empty());
        self.candidates.push(Candidate {
            id: format!("field-{}-{}", field.0, field.1),
            name: attrs.name.clone(),
            tooltip,
            placement: Placement {
                page,
                page_id,
                unit,
                rect: ordered((x, y), (u, v)),
                rotation: geometry.rotation(),
                existing: Some((field, widget)),
            },
        });
        Ok(())
    }
}

fn parent_matches(dict: &Dictionary, expected: Option<ObjectId>) -> bool {
    match dict.get(b"Parent").ok() {
        None | Some(Object::Null) => expected.is_none(),
        Some(obj) => obj.as_reference().is_ok_and(|id| Some(id) == expected),
    }
}

/// Identity of each usable field in this revision, before applying earlier signers' locks.
pub(super) fn eligible_fields(doc: &Document) -> Result<HashMap<String, ObjectId>, String> {
    let (_, form) = super::acroform(doc)?;
    let fields = array(doc, &form, b"Fields")?;
    Ok(Inventory::load(doc, &doc.get_pages(), &fields)?
        .into_iter()
        .filter_map(|c| Some((c.name, c.placement.existing?.0)))
        .collect())
}

pub(super) fn inspect(
    doc: &Document,
    fields: &[Object],
    permissions: &Permissions,
) -> Result<Value, String> {
    let pages = doc.get_pages();
    let rows: Vec<_> = Inventory::load(doc, &pages, fields)?
        .into_iter()
        .filter(|c| permissions.allows_field(&c.name))
        .map(|c| {
            let name = if c.name.is_empty() {
                "署名欄"
            } else {
                &c.name
            };
            let mut row = json!({ "id": c.id, "name": name, "page": c.placement.page, "rect": c.placement.unit });
            if let Some(tooltip) = c.tooltip { row["tooltip"] = tooltip.into(); }
            row
        })
        .collect();
    Ok(json!({ "pages": pages.len(), "fields": rows,
        "alreadySigned": permissions.signed(), "allowsNewField": permissions.allows_new_field() }))
}

pub(super) fn resolve(
    doc: &Document,
    fields: &[Object],
    target: &Target,
    permissions: &Permissions,
) -> Result<Placement, String> {
    let pages = doc.get_pages();
    match target {
        Target::Field(id) => Inventory::load(doc, &pages, fields)?
            .into_iter()
            .find(|c| c.id == *id && permissions.allows_field(&c.name))
            .map(|c| c.placement)
            .ok_or_else(|| INVALID_TARGET.into()),
        Target::Area { page, rect } => {
            if !permissions.allows_new_field() {
                return Err(EXISTING_ONLY.into());
            }
            if !valid_unit(*rect) {
                return Err(INVALID_RECT.into());
            }
            let &page_id = pages.get(page).ok_or("指定されたページがありません。")?;
            let geometry = PageGeometry::from_page(doc, page_id)?;
            let [x, y, u, v] = *rect;
            let area = ordered(geometry.pdf_point(x, y), geometry.pdf_point(u, v));
            if area[0] >= area[2] || area[1] >= area[3] {
                return Err(INVALID_RECT.into());
            }
            Ok(Placement {
                page: *page,
                page_id,
                unit: *rect,
                rect: area,
                rotation: geometry.rotation(),
                existing: None,
            })
        }
    }
}
