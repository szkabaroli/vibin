// JPEG/JFIF images — a chain of 0xFF-marker segments with big-endian
// lengths. After the SOS header the entropy-coded scan has no length
// field; it runs to the end of the file (swallowing the EOI marker).
// See src/pattern.rs for the pattern language reference.

format jpeg {
    magic = "ff d8 ff";
    root = jpeg_file;
}

enum marker : u8 {
    0x01 = "TEM",
    0xc0 = "SOF0 (baseline)",
    0xc1 = "SOF1",
    0xc2 = "SOF2 (progressive)",
    0xc4 = "DHT",
    0xd8 = "SOI",
    0xd9 = "EOI",
    0xda = "SOS",
    0xdb = "DQT",
    0xdd = "DRI",
    0xe0 = "APP0 (JFIF)",
    0xe1 = "APP1 (Exif)",
    0xe2 = "APP2 (ICC)",
    0xee = "APP14 (Adobe)",
    0xfe = "COM",
}

struct jpeg_file {
    segments: segment[];
}

struct segment {
    ff: u8;
    type: marker;
    body: match type {
        0xd8 = empty,
        0xd9 = empty,
        0x01 = empty,
        0xc0 = sof,
        0xc1 = sof,
        0xc2 = sof,
        0xe0 = app0,
        0xda = sos,
        _ = generic,
    };
}

struct empty {
}

// any length-prefixed segment we don't decode further
struct generic {
    length: be u16;
    data: u8[] span length - 2;
}

struct app0 {
    length: be u16;
    payload: app0_body span length - 2;
}

struct app0_body {
    identifier: char[5];
    version_major: u8;
    version_minor: u8;
    density_units: u8;
    x_density: be u16;
    y_density: be u16;
    thumb_width: u8;
    thumb_height: u8;
    thumbnail: u8[];
}

// start of frame: image dimensions and per-component sampling
struct sof {
    length: be u16;
    payload: sof_body span length - 2;
}

struct sof_body {
    precision: u8;
    height: be u16;
    width: be u16;
    component_count: u8;
    components: component[component_count];
}

struct component {
    id: u8;
    sampling: u8;
    quant_table: u8;
}

// start of scan: header, then entropy-coded data to end of file
struct sos {
    length: be u16;
    header: u8[] span length - 2;
    entropy_data: u8[];
}
