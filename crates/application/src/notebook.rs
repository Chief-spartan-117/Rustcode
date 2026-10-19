//! Jupyter notebooks (`.ipynb`, nbformat 4): reading and writing the file, and what a cell
//! output looks like. Everything the file holds that is not edited here (metadata, cell ids,
//! unknown fields) is kept, so saving a notebook does not lose anything.

use serde_json::{json, Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellKind {
    Code,
    Markdown,
    Raw,
}

impl CellKind {
    pub fn name(self) -> &'static str {
        match self {
            CellKind::Code => "code",
            CellKind::Markdown => "markdown",
            CellKind::Raw => "raw",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "code" => Some(CellKind::Code),
            "markdown" => Some(CellKind::Markdown),
            "raw" => Some(CellKind::Raw),
            _ => None,
        }
    }
}

/// One cell, with the fields this program edits pulled out of `extra`.
#[derive(Debug, Clone, PartialEq)]
pub struct NotebookCell {
    pub kind: CellKind,
    pub source: String,
    /// Outputs as they are in the file.
    pub outputs: Vec<Value>,
    pub execution_count: Option<u64>,
    /// The rest of the cell's fields: `id`, `metadata`, and anything unknown.
    pub extra: Map<String, Value>,
}

impl NotebookCell {
    pub fn new(kind: CellKind, source: &str) -> Self {
        let mut extra = Map::new();
        extra.insert("metadata".into(), json!({}));
        Self { kind, source: source.to_owned(), outputs: Vec::new(), execution_count: None, extra }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Notebook {
    pub cells: Vec<NotebookCell>,
    /// Notebook metadata: kernelspec, language_info, and the like.
    pub metadata: Value,
    pub nbformat: u64,
    pub nbformat_minor: u64,
    /// Top-level fields besides the ones above.
    extra: Map<String, Value>,
}

impl Notebook {
    /// An empty notebook for `language` with one empty code cell.
    pub fn new(language: &str) -> Self {
        let (name, display) = match language {
            "python" => ("python3", "Python 3"),
            other => (other, other),
        };
        Self {
            cells: vec![NotebookCell::new(CellKind::Code, "")],
            metadata: json!({
                "kernelspec": {"display_name": display, "language": language, "name": name},
                "language_info": {"name": language},
            }),
            nbformat: 4,
            nbformat_minor: 5,
            extra: Map::new(),
        }
    }

    /// The name of the kernel the notebook asks for (`kernelspec.name`), when it names one.
    pub fn kernel_name(&self) -> Option<String> {
        self.metadata.get("kernelspec")?.get("name")?.as_str().map(str::to_owned)
    }

    /// Record the kernel the notebook runs on, as Jupyter does (`kernelspec` of the metadata).
    pub fn set_kernelspec(&mut self, name: &str, display_name: &str, language: &str) {
        if !self.metadata.is_object() {
            self.metadata = json!({});
        }
        self.metadata["kernelspec"] = json!({"display_name": display_name, "language": language, "name": name});
    }

    /// The kernel to start: the interpreter the user picked (`metadata.rustcode.kernel`), else
    /// the one `kernelspec` names.
    pub fn preferred_kernel(&self) -> Option<String> {
        let picked = self.metadata.get("rustcode").and_then(|r| r.get("kernel")).and_then(Value::as_str);
        picked.map(str::to_owned).or_else(|| self.kernel_name())
    }

    /// Remember the kernel option picked in the program (`None` forgets it).
    pub fn set_preferred_kernel(&mut self, id: Option<&str>) {
        if !self.metadata.is_object() {
            self.metadata = json!({});
        }
        match id {
            Some(id) => self.metadata["rustcode"] = json!({"kernel": id}),
            None => {
                if let Some(map) = self.metadata.as_object_mut() {
                    map.remove("rustcode");
                }
            }
        }
    }

    /// The name to show for the kernel: `kernelspec.display_name`, else its name.
    pub fn kernel_display_name(&self) -> Option<String> {
        let spec = self.metadata.get("kernelspec")?;
        spec.get("display_name").or_else(|| spec.get("name"))?.as_str().map(str::to_owned)
    }

    /// The language of the code cells (`python` unless the metadata says otherwise).
    pub fn language(&self) -> String {
        let lookup = |path: &[&str]| {
            let mut node = &self.metadata;
            for key in path {
                node = node.get(*key)?;
            }
            node.as_str().map(str::to_owned)
        };
        lookup(&["kernelspec", "language"])
            .or_else(|| lookup(&["language_info", "name"]))
            .unwrap_or_else(|| "python".to_owned())
            .to_ascii_lowercase()
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let root: Value = serde_json::from_str(text).map_err(|e| format!("not a notebook: {e}"))?;
        let Value::Object(mut root) = root else { return Err("not a notebook: the file is not a JSON object".into()) };
        let nbformat = root.remove("nbformat").and_then(|v| v.as_u64()).ok_or("not a notebook: no nbformat")?;
        if nbformat < 4 {
            return Err(format!("notebook format {nbformat} is too old; open it in Jupyter and upgrade it to 4"));
        }
        let nbformat_minor = root.remove("nbformat_minor").and_then(|v| v.as_u64()).unwrap_or(0);
        let metadata = root.remove("metadata").unwrap_or_else(|| json!({}));
        let cells = match root.remove("cells") {
            Some(Value::Array(cells)) => cells,
            Some(_) => return Err("not a notebook: cells is not a list".into()),
            None => Vec::new(),
        };
        let cells = cells.into_iter().map(parse_cell).collect::<Result<Vec<_>, _>>()?;
        Ok(Self { cells, metadata, nbformat, nbformat_minor, extra: root })
    }

    /// The text of the file, written the way Jupyter does (one space of indent, sorted keys,
    /// a final newline) so that saving changes only what was edited.
    pub fn to_text(&self) -> String {
        let cells: Vec<Value> = self
            .cells
            .iter()
            .map(|cell| {
                let mut value = cell_to_value(cell);
                // Format 4.5 gives every cell an id.
                if self.nbformat_minor >= 5 && value.get("id").is_none() {
                    value["id"] = json!(fresh_id());
                }
                value
            })
            .collect();
        let mut root = self.extra.clone();
        root.insert("cells".into(), Value::Array(cells));
        root.insert("metadata".into(), self.metadata.clone());
        root.insert("nbformat".into(), json!(self.nbformat));
        root.insert("nbformat_minor".into(), json!(self.nbformat_minor));
        let mut out = Vec::new();
        let formatter = serde_json::ser::PrettyFormatter::with_indent(b" ");
        let mut serializer = serde_json::Serializer::with_formatter(&mut out, formatter);
        serde::Serialize::serialize(&sorted(Value::Object(root)), &mut serializer).expect("a JSON value serializes");
        let mut text = String::from_utf8(out).expect("JSON is UTF-8");
        text.push('\n');
        text
    }
}

/// `value` with the keys of every object in order, as Jupyter writes them.
fn sorted(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(entries.into_iter().map(|(key, value)| (key, sorted(value))).collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
        other => other,
    }
}

/// A cell id: eight hex digits that differ from run to run and from call to call.
fn fresh_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{:08x}", nanos.wrapping_mul(2_654_435_761).wrapping_add(count.wrapping_mul(40_503)))
}

fn parse_cell(value: Value) -> Result<NotebookCell, String> {
    let Value::Object(mut map) = value else { return Err("a cell is not an object".into()) };
    let kind = map
        .remove("cell_type")
        .and_then(|v| v.as_str().and_then(CellKind::parse))
        .ok_or("a cell has no known cell_type")?;
    let source = match map.remove("source") {
        Some(Value::String(text)) => text,
        Some(Value::Array(lines)) => lines.iter().filter_map(Value::as_str).collect(),
        _ => String::new(),
    };
    let outputs = match map.remove("outputs") {
        Some(Value::Array(outputs)) if kind == CellKind::Code => outputs,
        _ => Vec::new(),
    };
    let execution_count = map.remove("execution_count").and_then(|v| v.as_u64());
    Ok(NotebookCell { kind, source, outputs, execution_count, extra: map })
}

fn cell_to_value(cell: &NotebookCell) -> Value {
    let mut map = cell.extra.clone();
    map.insert("cell_type".into(), json!(cell.kind.name()));
    map.insert("source".into(), json!(source_lines(&cell.source)));
    map.entry("metadata").or_insert_with(|| json!({}));
    if cell.kind == CellKind::Code {
        map.insert("outputs".into(), Value::Array(cell.outputs.clone()));
        map.insert("execution_count".into(), cell.execution_count.map_or(Value::Null, |n| json!(n)));
    }
    Value::Object(map)
}

/// `"a\nb"` is stored as `["a\n", "b"]`.
fn source_lines(source: &str) -> Vec<String> {
    source.split_inclusive('\n').map(str::to_owned).collect()
}

/// What to show for one output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputView {
    Text { text: String, error: bool },
    /// A picture, base64 encoded as the file has it.
    Image { mime: String, data: String },
    /// Markup, with the plain text the kernel gave beside it. It is drawn as that text, and the
    /// markup itself can be opened as a page.
    Html { html: String, plain: Option<String> },
    /// A vector picture.
    Svg(String),
    Markdown(String),
}

