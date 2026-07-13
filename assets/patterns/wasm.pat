// WebAssembly — core modules and component-model binaries.
// See src/pattern.rs for the pattern language reference.

format wasm {
    magic = "00 61 73 6d 01 00 00 00"; // \0asm, version 1
    root = wasm_module;
}

format wasm_component {
    magic = "00 61 73 6d 0d 00 01 00"; // \0asm, version 13, layer 1
    root = component;
}

// ----- core module ----------------------------------------------------------

enum section_id : u8 {
    0 = "custom",
    1 = "type",
    2 = "import",
    3 = "function",
    4 = "table",
    5 = "memory",
    6 = "global",
    7 = "export",
    8 = "start",
    9 = "element",
    10 = "code",
    11 = "data",
    12 = "data count",
    13 = "tag",
}

enum export_kind : u8 {
    0 = "func",
    1 = "table",
    2 = "memory",
    3 = "global",
}

struct wasm_module {
    header: wasm_header;
    sections: wasm_section[];
}

struct wasm_header {
    magic: char[4];
    version: u32;
}

struct wasm_section {
    id: section_id;
    size: leb128;
    body: match id {
        0 = custom_section,
        1 = type_section,
        7 = export_section,
        8 = start_section,
        10 = code_section,
        12 = start_section,
        _ = raw_section,
    } span size;
}

struct raw_section {
    data: u8[];
}

struct custom_section {
    name: lstr;
    data: u8[];
}

struct type_section {
    count: leb128;
    types: functype[count];
}

struct functype {
    tag: u8;
    nparams: leb128;
    params: u8[nparams];
    nresults: leb128;
    results: u8[nresults];
}

struct export_section {
    count: leb128;
    entries: export_entry[count];
}

struct export_entry {
    name: lstr;
    kind: export_kind;
    index: leb128;
}

struct start_section {
    index: leb128;
}

struct code_section {
    count: leb128;
    bodies: funcbody[count];
}

struct funcbody {
    size: leb128;
    code: u8[size];
}

// ----- component model ------------------------------------------------------

enum component_section_id : u8 {
    0 = "custom",
    1 = "core module",
    2 = "core instance",
    3 = "core type",
    4 = "component",
    5 = "instance",
    6 = "alias",
    7 = "type",
    8 = "canon",
    9 = "start",
    10 = "import",
    11 = "export",
    12 = "value",
}

struct component {
    header: component_header;
    sections: component_section[];
}

struct component_header {
    magic: char[4];
    version: u16;
    layer: u16;
}

struct component_section {
    id: component_section_id;
    size: leb128;
    body: match id {
        0 = custom_section,
        _ = raw_section,
    } span size;
}
