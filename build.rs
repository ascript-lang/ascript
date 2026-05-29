fn main() {
    let dir = "docs/superpowers/specs/grammar/tree-sitter-ascript/src";
    println!("cargo:rerun-if-changed={}/parser.c", dir);
    cc::Build::new()
        .include(dir)
        .file(format!("{}/parser.c", dir))
        .warnings(false)
        .compile("tree_sitter_ascript");
}
