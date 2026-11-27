//! Standard PDF text-markup annotations. UI coordinates are fractions of the
//! visible, rotated page, with the origin at its upper left.
use crate::pdfseal::{array, editable_document, pdf_error};
use lopdf::{Document, IncrementalDocument, Object, ObjectId, Stream, dictionary};
use std::collections::{BTreeMap, HashSet};
use std::io::Write;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UnitRect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Right,
    Down,
    Left,
    Up,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextRun {
    pub bounds: UnitRect,
    pub direction: Direction,
}

impl TextRun {
    pub fn horizontal(bounds: UnitRect) -> Self {
        Self {
            bounds,
            direction: Direction::Right,
        }
    }

    /// Corners in reading order, including for text on a rotated page.
    fn corners(self) -> [(f32, f32); 4] {
        let r = self.bounds;
        let (tl, tr) = ((r.left, r.top), (r.right, r.top));
        let (bl, br) = ((r.left, r.bottom), (r.right, r.bottom));
        match self.direction {
            Direction::Right => [tl, tr, bl, br],
            Direction::Down => [tr, br, tl, bl],
            Direction::Left => [br, bl, tr, tl],
            Direction::Up => [bl, tl, br, tr],
        }
    }

    pub fn ink(self, kind: MarkKind) -> UnitRect {
        let mut r = self.bounds;
        if kind == MarkKind::Underline {
            let h = (r.bottom - r.top) * 0.1;
            let w = (r.right - r.left) * 0.1;
            match self.direction {
                Direction::Right => r.top = r.bottom - h,
                Direction::Down => r.right = r.left + w,
                Direction::Left => r.bottom = r.top + h,
                Direction::Up => r.left = r.right - w,
            }
        }
        r
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MarkKind {
    #[default]
    Highlight,
    Underline,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MarkColor {
    #[default]
    Yellow,
    Red,
    Green,
    Blue,
}

impl MarkColor {
    pub const ALL: [Self; 4] = [Self::Yellow, Self::Red, Self::Green, Self::Blue];

    pub fn rgb(self) -> [u8; 3] {
        match self {
            Self::Yellow => [255, 255, 0],
            Self::Red => [255, 0, 0],
            Self::Green => [0, 255, 0],
            Self::Blue => [0, 0, 255],
        }
    }

    #[cfg(windows)]
    pub fn label(self) -> &'static str {
        match self {
            Self::Yellow => "Yellow",
            Self::Red => "Red",
            Self::Green => "Green",
            Self::Blue => "Blue",
        }
    }
}

pub const HIGHLIGHT_OPACITY: f32 = 0.45;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MarkStyle {
    pub kind: MarkKind,
    pub color: MarkColor,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Mark {
    /// Zero-based page number.
    pub page: u32,
    pub style: MarkStyle,
    pub runs: Vec<TextRun>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct PageGeometry {
    crop: Bounds,
    rotation: i64,
}

#[derive(Clone, Copy, Debug)]
struct Bounds {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl Bounds {
    fn from_points((x, y): (f32, f32), (u, v): (f32, f32)) -> Self {
        Self {
            x0: x.min(u),
            y0: y.min(v),
            x1: x.max(u),
            y1: y.max(v),
        }
    }

    fn objects(self) -> Vec<Object> {
        [self.x0, self.y0, self.x1, self.y1]
            .map(Object::Real)
            .to_vec()
    }

    fn include(&mut self, rect: Self) {
        self.x0 = self.x0.min(rect.x0);
        self.y0 = self.y0.min(rect.y0);
        self.x1 = self.x1.max(rect.x1);
        self.y1 = self.y1.max(rect.y1);
    }
}

pub(crate) fn inherited<'a>(
    doc: &'a Document,
    mut id: ObjectId,
    key: &[u8],
) -> Result<Option<&'a Object>, String> {
    let mut seen = HashSet::new();
    while seen.insert(id) {
        let node = doc.get_dictionary(id).map_err(pdf_error)?;
        if node.has(key) {
            let value = node.get_deref(key, doc).map_err(pdf_error)?;
            if !value.is_null() {
                return Ok(Some(value));
            }
        }
        match node.get(b"Parent").ok() {
            None | Some(Object::Null) => return Ok(None),
            Some(parent) => id = parent.as_reference().map_err(pdf_error)?,
        }
    }
    Err("PDFのページツリーが循環しています。".into())
}

fn page_box(doc: &Document, object: &Object) -> Result<Bounds, String> {
    let coords = object.as_array().map_err(pdf_error)?;
    if coords.len() != 4 {
        return Err("PDFのページ範囲が不正です。".into());
    }
    let mut values = [0.0; 4];
    for (value, obj) in values.iter_mut().zip(coords) {
        *value = doc
            .dereference(obj)
            .and_then(|(_, o)| o.as_float())
            .map_err(pdf_error)?;
        if !value.is_finite() {
            return Err("PDFのページ寸法が不正です。".into());
        }
    }
    let [x, y, u, v] = values;
    Ok(Bounds::from_points((x, y), (u, v)))
}

impl PageGeometry {
    pub(crate) fn rotation(self) -> i64 {
        self.rotation
    }

    pub(crate) fn from_page(doc: &Document, page: ObjectId) -> Result<Self, String> {
        let media = inherited(doc, page, b"MediaBox")?.ok_or("PDFのページ寸法がありません。")?;
        let media = page_box(doc, media)?;
        let mut crop = inherited(doc, page, b"CropBox")?
            .map(|o| page_box(doc, o))
            .transpose()?
            .unwrap_or(media);
        crop.x0 = crop.x0.max(media.x0);
        crop.y0 = crop.y0.max(media.y0);
        crop.x1 = crop.x1.min(media.x1);
        crop.y1 = crop.y1.min(media.y1);
        let (w, h) = (crop.x1 - crop.x0, crop.y1 - crop.y0);
        if !w.is_finite() || !h.is_finite() || w <= 0.0 || h <= 0.0 {
            return Err("PDFの表示範囲が不正です。".into());
        }
        let rotation = inherited(doc, page, b"Rotate")?
            .map(Object::as_i64)
            .transpose()
            .map_err(pdf_error)?
            .unwrap_or(0)
            .rem_euclid(360);
        if rotation % 90 != 0 {
            return Err("PDFのページ回転角が不正です。".into());
        }
        Ok(Self { crop, rotation })
    }

    pub(crate) fn to_unit(self, x: f32, y: f32) -> (f32, f32) {
        let b = self.crop;
        let (u, v) = ((x - b.x0) / (b.x1 - b.x0), (y - b.y0) / (b.y1 - b.y0));
        match self.rotation {
            90 => (v, u),
            180 => (1.0 - u, v),
            270 => (1.0 - v, 1.0 - u),
            _ => (u, 1.0 - v),
        }
    }

    pub(crate) fn pdf_point(self, s: f32, t: f32) -> (f32, f32) {
        let (u, v) = match self.rotation {
            90 => (t, s),
            180 => (1.0 - s, t),
            270 => (1.0 - t, 1.0 - s),
            _ => (s, 1.0 - t),
        };
        let b = self.crop;
        (b.x0 + (b.x1 - b.x0) * u, b.y0 + (b.y1 - b.y0) * v)
    }
}

fn write_mark(
    update: &mut Document,
    page: ObjectId,
    geometry: PageGeometry,
    mark: &Mark,
) -> Result<ObjectId, String> {
    let mut quads = Vec::new();
    let mut rect: Option<Bounds> = None;
    let (subtype, blend, opacity) = match mark.style.kind {
        MarkKind::Highlight => ("Highlight", "Multiply", HIGHLIGHT_OPACITY),
        MarkKind::Underline => ("Underline", "Normal", 1.0),
    };
    let color = mark.style.color.rgb().map(|v| f32::from(v) / 255.0);
    let [red, green, blue] = color;
    let mut commands = format!("/Ink gs\n{red} {green} {blue} rg\n").into_bytes();
    for run in &mark.runs {
        let part = run.bounds;
        let coords = [part.left, part.top, part.right, part.bottom];
        if coords.iter().any(|n| !(0.0..=1.0).contains(n))
            || part.left >= part.right
            || part.top >= part.bottom
        {
            return Err("注釈の選択範囲が不正です。".into());
        }
        let tl = geometry.pdf_point(part.left, part.top);
        let br = geometry.pdf_point(part.right, part.bottom);
        // Acrobat and other readers use text order (top-left, top-right,
        // bottom-left, bottom-right) for markup QuadPoints.
        for (s, t) in run.corners() {
            let (x, y) = geometry.pdf_point(s, t);
            quads.extend([Object::Real(x), Object::Real(y)]);
        }
        let r = Bounds::from_points(tl, br);
        if r.x0 >= r.x1 || r.y0 >= r.y1 {
            return Err("注釈の選択範囲が小さすぎます。".into());
        }
        if let Some(all) = &mut rect {
            all.include(r);
        } else {
            rect = Some(r);
        }
        let ink = run.ink(mark.style.kind);
        let r = Bounds::from_points(
            geometry.pdf_point(ink.left, ink.top),
            geometry.pdf_point(ink.right, ink.bottom),
        );
        writeln!(
            commands,
            "{} {} {} {} re",
            r.x0,
            r.y0,
            r.x1 - r.x0,
            r.y1 - r.y0
        )
        .map_err(pdf_error)?;
    }
    let rect = rect.ok_or("注釈の範囲がありません。")?;
    commands.extend_from_slice(b"f\n");
    let state = dictionary! { "Type" => "ExtGState", "BM" => blend, "ca" => opacity };
    let form = Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Form", "BBox" => rect.objects(),
            "Resources" => dictionary! { "ExtGState" => dictionary! { "Ink" => state } },
        },
        commands,
    );
    let appearance = update.try_add_object(form).map_err(pdf_error)?;
    update
        .try_add_object(dictionary! {
            "Type" => "Annot", "Subtype" => subtype, "P" => page, "F" => 4,
            "Rect" => rect.objects(), "QuadPoints" => quads,
            "C" => color.map(Object::Real).to_vec(), "CA" => opacity,
            "AP" => dictionary! { "N" => appearance },
        })
        .map_err(pdf_error)
}

pub fn annotate(input: Vec<u8>, marks: &[Mark]) -> Result<Vec<u8>, String> {
    if marks.is_empty() {
        return Ok(input);
    }
    const TOO_LARGE: &str = "注釈付きPDFが4 GiBに達する文書には対応していません。";
    let input_len = input.len();
    if input_len >= u32::MAX as usize {
        return Err(TOO_LARGE.into());
    }
    let doc = editable_document(&input)?;
    let pages: Vec<_> = doc.page_iter().collect();
    let mut by_page = BTreeMap::<u32, Vec<&Mark>>::new();
    for mark in marks {
        by_page.entry(mark.page).or_default().push(mark);
    }
    let mut revision = IncrementalDocument::create_from(input, doc);
    for (number, marks) in by_page {
        let id = *pages
            .get(number as usize)
            .ok_or("注釈を付けるページがありません。")?;
        let previous = revision.get_prev_documents();
        let mut page = previous.get_dictionary(id).map_err(pdf_error)?.clone();
        let mut annots = array(previous, &page, b"Annots")?;
        let geometry = PageGeometry::from_page(previous, id)?;
        for mark in marks {
            annots.push(write_mark(&mut revision.new_document, id, geometry, mark)?.into());
        }
        page.set("Annots", annots);
        revision.new_document.set_object(id, page);
    }

    crate::pdfseal::require_transparency(&mut revision)?;
    let mut result = Vec::new();
    result
        .try_reserve_exact(
            input_len
                .saturating_add(8192)
                .saturating_add(marks.len().saturating_mul(1024)),
        )
        .map_err(|_| "注釈の保存に必要なメモリを確保できません。")?;
    revision.save_to(&mut result).map_err(pdf_error)?;
    if result.len() > u32::MAX as usize {
        return Err(TOO_LARGE.into());
    }
    Ok(result)
}

#[cfg(test)]
#[path = "highlight_tests.rs"]
mod tests;
