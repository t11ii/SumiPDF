//! On-demand positions of painted PDF glyphs in display coordinates.
mod fonts;
mod standard;
pub(crate) const HELVETICA_WIDTHS: &[u16; 95] = &standard::HELVETICA;

use crate::highlight::{Direction, PageGeometry, TextRun, UnitRect, inherited};
use fonts::Font;
use lopdf::{Dictionary, Document, Object, ObjectId, content::Content};
use std::collections::HashMap;

const MAX_CONTENT: usize = 16 * 1024 * 1024;
const MAX_GLYPHS: usize = 100_000;
const MAX_OPERATIONS: usize = 400_000;
const LIMIT: &str = "ページが複雑なため、文字位置を取得できません。";

pub struct TextDocument {
    doc: Document,
    pages: Vec<ObjectId>,
}

impl TextDocument {
    pub fn load(bytes: &[u8]) -> Result<Self, String> {
        let doc = crate::pdfseal::editable_document(bytes)?;
        let pages = doc.page_iter().collect();
        Ok(Self { doc, pages })
    }

    pub fn page(&self, index: u32) -> Result<TextLayout, String> {
        let id = *self
            .pages
            .get(index as usize)
            .ok_or("ページ番号が範囲外です。")?;
        let geom = PageGeometry::from_page(&self.doc, id)?;
        // PDF annotation coordinates remain usable even when text extraction
        // encounters unsupported operations or a page exceeds its work budget.
        let Ok(bytes) = self.doc.get_page_content_with_limit(id, MAX_CONTENT) else {
            return Ok(TextLayout::default());
        };
        let mut parser = Parser {
            doc: &self.doc,
            geom,
            layout: TextLayout::default(),
            fonts: Vec::new(),
            font_ids: HashMap::new(),
            budget: MAX_CONTENT,
            ops: 0,
        };
        let resources = inherited(&self.doc, id, b"Resources")?.and_then(|o| o.as_dict().ok());
        match parser.content(&bytes, resources, State::default(), 0) {
            Ok(()) => Ok(parser.layout),
            Err(_) => Ok(TextLayout::default()),
        }
    }
}

#[derive(Clone, Default)]
pub struct TextLayout {
    glyphs: Vec<UnitRect>,
    lines: Vec<Line>,
}
#[derive(Clone)]
struct Line {
    start: usize,
    bounds: UnitRect,
    direction: Direction,
}

fn union(a: UnitRect, b: UnitRect) -> UnitRect {
    UnitRect {
        left: a.left.min(b.left),
        top: a.top.min(b.top),
        right: a.right.max(b.right),
        bottom: a.bottom.max(b.bottom),
    }
}

fn distance(r: UnitRect, x: f32, y: f32) -> f32 {
    let dx = (r.left - x).max(x - r.right).max(0.0);
    let dy = (r.top - y).max(y - r.bottom).max(0.0);
    dx * dx + dy * dy
}

fn same_line(a: UnitRect, b: UnitRect, direction: Direction) -> bool {
    // Work in each axis's own normalized units: overlap across the baseline,
    // and a gap of at most a few character widths along it. Keep columns apart.
    let axes = |r: UnitRect| match direction {
        Direction::Right => (r.left, r.right, r.top, r.bottom),
        Direction::Left => (-r.right, -r.left, r.top, r.bottom),
        Direction::Down => (r.top, r.bottom, r.left, r.right),
        Direction::Up => (-r.bottom, -r.top, r.left, r.right),
    };
    let (a0, a1, p0, p1) = axes(a);
    let (b0, b1, q0, q1) = axes(b);
    let across = (p1 - p0).min(q1 - q0);
    let along = (a1 - a0).max(b1 - b0);
    p1.min(q1) - p0.max(q0) >= across * 0.45 && b0 - a1 <= along * 3.0 && b0 >= a0 - along * 0.35
}

impl TextLayout {
    pub fn is_empty(&self) -> bool {
        self.glyphs.is_empty()
    }

