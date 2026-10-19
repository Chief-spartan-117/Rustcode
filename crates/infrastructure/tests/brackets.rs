//! Bracket pairing over what the real TextMate grammars say is code, string or comment.

use std::sync::Arc;
use std::time::Duration;

use rustcode_application::{IncrementalHighlighter, SyntaxHighlighter};
use rustcode_domain::{BracketMap, ColorTheme, Language, TextBuffer};
use rustcode_infrastructure::TextMateHighlighter;

fn map_of(language: &str, text: &str) -> Arc<BracketMap> {
    let engine = TextMateHighlighter::new().expect("engine");
    let session = engine.start_session(&Language::new(language)).expect("session");
    let mut h = IncrementalHighlighter::new(session, Arc::new(ColorTheme::dark_plus()));
    let buffer = TextBuffer::from_text(text);
    while !h.advance(&buffer, Duration::from_secs(1)) {}
    h.bracket_map()
}

/// The brackets of line `line`, as the characters at their columns.
fn found(map: &BracketMap, line: usize) -> String {
    map.line(line).iter().map(|b| b.ch).collect()
}

#[test]
fn rust_strings_comments_and_chars_do_not_count() {
    let text = concat!(
        "fn f(a: &str) -> char {\n",
        "    let s = \"(not [real\"; // ) ] } stray\n",
        "    let c = '(';\n",
        "    /* { block\n",
        "       comment ( */\n",
        "    g(a)\n",
        "}\n",
    );
    let map = map_of("rust", text);
    assert_eq!(found(&map, 0), "()" .to_owned() + "{");
    assert_eq!(found(&map, 1), "");
    assert_eq!(found(&map, 2), "");
    assert_eq!(found(&map, 3), "");
    assert_eq!(found(&map, 4), "");
    assert_eq!(found(&map, 5), "()");
    assert_eq!(found(&map, 6), "}");
    // Every bracket that is code has a partner.
    assert!((0..7).all(|l| map.line(l).iter().all(|b| b.matched)));
    assert_eq!(map.partner((0, 22)), Some((6, 0)));
}

#[test]
fn typescript_template_expressions_are_code() {
    let text = "const s = `a ( ${f(x)} ) b`;\nconst o = { k: [1] };\n";
    let map = map_of("typescript", text);
    // The `(` and `)` in the literal text are skipped; the braces of `${}` and the call are code.
    assert_eq!(found(&map, 0), "{()}");
    assert_eq!(found(&map, 1), "{[]}");
    assert!(map.line(0).iter().all(|b| b.matched));
}

#[test]
fn python_fstring_fields_are_code_and_docstrings_are_not() {
    let text = "def f(x):\n    \"\"\"doc (\"\"\"\n    return f\"{g(x)} (\"\n";
    let map = map_of("python", text);
    assert_eq!(found(&map, 0), "()");
    assert_eq!(found(&map, 1), "");
    assert_eq!(found(&map, 2), "{()}");
    assert!(map.pairs().len() >= 3);
}

#[test]
fn generics_pair_up_and_comparisons_do_not() {
    let rust = map_of("rust", "let v: Vec<Vec<i32>> = a < b && c > d;\nfn f<T>(x: Option<T>) -> Result<(), E> {}\n");
    assert_eq!(found(&rust, 0), "<<>>");
    assert_eq!(rust.partner((0, 10)), Some((0, 19)));
    assert_eq!(rust.partner((0, 14)), Some((0, 18)));
    assert!(rust.line(0).iter().all(|b| b.matched));
    // `->` is not a bracket.
    assert_eq!(found(&rust, 1), "<>(<>)<()>{}");
    assert!(rust.line(1).iter().all(|b| b.matched));

    let ts = map_of("typescript", "const a: Array<number> = x < y ? 1 : 2;\nfunction f<T>(x: Map<string, T[]>) {}\n");
    assert_eq!(found(&ts, 0), "<>");
    assert_eq!(found(&ts, 1), "<>(<[]>){}");

    let java = map_of("java", "List<String> l = new ArrayList<>(); if (a < b) {}\n");
    assert_eq!(found(&java, 0), "<><>()(){}");

    let csharp = map_of("csharp", "List<int> l = new List<int>(); if (a < b) {}\n");
    assert_eq!(found(&csharp, 0), "<><>()(){}");
}

