//! Emmet abbreviations: `ul>li.item*3` becomes markup, `m10-20` becomes `margin: 10px 20px;`.
//!
//! The result is snippet text (`$1` tab stops, `$0` at the end), so the editor can step through
//! the places left to fill in.

/// What an abbreviation expands into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmmetSyntax {
    Html,
    /// JSX and TSX: `className`, and void tags close themselves.
    Jsx,
    Css,
}

/// The syntax for a file name, if Emmet applies to it.
pub fn syntax_for_file(name: &str) -> Option<EmmetSyntax> {
    let extension = name.rsplit_once('.')?.1.to_ascii_lowercase();
    match extension.as_str() {
        "html" | "htm" | "xhtml" | "vue" | "svelte" | "php" | "erb" | "hbs" | "ejs" | "njk" | "twig" | "xml" | "svg" => {
            Some(EmmetSyntax::Html)
        }
        "jsx" | "tsx" => Some(EmmetSyntax::Jsx),
        "css" | "scss" | "sass" | "less" | "styl" => Some(EmmetSyntax::Css),
        _ => None,
    }
}

/// The abbreviation that ends at the end of `before` (the text of the line up to the caret), and
/// where it starts, in chars.
pub fn abbreviation_at_end(before: &str) -> Option<(usize, String)> {
    let chars: Vec<char> = before.chars().collect();
    let mut start = chars.len();
    let mut depth = 0i32;
    let mut in_braces = false;
    let mut in_brackets = false;
    let mut in_quote: Option<char> = None;
    while start > 0 {
        let c = chars[start - 1];
        if let Some(quote) = in_quote {
            if c == quote {
                in_quote = None;
            }
            start -= 1;
            continue;
        }
        match c {
            '"' | '\'' if in_brackets => in_quote = Some(c),
            '}' => in_braces = true,
            '{' => {
                if !in_braces {
                    break;
                }
                in_braces = false;
            }
            ']' => in_brackets = true,
            '[' => {
                if !in_brackets {
                    break;
                }
                in_brackets = false;
            }
            ')' => depth += 1,
            '(' => {
                depth -= 1;
                if depth < 0 {
                    break;
                }
            }
            _ if in_braces || in_brackets => {}
            c if c.is_whitespace() => break,
            // A tag or attribute value around the abbreviation stops it.
            '<' | '>' if start == chars.len() => break,
            '"' | '\'' | '`' | ';' | ',' | '=' => break,
            c if c.is_alphanumeric() || "#.>+*^$@-_:!%/&|~".contains(c) => {}
            _ => break,
        }
        start -= 1;
    }
    let text: String = chars[start..].iter().collect();
    // `>` leading means a closing tag's end; `.`/`#`/digits alone are not abbreviations worth it.
    let text = text.trim_start_matches('>').to_owned();
    let start = chars.len() - text.chars().count();
    if text.is_empty() {
        None
    } else {
        Some((start, text))
    }
}

/// Expand `abbreviation`; `None` when it is not an abbreviation (so Tab just indents).
pub fn expand(abbreviation: &str, syntax: EmmetSyntax) -> Option<String> {
    match syntax {
        EmmetSyntax::Css => expand_css(abbreviation),
        EmmetSyntax::Html => expand_markup(abbreviation, false),
        EmmetSyntax::Jsx => expand_markup(abbreviation, true),
    }
}

// ---- markup ------------------------------------------------------------------------------------

const VOID: &[&str] = &["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr"];
const INLINE: &[&str] = &[
    "a", "abbr", "acronym", "b", "bdo", "big", "br", "button", "cite", "code", "dfn", "em", "font", "i", "img", "input", "kbd", "label",
    "map", "object", "output", "q", "samp", "select", "small", "span", "strike", "strong", "sub", "sup", "textarea", "tt", "u", "var",
];

#[derive(Debug, Clone, Default)]
struct Node {
    name: String,
    id: Option<String>,
    classes: Vec<String>,
    attributes: Vec<(String, Option<String>)>,
    text: Option<String>,
    children: Vec<Node>,
    /// Repeats, with `$` standing for the 1-based number.
    repeat: usize,
    /// A `(group)`: no tag of its own, only children.
    group: bool,
}

