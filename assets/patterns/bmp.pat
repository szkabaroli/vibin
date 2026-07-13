// BMP / DIB bitmaps (Windows). Little-endian: a 14-byte file header, a
// BITMAPINFOHEADER (or larger variant), then the pixel array located by
// the file header's data offset. width/height are signed (negative height
// = top-down) but shown unsigned. See src/pattern.rs for the language.

format bmp {
    magic = "42 4d"; // "BM"
    root = bmp_file;
}

enum compression : u32 {
    0 = "BI_RGB (none)",
    1 = "BI_RLE8",
    2 = "BI_RLE4",
    3 = "BI_BITFIELDS",
    4 = "BI_JPEG",
    5 = "BI_PNG",
}

struct bmp_file {
    header: file_header;
    info: info_header;
    pixels: u8[image_size] @ data_offset;
}

struct file_header {
    signature: char[2];
    file_size: u32;
    reserved1: u16;
    reserved2: u16;
    data_offset: u32;
}

struct info_header {
    header_size: u32;
    width: u32;
    height: u32;
    planes: u16;
    bits_per_pixel: u16;
    compression: compression;
    image_size: u32;
    x_pixels_per_meter: u32;
    y_pixels_per_meter: u32;
    colors_used: u32;
    colors_important: u32;
}
