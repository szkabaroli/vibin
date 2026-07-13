// SQLite 3 database files — 100-byte header, then the file tiles into
// fixed-size b-tree pages (page 1 shares its space with the header).
// All integers big-endian. Limitation: page_size 0x0001 means 65536,
// which an expression can't remap; such files (rare) misparse the pages.
// See src/pattern.rs for the pattern language reference.

format sqlite {
    magic = "53 51 4c 69 74 65 20 66 6f 72 6d 61 74 20 33 00"; // "SQLite format 3\0"
    root = sqlite_file;
}

enum text_encoding : u32 {
    1 = "UTF-8",
    2 = "UTF-16le",
    3 = "UTF-16be",
}

enum schema_format : u32 {
    1 = "legacy",
    2 = "ALTER TABLE ADD COLUMN",
    3 = "non-NULL defaults",
    4 = "DESC indexes + boolean",
}

enum page_type : u8 {
    2 = "interior index",
    5 = "interior table",
    10 = "leaf index",
    13 = "leaf table",
}

struct sqlite_file {
    header: db_header;
    // page 1's b-tree content starts right after the header
    page1: btree_page span page_size - 100;
    pages: page[];
}

struct db_header {
    magic: char[16];
    page_size: be u16;
    write_version: u8;
    read_version: u8;
    reserved_per_page: u8;
    max_payload_fraction: u8;
    min_payload_fraction: u8;
    leaf_payload_fraction: u8;
    change_counter: be u32;
    size_in_pages: be u32;
    freelist_trunk_page: be u32;
    freelist_pages: be u32;
    schema_cookie: be u32;
    schema: be schema_format;
    default_cache_size: be u32;
    largest_root_page: be u32;
    encoding: be text_encoding;
    user_version: be u32;
    incremental_vacuum: be u32;
    application_id: be u32;
    reserved: u8[20];
    version_valid_for: be u32;
    sqlite_version: be u32;
}

// remaining pages tile the rest of the file
struct page {
    body: btree_page span page_size;
}

struct btree_page {
    type: page_type;
    first_freeblock: be u16;
    cell_count: be u16;
    content_start: be u16;
    fragmented_bytes: u8;
    rest: match type {
        2 = interior_page,
        5 = interior_page,
        _ = leaf_page,
    };
}

struct interior_page {
    rightmost_pointer: be u32;
    cells: u8[];
}

struct leaf_page {
    cells: u8[];
}
