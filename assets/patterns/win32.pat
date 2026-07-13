// Shared prelude — win32-style type aliases and common structs, in the
// Prepended to every pattern, so any
// format (PE, CFB, LNK, registry hives…) can use these friendly names.
// Note: the language has only unsigned scalars, so signed Windows types
// (LONG, INT) alias to their unsigned width and display unsigned.
// See src/pattern.rs for the pattern language reference.

type BYTE = u8;
type CHAR = u8;
type UCHAR = u8;
type BOOLEAN = u8;

type WORD = u16;
type USHORT = u16;
type SHORT = u16;
type WCHAR = u16;
type ATOM = u16;
type LANGID = u16;

type DWORD = u32;
type DWORD32 = u32;
type ULONG = u32;
type LONG = u32;
type UINT = u32;
type INT = u32;
type BOOL = u32;
type COLORREF = u32;
type LCID = u32;

type QWORD = u64;
type DWORD64 = u64;
type ULONG64 = u64;
type ULONGLONG = u64;
type LONGLONG = u64;

// {00112233-4455-6677-8899-aabbccddeeff}
struct GUID {
    data1: DWORD;
    data2: WORD;
    data3: WORD;
    data4: BYTE[8];
}

// 100-nanosecond intervals since 1601; split across two DWORDs
struct FILETIME {
    low: DWORD;
    high: DWORD;
}

struct SYSTEMTIME {
    year: WORD;
    month: WORD;
    day_of_week: WORD;
    day: WORD;
    hour: WORD;
    minute: WORD;
    second: WORD;
    milliseconds: WORD;
}
