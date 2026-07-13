// iPod iTunesDB (/iPod_Control/iTunes/iTunesDB) and Genius/sync .itdb
// files. A recursive tree of chunks, each tagged with a 4-char "mh" code
// and carrying its own header_length and total_length, so one generic
// container walks the whole format: fixed header bytes up to
// header_length, then child chunks fill the rest of total_length. Fields
// are little-endian; the tag is read big-endian so the enum value matches
// the ASCII. Strings inside mhod objects are UTF-16 and shown raw.
// (Modern Music.app .musicdb / .itl are encrypted and not covered.)
// See src/pattern.rs for the pattern language reference.

format itunesdb {
    magic = "6d 68 62 64"; // "mhbd"
    root = chunk;
}

enum chunk_type : u32 {
    0x6d686264 = "mhbd (database)",
    0x6d687364 = "mhsd (dataset)",
    0x6d686c74 = "mhlt (track list)",
    0x6d686974 = "mhit (track item)",
    0x6d686f64 = "mhod (data object)",
    0x6d686c70 = "mhlp (playlist list)",
    0x6d687970 = "mhyp (playlist)",
    0x6d686970 = "mhip (playlist item)",
    0x6d686c61 = "mhla (album list)",
    0x6d686961 = "mhia (album item)",
}

enum mhod_type : u32 {
    1 = "title",
    2 = "location (path)",
    3 = "album",
    4 = "artist",
    5 = "genre",
    6 = "filetype",
    7 = "EQ setting",
    8 = "comment",
    9 = "category",
    12 = "composer",
    13 = "grouping",
    14 = "description",
    15 = "podcast enclosure URL",
    16 = "podcast RSS URL",
    18 = "subtitle",
    50 = "smart playlist rules",
    51 = "smart playlist data",
    52 = "library playlist index",
    100 = "column info (playlist item)",
}

struct chunk {
    tag: be chunk_type;
    header_length: u32;
    total_length: u32;
    body: match tag {
        0x6d686f64 = data_object, // mhod is a leaf
        _ = container,
    } span total_length - 12;
}

// generic node: type-specific header bytes, then nested child chunks that
// fill the remainder of this chunk's total_length
struct container {
    header_fields: u8[header_length - 12];
    children: chunk[];
}

struct data_object {
    subtype: mhod_type;
    data: u8[];
}
