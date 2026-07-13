fn main() {
    // Vendored tree-sitter grammars whose crates pin incompatible
    // tree-sitter versions — we compile just their C parsers.
    cc::Build::new()
        .include("grammars/dockerfile/src")
        .file("grammars/dockerfile/src/parser.c")
        .file("grammars/dockerfile/src/scanner.c")
        .warnings(false)
        .compile("tree-sitter-dockerfile");
    println!("cargo:rerun-if-changed=grammars/dockerfile/src");

    // go.mod: the only crate pins tree-sitter 0.20, so vendor its parser
    // (ABI 14, no external scanner) the same way.
    cc::Build::new()
        .include("grammars/gomod/src")
        .file("grammars/gomod/src/parser.c")
        .warnings(false)
        .compile("tree-sitter-gomod");
    println!("cargo:rerun-if-changed=grammars/gomod/src");
}
