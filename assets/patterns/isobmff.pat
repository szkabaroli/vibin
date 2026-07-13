// ISO base media file format — the box tree behind MP4/MOV, HEIF/HEIC and
// Sony .hif. Big-endian boxes: a u32 size (covering the 8-byte header),
// a 4-char type, then either child boxes or leaf data. Container boxes
// recurse; `meta` is a FullBox (4 extra version/flags bytes before its
// children). size 1 (64-bit largesize) and size 0 (extends to EOF) are
// not handled. Magic: an "ftyp" box at offset 0. See src/pattern.rs.

format isobmff {
    magic = "66 74 79 70" @ 4; // "ftyp" as the first box type
    root = isobmff_file;
}

enum box_type : u32 {
    0x66747970 = "ftyp (file type)",
    0x6d6f6f76 = "moov (movie)",
    0x7472616b = "trak (track)",
    0x6d646961 = "mdia (media)",
    0x6d696e66 = "minf (media info)",
    0x7374626c = "stbl (sample table)",
    0x64696e66 = "dinf (data info)",
    0x65647473 = "edts (edit)",
    0x75647461 = "udta (user data)",
    0x6d766578 = "mvex (movie extends)",
    0x6d6f6f66 = "moof (movie fragment)",
    0x74726166 = "traf (track fragment)",
    0x6d657461 = "meta (metadata)",
    0x69707270 = "iprp (item properties)",
    0x6970636f = "ipco (item property container)",
    0x6d766864 = "mvhd (movie header)",
    0x746b6864 = "tkhd (track header)",
    0x68646c72 = "hdlr (handler)",
    0x73747364 = "stsd (sample description)",
    0x6d646174 = "mdat (media data)",
    0x66726565 = "free (free space)",
    0x736b6970 = "skip (free space)",
    0x69696e66 = "iinf (item info)",
    0x696c6f63 = "iloc (item location)",
    0x7069746d = "pitm (primary item)",
}

struct isobmff_file {
    boxes: mp4_box[];
}

struct mp4_box {
    size: be u32;
    type: be box_type;
    body: match type {
        0x6d6f6f76 = box_children,
        0x7472616b = box_children,
        0x6d646961 = box_children,
        0x6d696e66 = box_children,
        0x7374626c = box_children,
        0x64696e66 = box_children,
        0x65647473 = box_children,
        0x75647461 = box_children,
        0x6d766578 = box_children,
        0x6d6f6f66 = box_children,
        0x74726166 = box_children,
        0x69707270 = box_children,
        0x6970636f = box_children,
        0x6d657461 = meta_box,
        0x66747970 = ftyp_box,
        _ = raw_box,
    } span size - 8;
}

struct box_children {
    children: mp4_box[];
}

// meta is a FullBox: version+flags precede the child boxes
struct meta_box {
    version_flags: be u32;
    children: mp4_box[];
}

struct ftyp_box {
    major_brand: char[4];
    minor_version: be u32;
    compatible_brands: u8[];
}

struct raw_box {
    data: u8[];
}
