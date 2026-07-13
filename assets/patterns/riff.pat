// RIFF containers — WAV (audio), AVI (video), WebP (image), ANI (animated
// cursor). Little-endian chunks: a 4-char id, a u32 size, then that many
// data bytes padded to an even count (word alignment). LIST chunks nest a
// list-type and sub-chunks. The id/type fourccs are read big-endian so the
// enum values match the ASCII. Big-endian "RIFX" is not covered.
// See src/pattern.rs for the pattern language reference.

format riff {
    magic = "52 49 46 46"; // "RIFF"
    root = riff_file;
}

enum fourcc : u32 {
    0x57415645 = "WAVE (audio)",
    0x41564920 = "AVI (video)",
    0x57454250 = "WEBP (image)",
    0x41434f4e = "ACON (animated cursor)",
    0x666d7420 = "fmt (format)",
    0x64617461 = "data (samples)",
    0x4c495354 = "LIST",
    0x66616374 = "fact",
    0x494e464f = "INFO",
    0x4a554e4b = "JUNK (padding)",
    0x63756520 = "cue points",
    0x62657874 = "bext (broadcast)",
    0x6864726c = "hdrl (header list)",
    0x61766968 = "avih (AVI header)",
    0x7374726c = "strl (stream list)",
    0x73747268 = "strh (stream header)",
    0x73747266 = "strf (stream format)",
    0x6d6f7669 = "movi (movie data)",
    0x69647831 = "idx1 (index)",
    0x56503820 = "VP8 (lossy)",
    0x5650384c = "VP8L (lossless)",
    0x56503858 = "VP8X (extended)",
}

enum wav_format : u16 {
    1 = "PCM",
    3 = "IEEE float",
    6 = "A-law",
    7 = "mu-law",
    0xfffe = "extensible",
}

struct riff_file {
    id: char[4];
    size: u32;
    form_type: be fourcc;
    chunks: chunk[];
}

struct chunk {
    id: be fourcc;
    size: u32;
    body: match id {
        0x4c495354 = list_chunk, // "LIST"
        0x666d7420 = fmt_chunk,  // "fmt "
        _ = raw_chunk,
    } span size;
    // chunks are word-aligned: odd sizes carry one pad byte
    pad: u8[size & 1];
}

struct list_chunk {
    list_type: be fourcc;
    children: chunk[];
}

struct fmt_chunk {
    audio_format: wav_format;
    channels: u16;
    sample_rate: u32;
    byte_rate: u32;
    block_align: u16;
    bits_per_sample: u16;
    extra: u8[];
}

struct raw_chunk {
    data: u8[];
}