    fn push(&mut self, r: UnitRect, direction: Direction) {
        let adjoining = self
            .lines
            .last()
            .zip(self.glyphs.last())
            .is_some_and(|(line, prev)| {
                line.direction == direction && same_line(*prev, r, direction)
            });
        if adjoining {
            let line = self.lines.last_mut().unwrap();
            line.bounds = union(line.bounds, r);
        } else {
            self.lines.push(Line {
                start: self.glyphs.len(),
                bounds: r,
                direction,
            });
        }
        self.glyphs.push(r);
    }
    fn end(&self, line: usize) -> usize {
        self.lines
            .get(line + 1)
            .map_or(self.glyphs.len(), |l| l.start)
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        for (i, line) in self.lines.iter().enumerate() {
            if distance(line.bounds, x, y) != 0.0 {
                continue;
            }
            if let Some(n) = self.glyphs[line.start..self.end(i)]
                .iter()
                .position(|&r| distance(r, x, y) == 0.0)
            {
                return Some(line.start + n);
            }
        }
        None
    }

    fn nearest(&self, x: f32, y: f32) -> Option<usize> {
        if let Some(hit) = self.hit(x, y) {
            return Some(hit);
        }
        let (i, line) = self
            .lines
            .iter()
            .enumerate()
            .min_by(|a, b| distance(a.1.bounds, x, y).total_cmp(&distance(b.1.bounds, x, y)))?;
        let (n, _) = self.glyphs[line.start..self.end(i)]
            .iter()
            .enumerate()
            .min_by(|a, b| distance(*a.1, x, y).total_cmp(&distance(*b.1, x, y)))?;
        Some(line.start + n)
    }

    pub fn range(&self, anchor: usize, x: f32, y: f32) -> Vec<TextRun> {
        if anchor >= self.glyphs.len() {
            return Vec::new();
        }
        let Some(endpoint) = self.nearest(x, y) else {
            return Vec::new();
        };
        let (first, last) = (anchor.min(endpoint), anchor.max(endpoint));
        let mut result = Vec::new();
        let start_line = self.lines.partition_point(|l| l.start <= first) - 1;
        for i in start_line..self.lines.len() {
            let q = &self.lines[i];
            if q.start > last {
                break;
            }
            let end = self.end(i);
            let bounds = if first <= q.start && end <= last + 1 {
                q.bounds
            } else {
                self.glyphs[first.max(q.start)..(last + 1).min(end)]
                    .iter()
                    .copied()
                    .reduce(union)
                    .unwrap()
            };
            result.push(TextRun {
                bounds,
                direction: q.direction,
            });
        }
        result
    }
}

#[derive(Clone, Copy)]
struct Matrix([f32; 6]);
impl Matrix {
    const ID: Self = Self([1., 0., 0., 1., 0., 0.]);
    fn of(items: &[Object]) -> Option<Self> {
        let [a, b, c, d, e, f] = items else {
            return None;
        };
        Some(Self([
            number(a)?,
            number(b)?,
            number(c)?,
            number(d)?,
            number(e)?,
            number(f)?,
        ]))
    }
    fn point(self, x: f32, y: f32) -> (f32, f32) {
        let [a, b, c, d, e, f] = self.0;
        (a * x + c * y + e, b * x + d * y + f)
    }
    fn then_local(self, local: Self) -> Self {
        let [a, b, c, d, _, _] = self.0;
        let [g, h, i, j, k, l] = local.0;
        let (x, y) = self.point(k, l);
        Self([
            a * g + c * h,
            b * g + d * h,
            a * i + c * j,
            b * i + d * j,
            x,
            y,
        ])
    }
    fn translate(&mut self, x: f32, y: f32) {
        (self.0[4], self.0[5]) = self.point(x, y);
    }
}
fn number(object: &Object) -> Option<f32> {
    object.as_float().ok().filter(|x| x.is_finite())
}

