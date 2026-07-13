// ZIP archives — also the container of .sketch, .fig, .docx, .jar, .apk…
// A well-formed zip is a chain of PK-signed records; we walk them front to
// back (the central directory is also reached that way). Streaming zips
// whose sizes live in trailing data descriptors degrade to a raw tail.
// See src/pattern.rs for the pattern language reference.

format zip {
    magic = "50 4b 03 04";
    root = zip_file;
}

// signatures read big-endian so they print as 0x504b.... ("PK..")
enum zip_sig : u32 {
    0x504b0304 = "local file",
    0x504b0102 = "central directory",
    0x504b0506 = "end of central directory",
    0x504b0606 = "zip64 end of central directory",
    0x504b0607 = "zip64 locator",
    0x504b0708 = "data descriptor",
}

enum compression : u16 {
    0 = "stored",
    8 = "deflated",
    9 = "deflate64",
    12 = "bzip2",
    14 = "lzma",
    93 = "zstd",
    99 = "AES encrypted",
}

flags gp_flags : u16 {
    0x1 = "ENCRYPTED",
    0x8 = "DATA_DESCRIPTOR",
    0x800 = "UTF8",
}

struct zip_file {
    records: record[];
}

struct record {
    sig: be zip_sig;
    body: match sig {
        0x504b0304 = local_file,
        0x504b0102 = central_entry,
        0x504b0506 = end_of_central,
        0x504b0708 = data_descriptor,
        _ = raw_tail,
    };
}

struct raw_tail {
    data: u8[];
}

struct local_file {
    version_needed: u16;
    flags: gp_flags;
    method: compression;
    mod_time: u16;
    mod_date: u16;
    crc32: u32;
    compressed_size: u32;
    uncompressed_size: u32;
    name_len: u16;
    extra_len: u16;
    name: char[name_len];
    extra: u8[extra_len];
    data: u8[compressed_size];
}

struct central_entry {
    version_made_by: u16;
    version_needed: u16;
    flags: gp_flags;
    method: compression;
    mod_time: u16;
    mod_date: u16;
    crc32: u32;
    compressed_size: u32;
    uncompressed_size: u32;
    name_len: u16;
    extra_len: u16;
    comment_len: u16;
    disk_start: u16;
    internal_attrs: u16;
    external_attrs: u32;
    local_header_offset: u32;
    name: char[name_len];
    extra: u8[extra_len];
    comment: char[comment_len];
}

struct end_of_central {
    disk: u16;
    central_dir_disk: u16;
    entries_on_disk: u16;
    entries_total: u16;
    central_dir_size: u32;
    central_dir_offset: u32;
    comment_len: u16;
    comment: char[comment_len];
}

struct data_descriptor {
    crc32: u32;
    compressed_size: u32;
    uncompressed_size: u32;
}