#[test]
fn cpp_templates_and_markup_tags_pair_up() {
    let cpp = map_of(
        "cpp",
        "std::vector<int> v; if (a < b && c > d) {}\nauto x = f<int>(y);\nstd::map<std::string, std::vector<int>> m;\nfor (int i=0; i<n; i++) {}\nx = y<<2;\n",
    );
    // Declarations are guessed from the text; calls come from the grammar; comparisons stay out.
    assert_eq!(found(&cpp, 0), "<>(){}");
    assert_eq!(cpp.partner((0, 11)), Some((0, 15)));
    assert_eq!(found(&cpp, 1), "<>()");
    assert_eq!(found(&cpp, 2), "<<>>");
    assert_eq!(cpp.partner((2, 8)), Some((2, 38)));
    assert_eq!(found(&cpp, 3), "(){}");
    assert_eq!(found(&cpp, 4), "");
    assert!((0..5).all(|l| cpp.line(l).iter().all(|b| b.matched)));

    // Each tag is a pair of its own, and the `>` of a tag's text is not one.
    let jsx = map_of("jsx", "const a = <div className=\"x\"><b>hi</b><br /></div>;\nconst c = a > b;\n");
    assert_eq!(found(&jsx, 0), "<><><><><>");
    assert_eq!(found(&jsx, 1), "");
    let xml = map_of("xml", "<a href=\"x\"><b/></a>\n");
    assert_eq!(found(&xml, 0), "<><><>");
    assert!(xml.line(0).iter().all(|b| b.matched && b.depth == 0));
}

/// The brackets of `text` when the language lists `pairs` as its brackets.
fn map_with(language: &str, text: &str, pairs: &[(&str, &str)]) -> Arc<BracketMap> {
    let engine = TextMateHighlighter::new().expect("engine");
    let session = engine.start_session(&Language::new(language)).expect("session");
    let mut h = IncrementalHighlighter::new(session, Arc::new(ColorTheme::dark_plus()));
    h.set_bracket_pairs(Some(pairs.iter().map(|(o, c)| ((*o).to_owned(), (*c).to_owned())).collect()));
    let buffer = TextBuffer::from_text(text);
    while !h.advance(&buffer, Duration::from_secs(1)) {}
    h.bracket_map()
}

#[test]
fn ruby_keywords_pair_but_names_strings_and_comments_do_not() {
    let text = concat!(
        "class Box\n",
        "  def open\n",
        "    items.each do |i|\n",
        "      puts \"end of #{i}\" # end\n",
        "      range = (1..3).end\n",
        "    end\n",
        "  end\n",
        "end\n",
    );
    let pairs = [("class", "end"), ("def", "end"), ("do", "end"), ("(", ")")];
    let map = map_with("ruby", text, &pairs);
    let words = |line: usize| map.line(line).iter().map(|b| (b.col, b.len)).collect::<Vec<_>>();
    assert_eq!(words(0), vec![(0, 5)], "class");
    assert_eq!(words(1), vec![(2, 3)], "def");
    // `do` is a keyword; the `end` in the string and the comment are not, nor is the method `.end`.
    assert_eq!(words(2), vec![(15, 2)], "do");
    assert_eq!(words(3), Vec::<(usize, usize)>::new());
    // The parentheses on the next line pair, and its `.end` is only a method name.
    assert_eq!(words(4).len(), 2);
    assert_eq!(words(5), vec![(4, 3)]);
    assert_eq!(words(6), vec![(2, 3)]);
    assert_eq!(words(7), vec![(0, 3)]);
    // Each `end` goes with the nearest block still open.
    assert_eq!(map.partner((2, 15)), Some((5, 4)));
    assert_eq!(map.partner((1, 2)), Some((6, 2)));
    assert_eq!(map.partner((0, 0)), Some((7, 0)));
    assert!((0..8).all(|l| map.line(l).iter().all(|b| b.matched)), "{:?}", (0..8).map(|l| words(l)).collect::<Vec<_>>());
}
