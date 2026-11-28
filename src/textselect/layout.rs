use crate::{
    highlight::{Direction, TextRun, UnitRect},
    winpdf::Document,
};

#[path = "search.rs"]
mod search;
pub(crate) use search::SearchPageCache;
use search::SearchText;

const LIMIT: &str = "ページが複雑なため、文字位置を取得できません。";

pub struct TextDocument {
    document: Document,
    #[cfg(test)]
    _fixture: Option<tempfile::TempPath>,
}

impl TextDocument {
    #[cfg(windows)]
    pub fn from_source(source: &crate::winpdf::SigningSource) -> Result<Self, String> {
        Ok(Self {
            document: Document::from_source(source)?,
            #[cfg(test)]
            _fixture: None,
        })
    }

    #[cfg(test)]
    fn load_for_search(bytes: &[u8]) -> Result<Self, String> {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
        file.write_all(bytes).map_err(|e| e.to_string())?;
        let path = file.into_temp_path();
        let document = Document::open_file(&path)?;
        Ok(Self {
            document,
            _fixture: Some(path),
        })
    }

    #[cfg(test)]
    pub(crate) fn selection(bytes: &[u8], index: u32) -> Result<TextLayout, String> {
        let mut selection = Self::load_for_search(bytes)?.page(index)?;
        selection.allows_annotations = crate::pdfseal::editable_document(bytes).is_ok();
        Ok(selection)
    }

    pub fn page(&self, index: u32) -> Result<TextLayout, String> {
        self.scan_page(index, None).map(|(layout, _)| layout)
    }

    pub fn page_count(&self) -> usize {
        self.document.page_count() as usize
    }

    #[cfg(test)]
    fn search_page(&self, index: u32, query: &str) -> Result<Vec<Vec<UnitRect>>, String> {
        let (layout, text) = self.scan_page(index, Some(SearchText::default()))?;
        Ok(text.unwrap().matches(&layout, query))
    }

    fn scan_page(
        &self,
        index: u32,
        mut text: Option<SearchText>,
    ) -> Result<(TextLayout, Option<SearchText>), String> {
        let mut layout = TextLayout::default();
        let mut previous_line = None;
        self.document
            .glyphs(index, |mut bounds, ch, line, direction| {
                let new_line = previous_line != Some(line);
                let coords = [bounds.left, bounds.top, bounds.right, bounds.bottom];
                if !coords.into_iter().all(f32::is_finite)
                    || bounds.right <= 0.0
                    || bounds.left >= 1.0
                    || bounds.bottom <= 0.0
                    || bounds.top >= 1.0
                {
                    return true;
                }
                bounds.left = bounds.left.max(0.0);
                bounds.top = bounds.top.max(0.0);
                bounds.right = bounds.right.min(1.0);
                bounds.bottom = bounds.bottom.min(1.0);
                if bounds.left >= bounds.right || bounds.top >= bounds.bottom {
                    // A ToUnicode mapping can emit several characters for one glyph.
                    if new_line || ch.is_none() {
                        return true;
                    }
                    bounds = *layout.glyphs.last().unwrap();
                }
                layout.push(bounds, direction, new_line);
                previous_line = Some(line);
                text.as_mut()
                    .is_none_or(|text| text.append(&layout, ch).is_ok())
            })?;
        Ok((layout, text))
    }
}

#[derive(Clone, Default)]
pub struct TextLayout {
    glyphs: Vec<UnitRect>,
    lines: Vec<Line>,
    pub allows_annotations: bool,
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

impl TextLayout {
    pub fn is_empty(&self) -> bool {
        self.glyphs.is_empty()
    }

    fn push(&mut self, bounds: UnitRect, direction: Direction, new_line: bool) {
        if new_line {
            self.lines.push(Line {
                start: self.glyphs.len(),
                bounds,
                direction,
            });
        } else if let Some(line) = self.lines.last_mut() {
            line.bounds = union(line.bounds, bounds);
        }
        self.glyphs.push(bounds);
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
        self.runs(first, last)
    }

    fn runs(&self, first: usize, last: usize) -> Vec<TextRun> {
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

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
