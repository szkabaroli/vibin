//! A small declarative pattern language for describing binary formats,
//! interpreted at load time. Formats are shipped as
//! `assets/patterns/*.pat` text files and interpreted against file bytes
//! to produce the hex viewer's structure tree — adding ELF/PE/… support
//! means writing a pattern, not Rust.
//!
//! ```text
//! // comments
//! format elf {
//!     magic = "7f 45 4c 46";       // dispatch bytes at offset 0
//!     root  = elf_file;            // struct parsed at offset 0
//! }
//!
//! enum machine : u16 {             // scalars can name their values
//!     0x3e = "x86-64",
//! }
//!
//! struct elf_file {
//!     header: elf_header;
//!     phdrs:  phdr[phnum] @ phoff; // count/offset reference prior fields
//! }
//!
//! struct elf_header {
//!     ident:   char[4];
//!     machine: machine;            // enum field
//!     phoff:   u64;                // scalars: u8 u16 u32 u64
//!     count:   be u32;             // be/le prefix; default is LE and the
//!                                  // prefix propagates into nested types
//!     pad:     u8[7];              // byte arrays preview as hex
//! }
//! ```
//!
//! Also available: `flags name : u32 { 0x1 = "X", ... }` (bit-set decode),
//! `leb128` and `lstr` (LEB128 varint / length-prefixed string), `type[]`
//! (repeat until the region ends), `match tag { 0 = a, _ = b } span size`
//! (tag-selected variants inside an exactly-sized region).
//!
//! Fields parse sequentially; `@ expr` parses at an absolute offset without
//! moving the cursor. Expressions are `ident | literal` combined with
//! `+ - *`. Field references see scalars already parsed in the enclosing
//! struct, then in ancestor scopes.

use crate::hex::HexNode;
use std::collections::HashMap;

// ----- AST -----------------------------------------------------------------

/// Byte order for scalar reads. Fields default to little-endian; a
/// `be`/`le` prefix overrides and propagates into nested types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endian {
    Little,
    Big,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scalar {
    U8,
    U16,
    U32,
    U64,
}

impl Scalar {
    fn size(&self) -> usize {
        match self {
            Scalar::U8 => 1,
            Scalar::U16 => 2,
            Scalar::U32 => 4,
            Scalar::U64 => 8,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Scalar::U8 => "u8",
            Scalar::U16 => "u16",
            Scalar::U32 => "u32",
            Scalar::U64 => "u64",
        }
    }

    fn read(&self, data: &[u8], pos: usize, endian: Endian) -> Option<u64> {
        let bytes = data.get(pos..pos + self.size())?;
        let mut v: u64 = 0;
        for (i, b) in bytes.iter().enumerate() {
            let shift = match endian {
                Endian::Little => 8 * i,
                Endian::Big => 8 * (self.size() - 1 - i),
            };
            v |= u64::from(*b) << shift;
        }
        Some(v)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldType {
    Scalar(Scalar),
    /// `char[n]`: fixed-size string.
    Char,
    /// Unsigned LEB128 (wasm-style variable-length integer).
    Leb128,
    /// `octal[n]`: an ASCII octal number in a fixed-width field (tar).
    Octal,
    /// LEB128 length-prefixed UTF-8 string.
    LStr,
    /// ASN.1 DER length: 1 byte if < 0x80, else a big-endian run.
    DerLen,
    Struct(String),
    Enum(String),
    /// Bit-set scalar: decodes as the names of its set bits.
    Flags(String),
    /// `match expr { 0 = some_struct, _ = fallback }`: variant selected by
    /// the value of an expression over already-parsed fields.
    Match {
        on: Expr,
        arms: Vec<(Option<u64>, String)>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Lit(u64),
    Field(String),
    Add(Box<Expr>, Box<Expr>),
    Sub(Box<Expr>, Box<Expr>),
    Mul(Box<Expr>, Box<Expr>),
    Div(Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Shl(Box<Expr>, Box<Expr>),
    Shr(Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub ty: FieldType,
    /// `[expr]` — array length (chars/bytes: size; structs: element count).
    pub count: Option<Expr>,
    /// `[]` — repeat until the end of the enclosing region.
    pub open_array: bool,
    /// `[] until N` — stop before an element starting with this byte.
    pub until_byte: Option<u8>,
    /// `@ expr` — parse at this absolute offset instead of the cursor.
    pub at: Option<Expr>,
    /// `span expr` — the field occupies exactly this many bytes; inner
    /// content parses inside that region and the cursor skips past it.
    pub span: Option<Expr>,
    /// `be`/`le` prefix; None inherits from the enclosing struct.
    pub endian: Option<Endian>,
}

#[derive(Debug, Clone)]
pub struct StructDef {
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone)]
pub struct EnumDef {
    pub base: Scalar,
    pub variants: Vec<(u64, String)>,
}

#[derive(Debug, Clone)]
pub struct Format {
    pub name: String,
    pub magic: Vec<u8>,
    /// Offset of the magic bytes (tar's "ustar" sits at 257).
    pub magic_offset: usize,
    pub root: String,
    pub structs: HashMap<String, StructDef>,
    pub enums: HashMap<String, EnumDef>,
    /// `flags name : u8 { 0x1 = "READ", ... }` — same shape as enums, but
    /// values decode as the union of their set bits.
    pub flags: HashMap<String, EnumDef>,
}

// ----- lexer ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Ident(String),
    Number(u64),
    Str(String),
    Punct(char),
}

fn lex(src: &str) -> Result<Vec<Token>, String> {
    let mut out = Vec::new();
    let mut chars = src.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' | '\r' | '\n' => {
                chars.next();
            }
            '/' => {
                chars.next();
                if chars.peek() == Some(&'/') {
                    for c in chars.by_ref() {
                        if c == '\n' {
                            break;
                        }
                    }
                } else {
                    out.push(Token::Punct('/'));
                }
            }
            '"' => {
                chars.next();
                let mut s = String::new();
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some(c) => s.push(c),
                        None => return Err("unterminated string".into()),
                    }
                }
                out.push(Token::Str(s));
            }
            '0'..='9' => {
                let mut s = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_ascii_alphanumeric() {
                        s.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let v = if let Some(hex) = s.strip_prefix("0x") {
                    u64::from_str_radix(hex, 16)
                } else if s.chars().all(|c| c.is_ascii_hexdigit())
                    && s.chars().any(|c| c.is_ascii_alphabetic())
                {
                    // bare hex like `7f` inside magic lists
                    u64::from_str_radix(&s, 16)
                } else {
                    s.parse()
                };
                out.push(Token::Number(v.map_err(|_| format!("bad number {s:?}"))?));
            }
            c if c.is_ascii_alphabetic() || c == '_' || c == '$' => {
                // `$end` is the built-in region-end identifier
                let mut s = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_ascii_alphanumeric() || c == '_' || c == '$' {
                        s.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                out.push(Token::Ident(s));
            }
            '{' | '}' | '[' | ']' | '(' | ')' | ':' | ';' | '=' | '@' | ',' | '+' | '-' | '*'
            | '&' | '|' | '<' | '>' => {
                out.push(Token::Punct(c));
                chars.next();
            }
            other => return Err(format!("unexpected character {other:?}")),
        }
    }
    Ok(out)
}