struct MarkupParser<'a> {
    chars: Vec<char>,
    at: usize,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl MarkupParser<'_> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn name(&mut self) -> String {
        let start = self.at;
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == ':' || c == '$' || c == '@' {
                self.at += 1;
            } else {
                break;
            }
        }
        self.chars[start..self.at].iter().collect()
    }

    /// `[a=1 b="two" c]`, `{text}` read up to the matching closer.
    fn balanced(&mut self, open: char, close: char) -> Option<String> {
        if self.peek() != Some(open) {
            return None;
        }
        self.at += 1;
        let start = self.at;
        let mut quote: Option<char> = None;
        while let Some(c) = self.peek() {
            match quote {
                Some(q) => {
                    if c == q {
                        quote = None;
                    }
                }
                None if c == close => {
                    let inner = self.chars[start..self.at].iter().collect();
                    self.at += 1;
                    return Some(inner);
                }
                None if (c == '"' || c == '\'') && open == '[' => quote = Some(c),
                None => {}
            }
            self.at += 1;
        }
        None
    }

    /// One element with its id, classes, attributes, text and repeat count.
    fn element(&mut self) -> Option<Node> {
        let mut node = Node { repeat: 1, ..Default::default() };
        if self.peek() == Some('(') {
            self.at += 1;
            let inner = self.sequence()?;
            if self.peek() != Some(')') {
                return None;
            }
            self.at += 1;
            node.group = true;
            node.children = inner;
        } else {
            node.name = self.name();
        }
        loop {
            match self.peek() {
                Some('#') => {
                    self.at += 1;
                    let id = self.name();
                    if id.is_empty() {
                        return None;
                    }
                    node.id = Some(id);
                }
                Some('.') => {
                    self.at += 1;
                    let class = self.name();
                    if class.is_empty() {
                        return None;
                    }
                    node.classes.push(class);
                }
                Some('[') => {
                    let inner = self.balanced('[', ']')?;
                    node.attributes.extend(parse_attributes(&inner));
                }
                Some('{') => node.text = Some(self.balanced('{', '}')?),
                Some('*') => {
                    self.at += 1;
                    let start = self.at;
                    while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                        self.at += 1;
                    }
                    let digits: String = self.chars[start..self.at].iter().collect();
                    node.repeat = digits.parse().ok().filter(|n| (1..=200).contains(n))?;
                }
                _ => break,
            }
        }
        if node.name.is_empty() && !node.group && node.id.is_none() && node.classes.is_empty() && node.attributes.is_empty() && node.text.is_none() {
            return None;
        }
        Some(node)
    }

    /// Siblings joined with `+`, children with `>`, climbing with `^`.
    fn sequence(&mut self) -> Option<Vec<Node>> {
        // The tree is built as a path from the root, so `^` can pop.
        let mut roots: Vec<Node> = Vec::new();
        let mut path: Vec<usize> = Vec::new();
        let first = self.element()?;
        roots.push(first);
        path.push(0);
        loop {
            match self.peek() {
                Some('>') => {
                    self.at += 1;
                    let child = self.element()?;
                    let parent = node_at(&mut roots, &path);
                    parent.children.push(child);
                    let index = parent.children.len() - 1;
                    path.push(index);
                }
                Some('+') => {
                    self.at += 1;
                    let sibling = self.element()?;
                    let last = path.pop().unwrap_or(0);
                    let _ = last;
                    if path.is_empty() {
                        roots.push(sibling);
                        path.push(roots.len() - 1);
                    } else {
                        let parent = node_at(&mut roots, &path);
                        parent.children.push(sibling);
                        let index = parent.children.len() - 1;
                        path.push(index);
                    }
                }
                Some('^') => {
                    self.at += 1;
                    path.pop();
                    // Climbing from the top level starts a new top-level sibling.
                    let sibling = self.element().or_else(|| None);
                    let Some(sibling) = sibling else { return None };
                    if path.len() <= 1 {
                        path.clear();
                        roots.push(sibling);
                        path.push(roots.len() - 1);
                    } else {
                        path.pop();
                        let parent = node_at(&mut roots, &path);
                        parent.children.push(sibling);
                        let index = parent.children.len() - 1;
                        path.push(index);
                    }
                }
                _ => break,
            }
        }
        Some(roots)
    }
}

fn node_at<'n>(roots: &'n mut [Node], path: &[usize]) -> &'n mut Node {
    let mut node = &mut roots[path[0]];
    for &index in &path[1..] {
        node = &mut node.children[index];
    }
    node
}

