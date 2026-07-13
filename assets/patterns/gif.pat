// GIF images (87a and 89a). Little-endian. The color table sizes are
// computed from bit fields of the packed byte: presence in bit 7, size N
// in bits 0-2, table length 3 * 2^(N+1) = 6 << N bytes. Frame pixel data
// and extension payloads are chains of size-prefixed sub-blocks ended by
// a zero byte. See src/pattern.rs for the pattern language reference.

format gif89 {
    magic = "47 49 46 38 39 61"; // "GIF89a"
    root = gif_file;
}

format gif87 {
    magic = "47 49 46 38 37 61"; // "GIF87a"
    root = gif_file;
}

enum block_type : u8 {
    0x2c = "image",
    0x21 = "extension",
    0x3b = "trailer",
}

enum extension_label : u8 {
    0x01 = "plain text",
    0xf9 = "graphics control",
    0xfe = "comment",
    0xff = "application",
}

struct gif_file {
    signature: char[6];
    width: u16;
    height: u16;
    packed: u8;
    background_color: u8;
    aspect_ratio: u8;
    global_color_table: u8[(packed / 128) * (6 << (packed & 7))];
    blocks: block[] until 0x3b;
    trailer: block_type;
}

struct block {
    introducer: block_type;
    body: match introducer {
        0x2c = image,
        0x21 = extension,
        _ = rest,
    };
}

struct rest {
    data: u8[];
}

struct image {
    left: u16;
    top: u16;
    width: u16;
    height: u16;
    packed: u8;
    local_color_table: u8[(packed / 128) * (6 << (packed & 7))];
    lzw_min_code_size: u8;
    data: sub_block[] until 0;
    terminator: u8;
}

struct extension {
    label: extension_label;
    data: sub_block[] until 0;
    terminator: u8;
}

struct sub_block {
    size: u8;
    data: u8[size];
}
