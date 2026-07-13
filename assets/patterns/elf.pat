// ELF64 (little-endian) — executables and shared objects.
// See src/pattern.rs for the pattern language reference.

format elf {
    magic = "7f 45 4c 46";
    root = elf_file;
}

enum e_type : u16 {
    0 = "NONE",
    1 = "REL",
    2 = "EXEC",
    3 = "DYN (shared object / PIE)",
    4 = "CORE",
}

enum e_machine : u16 {
    0x03 = "x86",
    0x28 = "ARM",
    0x3e = "x86-64",
    0xb7 = "aarch64",
    0xf3 = "RISC-V",
}

enum p_type : u32 {
    0 = "NULL",
    1 = "LOAD",
    2 = "DYNAMIC",
    3 = "INTERP",
    4 = "NOTE",
    6 = "PHDR",
    7 = "TLS",
}

struct elf_file {
    header: elf_header;
    phdrs: phdr[phnum] @ phoff;
    shdrs: shdr[shnum] @ shoff;
}

struct elf_header {
    magic: char[4];
    class: u8;
    endianness: u8;
    ei_version: u8;
    abi: u8;
    abi_version: u8;
    pad: u8[7];
    type: e_type;
    machine: e_machine;
    version: u32;
    entry: u64;
    phoff: u64;
    shoff: u64;
    flags: u32;
    ehsize: u16;
    phentsize: u16;
    phnum: u16;
    shentsize: u16;
    shnum: u16;
    shstrndx: u16;
}

flags p_flags : u32 {
    1 = "X",
    2 = "W",
    4 = "R",
}

struct phdr {
    type: p_type;
    flags: p_flags;
    offset: u64;
    vaddr: u64;
    paddr: u64;
    filesz: u64;
    memsz: u64;
    align: u64;
}

struct shdr {
    name: u32;
    type: u32;
    flags: u64;
    addr: u64;
    offset: u64;
    size: u64;
    link: u32;
    info: u32;
    addralign: u64;
    entsize: u64;
}
