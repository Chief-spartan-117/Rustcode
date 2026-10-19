//! `svg-render INPUT.svg OUTPUT.png SIZE` draws the SVG scaled to SIZE x SIZE pixels, with a
//! transparent background.

use std::process::exit;

use resvg::{tiny_skia, usvg};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input, output, size] = args.as_slice() else {
        eprintln!("usage: svg-render INPUT.svg OUTPUT.png SIZE");
        exit(2);
    };
    let size: u32 = size.parse().unwrap_or_else(|_| {
        eprintln!("SIZE must be a number of pixels");
        exit(2);
    });
    let data = std::fs::read(input).unwrap_or_else(|err| {
        eprintln!("cannot read {input}: {err}");
        exit(1);
    });
    let tree = usvg::Tree::from_data(&data, &usvg::Options::default()).unwrap_or_else(|err| {
        eprintln!("cannot parse {input}: {err}");
        exit(1);
    });
    let mut pixmap = tiny_skia::Pixmap::new(size, size).expect("a non-empty image");
    let scale = size as f32 / tree.size().width().max(tree.size().height());
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    pixmap.save_png(output).unwrap_or_else(|err| {
        eprintln!("cannot write {output}: {err}");
        exit(1);
    });
}
