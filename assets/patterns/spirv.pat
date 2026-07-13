// SPIR-V shader modules (Vulkan / WebGPU) — a 5-word header followed by a
// stream of instructions. Each instruction's first word packs the opcode
// (low 16 bits) and total word count (high 16 bits); reading them as two
// LE u16s lets instructions name themselves via the opcode enum.
// Big-endian SPIR-V (rare) is not covered. See src/pattern.rs.

format spirv {
    magic = "03 02 23 07"; // 0x07230203 little-endian
    root = spirv_file;
}

enum spirv_op : u16 {
    0 = "OpNop",
    3 = "OpSource",
    5 = "OpName",
    6 = "OpMemberName",
    11 = "OpExtInstImport",
    14 = "OpMemoryModel",
    15 = "OpEntryPoint",
    16 = "OpExecutionMode",
    17 = "OpCapability",
    19 = "OpTypeVoid",
    20 = "OpTypeBool",
    21 = "OpTypeInt",
    22 = "OpTypeFloat",
    23 = "OpTypeVector",
    24 = "OpTypeMatrix",
    25 = "OpTypeImage",
    26 = "OpTypeSampler",
    28 = "OpTypeArray",
    30 = "OpTypeStruct",
    32 = "OpTypePointer",
    33 = "OpTypeFunction",
    43 = "OpConstant",
    44 = "OpConstantComposite",
    54 = "OpFunction",
    55 = "OpFunctionParameter",
    56 = "OpFunctionEnd",
    57 = "OpFunctionCall",
    59 = "OpVariable",
    61 = "OpLoad",
    62 = "OpStore",
    65 = "OpAccessChain",
    71 = "OpDecorate",
    72 = "OpMemberDecorate",
    248 = "OpLabel",
    249 = "OpBranch",
    253 = "OpReturn",
    254 = "OpReturnValue",
}

struct spirv_file {
    magic: u32;
    version: u32;
    generator: u32;
    id_bound: u32;
    schema: u32;
    instructions: instruction[];
}

struct instruction {
    op: spirv_op;
    word_count: u16;
    operands: u8[(word_count - 1) * 4];
}
