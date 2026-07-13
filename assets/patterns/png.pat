// PNG (and APNG) — signature followed by length-prefixed chunks, all
// integers big-endian. See src/pattern.rs for the language reference.

format png {
    magic = "89 50 4e 47 0d 0a 1a 0a";
    root = png_file;
}

// chunk types are four ASCII bytes; reading them as a be u32 enum both
// labels them and names the chunk elements in the tree
enum chunk_type : u32 {
    0x49484452 = "IHDR",
    0x504c5445 = "PLTE",
    0x49444154 = "IDAT",
    0x49454e44 = "IEND",
    0x74455874 = "tEXt",
    0x7a545874 = "zTXt",
    0x69545874 = "iTXt",
    0x67414d41 = "gAMA",
    0x63485242 = "cHRM",
    0x73524742 = "sRGB",
    0x69434350 = "iCCP",
    0x624b4744 = "bKGD",
    0x70485973 = "pHYs",
    0x74494d45 = "tIME",
    0x74524e53 = "tRNS",
    0x61637454 = "acTL",
    0x6663544c = "fcTL",
    0x66644154 = "fdAT",
}

enum color_type : u8 {
    0 = "grayscale",
    2 = "truecolor",
    3 = "indexed",
    4 = "grayscale + alpha",
    6 = "truecolor + alpha",
}

enum interlace_method : u8 {
    0 = "none",
    1 = "Adam7",
}

struct png_file {
    signature: u8[8];
    chunks: chunk[];
}

struct chunk {
    length: be u32;
    type: be chunk_type;
    data: match type {
        0x49484452 = ihdr,
        0x70485973 = phys,
        0x74494d45 = time,
        0x61637454 = actl,
        _ = raw_chunk,
    } span length;
    crc: be u32;
}

struct raw_chunk {
    data: u8[];
}

struct ihdr {
    width: be u32;
    height: be u32;
    bit_depth: u8;
    color: color_type;
    compression: u8;
    filter: u8;
    interlace: interlace_method;
}

// physical pixel dimensions
struct phys {
    pixels_per_unit_x: be u32;
    pixels_per_unit_y: be u32;
    unit: u8;
}

struct time {
    year: be u16;
    month: u8;
    day: u8;
    hour: u8;
    minute: u8;
    second: u8;
}

// APNG animation control
struct actl {
    num_frames: be u32;
    num_plays: be u32;
}
