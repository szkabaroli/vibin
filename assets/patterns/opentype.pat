// OpenType / TrueType fonts (.ttf .otf) and collections (.ttc). Big-endian:
// an offset table names the outline flavor and table count, then a table
// directory of tag/checksum/offset/length records; each record claims its
// table's bytes via @ offset. Collections wrap several fonts, each parsed
// at its own offset. WOFF/WOFF2 (compressed) are separate formats.
// See src/pattern.rs for the pattern language reference.

format opentype {
    magic = "00 01 00 00"; // TrueType outlines
    root = sfnt;
}

format opentype {
    magic = "4f 54 54 4f"; // "OTTO" — CFF outlines
    root = sfnt;
}

format opentype {
    magic = "74 72 75 65"; // "true" — legacy Mac TrueType
    root = sfnt;
}

format opentype_collection {
    magic = "74 74 63 66"; // "ttcf"
    root = ttc_file;
}

enum sfnt_version : u32 {
    0x00010000 = "TrueType (1.0)",
    0x4f54544f = "OTTO (CFF outlines)",
    0x74727565 = "true (Mac)",
    0x74797031 = "typ1",
}

struct sfnt {
    version: be sfnt_version;
    num_tables: be u16;
    search_range: be u16;
    entry_selector: be u16;
    range_shift: be u16;
    tables: table_record[num_tables];
}

struct table_record {
    tag: char[4];
    checksum: be u32;
    offset: be u32;
    length: be u32;
    data: u8[length] @ offset;
}

struct ttc_file {
    tag: char[4]; // "ttcf"
    major_version: be u16;
    minor_version: be u16;
    num_fonts: be u32;
    fonts: ttc_offset[num_fonts];
}

// each collection entry is an offset to a full font offset table
struct ttc_offset {
    offset: be u32;
    font: sfnt @ offset;
}