fn parse_attributes(text: &str) -> Vec<(String, Option<String>)> {
    let chars: Vec<char> = text.chars().collect();
    let mut attributes = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        while at < chars.len() && chars[at].is_whitespace() {
            at += 1;
        }
        let start = at;
        while at < chars.len() && !chars[at].is_whitespace() && chars[at] != '=' {
            at += 1;
        }
        let name: String = chars[start..at].iter().collect();
        if name.is_empty() {
            break;
        }
        if at < chars.len() && chars[at] == '=' {
            at += 1;
            let value: String = if at < chars.len() && (chars[at] == '"' || chars[at] == '\'') {
                let quote = chars[at];
                at += 1;
                let from = at;
                while at < chars.len() && chars[at] != quote {
                    at += 1;
                }
                let value = chars[from..at].iter().collect();
                at += 1;
                value
            } else {
                let from = at;
                while at < chars.len() && !chars[at].is_whitespace() {
                    at += 1;
                }
                chars[from..at].iter().collect()
            };
            attributes.push((name, Some(value)));
        } else {
            attributes.push((name, None));
        }
    }
    attributes
}

/// Characters that mean something in snippet text.
fn snippet_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '$' | '}' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Replace each `$` run with the 1-based `number`, zero padded to the run's length.
fn number_dollars(text: &str, number: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut at = 0;
    while at < chars.len() {
        if chars[at] == '$' {
            let start = at;
            while at < chars.len() && chars[at] == '$' {
                at += 1;
            }
            out.push_str(&format!("{:0width$}", number, width = at - start));
        } else {
            out.push(chars[at]);
            at += 1;
        }
    }
    out
}

struct Writer {
    syntax: EmmetSyntax,
    stops: usize,
    /// The number of the repeat of the enclosing `(group)*n`, for `$` in its elements.
    group_number: Option<usize>,
}

impl Writer {
    fn stop(&mut self) -> String {
        self.stops += 1;
        format!("${}", self.stops)
    }

