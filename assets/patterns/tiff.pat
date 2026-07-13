// TIFF and TIFF-based raw images (Sony ARW, Nikon NEF, Adobe DNG, plain
// .tif). Little-endian ("II") only — big-endian "MM" TIFF (some Nikon /
// legacy Mac) is not covered. Header points at the first IFD, a table of
// 12-byte tag entries. IFD chaining and the Exif sub-IFD (via the ExifIFD
// tag's offset) are not followed. See src/pattern.rs for the language.

format tiff {
    magic = "49 49 2a 00"; // "II" + 42
    root = tiff_file;
}

enum tiff_type : u16 {
    1 = "BYTE",
    2 = "ASCII",
    3 = "SHORT",
    4 = "LONG",
    5 = "RATIONAL",
    6 = "SBYTE",
    7 = "UNDEFINED",
    8 = "SSHORT",
    9 = "SLONG",
    10 = "SRATIONAL",
    11 = "FLOAT",
    12 = "DOUBLE",
    13 = "IFD",
}

enum tiff_tag : u16 {
    0x0100 = "ImageWidth",
    0x0101 = "ImageLength",
    0x0102 = "BitsPerSample",
    0x0103 = "Compression",
    0x0106 = "PhotometricInterpretation",
    0x010e = "ImageDescription",
    0x010f = "Make",
    0x0110 = "Model",
    0x0111 = "StripOffsets",
    0x0112 = "Orientation",
    0x0115 = "SamplesPerPixel",
    0x0116 = "RowsPerStrip",
    0x0117 = "StripByteCounts",
    0x011a = "XResolution",
    0x011b = "YResolution",
    0x0128 = "ResolutionUnit",
    0x0131 = "Software",
    0x0132 = "DateTime",
    0x014a = "SubIFDs",
    0x828e = "CFAPattern",
    0x8769 = "ExifIFD",
    0x8825 = "GPSInfo",
    0xc612 = "DNGVersion",
    0xc614 = "UniqueCameraModel",
}

struct tiff_file {
    byte_order: char[2];
    magic: u16;
    first_ifd_offset: u32;
    ifd: ifd @ first_ifd_offset;
}

struct ifd {
    entry_count: u16;
    entries: ifd_entry[entry_count];
    next_ifd_offset: u32;
}

struct ifd_entry {
    tag: tiff_tag;
    type: tiff_type;
    count: u32;
    value_or_offset: u32;
}