/// The parts of `output` to draw, in order.
pub fn output_views(output: &Value) -> Vec<OutputView> {
    let text_of = |value: Option<&Value>| match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(lines)) => lines.iter().filter_map(Value::as_str).collect(),
        _ => String::new(),
    };
    match output.get("output_type").and_then(Value::as_str) {
        Some("stream") => {
            let error = output.get("name").and_then(Value::as_str) == Some("stderr");
            vec![OutputView::Text { text: text_of(output.get("text")), error }]
        }
        Some("error") => {
            let traceback: Vec<String> = output
                .get("traceback")
                .and_then(Value::as_array)
                .map(|lines| lines.iter().filter_map(Value::as_str).map(strip_ansi).collect())
                .unwrap_or_default();
            let text = if traceback.is_empty() {
                format!(
                    "{}: {}",
                    output.get("ename").and_then(Value::as_str).unwrap_or("Error"),
                    output.get("evalue").and_then(Value::as_str).unwrap_or("")
                )
            } else {
                traceback.join("\n")
            };
            vec![OutputView::Text { text, error: true }]
        }
        Some("execute_result" | "display_data") => {
            let Some(data) = output.get("data").and_then(Value::as_object) else { return Vec::new() };
            // The richest form that can be drawn: a picture, markup, text.
            for mime in ["image/png", "image/jpeg", "image/gif"] {
                if let Some(encoded) = data.get(mime) {
                    let encoded: String = text_of(Some(encoded)).chars().filter(|c| !c.is_whitespace()).collect();
                    return vec![OutputView::Image { mime: mime.to_owned(), data: encoded }];
                }
            }
            if let Some(svg) = data.get("image/svg+xml") {
                return vec![OutputView::Svg(text_of(Some(svg)))];
            }
            if let Some(html) = data.get("text/html") {
                let plain = data.get("text/plain").map(|p| text_of(Some(p)));
                return vec![OutputView::Html { html: text_of(Some(html)), plain }];
            }
            if let Some(markdown) = data.get("text/markdown") {
                return vec![OutputView::Markdown(text_of(Some(markdown)))];
            }
            if let Some(json) = data.get("application/json") {
                let text = serde_json::to_string_pretty(json).unwrap_or_default();
                return vec![OutputView::Text { text, error: false }];
            }
            for mime in ["text/latex", "text/plain"] {
                if data.contains_key(mime) {
                    return vec![OutputView::Text { text: text_of(data.get(mime)), error: false }];
                }
            }
            Vec::new()
        }
        _ => Vec::new(),
    }
}

