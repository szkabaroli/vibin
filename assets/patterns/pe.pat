// PE executables and DLLs (Windows) — MZ DOS stub, e_lfanew jump to the
// NT headers, COFF + optional header (PE32 / PE32+ selected by its magic),
// data directories, then the section table with each section claiming its
// raw bytes. See src/pattern.rs for the pattern language reference.

format pe {
    magic = "4d 5a"; // "MZ"
    root = pe_file;
}

enum machine : u16 {
    0x014c = "x86",
    0x0166 = "MIPS",
    0x01c0 = "ARM",
    0x01c4 = "ARMNT",
    0x0200 = "IA64",
    0x5032 = "RISC-V 32",
    0x5064 = "RISC-V 64",
    0x8664 = "x86-64",
    0xaa64 = "ARM64",
}

flags coff_characteristics : u16 {
    0x0002 = "EXECUTABLE_IMAGE",
    0x0020 = "LARGE_ADDRESS_AWARE",
    0x0100 = "32BIT_MACHINE",
    0x0200 = "DEBUG_STRIPPED",
    0x2000 = "DLL",
}

enum opt_magic : u16 {
    0x010b = "PE32",
    0x020b = "PE32+",
    0x0107 = "ROM",
}

enum subsystem : u16 {
    1 = "native",
    2 = "Windows GUI",
    3 = "Windows console",
    7 = "POSIX console",
    9 = "Windows CE GUI",
    10 = "EFI application",
    11 = "EFI boot driver",
    12 = "EFI runtime driver",
    14 = "Xbox",
    16 = "boot application",
}

flags dll_characteristics : u16 {
    0x0020 = "HIGH_ENTROPY_VA",
    0x0040 = "DYNAMIC_BASE",
    0x0080 = "FORCE_INTEGRITY",
    0x0100 = "NX_COMPAT",
    0x0400 = "NO_SEH",
    0x1000 = "APPCONTAINER",
    0x4000 = "GUARD_CF",
}

flags section_characteristics : u32 {
    0x00000020 = "CODE",
    0x00000040 = "INITIALIZED_DATA",
    0x00000080 = "UNINITIALIZED_DATA",
    0x02000000 = "DISCARDABLE",
    0x10000000 = "SHARED",
    0x20000000 = "EXECUTE",
    0x40000000 = "READ",
    0x80000000 = "WRITE",
}

struct pe_file {
    dos: dos_header;
    nt: nt_headers @ e_lfanew;
}

struct dos_header {
    magic: char[2];
    dos_fields: u8[58];
    e_lfanew: u32;
}

struct nt_headers {
    signature: char[4]; // "PE\0\0"
    coff: coff_header;
    optional: optional_header span size_of_optional;
    sections: section[num_sections];
}

struct coff_header {
    machine: machine;
    num_sections: u16;
    timestamp: u32;
    symbol_table_ptr: u32;
    num_symbols: u32;
    size_of_optional: u16;
    characteristics: coff_characteristics;
}

struct optional_header {
    magic: opt_magic;
    body: match magic {
        0x010b = optional32,
        0x020b = optional64,
        _ = raw_opt,
    };
}

struct raw_opt {
    data: u8[];
}

struct optional64 {
    linker_major: u8;
    linker_minor: u8;
    size_of_code: u32;
    size_of_initialized_data: u32;
    size_of_uninitialized_data: u32;
    entry_point: u32;
    base_of_code: u32;
    image_base: u64;
    section_alignment: u32;
    file_alignment: u32;
    os_major: u16;
    os_minor: u16;
    image_major: u16;
    image_minor: u16;
    subsystem_major: u16;
    subsystem_minor: u16;
    win32_version: u32;
    size_of_image: u32;
    size_of_headers: u32;
    checksum: u32;
    subsystem: subsystem;
    dll_flags: dll_characteristics;
    stack_reserve: u64;
    stack_commit: u64;
    heap_reserve: u64;
    heap_commit: u64;
    loader_flags: u32;
    num_data_directories: u32;
    data_directories: data_directory[num_data_directories];
}

struct optional32 {
    linker_major: u8;
    linker_minor: u8;
    size_of_code: u32;
    size_of_initialized_data: u32;
    size_of_uninitialized_data: u32;
    entry_point: u32;
    base_of_code: u32;
    base_of_data: u32;
    image_base: u32;
    section_alignment: u32;
    file_alignment: u32;
    os_major: u16;
    os_minor: u16;
    image_major: u16;
    image_minor: u16;
    subsystem_major: u16;
    subsystem_minor: u16;
    win32_version: u32;
    size_of_image: u32;
    size_of_headers: u32;
    checksum: u32;
    subsystem: subsystem;
    dll_flags: dll_characteristics;
    stack_reserve: u32;
    stack_commit: u32;
    heap_reserve: u32;
    heap_commit: u32;
    loader_flags: u32;
    num_data_directories: u32;
    data_directories: data_directory[num_data_directories];
}

// export, import, resource, … tables in index order
struct data_directory {
    virtual_address: u32;
    size: u32;
}

struct section {
    name: char[8];
    virtual_size: u32;
    virtual_address: u32;
    size_of_raw_data: u32;
    raw_data_ptr: u32;
    relocations_ptr: u32;
    line_numbers_ptr: u32;
    num_relocations: u16;
    num_line_numbers: u16;
    characteristics: section_characteristics;
    contents: u8[size_of_raw_data] @ raw_data_ptr;
}
