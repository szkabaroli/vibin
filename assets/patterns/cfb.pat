// OLE2 / Compound File Binary — the container behind legacy MS Office
// (.doc .xls .ppt), Windows Installer (.msi), and Outlook (.msg). It is a
// mini-FAT filesystem: sectors chained through allocation tables, with a
// red-black-tree directory of storages and streams. A forward-only parser
// can't follow the chains, so we decode the 512-byte header in full and
// preview the first directory sector (located via the header's pointers) —
// enough to show the version, sector layout, and the Root Entry plus the
// top-level streams. Stream contents and later directory sectors are not
// followed. See src/pattern.rs for the pattern language reference.

format cfb {
    magic = "d0 cf 11 e0 a1 b1 1a e1";
    root = cfb_file;
}

enum cfb_version : u16 {
    3 = "v3 (512-byte sectors)",
    4 = "v4 (4096-byte sectors)",
}

enum entry_type : u8 {
    0 = "unused",
    1 = "storage",
    2 = "stream",
    5 = "root storage",
}

struct cfb_file {
    header: cfb_header;
    // directory sector offset = (first_dir_sector + 1) * sector_size
    directory: directory_sector @ (first_dir_sector + 1) << sector_shift;
}

struct cfb_header {
    signature: BYTE[8];
    clsid: GUID;
    minor_version: WORD;
    major_version: cfb_version;
    byte_order: WORD;
    sector_shift: WORD;
    mini_sector_shift: WORD;
    reserved: BYTE[6];
    num_dir_sectors: DWORD;
    num_fat_sectors: DWORD;
    first_dir_sector: DWORD;
    transaction_signature: DWORD;
    mini_stream_cutoff: DWORD;
    first_minifat_sector: DWORD;
    num_minifat_sectors: DWORD;
    first_difat_sector: DWORD;
    num_difat_sectors: DWORD;
    difat: DWORD[109];
}

// one sector's worth of 128-byte directory entries (sector_size / 128)
struct directory_sector {
    entries: dir_entry[(1 << sector_shift) / 128];
}

struct dir_entry {
    name: char[64]; // UTF-16, shown with · between code units
    name_length: WORD;
    object_type: entry_type;
    color: BYTE;
    left_sibling: DWORD;
    right_sibling: DWORD;
    child: DWORD;
    clsid: GUID;
    state_bits: DWORD;
    create_time: FILETIME;
    modify_time: FILETIME;
    start_sector: DWORD;
    stream_size: QWORD;
}
