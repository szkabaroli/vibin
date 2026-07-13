// DirectX shader containers ("DXBC") — the wrapper for both legacy DXBC
// (SM 4/5 SHDR/SHEX bytecode) and modern DXIL (SM 6+, LLVM bitcode from
// dxc). FourCC part tags are read as LE u32 enums so parts name
// themselves. See src/pattern.rs for the pattern language reference.

format dxbc {
    magic = "44 58 42 43"; // "DXBC"
    root = dxbc_file;
}

enum part_type : u32 {
    0x46454452 = "RDEF (resource definitions)",
    0x4e475349 = "ISGN (input signature)",
    0x31475349 = "ISG1 (input signature)",
    0x4e47534f = "OSGN (output signature)",
    0x3147534f = "OSG1 (output signature)",
    0x31475350 = "PSG1 (patch signature)",
    0x52444853 = "SHDR (SM4/5 bytecode)",
    0x58454853 = "SHEX (SM5 bytecode)",
    0x4c495844 = "DXIL (SM6 program)",
    0x30565350 = "PSV0 (pipeline validation)",
    0x54415453 = "STAT (statistics)",
    0x48534148 = "HASH",
    0x4e444c49 = "ILDN (debug name)",
    0x42444c49 = "ILDB (debug bitcode)",
    0x30535452 = "RTS0 (root signature)",
    0x30494653 = "SFI0 (feature info)",
    0x54414452 = "RDAT (runtime data)",
}

struct dxbc_file {
    fourcc: char[4];
    hash: u8[16];
    version_major: u16;
    version_minor: u16;
    container_size: u32;
    part_count: u32;
    part_offsets: u32[part_count];
    parts: part[part_count];
}

struct part {
    type: part_type;
    size: u32;
    body: match type {
        0x4c495844 = dxil_program,
        _ = raw_part,
    } span size;
}

struct raw_part {
    data: u8[];
}

// DXIL program header, then LLVM bitcode ("BC\xc0\xde")
struct dxil_program {
    program_version: u32;
    size_in_dwords: u32;
    dxil_magic: char[4];
    dxil_version: u32;
    bitcode_offset: u32;
    bitcode_size: u32;
    bitcode: u8[bitcode_size];
}
