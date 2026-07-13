// Apple binary property list (bplist00) — preferences, provisioning
// profiles, app metadata, NSKeyedArchiver payloads. The object table
// follows the magic, but the trailer (which describes everything) is the
// last 32 bytes and the offset table sits just before it, both located
// with the `$end` region-end identifier. Individual objects use a tagged
// encoding that a forward parser can't stride, so we decode the trailer
// and mark the offset table region rather than each object.
// See src/pattern.rs for the pattern language reference.

format bplist {
    magic = "62 70 6c 69 73 74 30 30"; // "bplist00"
    root = bplist_file;
}

struct bplist_file {
    magic: char[8];
    trailer: bplist_trailer @ $end - 32;
    offset_table: offset_table @ offset_table_offset;
}

struct bplist_trailer {
    unused: u8[5];
    sort_version: u8;
    offset_int_size: u8;
    object_ref_size: u8;
    num_objects: be u64;
    top_object: be u64;
    offset_table_offset: be u64;
}

// num_objects offsets, each offset_int_size bytes, big-endian
struct offset_table {
    offsets: u8[num_objects * offset_int_size];
}
