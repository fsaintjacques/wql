use alloc::vec::Vec;

/// Encode a protobuf tag as varint bytes.
pub fn encode_tag(field_num: u32, wire_type: u8) -> Vec<u8> {
    let tag = (field_num << 3) | u32::from(wire_type);
    encode_varint(u64::from(tag))
}

/// Encode an unsigned varint.
pub fn encode_varint(mut v: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    loop {
        let mut byte = (v & 0x7F) as u8;
        v >>= 7;
        if v != 0 {
            byte |= 0x80;
        }
        buf.push(byte);
        if v == 0 {
            break;
        }
    }
    buf
}

/// Encode a varint field: tag + varint value.
pub fn encode_varint_field(field_num: u32, value: u64) -> Vec<u8> {
    let mut buf = encode_tag(field_num, 0);
    buf.extend_from_slice(&encode_varint(value));
    buf
}

/// Encode a sint (zigzag) field: tag + zigzag varint.
pub fn encode_sint_field(field_num: u32, value: i64) -> Vec<u8> {
    let zigzag = ((value << 1) ^ (value >> 63)) as u64;
    let mut buf = encode_tag(field_num, 0);
    buf.extend_from_slice(&encode_varint(zigzag));
    buf
}

/// Encode a length-delimited field: tag + length varint + payload.
pub fn encode_len_field(field_num: u32, payload: &[u8]) -> Vec<u8> {
    let mut buf = encode_tag(field_num, 2);
    buf.extend_from_slice(&encode_varint(payload.len() as u64));
    buf.extend_from_slice(payload);
    buf
}

/// Encode a fixed32 field: tag + 4 LE bytes.
pub fn encode_fixed32_field(field_num: u32, value: u32) -> Vec<u8> {
    let mut buf = encode_tag(field_num, 5);
    buf.extend_from_slice(&value.to_le_bytes());
    buf
}

/// Encode a fixed64 field: tag + 8 LE bytes.
pub fn encode_fixed64_field(field_num: u32, value: u64) -> Vec<u8> {
    let mut buf = encode_tag(field_num, 1);
    buf.extend_from_slice(&value.to_le_bytes());
    buf
}