// ----- parser --------------------------------------------------------------

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Result<Token, String> {
        let t = self.tokens.get(self.pos).cloned().ok_or("unexpected end of pattern")?;
        self.pos += 1;
        Ok(t)
    }

    fn expect_punct(&mut self, c: char) -> Result<(), String> {
        match self.next()? {
            Token::Punct(p) if p == c => Ok(()),
            other => Err(format!("expected {c:?}, found {other:?}")),
        }
    }

    fn expect_ident(&mut self) -> Result<String, String> {
        match self.next()? {
            Token::Ident(s) => Ok(s),
            other => Err(format!("expected identifier, found {other:?}")),
        }
    }

    fn eat_punct(&mut self, c: char) -> bool {
        if self.peek() == Some(&Token::Punct(c)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn primary(&mut self) -> Result<Expr, String> {
        match self.next()? {
            Token::Number(n) => Ok(Expr::Lit(n)),
            Token::Ident(name) => Ok(Expr::Field(name)),
            Token::Punct('(') => {
                let e = self.expr()?;
                self.expect_punct(')')?;
                Ok(e)
            }
            other => Err(format!("expected expression, found {other:?}")),
        }
    }

    fn expr(&mut self) -> Result<Expr, String> {
        let mut lhs = self.primary()?;
        // no precedence: operators chain left-to-right; use parens
        while let Some(Token::Punct(c @ ('+' | '-' | '*' | '/' | '&' | '|' | '<' | '>'))) =
            self.peek()
        {
            let op = *c;
            self.pos += 1;
            // shifts are two-character operators
            if op == '<' {
                self.expect_punct('<')?;
            } else if op == '>' {
                self.expect_punct('>')?;
            }
            let rhs = self.primary()?;
            lhs = match op {
                '+' => Expr::Add(Box::new(lhs), Box::new(rhs)),
                '-' => Expr::Sub(Box::new(lhs), Box::new(rhs)),
                '*' => Expr::Mul(Box::new(lhs), Box::new(rhs)),
                '/' => Expr::Div(Box::new(lhs), Box::new(rhs)),
                '&' => Expr::And(Box::new(lhs), Box::new(rhs)),
                '|' => Expr::Or(Box::new(lhs), Box::new(rhs)),
                '<' => Expr::Shl(Box::new(lhs), Box::new(rhs)),
                _ => Expr::Shr(Box::new(lhs), Box::new(rhs)),
            };
        }
        Ok(lhs)
    }
}

fn scalar_of(name: &str) -> Option<Scalar> {
    match name {
        "u8" => Some(Scalar::U8),
        "u16" => Some(Scalar::U16),
        "u32" => Some(Scalar::U32),
        "u64" => Some(Scalar::U64),
        _ => None,
    }
}

/// Parse one `.pat` file: any number of format/struct/enum items.
pub fn parse(src: &str) -> Result<Vec<Format>, String> {
    let mut p = Parser { tokens: lex(src)?, pos: 0 };
    let mut formats: Vec<(String, Vec<u8>, usize, String)> = Vec::new();
    let mut structs: HashMap<String, StructDef> = HashMap::new();
    let mut enums: HashMap<String, EnumDef> = HashMap::new();
    let mut flag_sets: HashMap<String, EnumDef> = HashMap::new();
    // `type NAME = base;` aliases (win32 DWORD/WORD/…); resolved to their
    // underlying scalar as fields are parsed
    let mut aliases: HashMap<String, Scalar> = HashMap::new();

    while p.peek().is_some() {
        match p.expect_ident()?.as_str() {
            "type" => {
                let name = p.expect_ident()?;
                p.expect_punct('=')?;
                let base = p.expect_ident()?;
                p.expect_punct(';')?;
                let scalar = scalar_of(&base)
                    .or_else(|| aliases.get(&base).cloned())
                    .ok_or_else(|| format!("type alias {name:?}: {base:?} is not a scalar"))?;
                aliases.insert(name, scalar);
            }
            "format" => {
                let name = p.expect_ident()?;
                p.expect_punct('{')?;
                let mut magic = Vec::new();
                let mut magic_offset = 0usize;
                let mut root = String::new();
                while !p.eat_punct('}') {
                    let key = p.expect_ident()?;
                    p.expect_punct('=')?;
                    match key.as_str() {
                        "magic" => {
                            // hex string: bare numbers would be ambiguous
                            // (is `45` decimal or hex?)
                            let Token::Str(s) = p.next()? else {
                                return Err(
                                    "magic must be a hex string, e.g. \"7f 45 4c 46\"".into()
                                );
                            };
                            let compact: String =
                                s.chars().filter(|c| !c.is_whitespace()).collect();
                            if !compact.len().is_multiple_of(2) {
                                return Err(format!("magic {s:?} has a half byte"));
                            }
                            for i in (0..compact.len()).step_by(2) {
                                let byte = u8::from_str_radix(&compact[i..i + 2], 16)
                                    .map_err(|_| format!("bad magic byte in {s:?}"))?;
                                magic.push(byte);
                            }
                            // optional `@ offset` (tar's magic is at 257)
                            if p.eat_punct('@') {
                                match p.next()? {
                                    Token::Number(n) => magic_offset = n as usize,
                                    other => {
                                        return Err(format!(
                                            "expected magic offset, found {other:?}"
                                        ));
                                    }
                                }
                            }
                            p.expect_punct(';')?;
                        }
                        "root" => {
                            root = p.expect_ident()?;
                            p.expect_punct(';')?;
                        }
                        other => return Err(format!("unknown format key {other:?}")),
                    }
                }
                if magic.is_empty() || root.is_empty() {
                    return Err(format!("format {name} needs both magic and root"));
                }
                formats.push((name, magic, magic_offset, root));
            }
            "struct" => {
                let name = p.expect_ident()?;
                p.expect_punct('{')?;
                let mut fields = Vec::new();
                while !p.eat_punct('}') {
                    let fname = p.expect_ident()?;
                    p.expect_punct(':')?;
                    let mut tname = p.expect_ident()?;
                    // endianness prefix: `flags: be u32;`
                    let endian = match tname.as_str() {
                        "be" => Some(Endian::Big),
                        "le" => Some(Endian::Little),
                        _ => None,
                    };
                    if endian.is_some() {
                        tname = p.expect_ident()?;
                    }
                    let ty = match tname.as_str() {
                        "char" => FieldType::Char,
                        "leb128" => FieldType::Leb128,
                        "lstr" => FieldType::LStr,
                        "octal" => FieldType::Octal,
                        "derlen" => FieldType::DerLen,
                        "match" => {
                            let on = p.expr()?;
                            p.expect_punct('{')?;
                            let mut arms = Vec::new();
                            while !p.eat_punct('}') {
                                let key = match p.next()? {
                                    Token::Number(n) => Some(n),
                                    Token::Ident(s) if s == "_" => None,
                                    other => {
                                        return Err(format!(
                                            "expected match value or _, found {other:?}"
                                        ));
                                    }
                                };
                                p.expect_punct('=')?;
                                arms.push((key, p.expect_ident()?));
                                p.eat_punct(',');
                            }
                            FieldType::Match { on, arms }
                        }
                        t => match scalar_of(t).or_else(|| aliases.get(t).cloned()) {
                            Some(s) => FieldType::Scalar(s),
                            // struct vs enum resolved after the whole file
                            None => FieldType::Struct(t.to_string()),
                        },
                    };
                    let (count, open_array) = if p.eat_punct('[') {
                        if p.eat_punct(']') {
                            (None, true)
                        } else {
                            let e = p.expr()?;
                            p.expect_punct(']')?;
                            (Some(e), false)
                        }
                    } else {
                        (None, false)
                    };
                    let until_byte = if p.peek() == Some(&Token::Ident("until".into())) {
                        if !open_array {
                            return Err(format!("field {fname:?}: until needs an open [] array"));
                        }
                        p.pos += 1;
                        match p.next()? {
                            Token::Number(n) => {
                                Some(u8::try_from(n).map_err(|_| "until byte > 0xff")?)
                            }
                            other => return Err(format!("expected until byte, found {other:?}")),
                        }
                    } else {
                        None
                    };
                    let at = if p.eat_punct('@') { Some(p.expr()?) } else { None };
                    let span = if p.peek() == Some(&Token::Ident("span".into())) {
                        p.pos += 1;
                        Some(p.expr()?)
                    } else {
                        None
                    };
                    p.expect_punct(';')?;
                    if matches!(ty, FieldType::Char | FieldType::Octal) && count.is_none() {
                        return Err(format!("{tname} field {fname:?} needs a [length]"));
                    }
                    fields.push(Field {
                        name: fname,
                        ty,
                        count,
                        open_array,
                        until_byte,
                        at,
                        span,
                        endian,
                    });
                }
                structs.insert(name, StructDef { fields });
            }
            kw @ ("enum" | "flags") => {
                let name = p.expect_ident()?;
                p.expect_punct(':')?;
                let base_name = p.expect_ident()?;
                let base = scalar_of(&base_name)
                    .or_else(|| aliases.get(&base_name).cloned())
                    .ok_or(format!("{kw} base must be a scalar (u8..u64)"))?;
                p.expect_punct('{')?;
                let mut variants = Vec::new();
                while !p.eat_punct('}') {
                    let value = match p.next()? {
                        Token::Number(n) => n,
                        other => return Err(format!("expected {kw} value, found {other:?}")),
                    };
                    p.expect_punct('=')?;
                    let label = match p.next()? {
                        Token::Str(s) => s,
                        other => return Err(format!("expected {kw} label, found {other:?}")),
                    };
                    variants.push((value, label));
                    p.eat_punct(',');
                }
                if kw == "enum" {
                    enums.insert(name, EnumDef { base, variants });
                } else {
                    flag_sets.insert(name, EnumDef { base, variants });
                }
            }
            other => return Err(format!("expected format/struct/enum/flags, found {other:?}")),
        }
    }

    // resolve struct-vs-enum/flags references now that everything is known
    for def in structs.values_mut() {
        for field in &mut def.fields {
            if let FieldType::Struct(name) = &field.ty {
                if enums.contains_key(name) {
                    field.ty = FieldType::Enum(name.clone());
                } else if flag_sets.contains_key(name) {
                    field.ty = FieldType::Flags(name.clone());
                }
            }
        }
    }
    for def in structs.values() {
        for field in &def.fields {
            match &field.ty {
                FieldType::Struct(name) if !structs.contains_key(name) => {
                    return Err(format!("unknown type {name:?}"));
                }
                FieldType::Match { arms, .. } => {
                    for (_, target) in arms {
                        if !structs.contains_key(target) {
                            return Err(format!("unknown match target {target:?}"));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    formats
        .into_iter()
        .map(|(name, magic, magic_offset, root)| {
            if !structs.contains_key(&root) {
                return Err(format!("unknown root struct {root:?}"));
            }
            Ok(Format {
                name,
                magic,
                magic_offset,
                root,
                structs: structs.clone(),
                enums: enums.clone(),
                flags: flag_sets.clone(),
            })
        })
        .collect()
}

// ----- interpreter ---------------------------------------------------------

const MAX_NODES: usize = 4096;
const MAX_ARRAY_CHILDREN: usize = 64;
// deeply recursive containers (iTunesDB chunks, ASN.1/DER certificate
// trees) need generous headroom; each nesting level costs ~3 eval frames
const MAX_DEPTH: usize = 64;

struct Eval<'a> {
    fmt: &'a Format,
    data: &'a [u8],
    nodes: Vec<HexNode>,
}

/// Scalar values of fields parsed so far, innermost struct scope last.
/// When a struct closes, its scalars are promoted to the parent scope, so
/// `phdrs: phdr[phnum] @ phoff` can reference fields of a nested header.
type Scopes = Vec<HashMap<String, u64>>;

fn eval_expr(expr: &Expr, scopes: &Scopes) -> Option<u64> {
    match expr {
        Expr::Lit(v) => Some(*v),
        Expr::Field(name) => scopes.iter().rev().find_map(|s| s.get(name.as_str()).copied()),
        Expr::Add(a, b) => eval_expr(a, scopes)?.checked_add(eval_expr(b, scopes)?),
        Expr::Sub(a, b) => eval_expr(a, scopes)?.checked_sub(eval_expr(b, scopes)?),
        Expr::Mul(a, b) => eval_expr(a, scopes)?.checked_mul(eval_expr(b, scopes)?),
        Expr::Div(a, b) => eval_expr(a, scopes)?.checked_div(eval_expr(b, scopes)?),
        Expr::And(a, b) => Some(eval_expr(a, scopes)? & eval_expr(b, scopes)?),
        Expr::Or(a, b) => Some(eval_expr(a, scopes)? | eval_expr(b, scopes)?),
        Expr::Shl(a, b) => {
            eval_expr(a, scopes)?.checked_shl(u32::try_from(eval_expr(b, scopes)?).ok()?)
        }
        Expr::Shr(a, b) => {
            eval_expr(a, scopes)?.checked_shr(u32::try_from(eval_expr(b, scopes)?).ok()?)
        }
    }
}

/// Unsigned LEB128 within `[pos, limit)`: (value, bytes consumed).
fn read_leb(data: &[u8], pos: usize, limit: usize) -> Option<(u64, usize)> {
    let mut value: u64 = 0;
    let mut shift = 0;
    for (i, &byte) in data.get(pos..limit.min(data.len()))?.iter().take(10).enumerate() {
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some((value, i + 1));
        }
        shift += 7;
    }
    None
}

/// Display form of a fixed-width string: trailing NUL padding dropped,
/// non-printable bytes as '·'.
fn printable(bytes: &[u8]) -> String {
    let trimmed = match bytes.iter().rposition(|&b| b != 0) {
        Some(last) => &bytes[..=last],
        None => &[],
    };
    trimmed.iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '·' }).collect()
}

impl<'a> Eval<'a> {
    fn push(&mut self, node: HexNode) -> bool {
        if self.nodes.len() >= MAX_NODES {
            return false;
        }
        self.nodes.push(node);
        true
    }

    /// The label of an element's first enum child, used to name array
    /// elements after their tag ("type", "custom") instead of "[7]".
    fn element_label(&self, elem_idx: usize, elem_depth: usize) -> Option<String> {
        self.nodes[elem_idx + 1..]
            .iter()
            .take_while(|n| n.depth > elem_depth)
            .find(|n| n.depth == elem_depth + 1 && self.fmt.enums.contains_key(&n.ty))
            // strip the trailing " (0x..)" echo; labels may contain parens
            .map(|n| n.detail.rsplit_once(" (").map_or("", |(l, _)| l).to_string())
            .filter(|s| !s.is_empty() && s != "?")
    }

    /// Parse `def` at `pos`; returns the end offset. Appends child nodes.
    /// `limit` is the end of the enclosing region (a `span` or the file).
    fn eval_struct(
        &mut self,
        def: &StructDef,
        pos: usize,
        depth: usize,
        scopes: &mut Scopes,
        limit: usize,
        endian: Endian,
    ) -> Option<usize> {
        if depth > MAX_DEPTH {
            return None;
        }
        // `$end` resolves to this region's end offset, so trailers at the
        // tail of a file (binary plist, ZIP central dir) can be located
        let mut scope = HashMap::new();
        scope.insert("$end".to_string(), limit as u64);
        scopes.push(scope);
        let mut cursor = pos;
        let mut ok = true;
        for field in &def.fields {
            let Some(at) = (match &field.at {
                Some(expr) => eval_expr(expr, scopes).map(|v| v as usize),
                None => Some(cursor),
            }) else {
                ok = false;
                break;
            };
            let Some(end) = self.eval_field(field, at, depth, scopes, limit, endian) else {
                ok = false;
                break;
            };
            // only sequential fields advance the cursor; `@` fields park
            // their content elsewhere (and get their own covering node), so
            // they must not push the next field or array element forward
            if field.at.is_none() {
                cursor = end;
            }
        }
        // promote this struct's scalars so later parent fields can use them,
        // but not the per-region `$end` (each scope keeps its own)
        let scope = scopes.pop().expect("scope");
        if let Some(parent) = scopes.last_mut() {
            for (k, v) in scope {
                if !k.starts_with('$') {
                    parent.insert(k, v);
                }
            }
        }
        ok.then_some(cursor)
    }

    fn eval_field(
        &mut self,
        field: &Field,
        pos: usize,
        depth: usize,
        scopes: &mut Scopes,
        limit: usize,
        endian: Endian,
    ) -> Option<usize> {
        // a `be`/`le` prefix overrides and propagates into nested types
        let endian = field.endian.unwrap_or(endian);
        // `span n`: the field owns exactly n bytes; content parses inside
        // that region and the cursor lands at its end even if the inner
        // parse stops early (or fails — partial children are kept)
        if let Some(expr) = &field.span {
            let span = eval_expr(expr, scopes)? as usize;
            let end = pos.checked_add(span)?.min(limit).min(self.data.len());
            let inner = Field { span: None, ..field.clone() };
            let idx = self.nodes.len();
            if self.eval_field(&inner, pos, depth, scopes, end, endian).is_none()
                && self.nodes.len() == idx
            {
                // nothing parsed: at least show the raw bytes
                self.push(HexNode {
                    name: field.name.clone(),
                    ty: format!("u8[{span}]"),
                    detail: String::new(),
                    start: pos,
                    end,
                    depth,
                });
            }
            // the field's own node covers the whole span
            if let Some(node) = self.nodes.get_mut(idx) {
                node.end = node.end.max(end);
            }
            return Some(end);
        }
        let count = match &field.count {
            Some(expr) => Some(eval_expr(expr, scopes)? as usize),
            None => None,
        };
        match (&field.ty, count) {
            (FieldType::Scalar(s), None) if !field.open_array => {
                let end = pos + s.size();
                if end > limit {
                    return None;
                }
                let v = s.read(self.data, pos, endian)?;
                scopes.last_mut().expect("scope").insert(field.name.clone(), v);
                self.push(HexNode {
                    name: field.name.clone(),
                    ty: s.name().into(),
                    detail: format!("{v} ({v:#x})"),
                    start: pos,
                    end,
                    depth,
                });
                Some(end)
            }
            (FieldType::Leb128, None) => {
                let (v, n) = read_leb(self.data, pos, limit)?;
                scopes.last_mut().expect("scope").insert(field.name.clone(), v);
                self.push(HexNode {
                    name: field.name.clone(),
                    ty: "leb128".into(),
                    detail: format!("{v} ({v:#x})"),
                    start: pos,
                    end: pos + n,
                    depth,
                });
                Some(pos + n)
            }
            (FieldType::DerLen, None) => {
                // ASN.1 DER length: short form (< 0x80) is the byte itself;
                // long form (0x8n) is n big-endian length bytes
                let first = *self.data.get(pos)?;
                let (v, n) = if first < 0x80 {
                    (u64::from(first), 1usize)
                } else {
                    let count = (first & 0x7f) as usize;
                    if count == 0 || count > 8 {
                        return None; // indefinite form or absurd width
                    }
                    let mut v: u64 = 0;
                    for i in 0..count {
                        v = (v << 8) | u64::from(*self.data.get(pos + 1 + i)?);
                    }
                    (v, 1 + count)
                };
                if pos + n > limit {
                    return None;
                }
                scopes.last_mut().expect("scope").insert(field.name.clone(), v);
                self.push(HexNode {
                    name: field.name.clone(),
                    ty: "derlen".into(),
                    detail: format!("{v} ({v:#x})"),
                    start: pos,
                    end: pos + n,
                    depth,
                });
                Some(pos + n)
            }
            (FieldType::LStr, None) => {
                let (len, n) = read_leb(self.data, pos, limit)?;
                let start = pos + n;
                let end = start.checked_add(len as usize)?;
                if end > limit {
                    return None;
                }
                let bytes = self.data.get(start..end)?;
                self.push(HexNode {
                    name: field.name.clone(),
                    ty: "str".into(),
                    detail: format!("\"{}\"", printable(bytes)),
                    start: pos,
                    end,
                    depth,
                });
                Some(end)
            }
            (FieldType::Scalar(s), maybe_n) => {
                // scalar array ([n] or [] until region end): hex preview
                let end = match maybe_n {
                    Some(n) => (pos + n * s.size()).min(limit).min(self.data.len()),
                    None => limit.min(self.data.len()),
                };
                let n_shown = (end.saturating_sub(pos)) / s.size().max(1);
                let preview: Vec<String> = self.data[pos.min(end)..end]
                    .iter()
                    .take(8)
                    .map(|b| format!("{b:02x}"))
                    .collect();
                let ellipsis = if end.saturating_sub(pos) > 8 { " …" } else { "" };
                self.push(HexNode {
                    name: field.name.clone(),
                    ty: format!("{}[{n_shown}]", s.name()),
                    detail: format!("[{}{ellipsis}]", preview.join(" ")),
                    start: pos,
                    end,
                    depth,
                });
                Some(end)
            }
            (FieldType::Char, Some(n)) => {
                let end = pos + n;
                if end > limit {
                    return None;
                }
                let bytes = self.data.get(pos..end)?;
                self.push(HexNode {
                    name: field.name.clone(),
                    ty: format!("char[{n}]"),
                    detail: format!("\"{}\"", printable(bytes)),
                    start: pos,
                    end,
                    depth,
                });
                Some(end)
            }
            (FieldType::Octal, Some(n)) => {
                // fixed-width ASCII octal number (tar headers)
                let end = pos + n;
                if end > limit {
                    return None;
                }
                let bytes = self.data.get(pos..end)?;
                let text: String =
                    bytes.iter().map(|&b| b as char).filter(|c| c.is_digit(8)).collect();
                let v = if text.is_empty() { 0 } else { u64::from_str_radix(&text, 8).ok()? };
                scopes.last_mut().expect("scope").insert(field.name.clone(), v);
                self.push(HexNode {
                    name: field.name.clone(),
                    ty: format!("octal[{n}]"),
                    detail: format!("{v} ({v:#o})"),
                    start: pos,
                    end,
                    depth,
                });
                Some(end)
            }
            (FieldType::Enum(name), None) => {
                let def = self.fmt.enums.get(name)?;
                let end = pos + def.base.size();
                if end > limit {
                    return None;
                }
                let v = def.base.read(self.data, pos, endian)?;
                scopes.last_mut().expect("scope").insert(field.name.clone(), v);
                let label = def
                    .variants
                    .iter()
                    .find(|(value, _)| *value == v)
                    .map(|(_, label)| label.as_str())
                    .unwrap_or("?");
                self.push(HexNode {
                    name: field.name.clone(),
                    ty: name.clone(),
                    detail: format!("{label} ({v:#x})"),
                    start: pos,
                    end,
                    depth,
                });
                Some(end)
            }
            (FieldType::Flags(name), None) => {
                let def = self.fmt.flags.get(name)?;
                let end = pos + def.base.size();
                if end > limit {
                    return None;
                }
                let v = def.base.read(self.data, pos, endian)?;
                scopes.last_mut().expect("scope").insert(field.name.clone(), v);
                let set: Vec<&str> = def
                    .variants
                    .iter()
                    .filter(|(mask, _)| *mask != 0 && v & mask == *mask)
                    .map(|(_, label)| label.as_str())
                    .collect();
                let names = if set.is_empty() { "none".to_string() } else { set.join(" | ") };
                self.push(HexNode {
                    name: field.name.clone(),
                    ty: name.clone(),
                    detail: format!("{names} ({v:#x})"),
                    start: pos,
                    end,
                    depth,
                });
                Some(end)
            }
            (FieldType::Match { on, arms }, None) => {
                let v = eval_expr(on, scopes)?;
                let target = arms
                    .iter()
                    .find(|(key, _)| *key == Some(v))
                    .or_else(|| arms.iter().find(|(key, _)| key.is_none()))
                    .map(|(_, t)| t.clone());
                let Some(target) = target else {
                    // no arm: raw bytes to the end of the region
                    let end = limit.min(self.data.len());
                    self.push(HexNode {
                        name: field.name.clone(),
                        ty: format!("u8[{}]", end.saturating_sub(pos)),
                        detail: String::new(),
                        start: pos,
                        end,
                        depth,
                    });
                    return Some(end);
                };
                let def = self.fmt.structs.get(&target)?.clone();
                let idx = self.nodes.len();
                self.push(HexNode {
                    name: field.name.clone(),
                    ty: format!("struct {target}"),
                    detail: "{ ... }".into(),
                    start: pos,
                    end: pos,
                    depth,
                });
                let end = self.eval_struct(&def, pos, depth + 1, scopes, limit, endian)?;
                self.nodes[idx].end = end;
                Some(end)
            }
            (FieldType::Struct(name), None) if !field.open_array => {
                let def = self.fmt.structs.get(name)?.clone();
                let idx = self.nodes.len();
                self.push(HexNode {
                    name: field.name.clone(),
                    ty: format!("struct {name}"),
                    detail: "{ ... }".into(),
                    start: pos,
                    end: pos,
                    depth,
                });
                let end = self.eval_struct(&def, pos, depth + 1, scopes, limit, endian)?;
                self.nodes[idx].end = end;
                Some(end)
            }
            (FieldType::Struct(name), maybe_n) => {
                // struct array: [n] or [] repeating until the region end
                let def = self.fmt.structs.get(name)?.clone();
                let idx = self.nodes.len();
                self.push(HexNode {
                    name: field.name.clone(),
                    ty: String::new(), // set after the loop
                    detail: "{ ... }".into(),
                    start: pos,
                    end: pos,
                    depth,
                });
                let mut cursor = pos;
                let mut parsed = 0usize;
                let mut hidden = 0usize;
                let mut hidden_start = cursor;
                loop {
                    match maybe_n {
                        Some(n) if parsed + hidden >= n => break,
                        None if cursor >= limit.min(self.data.len()) => break,
                        _ => {}
                    }
                    // `[] until N`: stop before an element led by this byte
                    if let Some(t) = field.until_byte
                        && self.data.get(cursor) == Some(&t)
                    {
                        break;
                    }
                    // runaway guard for corrupt element counts
                    if parsed + hidden >= 10_000 {
                        break;
                    }
                    let elem_idx = self.nodes.len();
                    if !self.push(HexNode {
                        name: format!("[{parsed}]"),
                        ty: format!("struct {name}"),
                        detail: "{ ... }".into(),
                        start: cursor,
                        end: cursor,
                        depth: depth + 1,
                    }) {
                        break;
                    }
                    let Some(end) =
                        self.eval_struct(&def, cursor, depth + 2, scopes, limit, endian)
                    else {
                        // truncated element: drop its placeholder node
                        if maybe_n.is_some() {
                            return None;
                        }
                        self.nodes.truncate(elem_idx);
                        break;
                    };
                    if parsed < MAX_ARRAY_CHILDREN {
                        self.nodes[elem_idx].end = end;
                        // name elements after their tag enum
                        if let Some(label) = self.element_label(elem_idx, depth + 1) {
                            self.nodes[elem_idx].name = label;
                        }
                        parsed += 1;
                    } else {
                        // over the display cap: keep walking (the cursor
                        // must stay correct) but drop the element's nodes
                        self.nodes.truncate(elem_idx);
                        if hidden == 0 {
                            hidden_start = cursor;
                        }
                        hidden += 1;
                    }
                    if end <= cursor {
                        break; // zero-size element: avoid spinning
                    }
                    cursor = end;
                }
                if hidden > 0 {
                    self.push(HexNode {
                        name: format!("… {hidden} more"),
                        ty: String::new(),
                        detail: String::new(),
                        start: hidden_start,
                        end: cursor,
                        depth: depth + 1,
                    });
                }
                self.nodes[idx].end = cursor;
                self.nodes[idx].ty = format!("{name}[{}]", parsed + hidden);
                Some(cursor)
            }
            _ => None,
        }
    }
}

/// Try every known format's magic against `data`; interpret on a match.
pub fn evaluate(fmt: &Format, data: &[u8]) -> Vec<HexNode> {
    let mut eval = Eval { fmt, data, nodes: Vec::new() };
    eval.push(HexNode {
        name: fmt.name.clone(),
        ty: format!("struct {}", fmt.root),
        detail: "{ ... }".into(),
        start: 0,
        end: data.len(),
        depth: 0,
    });
    let Some(root) = fmt.structs.get(&fmt.root).cloned() else {
        return eval.nodes;
    };
    let mut scopes: Scopes = Vec::new();
    // a parse error mid-way keeps whatever nodes were produced so far
    let _ = eval.eval_struct(&root, 0, 1, &mut scopes, data.len(), Endian::Little);
    eval.nodes
}

/// The compiled-in pattern library, parsed once.
pub fn builtin_formats() -> &'static [Format] {
    static FORMATS: std::sync::OnceLock<Vec<Format>> = std::sync::OnceLock::new();
    FORMATS.get_or_init(|| {
        const SOURCES: &[(&str, &str)] = &[
            ("elf", include_str!("../assets/patterns/elf.pat")),
            ("wasm", include_str!("../assets/patterns/wasm.pat")),
            ("macho", include_str!("../assets/patterns/macho.pat")),
            ("png", include_str!("../assets/patterns/png.pat")),
            ("zip", include_str!("../assets/patterns/zip.pat")),
            ("sqlite", include_str!("../assets/patterns/sqlite.pat")),
            ("tar", include_str!("../assets/patterns/tar.pat")),
            ("gif", include_str!("../assets/patterns/gif.pat")),
            ("jpeg", include_str!("../assets/patterns/jpeg.pat")),
            ("dxbc", include_str!("../assets/patterns/dxbc.pat")),
            ("spirv", include_str!("../assets/patterns/spirv.pat")),
            ("pe", include_str!("../assets/patterns/pe.pat")),
            ("bmp", include_str!("../assets/patterns/bmp.pat")),
            ("ico", include_str!("../assets/patterns/ico.pat")),
            ("itunesdb", include_str!("../assets/patterns/itunesdb.pat")),
            ("tiff", include_str!("../assets/patterns/tiff.pat")),
            ("isobmff", include_str!("../assets/patterns/isobmff.pat")),
            ("der", include_str!("../assets/patterns/der.pat")),
            ("riff", include_str!("../assets/patterns/riff.pat")),
            ("opentype", include_str!("../assets/patterns/opentype.pat")),
            ("cfb", include_str!("../assets/patterns/cfb.pat")),
            ("bplist", include_str!("../assets/patterns/bplist.pat")),
        ];
        // shared prelude of win32 type aliases and common structs, made
        // available to every pattern by prepending it before parsing
        const PRELUDE: &str = include_str!("../assets/patterns/win32.pat");
        let mut out = Vec::new();
        for (name, src) in SOURCES {
            let combined = format!("{PRELUDE}\n{src}");
            match parse(&combined) {
                Ok(formats) => out.extend(formats),
                Err(e) => {
                    debug_assert!(false, "builtin pattern {name} failed to parse: {e}");
                }
            }
        }
        out
    })
}

/// Structure tree for `data` if any pattern's magic matches.
pub fn match_and_evaluate(data: &[u8]) -> Option<Vec<HexNode>> {
    builtin_formats()
        .iter()
        .find(|f| {
            data.get(f.magic_offset..f.magic_offset + f.magic.len())
                .is_some_and(|bytes| bytes == f.magic)
        })
        .map(|f| evaluate(f, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEMO: &str = r#"
        // toy format for tests
        format demo {
            magic = "de ad";
            root = file;
        }
        enum kind : u8 {
            1 = "alpha",
            2 = "beta",
        }
        struct file {
            header: head;
            items: item[count] @ items_at;
        }
        struct head {
            magic: char[2];
            count: u8;
            items_at: u8;
        }
        struct item {
            kind: kind;
            value: u16;
        }
    "#;

    fn demo_bytes() -> Vec<u8> {
        vec![
            0xde, 0xad, // magic
            2,    // count
            6,    // items_at
            0xff, 0xff, // gap
            1, 0x34, 0x12, // item[0]: alpha, 0x1234
            2, 0x78, 0x56, // item[1]: beta, 0x5678
        ]
    }

    #[test]
    fn parses_and_evaluates_the_demo_format() {
        let formats = parse(DEMO).unwrap();
        assert_eq!(formats.len(), 1);
        let nodes = evaluate(&formats[0], &demo_bytes());
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        // array elements take their first enum child's label as their name
        assert_eq!(
            names,
            vec![
                "demo", "header", "magic", "count", "items_at", "items", "alpha", "kind", "value",
                "beta", "kind", "value"
            ]
        );
        // field references resolved across scopes: 2 items at offset 6
        let items = &nodes[5];
        assert_eq!((items.start, items.end), (6, 12));
        assert_eq!(items.ty, "item[2]");
        // enum decodes to its label
        let kind = &nodes[7];
        assert_eq!(kind.detail, "alpha (0x1)");
        assert_eq!(kind.ty, "kind");
        // scalars little-endian with hex echo
        assert_eq!(nodes[8].detail, "4660 (0x1234)");
        // char preview
        assert_eq!(nodes[2].detail, "\"··\"");
        // struct spans cover their fields, root spans the file
        assert_eq!((nodes[1].start, nodes[1].end), (0, 4));
        assert_eq!((nodes[0].start, nodes[0].end), (0, 12));
    }

    #[test]
    fn magic_dispatch_and_graceful_truncation() {
        let formats = parse(DEMO).unwrap();
        let f = &formats[0];
        assert_eq!(f.magic, vec![0xde, 0xad]);
        // truncated file: keeps the nodes parsed before the error
        let nodes = evaluate(f, &demo_bytes()[..7]);
        assert!(nodes.iter().any(|n| n.name == "count"));
        let last = nodes.last().unwrap();
        assert!(last.end <= 7, "no node reaches past the data");
    }

    #[test]
    fn expressions_combine_fields_and_literals() {
        let src = r#"
            format x { magic = "01"; root = f; }
            struct f {
                n: u8;
                body: u8[n * 2 + 1];
            }
        "#;
        let formats = parse(src).unwrap();
        let nodes = evaluate(&formats[0], &[3, 9, 9, 9, 9, 9, 9, 9, 9]);
        let body = nodes.iter().find(|n| n.name == "body").unwrap();
        assert_eq!(body.ty, "u8[7]");
        assert_eq!((body.start, body.end), (1, 8));
    }

    #[test]
    fn parse_errors_are_reported() {
        assert!(parse(r#"format x { magic = "01"; }"#).unwrap_err().contains("root"));
        assert!(
            parse(r#"struct s { f: nosuch; } format x { magic = "01"; root = s; }"#)
                .unwrap_err()
                .contains("nosuch")
        );
        assert!(parse("struct s { f: char; }").unwrap_err().contains("length"));
        assert!(parse("wibble").unwrap_err().contains("format/struct/enum"));
    }

    #[test]
    fn builtin_wasm_pattern_parses_a_core_module() {
        // header + export section: 1 entry "run" → func 0
        let mut data: Vec<u8> = b"\0asm\x01\0\0\0".to_vec();
        data.extend_from_slice(&[7, 7, 1, 3, b'r', b'u', b'n', 0, 0]);
        let nodes = match_and_evaluate(&data).expect("wasm magic matched");
        assert_eq!(nodes[0].name, "wasm");
        // the section element is renamed after its id enum
        let section = nodes.iter().find(|n| n.ty == "struct wasm_section").unwrap();
        assert_eq!(section.name, "export");
        assert_eq!((section.start, section.end), (8, data.len()));
        // export entry: lstr name, enum kind, leb index
        let name = nodes.iter().find(|n| n.ty == "str").unwrap();
        assert_eq!(name.detail, "\"run\"");
        let kind = nodes.iter().find(|n| n.ty == "export_kind").unwrap();
        assert_eq!(kind.detail, "func (0x0)");
        // the body's span pins the section end even if entries were short
        let body = nodes.iter().find(|n| n.name == "body").unwrap();
        assert_eq!(body.end, data.len());
    }

    #[test]
    fn builtin_component_model_pattern_matches_layer_1() {
        // component header (version 13, layer 1) + one custom section
        let mut data: Vec<u8> = b"\0asm\x0d\0\x01\0".to_vec();
        data.extend_from_slice(&[0, 4, 2, b'h', b'i', 9]);
        let nodes = match_and_evaluate(&data).expect("component magic matched");
        assert_eq!(nodes[0].name, "wasm_component");
        let layer = nodes.iter().find(|n| n.name == "layer").unwrap();
        assert_eq!(layer.detail, "1 (0x1)");
        let section = nodes.iter().find(|n| n.ty == "struct component_section").unwrap();
        assert_eq!(section.name, "custom");
        let name = nodes.iter().find(|n| n.ty == "str").unwrap();
        assert_eq!(name.detail, "\"hi\"");
        // a plain core module must NOT dispatch to the component format
        let core: Vec<u8> = b"\0asm\x01\0\0\0".to_vec();
        let nodes = match_and_evaluate(&core).expect("core magic");
        assert_eq!(nodes[0].name, "wasm");
    }

    #[test]
    fn leb128_and_span_semantics() {
        assert_eq!(read_leb(&[0x08], 0, 1), Some((8, 1)));
        assert_eq!(read_leb(&[0xe5, 0x8e, 0x26], 0, 3), Some((624485, 3)));
        assert_eq!(read_leb(&[0x80, 0x80], 0, 2), None); // unterminated
        assert_eq!(read_leb(&[0xe5, 0x8e, 0x26], 0, 1), None); // limit cuts it
        // span skips past a variant that parses short
        let src = r#"
            format x { magic = "aa"; root = f; }
            struct f { tag: u8; body: match tag { 1 = one, _ = raw } span 4; after: u8; }
            struct one { v: u8; }
            struct raw { data: u8[]; }
        "#;
        let formats = parse(src).unwrap();
        let nodes = evaluate(&formats[0], &[1, 7, 0, 0, 0, 42]);
        // body is the `one` variant (1 byte) but spans 4 bytes
        let body = nodes.iter().find(|n| n.name == "body").unwrap();
        assert_eq!((body.start, body.end), (1, 5));
        assert_eq!(body.ty, "struct one");
        // the cursor continued after the span: `after` reads byte 5
        let after = nodes.iter().find(|n| n.name == "after").unwrap();
        assert_eq!(after.detail, "42 (0x2a)");
    }

    #[test]
    fn flags_decode_as_set_bit_names() {
        let src = r#"
            format x { magic = "aa"; root = f; }
            flags perms : u8 { 1 = "X", 2 = "W", 4 = "R", }
            struct f { magic: u8; a: perms; b: perms; c: perms; }
        "#;
        let formats = parse(src).unwrap();
        let nodes = evaluate(&formats[0], &[0xaa, 5, 0, 0xff]);
        let get = |n: &str| nodes.iter().find(|x| x.name == n).unwrap().detail.clone();
        assert_eq!(get("a"), "X | R (0x5)");
        assert_eq!(get("b"), "none (0x0)");
        // unknown bits keep the hex echo honest
        assert_eq!(get("c"), "X | W | R (0xff)");
        assert_eq!(nodes.iter().find(|x| x.name == "a").unwrap().ty, "perms");
    }

    #[test]
    fn endianness_prefix_reads_big_endian_and_inherits() {
        let src = r#"
            format x { magic = "aa"; root = f; }
            struct f {
                tag: u8;
                a: be u16;
                b: u16;
                nested: be inner;
            }
            struct inner {
                big: u32;
                little: le u16;
            }
        "#;
        let formats = parse(src).unwrap();
        let nodes = evaluate(&formats[0], &[0xaa, 0x12, 0x34, 0x12, 0x34, 0, 0, 0, 7, 0x01, 0x00]);
        let get = |n: &str| nodes.iter().find(|x| x.name == n).unwrap().detail.clone();
        assert_eq!(get("a"), "4660 (0x1234)", "be prefix");
        assert_eq!(get("b"), "13330 (0x3412)", "default stays little-endian");
        // `be` on a struct propagates to its scalars…
        assert_eq!(get("big"), "7 (0x7)");
        // …until an explicit `le` overrides it back
        assert_eq!(get("little"), "1 (0x1)");
    }

    #[test]
    fn builtin_fat_macho_pattern_reads_big_endian_header() {
        let be32 = |v: u32| v.to_be_bytes();
        let mut data = Vec::new();
        data.extend_from_slice(&be32(0xcafe_babe));
        data.extend_from_slice(&be32(1)); // nfat_arch
        data.extend_from_slice(&be32(0x0100_000c)); // arm64
        data.extend_from_slice(&be32(0x80000002)); // cpusubtype
        data.extend_from_slice(&be32(28)); // offset: right after this table
        data.extend_from_slice(&be32(4)); // size
        data.extend_from_slice(&be32(14)); // align (2^14)
        data.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]); // the "slice"
        let nodes = match_and_evaluate(&data).expect("fat magic matched");
        assert_eq!(nodes[0].name, "macho_fat");
        let n = nodes.iter().find(|n| n.name == "nfat_arch").unwrap();
        assert_eq!(n.detail, "1 (0x1)");
        // the arch element is renamed after its (big-endian) cpu_type enum
        let arch = nodes.iter().find(|n| n.ty == "struct fat_arch").unwrap();
        assert_eq!(arch.name, "arm64");
        // the slice bytes are claimed via @ offset
        let slice = nodes.iter().find(|n| n.name == "slice").unwrap();
        assert_eq!((slice.start, slice.end), (28, 32));
        assert_eq!(slice.detail, "[de ad be ef]");
    }

    #[test]
    fn builtin_macho_pattern_parses_load_commands() {
        let le32 = |v: u32| v.to_le_bytes();
        let mut data = Vec::new();
        data.extend_from_slice(&le32(0xfeed_facf)); // magic
        data.extend_from_slice(&le32(0x0100_000c)); // arm64
        data.extend_from_slice(&le32(0)); // cpusubtype
        data.extend_from_slice(&le32(2)); // EXECUTE
        data.extend_from_slice(&le32(2)); // ncmds
        data.extend_from_slice(&le32(48)); // sizeofcmds
        data.extend_from_slice(&le32(0)); // flags
        data.extend_from_slice(&le32(0)); // reserved
        // LC_MAIN: entryoff 0x4000
        data.extend_from_slice(&le32(0x8000_0028));
        data.extend_from_slice(&le32(24));
        data.extend_from_slice(&0x4000u64.to_le_bytes());
        data.extend_from_slice(&0u64.to_le_bytes());
        // LC_UUID
        data.extend_from_slice(&le32(0x1b));
        data.extend_from_slice(&le32(24));
        data.extend_from_slice(&[0xab; 16]);

        let nodes = match_and_evaluate(&data).expect("mach-o magic matched");
        assert_eq!(nodes[0].name, "macho");
        let cpu = nodes.iter().find(|n| n.name == "cputype").unwrap();
        assert_eq!(cpu.detail, "arm64 (0x100000c)");
        // command elements are renamed after their lc_type enum
        let cmds: Vec<&str> = nodes
            .iter()
            .filter(|n| n.ty == "struct load_command")
            .map(|n| n.name.as_str())
            .collect();
        assert_eq!(cmds, vec!["LC_MAIN", "LC_UUID"]);
        let entry = nodes.iter().find(|n| n.name == "entryoff").unwrap();
        assert_eq!(entry.detail, "16384 (0x4000)");
        // spans keep commands tiling: second command starts where LC_MAIN ends
        let (main, uuid) = (
            nodes.iter().find(|n| n.name == "LC_MAIN").unwrap(),
            nodes.iter().find(|n| n.name == "LC_UUID").unwrap(),
        );
        assert_eq!(main.end, uuid.start);
        assert_eq!(uuid.end, data.len());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn macho_pattern_survives_a_real_binary() {
        // the test executable itself is a single-arch Mach-O
        let exe = std::env::current_exe().unwrap();
        let data = std::fs::read(exe).unwrap();
        let Some(nodes) = match_and_evaluate(&data) else {
            return; // universal (fat) binary — not covered by this pattern
        };
        assert_eq!(nodes[0].name, "macho");
        assert!(nodes.iter().any(|n| n.name == "LC_SEGMENT_64"));
        assert!(
            nodes.iter().any(|n| n.name == "segname" && n.detail.contains("__TEXT")),
            "segment names decode"
        );
        assert!(nodes.iter().all(|n| n.end <= data.len()));
    }

    #[test]
    fn builtin_png_pattern_parses_chunks() {
        let mut data: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        // IHDR: 2x3, 8-bit truecolor+alpha
        data.extend_from_slice(&13u32.to_be_bytes());
        data.extend_from_slice(b"IHDR");
        data.extend_from_slice(&2u32.to_be_bytes());
        data.extend_from_slice(&3u32.to_be_bytes());
        data.extend_from_slice(&[8, 6, 0, 0, 0]);
        data.extend_from_slice(&[0x11; 4]); // crc
        // IEND
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(b"IEND");
        data.extend_from_slice(&[0x22; 4]); // crc

        let nodes = match_and_evaluate(&data).expect("png magic matched");
        assert_eq!(nodes[0].name, "png");
        // chunk elements take their type's label
        let chunks: Vec<&str> =
            nodes.iter().filter(|n| n.ty == "struct chunk").map(|n| n.name.as_str()).collect();
        assert_eq!(chunks, vec!["IHDR", "IEND"]);
        // big-endian dimensions and enum decode
        let get = |n: &str| nodes.iter().find(|x| x.name == n).unwrap().detail.clone();
        assert_eq!(get("width"), "2 (0x2)");
        assert_eq!(get("height"), "3 (0x3)");
        assert_eq!(get("color"), "truecolor + alpha (0x6)");
        // chunks tile the file exactly: IEND ends at eof
        let iend = nodes.iter().find(|n| n.name == "IEND").unwrap();
        assert_eq!(iend.end, data.len());
        // the IHDR data span ends where its crc begins
        let ihdr = nodes.iter().find(|n| n.name == "IHDR").unwrap();
        let crc = nodes
            .iter()
            .find(|n| n.name == "crc" && n.start > ihdr.start && n.end <= ihdr.end)
            .unwrap();
        assert_eq!(crc.start, 8 + 8 + 13);
    }

    #[test]
    fn builtin_zip_pattern_walks_records() {
        let le16 = |v: u16| v.to_le_bytes();
        let le32 = |v: u32| v.to_le_bytes();
        let mut data = Vec::new();
        // local file "a.txt", stored, contents "hi"
        data.extend_from_slice(b"PK\x03\x04");
        data.extend_from_slice(&le16(10)); // version needed
        data.extend_from_slice(&le16(0x800)); // flags: UTF8
        data.extend_from_slice(&le16(0)); // stored
        data.extend_from_slice(&le16(0)); // time
        data.extend_from_slice(&le16(0)); // date
        data.extend_from_slice(&le32(0xdeadbeef)); // crc
        data.extend_from_slice(&le32(2)); // csize
        data.extend_from_slice(&le32(2)); // usize
        data.extend_from_slice(&le16(5)); // name len
        data.extend_from_slice(&le16(0)); // extra len
        data.extend_from_slice(b"a.txthi");
        let cd_offset = data.len() as u32;
        // central directory entry for it
        data.extend_from_slice(b"PK\x01\x02");
        data.extend_from_slice(&le16(20));
        data.extend_from_slice(&le16(10));
        data.extend_from_slice(&le16(0x800));
        data.extend_from_slice(&le16(0));
        data.extend_from_slice(&le16(0));
        data.extend_from_slice(&le16(0));
        data.extend_from_slice(&le32(0xdeadbeef));
        data.extend_from_slice(&le32(2));
        data.extend_from_slice(&le32(2));
        data.extend_from_slice(&le16(5));
        data.extend_from_slice(&le16(0));
        data.extend_from_slice(&le16(0)); // comment len
        data.extend_from_slice(&le16(0)); // disk
        data.extend_from_slice(&le16(0)); // internal
        data.extend_from_slice(&le32(0)); // external
        data.extend_from_slice(&le32(0)); // local offset
        data.extend_from_slice(b"a.txt");
        let cd_size = data.len() as u32 - cd_offset;
        // end of central directory
        data.extend_from_slice(b"PK\x05\x06");
        data.extend_from_slice(&le16(0));
        data.extend_from_slice(&le16(0));
        data.extend_from_slice(&le16(1));
        data.extend_from_slice(&le16(1));
        data.extend_from_slice(&le32(cd_size));
        data.extend_from_slice(&le32(cd_offset));
        data.extend_from_slice(&le16(0));

        let nodes = match_and_evaluate(&data).expect("zip magic matched");
        assert_eq!(nodes[0].name, "zip");
        // records rename themselves after the signature enum
        let records: Vec<&str> =
            nodes.iter().filter(|n| n.ty == "struct record").map(|n| n.name.as_str()).collect();
        assert_eq!(records, vec!["local file", "central directory", "end of central directory"]);
        // filename, method, and flags decode; records tile the file
        assert!(nodes.iter().any(|n| n.name == "name" && n.detail == "\"a.txt\""));
        assert!(nodes.iter().any(|n| n.name == "method" && n.detail == "stored (0x0)"));
        assert!(nodes.iter().any(|n| n.name == "flags" && n.detail == "UTF8 (0x800)"));
        assert_eq!(nodes.iter().rfind(|n| n.ty == "struct record").unwrap().end, data.len());
        // the stored payload is claimed
        let payload = nodes.iter().find(|n| n.name == "data").unwrap();
        assert_eq!(payload.detail, "[68 69]"); // "hi"
    }

    #[test]
    fn builtin_sqlite_pattern_parses_header_and_pages() {
        let mut data = vec![0u8; 1024]; // two 512-byte pages
        data[0..16].copy_from_slice(b"SQLite format 3\0");
        data[16..18].copy_from_slice(&512u16.to_be_bytes());
        data[18] = 1; // write version
        data[19] = 1; // read version
        data[21] = 64;
        data[22] = 32;
        data[23] = 32;
        data[28..32].copy_from_slice(&2u32.to_be_bytes()); // size in pages
        data[44..48].copy_from_slice(&4u32.to_be_bytes()); // schema format
        data[56..60].copy_from_slice(&1u32.to_be_bytes()); // utf-8
        data[100] = 13; // page 1: leaf table
        data[103..105].copy_from_slice(&512u16.to_be_bytes()); // content start
        data[512] = 5; // page 2: interior table
        data[512 + 8..512 + 12].copy_from_slice(&7u32.to_be_bytes()); // right ptr

        let nodes = match_and_evaluate(&data).expect("sqlite magic matched");
        assert_eq!(nodes[0].name, "sqlite");
        let get = |n: &str| nodes.iter().find(|x| x.name == n).unwrap().detail.clone();
        assert_eq!(get("page_size"), "512 (0x200)");
        assert_eq!(get("encoding"), "UTF-8 (0x1)");
        assert_eq!(get("schema"), "DESC indexes + boolean (0x4)");
        // page 1's b-tree sits after the header and fills the page
        let page1 = nodes.iter().find(|n| n.name == "page1").unwrap();
        assert_eq!((page1.start, page1.end), (100, 512));
        let types: Vec<&str> =
            nodes.iter().filter(|n| n.name == "type").map(|n| n.detail.as_str()).collect();
        assert_eq!(types, vec!["leaf table (0xd)", "interior table (0x5)"]);
        // the interior page decodes its rightmost child pointer
        assert_eq!(get("rightmost_pointer"), "7 (0x7)");
        // remaining pages tile to eof
        let pages = nodes.iter().find(|n| n.name == "pages").unwrap();
        assert_eq!((pages.start, pages.end), (512, 1024));
        assert_eq!(pages.ty, "page[1]");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn sqlite_pattern_survives_a_real_database() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("real.db");
        let status = std::process::Command::new("sqlite3")
            .arg(&db)
            .arg("CREATE TABLE t(a INTEGER, b TEXT); INSERT INTO t VALUES (1, 'hello');")
            .status();
        let Ok(status) = status else { return }; // no sqlite3 on PATH
        assert!(status.success());
        let data = std::fs::read(&db).unwrap();
        let nodes = match_and_evaluate(&data).expect("real db matched");
        assert_eq!(nodes[0].name, "sqlite");
        assert!(nodes.iter().all(|n| n.end <= data.len()));
        // pages tile the whole file
        let pages = nodes.iter().find(|n| n.name == "pages").unwrap();
        assert_eq!(pages.end, data.len());
        assert!(nodes.iter().any(|n| n.name == "type" && n.detail.contains("leaf table")));
    }

    #[test]
    fn builtin_tar_pattern_walks_entries() {
        // one 5-byte file + the two zero end blocks
        let mut data = vec![0u8; 512 * 4];
        data[0..9].copy_from_slice(b"hello.txt");
        data[100..107].copy_from_slice(b"0000644"); // mode
        data[124..135].copy_from_slice(b"00000000005"); // size = 5
        data[156] = b'0'; // regular file
        data[257..262].copy_from_slice(b"ustar");
        data[265..268].copy_from_slice(b"dev"); // uname
        data[512..517].copy_from_slice(b"hello");

        let nodes = match_and_evaluate(&data).expect("tar magic matched");
        assert_eq!(nodes[0].name, "tar");
        let get = |n: &str| nodes.iter().find(|x| x.name == n).unwrap().detail.clone();
        // NUL padding is trimmed from fixed-width strings
        assert_eq!(get("name"), "\"hello.txt\"");
        assert_eq!(get("uname"), "\"dev\"");
        // octal fields decode: mode 644, size 5
        assert_eq!(get("mode"), "420 (0o644)");
        assert_eq!(get("size"), "5 (0o5)");
        assert_eq!(get("type"), "file (0x30)");
        // data span rounds 5 bytes up to one 512-byte block
        let payload = nodes.iter().find(|n| n.name == "data").unwrap();
        assert_eq!((payload.start, payload.end), (512, 1024));
        // the two zero end blocks parse as end-marker entries
        let types: Vec<String> =
            nodes.iter().filter(|n| n.name == "type").map(|n| n.detail.clone()).collect();
        assert_eq!(types, vec!["file (0x30)", "end marker (0x0)", "end marker (0x0)"]);
        let last = nodes.iter().rfind(|n| n.ty == "struct entry").unwrap();
        assert_eq!(last.end, data.len());
    }

    #[test]
    fn tar_pattern_survives_a_real_archive() {
        // hermetic real-world check: tar is universally available
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello vibin").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/b.bin"), vec![0u8; 700]).unwrap();
        let archive = dir.path().join("t.tar");
        let status = std::process::Command::new("tar")
            .arg("-cf")
            .arg(&archive)
            .arg("-C")
            .arg(dir.path())
            .args(["a.txt", "sub"])
            .status();
        let Ok(status) = status else { return }; // no tar on PATH
        assert!(status.success());
        let data = std::fs::read(&archive).unwrap();
        let nodes = match_and_evaluate(&data).expect("real tar matched");
        assert!(nodes.iter().any(|n| n.name == "name" && n.detail.contains("a.txt")));
        assert!(nodes.iter().any(|n| n.name == "type" && n.detail.contains("directory")));
        assert!(nodes.iter().all(|n| n.end <= data.len()));
    }

    #[test]
    fn parens_and_bitwise_operators() {
        // the GIF color-table size idiom: presence bit × (6 << size bits)
        let src = r#"
            format x { magic = "aa"; root = f; }
            struct f {
                packed: u8;
                table: u8[(packed / 128) * (6 << (packed & 7))];
            }
        "#;
        let formats = parse(src).unwrap();
        let mut data = vec![0x87u8]; // bit 7 set, size bits = 7 → 6<<7 = 768
        data.extend_from_slice(&[0u8; 768]);
        let nodes = evaluate(&formats[0], &data);
        let table = nodes.iter().find(|n| n.name == "table").unwrap();
        assert_eq!(table.ty, "u8[768]");
        assert_eq!((table.start, table.end), (1, 769));
        // presence bit clear → empty table
        let nodes = evaluate(&formats[0], &[0x07]);
        let table = nodes.iter().find(|n| n.name == "table").unwrap();
        assert_eq!(table.ty, "u8[0]");
    }

    #[test]
    fn builtin_gif_pattern_parses_the_party_parrot() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/parrot.gif");
        let data = std::fs::read(path).unwrap();
        let nodes = match_and_evaluate(&data).expect("gif magic matched");
        assert_eq!(nodes[0].name, "gif89");
        let get = |n: &str| nodes.iter().find(|x| x.name == n).unwrap().detail.clone();
        assert_eq!(get("signature"), "\"GIF89a\"");
        // the animation has frames and control extensions, named by enum
        let kinds: Vec<&str> =
            nodes.iter().filter(|n| n.ty == "struct block").map(|n| n.name.as_str()).collect();
        assert!(kinds.contains(&"image"), "{kinds:?}");
        assert!(kinds.contains(&"extension"));
        // `until 0x3b` walks every frame: the trailer lands exactly at eof
        let trailer = nodes.iter().find(|n| n.name == "trailer").unwrap();
        assert_eq!(trailer.end, data.len());
        assert!(nodes.iter().all(|n| n.end <= data.len()));
    }

    #[test]
    fn builtin_jpeg_pattern_parses_segments() {
        let mut data: Vec<u8> = vec![0xff, 0xd8]; // SOI
        data.extend_from_slice(&[0xff, 0xe0, 0x00, 0x10]); // APP0 len 16
        data.extend_from_slice(b"JFIF\0");
        data.extend_from_slice(&[1, 2, 0]); // version 1.2, units 0
        data.extend_from_slice(&72u16.to_be_bytes());
        data.extend_from_slice(&72u16.to_be_bytes());
        data.extend_from_slice(&[0, 0]); // no thumbnail
        data.extend_from_slice(&[0xff, 0xc0, 0x00, 0x11]); // SOF0 len 17
        data.push(8); // precision
        data.extend_from_slice(&256u16.to_be_bytes()); // height
        data.extend_from_slice(&128u16.to_be_bytes()); // width
        data.push(3); // components
        data.extend_from_slice(&[1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        data.extend_from_slice(&[0xff, 0xda, 0x00, 0x0c]); // SOS len 12
        data.extend_from_slice(&[3, 1, 0, 2, 0x11, 3, 0x11, 0, 63, 0]);
        data.extend_from_slice(&[0x12, 0x34, 0x56]); // entropy
        data.extend_from_slice(&[0xff, 0xd9]); // EOI

        let nodes = match_and_evaluate(&data).expect("jpeg magic matched");
        assert_eq!(nodes[0].name, "jpeg");
        let segments: Vec<&str> =
            nodes.iter().filter(|n| n.ty == "struct segment").map(|n| n.name.as_str()).collect();
        assert_eq!(segments, vec!["SOI", "APP0 (JFIF)", "SOF0 (baseline)", "SOS"]);
        let get = |n: &str| nodes.iter().find(|x| x.name == n).unwrap().detail.clone();
        assert_eq!(get("identifier"), "\"JFIF\"");
        assert_eq!(get("width"), "128 (0x80)");
        assert_eq!(get("height"), "256 (0x100)");
        assert_eq!(get("component_count"), "3 (0x3)");
        // the entropy-coded scan runs to the end of the file
        let entropy = nodes.iter().find(|n| n.name == "entropy_data").unwrap();
        assert_eq!(entropy.end, data.len());
    }

    #[test]
    fn builtin_dxbc_pattern_finds_the_dxil_bitcode() {
        let le32 = |v: u32| v.to_le_bytes();
        // DXIL program: 24-byte header + 8 bytes of bitcode
        let mut dxil = Vec::new();
        dxil.extend_from_slice(&le32(0x60)); // program version (SM 6.0)
        dxil.extend_from_slice(&le32(8)); // size in dwords
        dxil.extend_from_slice(b"DXIL");
        dxil.extend_from_slice(&le32(0x102));
        dxil.extend_from_slice(&le32(24)); // bitcode offset
        dxil.extend_from_slice(&le32(8)); // bitcode size
        dxil.extend_from_slice(b"BC\xc0\xde\x01\x02\x03\x04");

        let mut data = Vec::new();
        data.extend_from_slice(b"DXBC");
        data.extend_from_slice(&[0xaa; 16]); // hash
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        let header_len = 4 + 16 + 4 + 4 + 4 + 2 * 4; // through the offset table
        let part1_off = header_len as u32;
        let part2_off = part1_off + 8 + dxil.len() as u32;
        let total = part2_off + 8 + 4;
        data.extend_from_slice(&le32(total));
        data.extend_from_slice(&le32(2)); // part count
        data.extend_from_slice(&le32(part1_off));
        data.extend_from_slice(&le32(part2_off));
        data.extend_from_slice(b"DXIL");
        data.extend_from_slice(&le32(dxil.len() as u32));
        data.extend_from_slice(&dxil);
        data.extend_from_slice(b"STAT");
        data.extend_from_slice(&le32(4));
        data.extend_from_slice(&[9; 4]);

        let nodes = match_and_evaluate(&data).expect("dxbc magic matched");
        assert_eq!(nodes[0].name, "dxbc");
        // parts name themselves from the fourcc enum
        let parts: Vec<&str> =
            nodes.iter().filter(|n| n.ty == "struct part").map(|n| n.name.as_str()).collect();
        assert_eq!(parts, vec!["DXIL (SM6 program)", "STAT (statistics)"]);
        let get = |n: &str| nodes.iter().find(|x| x.name == n).unwrap().detail.clone();
        assert_eq!(get("dxil_magic"), "\"DXIL\"");
        assert_eq!(get("bitcode_size"), "8 (0x8)");
        // the LLVM bitcode blob is claimed with its BC magic visible
        let bitcode = nodes.iter().find(|n| n.name == "bitcode").unwrap();
        assert!(bitcode.detail.starts_with("[42 43 c0 de"));
        // parts tile to the end of the container
        let last = nodes.iter().rfind(|n| n.ty == "struct part").unwrap();
        assert_eq!(last.end, data.len());
    }

    #[test]
    fn builtin_spirv_pattern_decodes_instructions() {
        let le32 = |v: u32| v.to_le_bytes();
        let mut data = Vec::new();
        data.extend_from_slice(&le32(0x0723_0203)); // magic
        data.extend_from_slice(&le32(0x0001_0500)); // version 1.5
        data.extend_from_slice(&le32(0x0008_000b)); // generator
        data.extend_from_slice(&le32(20)); // id bound
        data.extend_from_slice(&le32(0)); // schema
        // OpCapability Shader: word count 2, opcode 17
        data.extend_from_slice(&le32(2 << 16 | 17));
        data.extend_from_slice(&le32(1));
        // OpMemoryModel Logical GLSL450: word count 3, opcode 14
        data.extend_from_slice(&le32(3 << 16 | 14));
        data.extend_from_slice(&le32(0));
        data.extend_from_slice(&le32(1));
        // OpTypeVoid %2: word count 2, opcode 19
        data.extend_from_slice(&le32(2 << 16 | 19));
        data.extend_from_slice(&le32(2));

        let nodes = match_and_evaluate(&data).expect("spirv magic matched");
        assert_eq!(nodes[0].name, "spirv");
        let ops: Vec<&str> = nodes
            .iter()
            .filter(|n| n.ty == "struct instruction")
            .map(|n| n.name.as_str())
            .collect();
        assert_eq!(ops, vec!["OpCapability", "OpMemoryModel", "OpTypeVoid"]);
        // operand sizes derive from the packed word count
        let sizes: Vec<&str> =
            nodes.iter().filter(|n| n.name == "operands").map(|n| n.ty.as_str()).collect();
        assert_eq!(sizes, vec!["u8[4]", "u8[8]", "u8[4]"]);
        // instructions tile to the end of the module
        let insts = nodes.iter().find(|n| n.name == "instructions").unwrap();
        assert_eq!((insts.start, insts.end), (20, data.len()));
    }

    #[test]
    fn builtin_pe_pattern_follows_e_lfanew_to_nt_headers() {
        let le16 = |v: u16| v.to_le_bytes();
        let le32 = |v: u32| v.to_le_bytes();
        let mut data = Vec::new();
        data.extend_from_slice(b"MZ");
        data.extend_from_slice(&[0u8; 58]); // dos fields
        let e_lfanew = 0x80u32;
        data.extend_from_slice(&le32(e_lfanew));
        data.resize(e_lfanew as usize, 0); // dos stub padding
        // NT headers at e_lfanew
        data.extend_from_slice(b"PE\0\0");
        // COFF: x86-64, 1 section, PE32+ optional header of 0x70 bytes
        data.extend_from_slice(&le16(0x8664));
        data.extend_from_slice(&le16(1)); // num sections
        data.extend_from_slice(&le32(0)); // timestamp
        data.extend_from_slice(&le32(0)); // symbol ptr
        data.extend_from_slice(&le32(0)); // num symbols
        let opt_size = 0x70u16;
        data.extend_from_slice(&le16(opt_size));
        data.extend_from_slice(&le16(0x2022)); // EXECUTABLE_IMAGE | DLL | LARGE_ADDRESS_AWARE
        let opt_start = data.len();
        data.extend_from_slice(&le16(0x020b)); // PE32+
        data.resize(opt_start + 22, 0); // through entry_point etc.
        data[opt_start + 16..opt_start + 20].copy_from_slice(&le32(0x1000)); // entry point
        // pad the optional header to opt_size, then set subsystem/flags/dirs
        data.resize(opt_start + opt_size as usize, 0);
        // subsystem at offset 68 in the PE32+ optional header, num dirs at 108
        data[opt_start + 68..opt_start + 70].copy_from_slice(&le16(2)); // GUI
        data[opt_start + 70..opt_start + 72].copy_from_slice(&le16(0x0140)); // DYNAMIC_BASE|NX
        data[opt_start + 108..opt_start + 112].copy_from_slice(&le32(0)); // no dirs
        // one section ".text", raw data 4 bytes at end
        let sec_start = data.len();
        let raw_ptr = (sec_start + 40 + 4) as u32; // after this 40-byte header (padded)
        data.extend_from_slice(b".text\0\0\0");
        data.extend_from_slice(&le32(0x10)); // virtual size
        data.extend_from_slice(&le32(0x1000)); // virtual address
        data.extend_from_slice(&le32(4)); // size of raw data
        data.extend_from_slice(&le32(raw_ptr));
        data.extend_from_slice(&[0u8; 12]); // reloc/line ptrs, counts
        data.extend_from_slice(&le32(0x60000020)); // CODE|EXECUTE|READ
        data.extend_from_slice(&[0xcc; 4]); // the section's raw bytes

        let nodes = match_and_evaluate(&data).expect("pe magic matched");
        assert_eq!(nodes[0].name, "pe");
        // e_lfanew jump landed on the PE signature
        let sig = nodes.iter().find(|n| n.name == "signature").unwrap();
        assert_eq!(sig.start, e_lfanew as usize);
        let get = |n: &str| nodes.iter().find(|x| x.name == n).unwrap().detail.clone();
        let by_ty = |t: &str| nodes.iter().find(|n| n.ty == t).unwrap().detail.clone();
        assert_eq!(get("machine"), "x86-64 (0x8664)");
        assert!(by_ty("coff_characteristics").contains("DLL"));
        assert_eq!(by_ty("opt_magic"), "PE32+ (0x20b)");
        assert_eq!(get("subsystem"), "Windows GUI (0x2)");
        assert!(get("dll_flags").contains("DYNAMIC_BASE"));
        // the section decodes and claims its raw bytes via @ raw_data_ptr
        assert_eq!(get("name"), "\".text\"");
        assert!(by_ty("section_characteristics").contains("EXECUTE"));
        let contents = nodes.iter().find(|n| n.name == "contents").unwrap();
        assert_eq!((contents.start, contents.end), (raw_ptr as usize, data.len()));
    }

    #[test]
    fn builtin_bmp_pattern_decodes_headers_and_pixels() {
        let le16 = |v: u16| v.to_le_bytes();
        let le32 = |v: u32| v.to_le_bytes();
        let mut data = Vec::new();
        data.extend_from_slice(b"BM");
        data.extend_from_slice(&le32(70)); // file size
        data.extend_from_slice(&le16(0));
        data.extend_from_slice(&le16(0));
        data.extend_from_slice(&le32(54)); // data offset
        // BITMAPINFOHEADER (40 bytes)
        data.extend_from_slice(&le32(40)); // header size
        data.extend_from_slice(&le32(2)); // width
        data.extend_from_slice(&le32(2)); // height
        data.extend_from_slice(&le16(1)); // planes
        data.extend_from_slice(&le16(24)); // bpp
        data.extend_from_slice(&le32(0)); // BI_RGB
        data.extend_from_slice(&le32(16)); // image size
        data.extend_from_slice(&le32(2835));
        data.extend_from_slice(&le32(2835));
        data.extend_from_slice(&le32(0));
        data.extend_from_slice(&le32(0));
        data.extend_from_slice(&[0xff; 16]); // pixels

        let nodes = match_and_evaluate(&data).expect("bmp magic matched");
        assert_eq!(nodes[0].name, "bmp");
        let get = |n: &str| nodes.iter().find(|x| x.name == n).unwrap().detail.clone();
        assert_eq!(get("signature"), "\"BM\"");
        assert_eq!(get("width"), "2 (0x2)");
        assert_eq!(get("bits_per_pixel"), "24 (0x18)");
        assert_eq!(get("compression"), "BI_RGB (none) (0x0)");
        // pixels claimed via @ data_offset, filling to eof
        let pixels = nodes.iter().find(|n| n.name == "pixels").unwrap();
        assert_eq!((pixels.start, pixels.end), (54, data.len()));
    }

    #[test]
    fn builtin_ico_pattern_walks_directory_entries() {
        let le16 = |v: u16| v.to_le_bytes();
        let le32 = |v: u32| v.to_le_bytes();
        let mut data = Vec::new();
        data.extend_from_slice(&le16(0)); // reserved
        data.extend_from_slice(&le16(1)); // type: icon
        data.extend_from_slice(&le16(2)); // two images
        let dir_end = 6 + 2 * 16;
        // entry 0: 16x16, BMP data
        data.extend_from_slice(&[16, 16, 0, 0]);
        data.extend_from_slice(&le16(1));
        data.extend_from_slice(&le16(32));
        data.extend_from_slice(&le32(4)); // size
        data.extend_from_slice(&le32(dir_end as u32)); // offset
        // entry 1: 0x0 (=256), PNG data
        data.extend_from_slice(&[0, 0, 0, 0]);
        data.extend_from_slice(&le16(1));
        data.extend_from_slice(&le16(32));
        data.extend_from_slice(&le32(8)); // size
        data.extend_from_slice(&le32(dir_end as u32 + 4)); // offset
        data.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]); // "BMP" bytes
        data.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]); // PNG

        let nodes = match_and_evaluate(&data).expect("ico magic matched");
        assert_eq!(nodes[0].name, "ico");
        assert_eq!(nodes.iter().find(|n| n.name == "type").unwrap().detail, "icon (0x1)");
        // two entries, each claiming its image bytes via @ offset
        let images: Vec<(usize, usize)> =
            nodes.iter().filter(|n| n.name == "image").map(|n| (n.start, n.end)).collect();
        assert_eq!(images, vec![(dir_end, dir_end + 4), (dir_end + 4, data.len())]);
        // the embedded PNG magic is visible in the claimed blob
        let png = nodes.iter().find(|n| n.name == "image" && n.start == dir_end + 4).unwrap();
        assert!(png.detail.starts_with("[89 50 4e 47"));
    }

    #[test]
    fn builtin_itunesdb_pattern_walks_the_chunk_tree() {
        let le32 = |v: u32| v.to_le_bytes();
        // build innermost-out: mhod(title) < mhit < mhsd < mhbd
        let mut mhod = Vec::new();
        mhod.extend_from_slice(b"mhod");
        mhod.extend_from_slice(&le32(24)); // header length
        mhod.extend_from_slice(&le32(28)); // total length
        mhod.extend_from_slice(&le32(1)); // subtype: title
        mhod.extend_from_slice(&[0u8; 8]); // reserved header
        mhod.extend_from_slice(&[0x41, 0x42, 0x43, 0x44]); // "string"

        let mut mhit = Vec::new();
        mhit.extend_from_slice(b"mhit");
        mhit.extend_from_slice(&le32(20)); // header length
        mhit.extend_from_slice(&le32(20 + mhod.len() as u32));
        mhit.extend_from_slice(&[0u8; 8]); // fixed header (id, rating…)
        mhit.extend_from_slice(&mhod);

        let mut mhlt = Vec::new();
        mhlt.extend_from_slice(b"mhlt");
        mhlt.extend_from_slice(&le32(16));
        mhlt.extend_from_slice(&le32(16)); // list header: children follow as siblings
        mhlt.extend_from_slice(&le32(1)); // track count

        let mut mhsd = Vec::new();
        mhsd.extend_from_slice(b"mhsd");
        mhsd.extend_from_slice(&le32(16));
        mhsd.extend_from_slice(&le32(16 + mhlt.len() as u32 + mhit.len() as u32));
        mhsd.extend_from_slice(&le32(1)); // dataset index
        mhsd.extend_from_slice(&mhlt);
        mhsd.extend_from_slice(&mhit);

        let mut data = Vec::new();
        data.extend_from_slice(b"mhbd");
        data.extend_from_slice(&le32(16));
        data.extend_from_slice(&le32(16 + mhsd.len() as u32));
        data.extend_from_slice(&le32(1)); // version
        data.extend_from_slice(&mhsd);

        let nodes = match_and_evaluate(&data).expect("itunesdb magic matched");
        assert_eq!(nodes[0].name, "itunesdb");
        // the fourcc tags decode via the big-endian enum, in tree order
        let tags: Vec<&str> =
            nodes.iter().filter(|n| n.ty == "chunk_type").map(|n| n.detail.as_str()).collect();
        assert_eq!(
            tags,
            vec![
                "mhbd (database) (0x6d686264)",
                "mhsd (dataset) (0x6d687364)",
                "mhlt (track list) (0x6d686c74)",
                "mhit (track item) (0x6d686974)",
                "mhod (data object) (0x6d686f64)",
            ]
        );
        // the mhod leaf decodes its subtype
        assert_eq!(nodes.iter().find(|n| n.ty == "mhod_type").unwrap().detail, "title (0x1)");
        // the whole tree tiles: mhbd covers the file, mhit nests its mhod
        assert_eq!(nodes[0].end, data.len());
        let mhit_node =
            nodes.iter().find(|n| n.detail == "mhit (track item) (0x6d686974)").unwrap();
        let mhit_chunk =
            nodes.iter().find(|n| n.ty == "struct chunk" && n.start == mhit_node.start);
        assert!(mhit_chunk.is_some());
    }

    #[test]
    fn builtin_tiff_pattern_reads_ifd_entries() {
        let le16 = |v: u16| v.to_le_bytes();
        let le32 = |v: u32| v.to_le_bytes();
        let mut data = Vec::new();
        data.extend_from_slice(b"II");
        data.extend_from_slice(&le16(42));
        data.extend_from_slice(&le32(8)); // first IFD at offset 8
        // IFD: one entry (ImageWidth = 1920), then next-IFD = 0
        data.extend_from_slice(&le16(1)); // entry count
        data.extend_from_slice(&le16(0x0100)); // ImageWidth
        data.extend_from_slice(&le16(3)); // SHORT
        data.extend_from_slice(&le32(1)); // count
        data.extend_from_slice(&le32(1920)); // value
        data.extend_from_slice(&le32(0)); // no next IFD

        let nodes = match_and_evaluate(&data).expect("tiff magic matched");
        assert_eq!(nodes[0].name, "tiff");
        // the IFD is reached via @ first_ifd_offset
        let ifd = nodes.iter().find(|n| n.name == "ifd").unwrap();
        assert_eq!(ifd.start, 8);
        // the entry's tag and type decode via their enums
        assert_eq!(nodes.iter().find(|n| n.ty == "tiff_tag").unwrap().detail, "ImageWidth (0x100)");
        assert_eq!(nodes.iter().find(|n| n.ty == "tiff_type").unwrap().detail, "SHORT (0x3)");
        assert_eq!(
            nodes.iter().find(|n| n.name == "value_or_offset").unwrap().detail,
            "1920 (0x780)"
        );
    }

    #[test]
    fn builtin_isobmff_pattern_walks_the_box_tree() {
        let be32 = |v: u32| v.to_be_bytes();
        let mut data = Vec::new();
        // ftyp box (size 20)
        data.extend_from_slice(&be32(20));
        data.extend_from_slice(b"ftyp");
        data.extend_from_slice(b"isom");
        data.extend_from_slice(&be32(0)); // minor version
        data.extend_from_slice(b"isom"); // compatible brand
        // moov box (size 24) containing an mvhd leaf (size 16)
        data.extend_from_slice(&be32(24));
        data.extend_from_slice(b"moov");
        data.extend_from_slice(&be32(16));
        data.extend_from_slice(b"mvhd");
        data.extend_from_slice(&[0u8; 8]);

        let nodes = match_and_evaluate(&data).expect("isobmff magic matched");
        assert_eq!(nodes[0].name, "isobmff");
        // big-endian fourcc decodes via the box_type enum, and boxes nest
        let boxes: Vec<&str> =
            nodes.iter().filter(|n| n.ty == "box_type").map(|n| n.detail.as_str()).collect();
        assert_eq!(
            boxes,
            vec![
                "ftyp (file type) (0x66747970)",
                "moov (movie) (0x6d6f6f76)",
                "mvhd (movie header) (0x6d766864)",
            ]
        );
        // ftyp decodes its brand; boxes tile the file
        assert_eq!(nodes.iter().find(|n| n.name == "major_brand").unwrap().detail, "\"isom\"");
        let last = nodes.iter().rfind(|n| n.ty == "struct mp4_box").unwrap();
        assert_eq!(nodes.iter().filter(|n| n.ty == "struct mp4_box").count(), 3);
        assert!(last.end <= data.len());
    }

    #[test]
    fn builtin_der_pattern_walks_the_tlv_tree() {
        // SEQUENCE (long form len 7) { INTEGER 1, OCTET STRING "hi" }
        let data = vec![
            0x30, 0x81, 0x07, // SEQUENCE, length 7
            0x02, 0x01, 0x01, // INTEGER, length 1, value 1
            0x04, 0x02, 0x68, 0x69, // OCTET STRING, length 2, "hi"
        ];
        let nodes = match_and_evaluate(&data).expect("der magic matched");
        assert_eq!(nodes[0].name, "der");
        // tags decode via the enum; constructed vs primitive via `tag & 0x20`
        let tags: Vec<&str> =
            nodes.iter().filter(|n| n.ty == "der_tag").map(|n| n.detail.as_str()).collect();
        assert_eq!(tags, vec!["SEQUENCE (0x30)", "INTEGER (0x2)", "OCTET STRING (0x4)"]);
        // the variable-width length decodes: long form 0x81 0x07 = 7
        let lengths: Vec<&str> =
            nodes.iter().filter(|n| n.ty == "derlen").map(|n| n.detail.as_str()).collect();
        assert_eq!(lengths, vec!["7 (0x7)", "1 (0x1)", "2 (0x2)"]);
        // the SEQUENCE recursed into two children; leaves hold raw bytes
        let seq = nodes.iter().find(|n| n.ty == "struct constructed").unwrap();
        assert_eq!((seq.start, seq.end), (3, data.len()));
        let octet = nodes.iter().rfind(|n| n.name == "data").unwrap();
        assert_eq!(octet.detail, "[68 69]"); // "hi"
    }

    #[test]
    fn short_form_der_length_decodes() {
        // a bare short-form length (< 0x80) reads as itself
        let data = vec![0x30, 0x82, 0x00, 0x03, 0x02, 0x01, 0x05];
        // SEQUENCE len 3 { INTEGER len 1 value 5 }
        let nodes = match_and_evaluate(&data).expect("der matched");
        let inner_len = nodes.iter().filter(|n| n.ty == "derlen").nth(1).unwrap();
        assert_eq!(inner_len.detail, "1 (0x1)");
        let value = nodes.iter().rfind(|n| n.name == "data").unwrap();
        assert_eq!(value.detail, "[05]");
    }

    #[test]
    fn builtin_riff_pattern_parses_wav_with_word_padding() {
        let le16 = |v: u16| v.to_le_bytes();
        let le32 = |v: u32| v.to_le_bytes();
        let mut body = Vec::new();
        body.extend_from_slice(b"WAVE");
        // fmt chunk (16 bytes): PCM, stereo, 44.1kHz, 16-bit
        body.extend_from_slice(b"fmt ");
        body.extend_from_slice(&le32(16));
        body.extend_from_slice(&le16(1)); // PCM
        body.extend_from_slice(&le16(2)); // channels
        body.extend_from_slice(&le32(44100));
        body.extend_from_slice(&le32(176400));
        body.extend_from_slice(&le16(4));
        body.extend_from_slice(&le16(16));
        // an odd-sized JUNK chunk (3 bytes) → one pad byte follows
        body.extend_from_slice(b"JUNK");
        body.extend_from_slice(&le32(3));
        body.extend_from_slice(&[0xaa, 0xbb, 0xcc]);
        body.push(0); // word-align pad
        // data chunk
        let data_at = 8 + body.len();
        body.extend_from_slice(b"data");
        body.extend_from_slice(&le32(4));
        body.extend_from_slice(&[1, 2, 3, 4]);

        let mut data = Vec::new();
        data.extend_from_slice(b"RIFF");
        data.extend_from_slice(&le32(body.len() as u32));
        data.extend_from_slice(&body);

        let nodes = match_and_evaluate(&data).expect("riff magic matched");
        assert_eq!(nodes[0].name, "riff");
        assert_eq!(
            nodes.iter().find(|n| n.name == "form_type").unwrap().detail,
            "WAVE (audio) (0x57415645)"
        );
        // the fmt chunk decodes its audio parameters
        let get = |n: &str| nodes.iter().find(|x| x.name == n).unwrap().detail.clone();
        assert_eq!(get("audio_format"), "PCM (0x1)");
        assert_eq!(get("channels"), "2 (0x2)");
        assert_eq!(get("sample_rate"), "44100 (0xac44)");
        assert_eq!(get("bits_per_sample"), "16 (0x10)");
        // chunks are word-aligned: the pad byte after JUNK pushes `data` to
        // its correct offset, proving `pad: u8[size & 1]` fired
        let data_chunk = nodes
            .iter()
            .find(|n| n.ty == "fourcc" && n.detail.starts_with("data") && n.start == data_at);
        assert!(data_chunk.is_some(), "data chunk lands after the pad byte");
    }

    #[test]
    fn builtin_opentype_pattern_reads_the_table_directory() {
        let be16 = |v: u16| v.to_be_bytes();
        let be32 = |v: u32| v.to_be_bytes();
        let mut data = Vec::new();
        data.extend_from_slice(&be32(0x0001_0000)); // TrueType
        data.extend_from_slice(&be16(1)); // num tables
        data.extend_from_slice(&be16(16)); // search range
        data.extend_from_slice(&be16(0)); // entry selector
        data.extend_from_slice(&be16(0)); // range shift
        // one record: "head" at offset 28, length 4
        data.extend_from_slice(b"head");
        data.extend_from_slice(&be32(0)); // checksum
        data.extend_from_slice(&be32(28)); // offset
        data.extend_from_slice(&be32(4)); // length
        data.extend_from_slice(&[0xca, 0xfe, 0xba, 0xbe]); // the head table bytes

        let nodes = match_and_evaluate(&data).expect("opentype magic matched");
        assert_eq!(nodes[0].name, "opentype");
        // the flavor decodes via the sfnt_version enum
        assert_eq!(
            nodes.iter().find(|n| n.ty == "sfnt_version").unwrap().detail,
            "TrueType (1.0) (0x10000)"
        );
        assert_eq!(nodes.iter().find(|n| n.name == "num_tables").unwrap().detail, "1 (0x1)");
        // the record's tag is the real 4cc, and its table bytes are claimed
        assert_eq!(nodes.iter().find(|n| n.name == "tag").unwrap().detail, "\"head\"");
        let table = nodes.iter().find(|n| n.name == "data").unwrap();
        assert_eq!((table.start, table.end), (28, 32));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn opentype_pattern_survives_a_real_font() {
        // system fonts are plentiful; find any .ttf / .ttc
        let candidates = [
            "/System/Library/Fonts/Monaco.ttf",
            "/System/Library/Fonts/Menlo.ttc",
            "/System/Library/Fonts/Geneva.ttf",
            "/System/Library/Fonts/Helvetica.ttc",
        ];
        let Some(path) = candidates.iter().find(|p| std::path::Path::new(p).exists()) else {
            return;
        };
        let data = std::fs::read(path).unwrap();
        let nodes = match_and_evaluate(&data).expect("real font matched");
        assert!(nodes[0].name.starts_with("opentype"));
        // real fonts carry the standard tables
        assert!(nodes.iter().any(|n| n.name == "tag" && n.detail.contains("cmap")));
        assert!(nodes.iter().all(|n| n.end <= data.len()));
    }

    #[test]
    fn builtin_cfb_pattern_decodes_header_and_root_entry() {
        let le16 = |v: u16| v.to_le_bytes();
        let le32 = |v: u32| v.to_le_bytes();
        let mut header = Vec::new();
        header.extend_from_slice(&[0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1]);
        header.extend_from_slice(&[0u8; 16]); // clsid
        header.extend_from_slice(&le16(0x3e)); // minor version
        header.extend_from_slice(&le16(3)); // major version
        header.extend_from_slice(&le16(0xfffe)); // byte order
        header.extend_from_slice(&le16(9)); // sector shift → 512
        header.extend_from_slice(&le16(6)); // mini sector shift
        header.extend_from_slice(&[0u8; 6]); // reserved
        header.extend_from_slice(&le32(0)); // num dir sectors
        header.extend_from_slice(&le32(1)); // num FAT sectors
        header.extend_from_slice(&le32(0)); // first dir sector → offset 512
        header.extend_from_slice(&le32(0)); // transaction sig
        header.extend_from_slice(&le32(4096)); // mini cutoff
        header.extend_from_slice(&le32(0xffff_fffe)); // first minifat
        header.extend_from_slice(&le32(0)); // num minifat
        header.extend_from_slice(&le32(0xffff_fffe)); // first difat
        header.extend_from_slice(&le32(0)); // num difat
        header.extend_from_slice(&le32(0)); // DIFAT[0] = FAT sector 0
        for _ in 1..109 {
            header.extend_from_slice(&le32(0xffff_ffff));
        }
        assert_eq!(header.len(), 512);

        // directory sector (512 bytes = 4 entries); entry 0 = Root Entry
        let mut dir = vec![0u8; 512];
        for (i, u) in "Root Entry".encode_utf16().enumerate() {
            dir[i * 2..i * 2 + 2].copy_from_slice(&u.to_le_bytes());
        }
        dir[64..66].copy_from_slice(&le16(22)); // name length
        dir[66] = 5; // object type: root storage
        // entries 1..3 stay type 0 (unused)

        let mut data = header;
        data.extend_from_slice(&dir);

        let nodes = match_and_evaluate(&data).expect("cfb magic matched");
        assert_eq!(nodes[0].name, "cfb");
        let get = |n: &str| nodes.iter().find(|x| x.name == n).unwrap().detail.clone();
        assert_eq!(get("major_version"), "v3 (512-byte sectors) (0x3)");
        assert_eq!(get("sector_shift"), "9 (0x9)");
        // the directory is located via (first_dir_sector + 1) << sector_shift
        let dir_node = nodes.iter().find(|n| n.name == "directory").unwrap();
        assert_eq!(dir_node.start, 512);
        // the first directory entry is the root storage, name decoded
        let root = nodes.iter().find(|n| n.ty == "entry_type").unwrap();
        assert_eq!(root.detail, "root storage (0x5)");
        assert!(nodes.iter().any(|n| n.name == "name" && n.detail.contains("R·o·o·t")));
        // four entries in the 512-byte sector
        assert_eq!(nodes.iter().filter(|n| n.ty == "struct dir_entry").count(), 4);
    }

    #[test]
    fn builtin_bplist_pattern_locates_the_trailer_via_end() {
        let mut trailer = Vec::new();
        trailer.extend_from_slice(&[0u8; 5]); // unused
        trailer.push(0); // sort version
        trailer.push(1); // offset int size
        trailer.push(1); // object ref size
        trailer.extend_from_slice(&1u64.to_be_bytes()); // num objects
        trailer.extend_from_slice(&0u64.to_be_bytes()); // top object
        trailer.extend_from_slice(&9u64.to_be_bytes()); // offset table offset
        assert_eq!(trailer.len(), 32);

        let mut data = b"bplist00".to_vec();
        data.push(0x2a); // object at offset 8
        data.push(0x08); // offset table entry at offset 9 → object 0
        data.extend_from_slice(&trailer); // trailer at offset 10

        let nodes = match_and_evaluate(&data).expect("bplist magic matched");
        assert_eq!(nodes[0].name, "bplist");
        assert_eq!(nodes.iter().find(|n| n.name == "magic").unwrap().detail, "\"bplist00\"");
        // the trailer is found at $end - 32 even though the parser walked
        // forward from the magic
        let trailer_node = nodes.iter().find(|n| n.name == "trailer").unwrap();
        assert_eq!(trailer_node.start, data.len() - 32);
        let get = |n: &str| nodes.iter().find(|x| x.name == n).unwrap().detail.clone();
        assert_eq!(get("num_objects"), "1 (0x1)");
        assert_eq!(get("offset_int_size"), "1 (0x1)");
        assert_eq!(get("offset_table_offset"), "9 (0x9)");
        // the offset table is then located via the trailer's pointer
        let table = nodes.iter().find(|n| n.name == "offset_table").unwrap();
        assert_eq!(table.start, 9);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn bplist_pattern_survives_a_real_plist() {
        // plutil ships with macOS; convert an XML plist to binary1
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.plist");
        std::fs::write(
            &path,
            "<?xml version=\"1.0\"?>\n<plist version=\"1.0\"><dict>\
             <key>name</key><string>vibin</string>\
             <key>count</key><integer>42</integer></dict></plist>",
        )
        .unwrap();
        let ok =
            std::process::Command::new("plutil").args(["-convert", "binary1"]).arg(&path).status();
        let Ok(status) = ok else { return };
        if !status.success() {
            return;
        }
        let data = std::fs::read(&path).unwrap();
        if !data.starts_with(b"bplist00") {
            return;
        }
        let nodes = match_and_evaluate(&data).expect("real bplist matched");
        assert_eq!(nodes[0].name, "bplist");
        // a dict with two entries → at least 5 objects (dict + 2 keys + 2 vals)
        let num = nodes.iter().find(|n| n.name == "num_objects").unwrap();
        assert!(
            num.detail.starts_with("5 ")
                || num.detail.contains("(0x5)")
                || num.detail.contains("(0x6)")
        );
        assert!(nodes.iter().all(|n| n.end <= data.len()));
    }

    #[test]
    fn type_aliases_and_shared_structs() {
        // aliases resolve to their scalar; a prelude struct (GUID) is usable
        let src = r#"
            type DWORD = u32;
            type WORD = u16;
            type BYTE = u8;
            struct GUID { data1: DWORD; data2: WORD; data3: WORD; data4: BYTE[8]; }
            format x { magic = "aa"; root = f; }
            struct f {
                tag: BYTE;
                count: DWORD;
                id: GUID;
            }
        "#;
        let formats = parse(src).unwrap();
        let mut data = vec![0xaa]; // tag
        data.extend_from_slice(&7u32.to_le_bytes()); // count
        data.extend_from_slice(&[0x11; 16]); // guid
        let nodes = evaluate(&formats[0], &data);
        // DWORD field displays like a u32
        let count = nodes.iter().find(|n| n.name == "count").unwrap();
        assert_eq!((count.ty.as_str(), count.detail.as_str()), ("u32", "7 (0x7)"));
        // GUID expands into its member fields
        let guid = nodes.iter().find(|n| n.name == "id").unwrap();
        assert_eq!(guid.ty, "struct GUID");
        assert!(nodes.iter().any(|n| n.name == "data1" && n.ty == "u32"));
        assert!(nodes.iter().any(|n| n.name == "data4" && n.ty == "u8[8]"));
    }

    #[test]
    fn cfb_pattern_uses_win32_prelude_types() {
        // the CFB pattern now references GUID / FILETIME / DWORD from the
        // shared win32 prelude — confirm it still parses and matches
        let mut data = vec![0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1];
        data.extend_from_slice(&[0u8; 16]); // clsid (GUID)
        data.extend_from_slice(&0x3eu16.to_le_bytes());
        data.extend_from_slice(&3u16.to_le_bytes());
        data.extend_from_slice(&0xfffeu16.to_le_bytes());
        data.extend_from_slice(&9u16.to_le_bytes()); // sector shift
        data.resize(76, 0);
        data.extend_from_slice(&0u32.to_le_bytes()); // first_dir_sector at the right spot
        data.resize(512, 0);
        data.resize(1024, 0); // one directory sector of zeros
        // fix first_dir_sector (offset 48) = 0 → directory at 512
        let nodes = match_and_evaluate(&data).expect("cfb matched with prelude types");
        assert_eq!(nodes[0].name, "cfb");
        // the clsid decodes as a GUID struct now
        assert!(nodes.iter().any(|n| n.name == "clsid" && n.ty == "struct GUID"));
        assert!(nodes.iter().any(|n| n.name == "data1"));
    }

    #[test]
    fn builtin_elf_pattern_parses_and_matches() {
        let formats = builtin_formats();
        let elf = formats.iter().find(|f| f.name == "elf").expect("elf pattern");
        assert_eq!(elf.magic, vec![0x7f, b'E', b'L', b'F']);

        // minimal ELF64 header: e_ident + fields, no phdrs/shdrs
        let mut data = vec![0u8; 64];
        data[0..4].copy_from_slice(b"\x7fELF");
        data[4] = 2; // ELFCLASS64
        data[5] = 1; // little-endian
        data[16] = 2; // ET_EXEC
        data[18] = 0x3e; // EM_X86_64
        data[54] = 56; // phentsize
        let nodes = match_and_evaluate(&data).expect("magic matched");
        let machine = nodes.iter().find(|n| n.name == "machine").unwrap();
        assert_eq!(machine.detail, "x86-64 (0x3e)");
        let ty = nodes.iter().find(|n| n.name == "type").unwrap();
        assert!(ty.detail.starts_with("EXEC"));
        // phnum is 0 → empty table located at phoff 0
        let phdrs = nodes.iter().find(|n| n.name == "phdrs").unwrap();
        assert_eq!(phdrs.ty, "phdr[0]");
    }
}
