// ASN.1 DER — the tag/length/value encoding behind X.509 certificates
// (.der .cer .crt), private keys (.key), and PKCS#12 (.pfx/.p12) and
// PKCS#7 (.p7b) containers. Each TLV's tag bit 0x20 marks a "constructed"
// value (children) vs a "primitive" one (raw bytes). Length uses DER's
// variable-width encoding (`derlen`). DER files have no fixed magic, so we
// dispatch on a top-level SEQUENCE with a long-form length (0x30 0x8n) —
// which covers essentially all real certs/keys/containers (they exceed
// 128 bytes). Primitive values (INTEGER, OID, strings) are shown raw, not
// decoded. See src/pattern.rs for the pattern language reference.

format der {
    magic = "30 81"; // SEQUENCE, 1-byte length
    root = tlv;
}

format der {
    magic = "30 82"; // SEQUENCE, 2-byte length
    root = tlv;
}

format der {
    magic = "30 83"; // SEQUENCE, 3-byte length (large PKCS#12)
    root = tlv;
}

enum der_tag : u8 {
    0x01 = "BOOLEAN",
    0x02 = "INTEGER",
    0x03 = "BIT STRING",
    0x04 = "OCTET STRING",
    0x05 = "NULL",
    0x06 = "OBJECT IDENTIFIER",
    0x0a = "ENUMERATED",
    0x0c = "UTF8String",
    0x10 = "SEQUENCE",
    0x11 = "SET",
    0x13 = "PrintableString",
    0x14 = "T61String",
    0x16 = "IA5String",
    0x17 = "UTCTime",
    0x18 = "GeneralizedTime",
    0x1e = "BMPString",
    0x30 = "SEQUENCE",
    0x31 = "SET",
    0xa0 = "[0] (context)",
    0xa1 = "[1] (context)",
    0xa2 = "[2] (context)",
    0xa3 = "[3] (context)",
    0xa4 = "[4] (context)",
}

struct tlv {
    tag: der_tag;
    length: derlen;
    // bit 0x20 of the tag distinguishes constructed from primitive
    content: match tag & 0x20 {
        0x20 = constructed,
        _ = primitive,
    } span length;
}

struct constructed {
    children: tlv[];
}

struct primitive {
    data: u8[];
}
