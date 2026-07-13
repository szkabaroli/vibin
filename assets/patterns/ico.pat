// Windows icon (.ico) and cursor (.cur) files. Little-endian: a directory
// header, then one entry per image; each entry points at its image bytes,
// which are either a headerless BMP (DIB) or a whole PNG. We claim the
// image bytes as a blob — the PNG magic is visible in the dump when
// present. width/height of 0 mean 256. See src/pattern.rs for the language.

format ico {
    magic = "00 00 01 00"; // reserved 0, type 1 (icon)
    root = ico_file;
}

format cur {
    magic = "00 00 02 00"; // reserved 0, type 2 (cursor)
    root = ico_file;
}

enum image_type : u16 {
    1 = "icon",
    2 = "cursor",
}

struct ico_file {
    reserved: u16;
    type: image_type;
    count: u16;
    entries: icon_entry[count];
}

struct icon_entry {
    width: u8;
    height: u8;
    color_count: u8;
    reserved: u8;
    planes: u16;
    bits_per_pixel: u16;
    image_size: u32;
    image_offset: u32;
    image: u8[image_size] @ image_offset;
}
