// tar archives (POSIX ustar / GNU). 512-byte blocks: each entry is a
// header block followed by the file data rounded up to whole blocks.
// Numbers are fixed-width ASCII octal (`octal[n]`). The archive ends with
// two zero blocks, which parse as empty entries. Magic "ustar" sits at
// offset 257 inside the first header. See src/pattern.rs for the language.

format tar {
    magic = "75 73 74 61 72" @ 257; // "ustar"
    root = tar_file;
}

enum entry_type : u8 {
    0 = "end marker",
    0x30 = "file",
    0x31 = "hard link",
    0x32 = "symlink",
    0x33 = "char device",
    0x34 = "block device",
    0x35 = "directory",
    0x36 = "fifo",
    0x37 = "contiguous",
    0x4b = "GNU long link name",
    0x4c = "GNU long file name",
    0x67 = "pax global header",
    0x78 = "pax extended header",
}

struct tar_file {
    entries: entry[];
}

struct entry {
    header: tar_header span 512;
    // data padded to whole 512-byte blocks (left-to-right evaluation:
    // ((size + 511) / 512) * 512)
    data: u8[] span size + 511 / 512 * 512;
}

struct tar_header {
    name: char[100];
    mode: octal[8];
    uid: octal[8];
    gid: octal[8];
    size: octal[12];
    mtime: octal[12];
    checksum: octal[8];
    type: entry_type;
    linkname: char[100];
    ustar: char[6];
    version: char[2];
    uname: char[32];
    gname: char[32];
    devmajor: octal[8];
    devminor: octal[8];
    prefix: char[155];
}