/// Remove terminal color codes (`ESC [ ... m`), which tracebacks are full of.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// A `stream` output.
pub fn stream_output(name: &str, text: &str) -> Value {
    json!({"output_type": "stream", "name": name, "text": source_lines(text)})
}

/// An `execute_result` output with a plain text value.
pub fn result_output(execution_count: u64, text: &str) -> Value {
    json!({
        "output_type": "execute_result",
        "execution_count": execution_count,
        "metadata": {},
        "data": {"text/plain": source_lines(text)},
    })
}

/// A `display_data` output with one picture.
pub fn image_output(mime: &str, data: &str) -> Value {
    json!({"output_type": "display_data", "metadata": {}, "data": {mime: data, "text/plain": ["<Figure>"]}})
}

/// An `error` output.
pub fn error_output(name: &str, value: &str, traceback: &[String]) -> Value {
    json!({"output_type": "error", "ename": name, "evalue": value, "traceback": traceback})
}

/// Add stream text to `outputs`, joining it to the stream before it when that has the same name.
pub fn append_stream(outputs: &mut Vec<Value>, name: &str, text: &str) {
    if let Some(last) = outputs.last_mut() {
        if last.get("output_type").and_then(Value::as_str) == Some("stream") && last.get("name").and_then(Value::as_str) == Some(name) {
            let mut joined: String = match last.get("text") {
                Some(Value::Array(lines)) => lines.iter().filter_map(Value::as_str).collect(),
                Some(Value::String(text)) => text.clone(),
                _ => String::new(),
            };
            joined.push_str(text);
            last["text"] = json!(source_lines(&joined));
            return;
        }
    }
    outputs.push(stream_output(name, text));
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r##"{
 "cells": [
  {
   "cell_type": "markdown",
   "id": "intro",
   "metadata": {},
   "source": ["# Title\n", "text"]
  },
  {
   "cell_type": "code",
   "execution_count": 3,
   "id": "c1",
   "metadata": {"tags": ["x"]},
   "outputs": [
    {"name": "stdout", "output_type": "stream", "text": ["hi\n"]},
    {"output_type": "execute_result", "execution_count": 3, "metadata": {}, "data": {"text/plain": ["42"]}}
   ],
   "source": "print('hi')\n42"
  }
 ],
 "metadata": {"kernelspec": {"display_name": "Python 3", "language": "python", "name": "python3"}, "custom": 1},
 "nbformat": 4,
 "nbformat_minor": 5,
 "future_field": true
}"##;

    #[test]
    fn reads_cells_sources_and_outputs() {
        let nb = Notebook::parse(SAMPLE).unwrap();
        assert_eq!(nb.cells.len(), 2);
        assert_eq!(nb.cells[0].kind, CellKind::Markdown);
        assert_eq!(nb.cells[0].source, "# Title\ntext");
        assert_eq!(nb.cells[1].source, "print('hi')\n42");
        assert_eq!(nb.cells[1].execution_count, Some(3));
        assert_eq!(nb.cells[1].outputs.len(), 2);
        assert_eq!(nb.language(), "python");
    }

    #[test]
    fn writing_back_keeps_every_field() {
        let nb = Notebook::parse(SAMPLE).unwrap();
        let again = Notebook::parse(&nb.to_text()).unwrap();
        assert_eq!(nb, again);
        let text = nb.to_text();
        assert!(text.contains("\"future_field\": true"));
        assert!(text.contains("\"id\": \"intro\""));
        assert!(text.contains("\"tags\""));
        assert!(text.ends_with("}\n"));
        // One space of indent, like Jupyter.
        assert!(text.contains("\n \"cells\": ["));
    }

    #[test]
    fn keys_are_sorted_and_new_cells_get_ids() {
        let mut nb = Notebook::parse(SAMPLE).unwrap();
        nb.cells.push(NotebookCell::new(CellKind::Code, "1"));
        nb.cells.push(NotebookCell::new(CellKind::Code, "2"));
        let text = nb.to_text();
        let value: Value = serde_json::from_str(&text).unwrap();
        let id = |i: usize| value["cells"][i]["id"].as_str().unwrap().to_owned();
        assert_eq!(id(0), "intro");
        assert_eq!(id(2).len(), 8);
        assert_ne!(id(2), id(3));
        let top: Vec<&String> = value.as_object().unwrap().keys().collect();
        assert_eq!(top, ["cells", "future_field", "metadata", "nbformat", "nbformat_minor"]);
        let cell: Vec<&String> = value["cells"][1].as_object().unwrap().keys().collect();
        assert_eq!(cell, ["cell_type", "execution_count", "id", "metadata", "outputs", "source"]);
    }

    #[test]
    fn edited_source_is_stored_as_lines() {
        let mut nb = Notebook::parse(SAMPLE).unwrap();
        nb.cells[1].source = "a\nb\n".into();
        let value: Value = serde_json::from_str(&nb.to_text()).unwrap();
        assert_eq!(value["cells"][1]["source"], json!(["a\n", "b\n"]));
    }

    #[test]
    fn old_and_broken_files_are_refused() {
        assert!(Notebook::parse("{}").is_err());
        assert!(Notebook::parse("[]").is_err());
        assert!(Notebook::parse("{\"nbformat\": 3}").unwrap_err().contains("too old"));
        assert!(Notebook::parse("nope").is_err());
    }

    #[test]
    fn a_new_notebook_has_a_code_cell_and_a_language() {
        let nb = Notebook::new("python");
        assert_eq!(nb.cells.len(), 1);
        assert_eq!(nb.language(), "python");
        let again = Notebook::parse(&nb.to_text()).unwrap();
        assert_eq!(again.cells.len(), 1);
        assert_eq!(again.metadata, nb.metadata);
    }

    #[test]
    fn outputs_turn_into_things_to_draw() {
        let nb = Notebook::parse(SAMPLE).unwrap();
        assert_eq!(output_views(&nb.cells[1].outputs[0]), vec![OutputView::Text { text: "hi\n".into(), error: false }]);
        assert_eq!(output_views(&nb.cells[1].outputs[1]), vec![OutputView::Text { text: "42".into(), error: false }]);
        let png = image_output("image/png", "AAAA");
        assert_eq!(output_views(&png), vec![OutputView::Image { mime: "image/png".into(), data: "AAAA".into() }]);
        let err = error_output("ValueError", "bad", &["\u{1b}[0;31mTraceback\u{1b}[0m".to_owned(), "ValueError: bad".to_owned()]);
        assert_eq!(output_views(&err), vec![OutputView::Text { text: "Traceback\nValueError: bad".into(), error: true }]);
    }

    #[test]
    fn rich_outputs_keep_every_form_they_can_be_drawn_in() {
        let output = |data: Value| json!({"output_type": "display_data", "metadata": {}, "data": data});
        assert_eq!(
            output_views(&output(json!({"text/html": ["<b>x</b>"], "text/plain": "x"}))),
            vec![OutputView::Html { html: "<b>x</b>".into(), plain: Some("x".into()) }]
        );
        assert_eq!(output_views(&output(json!({"image/svg+xml": "<svg/>", "text/plain": "p"}))), vec![OutputView::Svg("<svg/>".into())]);
        assert_eq!(output_views(&output(json!({"text/markdown": "# t"}))), vec![OutputView::Markdown("# t".into())]);
        let json_view = output_views(&output(json!({"application/json": {"a": 1}})));
        assert!(matches!(&json_view[0], OutputView::Text { text, .. } if text.contains("\"a\": 1")));
        // A picture beats markup, and a widget that cannot be drawn shows its text.
        assert!(matches!(output_views(&output(json!({"image/png": "AAAA", "text/html": "x"})))[0], OutputView::Image { .. }));
        assert_eq!(
            output_views(&output(json!({"application/vnd.jupyter.widget-view+json": {"model_id": "m"}, "text/plain": "IntSlider(value=1)"}))),
            vec![OutputView::Text { text: "IntSlider(value=1)".into(), error: false }]
        );
        let nb = Notebook::parse(SAMPLE).unwrap();
        assert_eq!(nb.kernel_name().as_deref(), Some("python3"));
        assert_eq!(Notebook::new("python").kernel_name().as_deref(), Some("python3"));
    }

    #[test]
    fn stream_text_joins_the_stream_before_it() {
        let mut outputs = Vec::new();
        append_stream(&mut outputs, "stdout", "a\n");
        append_stream(&mut outputs, "stdout", "b\n");
        append_stream(&mut outputs, "stderr", "e\n");
        assert_eq!(outputs.len(), 2);
        assert_eq!(output_views(&outputs[0]), vec![OutputView::Text { text: "a\nb\n".into(), error: false }]);
    }
}