#[derive(Clone, Copy)]
struct State {
    ctm: Matrix,
    font: Option<usize>,
    size: f32,
    scale: f32,
    spacing: f32,
    word: f32,
    leading: f32,
    rise: f32,
}
impl Default for State {
    fn default() -> Self {
        Self {
            ctm: Matrix::ID,
            font: None,
            size: 0.,
            scale: 1.,
            spacing: 0.,
            word: 0.,
            leading: 0.,
            rise: 0.,
        }
    }
}
struct Position {
    line: Matrix,
    text: Matrix,
    valid: bool,
}
impl Position {
    fn new() -> Self {
        Self {
            line: Matrix::ID,
            text: Matrix::ID,
            valid: true,
        }
    }
    fn set(&mut self, m: Matrix) {
        self.line = m;
        self.text = m;
        self.valid = true;
    }
    fn line_move(&mut self, x: f32, y: f32) {
        self.line.translate(x, y);
        self.set(self.line);
    }
}

fn resource<'a>(
    doc: &'a Document,
    resources: Option<&'a Dictionary>,
    kind: &[u8],
    name: &Object,
) -> Option<&'a Object> {
    resources?
        .get_deref(kind, doc)
        .ok()?
        .as_dict()
        .ok()?
        .get_deref(name.as_name().ok()?, doc)
        .ok()
}

struct Parser<'a> {
    doc: &'a Document,
    geom: PageGeometry,
    layout: TextLayout,
    fonts: Vec<Font>,
    font_ids: HashMap<usize, Option<usize>>,
    budget: usize,
    ops: usize,
}
impl<'a> Parser<'a> {
    fn font(&mut self, dict: &'a Dictionary) -> Option<usize> {
        // Dictionaries belong to the immutable document throughout this page walk.
        let key = std::ptr::from_ref(dict) as usize;
        *self.font_ids.entry(key).or_insert_with(|| {
            let f = Font::from_dict(self.doc, dict)?;
            let n = self.fonts.len();
            self.fonts.push(f);
            Some(n)
        })
    }