    fn default_name(parent: Option<&str>, node: &Node) -> String {
        // A bare `.item` or `#id` is a div, or what fits its parent.
        let _ = node;
        match parent {
            Some("ul" | "ol") => "li",
            Some("table" | "tbody" | "thead" | "tfoot") => "tr",
            Some("tr") => "td",
            Some("select" | "optgroup") => "option",
            Some("p" | "span" | "em" | "strong" | "a" | "b" | "i" | "label" | "button" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6") => "span",
            _ => "div",
        }
        .to_owned()
    }

    /// The nodes at one level, each on its own line at `depth`.
    fn level(&mut self, nodes: &[Node], parent: Option<&str>, depth: usize, out: &mut Vec<String>) {
        for node in nodes {
            for number in 1..=node.repeat {
                if node.group {
                    let outer = self.group_number.replace(number);
                    self.level(&node.children, parent, depth, out);
                    self.group_number = outer;
                } else {
                    self.element(node, number, parent, depth, out);
                }
            }
        }
    }

    fn element(&mut self, node: &Node, number: usize, parent: Option<&str>, depth: usize, out: &mut Vec<String>) {
        let indent = "  ".repeat(depth);
        let mut name = if node.name.is_empty() { Self::default_name(parent, node) } else { node.name.clone() };
        let n = if node.repeat > 1 { number } else { self.group_number.unwrap_or(1) };
        let numbered = |text: &str| if text.contains('$') { number_dollars(text, n) } else { text.to_owned() };
        name = numbered(&name);

        let (mut attributes, mut implied_text) = (Vec::<(String, String)>::new(), None::<String>);
        // Defaults of well-known tags, where the value is left to fill in.
        match name.as_str() {
            "a" => attributes.push(("href".into(), String::new())),
            "img" => {
                attributes.push(("src".into(), String::new()));
                attributes.push(("alt".into(), String::new()));
            }
            "link" => {
                attributes.push(("rel".into(), "stylesheet".into()));
                attributes.push(("href".into(), String::new()));
            }
            "script" => {}
            "input" => attributes.push(("type".into(), "text".into())),
            "form" => attributes.push(("action".into(), String::new())),
            "label" => {}
            "meta" => {}
            _ => {}
        }
        if name == "btn" {
            name = "button".into();
        }
        let mut id = node.id.clone().map(|id| numbered(&id));
        let mut classes: Vec<String> = node.classes.iter().map(|c| numbered(c)).collect();
        let class_attribute = if self.syntax == EmmetSyntax::Jsx { "className" } else { "class" };
        let mut explicit: Vec<(String, Option<String>)> = node.attributes.iter().map(|(k, v)| (numbered(k), v.as_ref().map(|v| numbered(v)))).collect();
        // Explicit attributes replace the defaults of the same name.
        attributes.retain(|(key, _)| !explicit.iter().any(|(k, _)| k == key));
        let mut parts: Vec<String> = Vec::new();
        if let Some(id) = id.take() {
            parts.push(format!("id=\"{}\"", snippet_escape(&id)));
        }
        if !classes.is_empty() {
            parts.push(format!("{class_attribute}=\"{}\"", snippet_escape(&classes.join(" "))));
            classes.clear();
        }
        for (key, value) in attributes {
            let value = if value.is_empty() { self.stop() } else { snippet_escape(&value) };
            parts.push(format!("{key}=\"{value}\""));
        }
        for (key, value) in explicit.drain(..) {
            let key = if self.syntax == EmmetSyntax::Jsx && key == "class" { class_attribute.to_owned() } else { key };
            match value {
                Some(value) if value.is_empty() => {
                    let stop = self.stop();
                    parts.push(format!("{key}=\"{stop}\""))
                }
                Some(value) => parts.push(format!("{key}=\"{}\"", snippet_escape(&value))),
                None => parts.push(format!("{key}=\"{}\"", self.stop())),
            }
        }
        if let Some(text) = &node.text {
            implied_text = Some(snippet_escape(&numbered(text)));
        }
        let open_tag = if parts.is_empty() { name.clone() } else { format!("{name} {}", parts.join(" ")) };

        let void = VOID.contains(&name.as_str());
        if void {
            let close = if self.syntax == EmmetSyntax::Jsx { " />" } else { ">" };
            out.push(format!("{indent}<{open_tag}{close}"));
            return;
        }
        if node.children.is_empty() {
            // `a` and `button` are text-only; `{text}` is inline; empty tags get a stop inside.
            let inside = implied_text.unwrap_or_else(|| self.stop());
            out.push(format!("{indent}<{open_tag}>{inside}</{name}>"));
            return;
        }
        // Inline parents with only inline children stay on one line.
        let mut children = Vec::new();
        self.level(&node.children, Some(&name), depth + 1, &mut children);
        let inline = INLINE.contains(&name.as_str()) && node.children.iter().all(|c| INLINE.contains(&c.name.as_str()) && c.children.is_empty());
        if inline {
            let flat: String = children.iter().map(|c| c.trim_start().to_owned()).collect();
            out.push(format!("{indent}<{open_tag}>{}{flat}</{name}>", implied_text.unwrap_or_default()));
        } else {
            out.push(format!("{indent}<{open_tag}>{}", implied_text.unwrap_or_default()));
            out.extend(children);
            out.push(format!("{indent}</{name}>"));
        }
    }
}

fn expand_markup(abbreviation: &str, jsx: bool) -> Option<String> {
    let abbreviation = abbreviation.trim();
    if abbreviation == "!" {
        return Some(
            "<!DOCTYPE html>\n<html lang=\"${1:en}\">\n<head>\n  <meta charset=\"UTF-8\">\n  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\">\n  <title>${2:Document}</title>\n</head>\n<body>\n  $0\n</body>\n</html>"
                .to_owned(),
        );
    }
    if abbreviation.is_empty() {
        return None;
    }
    let mut parser = MarkupParser { chars: abbreviation.chars().collect(), at: 0, _marker: Default::default() };
    let roots = parser.sequence()?;
    if parser.at != parser.chars.len() {
        return None;
    }
    // A lone word that is no known tag is not worth taking over Tab for.
    if roots.len() == 1 && is_plain_word(&roots[0]) && !KNOWN_TAGS.contains(&roots[0].name.as_str()) {
        return None;
    }
    let syntax = if jsx { EmmetSyntax::Jsx } else { EmmetSyntax::Html };
    let mut writer = Writer { syntax, stops: 0, group_number: None };
    let mut lines = Vec::new();
    writer.level(&roots, None, 0, &mut lines);
    let mut text = lines.join("\n");
    // The caret ends after the markup when nothing is left to fill in.
    if writer.stops == 0 {
        text.push_str("$0");
    } else {
        text = renumber_last_as_final(text, writer.stops);
    }
    Some(text)
}

/// A node that is only a name: no class, id, attributes, text, children or repeat.
fn is_plain_word(node: &Node) -> bool {
    node.id.is_none() && node.classes.is_empty() && node.attributes.is_empty() && node.text.is_none() && node.children.is_empty() && node.repeat == 1 && !node.group
}

/// Tab stops are numbered in order; the last one becomes `$0` so Tab ends there.
fn renumber_last_as_final(text: String, stops: usize) -> String {
    text.replace(&format!("${stops}"), "$0")
}

const KNOWN_TAGS: &[&str] = &[
    "a", "abbr", "address", "article", "aside", "audio", "b", "blockquote", "body", "br", "button", "canvas", "caption", "code", "col",
    "details", "div", "dl", "dt", "dd", "em", "fieldset", "figure", "footer", "form", "h1", "h2", "h3", "h4", "h5", "h6", "head", "header",
    "hr", "html", "i", "iframe", "img", "input", "label", "legend", "li", "link", "main", "meta", "nav", "ol", "optgroup", "option", "p",
    "pre", "script", "section", "select", "small", "span", "strong", "style", "sub", "summary", "sup", "table", "tbody", "td", "textarea",
    "tfoot", "th", "thead", "title", "tr", "ul", "video", "btn",
];

// ---- css ---------------------------------------------------------------------------------------

/// Property abbreviations, and the unit a bare number gets (`None`: stays a bare number).
const PROPERTIES: &[(&str, &str, bool)] = &[
    ("m", "margin", true),
    ("mt", "margin-top", true),
    ("mr", "margin-right", true),
    ("mb", "margin-bottom", true),
    ("ml", "margin-left", true),
    ("p", "padding", true),
    ("pt", "padding-top", true),
    ("pr", "padding-right", true),
    ("pb", "padding-bottom", true),
    ("pl", "padding-left", true),
    ("w", "width", true),
    ("h", "height", true),
    ("miw", "min-width", true),
    ("maw", "max-width", true),
    ("mih", "min-height", true),
    ("mah", "max-height", true),
    ("t", "top", true),
    ("r", "right", true),
    ("b", "bottom", true),
    ("l", "left", true),
    ("z", "z-index", false),
    ("fz", "font-size", true),
    ("fw", "font-weight", false),
    ("lh", "line-height", false),
    ("op", "opacity", false),
    ("bd", "border", true),
    ("bdrs", "border-radius", true),
    ("bdw", "border-width", true),
    ("gap", "gap", true),
    ("fl", "flex", false),
    ("fg", "flex-grow", false),
    ("fsh", "flex-shrink", false),
    ("ord", "order", false),
    ("c", "color", false),
    ("bg", "background", false),
    ("bgc", "background-color", false),
    ("bgi", "background-image", false),
    ("d", "display", false),
    ("pos", "position", false),
    ("ov", "overflow", false),
    ("ovx", "overflow-x", false),
    ("ovy", "overflow-y", false),
    ("ta", "text-align", false),
    ("td", "text-decoration", false),
    ("tt", "text-transform", false),
    ("ff", "font-family", false),
    ("fs", "font-style", false),
    ("fxd", "flex-direction", false),
    ("fxw", "flex-wrap", false),
    ("jc", "justify-content", false),
    ("ai", "align-items", false),
    ("as", "align-self", false),
    ("cur", "cursor", false),
    ("va", "vertical-align", false),
    ("ws", "white-space", false),
    ("v", "visibility", false),
    ("bxsh", "box-shadow", false),
    ("bxz", "box-sizing", false),
    ("tr", "transition", false),
    ("trf", "transform", false),
    ("anim", "animation", false),
    ("ol", "outline", false),
    ("lis", "list-style", false),
    ("cnt", "content", false),
];

/// Whole-declaration aliases.
const DECLARATIONS: &[(&str, &str)] = &[
    ("db", "display: block"),
    ("di", "display: inline"),
    ("dib", "display: inline-block"),
    ("df", "display: flex"),
    ("dif", "display: inline-flex"),
    ("dg", "display: grid"),
    ("dn", "display: none"),
    ("dt", "display: table"),
    ("posa", "position: absolute"),
    ("posr", "position: relative"),
    ("posf", "position: fixed"),
    ("poss", "position: sticky"),
    ("fl", "float: left"),
    ("fr", "float: right"),
    ("fn", "float: none"),
    ("tac", "text-align: center"),
    ("tal", "text-align: left"),
    ("tar", "text-align: right"),
    ("fwb", "font-weight: bold"),
    ("fwn", "font-weight: normal"),
    ("fsi", "font-style: italic"),
    ("tdn", "text-decoration: none"),
    ("tdu", "text-decoration: underline"),
    ("jcc", "justify-content: center"),
    ("jcsb", "justify-content: space-between"),
    ("jcsa", "justify-content: space-around"),
    ("aic", "align-items: center"),
    ("ais", "align-items: flex-start"),
    ("aie", "align-items: flex-end"),
    ("curp", "cursor: pointer"),
    ("ovh", "overflow: hidden"),
    ("ova", "overflow: auto"),
    ("m0a", "margin: 0 auto"),
    ("bxzb", "box-sizing: border-box"),
    ("wsn", "white-space: nowrap"),
    ("vh", "visibility: hidden"),
];

fn expand_css(abbreviation: &str) -> Option<String> {
    let text = abbreviation.trim();
    let (text, important) = match text.strip_suffix('!') {
        Some(rest) => (rest, true),
        None => (text, false),
    };
    if text.is_empty() {
        return None;
    }
    let tail = if important { " !important" } else { "" };
    if let Some((_, declaration)) = DECLARATIONS.iter().find(|(key, _)| *key == text) {
        return Some(format!("{declaration}{tail};$0"));
    }
    // Property letters, then a value: `m10`, `p10-20`, `w50p`, `lh1.5`, `m-10`, `c#f00`.
    let split = text.find(|c: char| c.is_ascii_digit() || c == '-' || c == '#' || c == '.').unwrap_or(text.len());
    let (key, value) = text.split_at(split);
    let (_, property, unit) = PROPERTIES.iter().find(|(abbr, ..)| *abbr == key)?;
    if value.is_empty() {
        return Some(format!("{property}: ${{1}}{tail};$0"));
    }
    let value = css_value(value, *unit)?;
    Some(format!("{property}: {value}{tail};$0"))
}

/// `10-20` → `10px 20px`; `50p` → `50%`; `1.5e` → `1.5em`; `#f00` stays.
fn css_value(value: &str, px: bool) -> Option<String> {
    if let Some(color) = value.strip_prefix('#') {
        let hex = color.len() == 3 || color.len() == 6;
        return (hex && color.chars().all(|c| c.is_ascii_hexdigit())).then(|| format!("#{color}"));
    }
    let mut parts = Vec::new();
    // Values are separated by `-`, except a minus right before a digit at the start of one.
    let chars: Vec<char> = value.chars().collect();
    let mut at = 0;
    while at < chars.len() {
        let mut part = String::new();
        if chars[at] == '-' && at + 1 < chars.len() && chars[at + 1].is_ascii_digit() && (at == 0 || chars[at - 1] == '-') {
            part.push('-');
            at += 1;
        } else if chars[at] == '-' {
            at += 1;
            continue;
        }
        while at < chars.len() && (chars[at].is_ascii_digit() || chars[at] == '.') {
            part.push(chars[at]);
            at += 1;
        }
        let mut suffix = String::new();
        while at < chars.len() && chars[at].is_ascii_alphabetic() {
            suffix.push(chars[at]);
            at += 1;
        }
        if part.trim_start_matches('-').is_empty() {
            return None;
        }
        part.push_str(match suffix.as_str() {
            "" if px && part.trim_start_matches('-') != "0" => "px",
            "" => "",
            "p" => "%",
            "e" => "em",
            "r" => "rem",
            "x" => "ex",
            "vw" | "vh" | "px" | "em" | "rem" | "ch" | "s" | "ms" | "deg" => &suffix,
            _ => return None,
        });
        parts.push(part);
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn html(abbr: &str) -> String {
        expand(abbr, EmmetSyntax::Html).unwrap_or_else(|| panic!("{abbr} did not expand"))
    }

    #[test]
    fn a_class_makes_a_div() {
        assert_eq!(html(".box"), "<div class=\"box\">$0</div>");
        assert_eq!(html("#main"), "<div id=\"main\">$0</div>");
    }

    #[test]
    fn nesting_and_siblings() {
        assert_eq!(html("ul>li"), "<ul>\n  <li>$0</li>\n</ul>");
        assert_eq!(html("div+p"), "<div>$1</div>\n<p>$0</p>");
        assert_eq!(html("div>p^span"), "<div>\n  <p>$1</p>\n</div>\n<span>$0</span>");
    }

    #[test]
    fn multiplication_numbers_with_dollar() {
        assert_eq!(
            html("ul>li.item$*3"),
            "<ul>\n  <li class=\"item1\">$1</li>\n  <li class=\"item2\">$2</li>\n  <li class=\"item3\">$0</li>\n</ul>"
        );
        assert_eq!(html("(p.a$)*2"), "<p class=\"a1\">$1</p>\n<p class=\"a2\">$0</p>");
    }

    #[test]
    fn text_attributes_and_defaults() {
        assert_eq!(html("a{Go}"), "<a href=\"$0\">Go</a>");
        assert_eq!(html("img"), "<img src=\"$1\" alt=\"$0\">");
        assert_eq!(html("input[name=q]"), "<input type=\"text\" name=\"q\">$0");
        assert_eq!(html("p[title=\"a b\"]{hi}"), "<p title=\"a b\">hi</p>$0");
    }

    #[test]
    fn groups_and_implicit_names() {
        assert_eq!(html("(p+p)*2"), "<p>$1</p>\n<p>$2</p>\n<p>$3</p>\n<p>$0</p>");
        assert_eq!(html("ul>.x"), "<ul>\n  <li class=\"x\">$0</li>\n</ul>");
    }

    #[test]
    fn doctype_boilerplate() {
        assert!(html("!").starts_with("<!DOCTYPE html>"));
    }

    #[test]
    fn jsx_uses_class_name_and_closes_void_tags() {
        assert_eq!(expand(".a", EmmetSyntax::Jsx).unwrap(), "<div className=\"a\">$0</div>");
        assert_eq!(expand("br", EmmetSyntax::Jsx).unwrap(), "<br />$0");
    }

    #[test]
    fn plain_words_are_not_abbreviations() {
        assert_eq!(expand("hello", EmmetSyntax::Html), None);
        assert_eq!(expand("foo>", EmmetSyntax::Html), None);
        assert!(expand("div", EmmetSyntax::Html).is_some());
    }

    #[test]
    fn css_properties_and_values() {
        let css = |a| expand(a, EmmetSyntax::Css);
        assert_eq!(css("m10").unwrap(), "margin: 10px;$0");
        assert_eq!(css("p10-20").unwrap(), "padding: 10px 20px;$0");
        assert_eq!(css("w50p").unwrap(), "width: 50%;$0");
        assert_eq!(css("m-10").unwrap(), "margin: -10px;$0");
        assert_eq!(css("lh1.5").unwrap(), "line-height: 1.5;$0");
        assert_eq!(css("z10").unwrap(), "z-index: 10;$0");
        assert_eq!(css("c#f00").unwrap(), "color: #f00;$0");
        assert_eq!(css("m0").unwrap(), "margin: 0;$0");
        assert_eq!(css("df").unwrap(), "display: flex;$0");
        assert_eq!(css("jcsb").unwrap(), "justify-content: space-between;$0");
        assert_eq!(css("w100!").unwrap(), "width: 100px !important;$0");
        assert_eq!(css("m").unwrap(), "margin: ${1};$0");
        assert_eq!(css("nonsense"), None);
    }

    #[test]
    fn finds_the_abbreviation_before_the_caret() {
        assert_eq!(abbreviation_at_end("    ul>li*3"), Some((4, "ul>li*3".to_owned())));
        assert_eq!(abbreviation_at_end("x a[href=\"a b\"]{hi there}"), Some((2, "a[href=\"a b\"]{hi there}".to_owned())));
        assert_eq!(abbreviation_at_end("   "), None);
        assert_eq!(abbreviation_at_end("(p+p)*2"), Some((0, "(p+p)*2".to_owned())));
    }

    #[test]
    fn files_pick_a_syntax() {
        assert_eq!(syntax_for_file("a.html"), Some(EmmetSyntax::Html));
        assert_eq!(syntax_for_file("a.tsx"), Some(EmmetSyntax::Jsx));
        assert_eq!(syntax_for_file("a.scss"), Some(EmmetSyntax::Css));
        assert_eq!(syntax_for_file("a.rs"), None);
    }
}
