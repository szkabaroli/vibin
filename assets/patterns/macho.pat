// Mach-O: 64-bit little-endian binaries (arm64 / x86-64 executables,
// dylibs, .o) plus fat/universal wrappers, whose header is big-endian.
// (Java .class files share the fat magic "ca fe ba be" — known ambiguity.)
// See src/pattern.rs for the pattern language reference.

format macho {
    magic = "cf fa ed fe";
    root = macho_file;
}

format macho_fat {
    magic = "ca fe ba be";
    root = fat_file;
}

struct fat_file {
    header: be fat_header;
    archs: be fat_arch[nfat_arch];
}

struct fat_header {
    magic: u32;
    nfat_arch: u32;
}

struct fat_arch {
    cputype: cpu_type;
    cpusubtype: u32;
    offset: u32;
    size: u32;
    align: u32;
    slice: u8[size] @ offset;
}

enum cpu_type : u32 {
    0x01000007 = "x86-64",
    0x0100000c = "arm64",
}

enum file_type : u32 {
    1 = "OBJECT",
    2 = "EXECUTE",
    4 = "CORE",
    6 = "DYLIB",
    8 = "BUNDLE",
    10 = "DSYM",
    11 = "KEXT",
}

enum lc_type : u32 {
    0x02 = "LC_SYMTAB",
    0x0b = "LC_DYSYMTAB",
    0x0c = "LC_LOAD_DYLIB",
    0x0d = "LC_ID_DYLIB",
    0x0e = "LC_LOAD_DYLINKER",
    0x19 = "LC_SEGMENT_64",
    0x1b = "LC_UUID",
    0x1d = "LC_CODE_SIGNATURE",
    0x24 = "LC_VERSION_MIN_MACOS",
    0x26 = "LC_FUNCTION_STARTS",
    0x29 = "LC_DATA_IN_CODE",
    0x2a = "LC_SOURCE_VERSION",
    0x32 = "LC_BUILD_VERSION",
    0x8000001c = "LC_RPATH",
    0x80000022 = "LC_DYLD_INFO_ONLY",
    0x80000028 = "LC_MAIN",
    0x80000034 = "LC_DYLD_EXPORTS_TRIE",
    0x80000035 = "LC_DYLD_CHAINED_FIXUPS",
}

struct macho_file {
    header: macho_header;
    commands: load_command[ncmds];
}

flags mh_flags : u32 {
    0x1 = "NOUNDEFS",
    0x4 = "DYLDLINK",
    0x80 = "TWOLEVEL",
    0x8000 = "WEAK_DEFINES",
    0x10000 = "BINDS_TO_WEAK",
    0x200000 = "PIE",
    0x800000 = "HAS_TLV_DESCRIPTORS",
}

struct macho_header {
    magic: u32;
    cputype: cpu_type;
    cpusubtype: u32;
    filetype: file_type;
    ncmds: u32;
    sizeofcmds: u32;
    flags: mh_flags;
    reserved: u32;
}

// cmdsize covers the whole command including cmd/cmdsize themselves
struct load_command {
    cmd: lc_type;
    cmdsize: u32;
    body: match cmd {
        0x19 = segment64,
        0x02 = symtab,
        0x1b = uuid_cmd,
        0x80000028 = main_cmd,
        _ = raw_command,
    } span cmdsize - 8;
}

struct raw_command {
    data: u8[];
}

struct segment64 {
    segname: char[16];
    vmaddr: u64;
    vmsize: u64;
    fileoff: u64;
    filesize: u64;
    maxprot: u32;
    initprot: u32;
    nsects: u32;
    seg_flags: u32;
    sections: section64[nsects];
}

struct section64 {
    sectname: char[16];
    segname: char[16];
    addr: u64;
    size: u64;
    offset: u32;
    align: u32;
    reloff: u32;
    nreloc: u32;
    flags: u32;
    reserved1: u32;
    reserved2: u32;
    reserved3: u32;
}

struct symtab {
    symoff: u32;
    nsyms: u32;
    stroff: u32;
    strsize: u32;
}

struct uuid_cmd {
    uuid: u8[16];
}

struct main_cmd {
    entryoff: u64;
    stacksize: u64;
}