    fn content(
        &mut self,
        bytes: &[u8],
        resources: Option<&'a Dictionary>,
        mut state: State,
        depth: usize,
    ) -> Result<(), String> {
        self.budget = self.budget.checked_sub(bytes.len()).ok_or(LIMIT)?;
        let mut stack = Vec::new();
        let mut pos = Position::new();
        for op in Content::decode_iter(bytes) {
            self.ops += 1;
            if self.ops > MAX_OPERATIONS {
                return Err(LIMIT.into());
            }
            let op = op.map_err(|e| e.to_string())?;
            let args = &op.operands;
            let val = |i| args.get(i).and_then(number).unwrap_or(0.);
            match (op.operator.as_str(), args.as_slice()) {
                ("q", _) => {
                    if stack.len() >= 256 {
                        return Err(LIMIT.into());
                    }
                    stack.push(state);
                }
                ("Q", _) => {
                    if let Some(s) = stack.pop() {
                        state = s;
                    }
                }
                ("cm", _) => {
                    if let Some(m) = Matrix::of(args) {
                        state.ctm = state.ctm.then_local(m);
                    }
                }
                ("Do", [name]) => {
                    if let Some(form) = resource(self.doc, resources, b"XObject", name)
                        .and_then(|o| o.as_stream().ok())
                        && form.dict.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Form")
                    {
                        if depth >= 16 {
                            return Err(LIMIT.into());
                        }
                        let mut inside = state;
                        if let Ok(matrix) = form
                            .dict
                            .get_deref(b"Matrix", self.doc)
                            .and_then(Object::as_array)
                            && let Some(m) = Matrix::of(matrix)
                        {
                            inside.ctm = state.ctm.then_local(m);
                        }
                        let local = form
                            .dict
                            .get_deref(b"Resources", self.doc)
                            .ok()
                            .and_then(|o| o.as_dict().ok())
                            .or(resources);
                        let data = form
                            .get_plain_content_with_limit(self.budget)
                            .map_err(|e| e.to_string())?;
                        self.content(&data, local, inside, depth + 1)?;
                    }
                }
                ("BT", _) => pos.set(Matrix::ID),
                ("Tm", _) => {
                    if let Some(m) = Matrix::of(args) {
                        pos.set(m);
                    }
                }
                ("Td" | "TD", [_, _]) => {
                    if op.operator == "TD" {
                        state.leading = -val(1);
                    }
                    pos.line_move(val(0), val(1));
                }
                ("T*", _) => pos.line_move(0., -state.leading),
                ("Tf", [name, size]) => {
                    state.font = resource(self.doc, resources, b"Font", name)
                        .and_then(|o| o.as_dict().ok())
                        .and_then(|d| self.font(d));
                    state.size = number(size).unwrap_or(0.);
                }
                ("Tz", _) => state.scale = val(0) / 100.,
                ("Tc", _) => state.spacing = val(0),
                ("Tw", _) => state.word = val(0),
                ("Ts", _) => state.rise = val(0),
                ("TL", _) => state.leading = val(0),
                ("Tj", [s]) => self.show(s, state, &mut pos)?,
                ("'", [s]) => {
                    pos.line_move(0., -state.leading);
                    self.show(s, state, &mut pos)?;
                }
                ("\"", [_, _, s]) => {
                    state.word = val(0);
                    state.spacing = val(1);
                    pos.line_move(0., -state.leading);
                    self.show(s, state, &mut pos)?;
                }
                ("TJ", [Object::Array(items)]) => {
                    for item in items {
                        if let Some(kern) = number(item) {
                            pos.text
                                .translate(-kern / 1000. * state.size * state.scale, 0.);
                        } else {
                            self.show(item, state, &mut pos)?;
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn show(&mut self, obj: &Object, s: State, pos: &mut Position) -> Result<(), String> {
        let Ok(data) = obj.as_str() else {
            return Ok(());
        };
        let Some(font) = s.font.map(|i| &self.fonts[i]) else {
            pos.valid = false;
            return Ok(());
        };
        if !pos.valid {
            return Ok(());
        }
        let unit = if font.two_byte { 2 } else { 1 };
        if data.len() % unit != 0 {
            pos.valid = false;
            return Ok(());
        }
        for chunk in data.chunks_exact(unit) {
            let code = if font.two_byte {
                u16::from_be_bytes([chunk[0], chunk[1]])
            } else {
                u16::from(chunk[0])
            };
            let Some(advance) = font.advance(code) else {
                pos.valid = false;
                break;
            };
            let width = advance * s.size * s.scale;
            let m = s.ctm.then_local(pos.text);
            let project = |x, y| {
                let (px, py) = m.point(x, y);
                self.geom.to_unit(px, py)
            };
            let lo = font.descent * s.size + s.rise;
            let hi = font.ascent * s.size + s.rise;
            let (x0, y0) = project(0., lo);
            let (x1, y1) = project(width, lo);
            let (x2, y2) = project(0., hi);
            let (x3, y3) = project(width, hi);
            let r = UnitRect {
                left: x0.min(x1).min(x2).min(x3).clamp(0., 1.),
                right: x0.max(x1).max(x2).max(x3).clamp(0., 1.),
                top: y0.min(y1).min(y2).min(y3).clamp(0., 1.),
                bottom: y0.max(y1).max(y2).max(y3).clamp(0., 1.),
            };
            if r.left < r.right && r.top < r.bottom {
                if self.layout.glyphs.len() >= MAX_GLYPHS {
                    return Err(LIMIT.into());
                }
                let direction = if (x1 - x0).abs() >= (y1 - y0).abs() {
                    if x1 >= x0 {
                        Direction::Right
                    } else {
                        Direction::Left
                    }
                } else if y1 >= y0 {
                    Direction::Down
                } else {
                    Direction::Up
                };
                self.layout.push(r, direction);
            }
            let word = if !font.two_byte && code == 32 {
                s.word
            } else {
                0.
            };
            pos.text.translate(width + (s.spacing + word) * s.scale, 0.);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "textselect/tests.rs"]
mod tests;
