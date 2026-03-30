use crate::error::RuntimeError;
use wql_ir::WireType;

/// A single protobuf field as read from the wire.
#[derive(Debug, PartialEq)]
pub(crate) struct WireField<'a> {
    /// Raw tag varint bytes (for verbatim COPY).
    pub tag_bytes: &'a [u8],
    /// Decoded field number.
    pub field_num: u32,
    /// Decoded wire type.
    pub wire_type: WireType,
    /// Raw value bytes (everything after the tag, up to end of this field).
    /// For LEN: includes the length prefix varint + payload.
    pub value_bytes: &'a [u8],
    /// For LEN fields: payload bytes only (excludes length prefix).
    /// For non-LEN fields: empty slice.
    pub len_payload: &'a [u8],
}

/// Read a varint from `buf` starting at `pos`. Returns `(value, bytes_consumed)`.
pub(crate) fn read_varint(buf: &[u8], pos: usize) -> Result<(u64, usize), RuntimeError> {
    let mut value: u64 = 0;
    let mut shift: u32 = 0;
    let mut i = pos;

    loop {
        if i >= buf.len() {
            return Err(RuntimeError::MalformedInput);
        }
        let byte = buf[i];
        // Varint must not exceed 10 bytes (64 bits).
        if shift >= 70 {
            return Err(RuntimeError::MalformedInput);
        }
        // On the 10th byte (shift == 63), only bit 0 is valid — reject
        // non-canonical overlong encodings where upper bits would be discarded.
        if shift == 63 && byte & 0xFE != 0 {
            return Err(RuntimeError::MalformedInput);
        }
        value |= u64::from(byte & 0x7F) << shift;
        i += 1;
        if byte & 0x80 == 0 {
            return Ok((value, i - pos));
        }
        shift += 7;
    }
}

/// Iterator over wire fields in a protobuf message byte slice.
pub(crate) struct WireScanner<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> WireScanner<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
}

impl<'a> Iterator for WireScanner<'a> {
    type Item = Result<WireField<'a>, RuntimeError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.buf.len() {
            return None;
        }

        match self.scan_field() {
            Ok(field) => Some(Ok(field)),
            Err(e) => {
                // Park at end so subsequent calls return None.
                self.pos = self.buf.len();
                Some(Err(e))
            }
        }
    }
}

impl<'a> WireScanner<'a> {
    fn scan_field(&mut self) -> Result<WireField<'a>, RuntimeError> {
        let tag_start = self.pos;
        let (tag, tag_len) = read_varint(self.buf, self.pos)?;
        self.pos += tag_len;

        let wire_type_raw = (tag & 0x07) as u8;
        let field_num =
            u32::try_from(tag >> 3).map_err(|_| RuntimeError::MalformedInput)?;
        let wire_type = WireType::from_u8(wire_type_raw).ok_or(RuntimeError::MalformedInput)?;

        let tag_bytes = &self.buf[tag_start..self.pos];
        let value_start = self.pos;

        match wire_type {
            WireType::Varint => {
                let (_, consumed) = read_varint(self.buf, self.pos)?;
                self.pos += consumed;
            }
            WireType::I64 => {
                if self.pos + 8 > self.buf.len() {
                    return Err(RuntimeError::MalformedInput);
                }
                self.pos += 8;
            }
            WireType::I32 => {
                if self.pos + 4 > self.buf.len() {
                    return Err(RuntimeError::MalformedInput);
                }
                self.pos += 4;
            }
            WireType::Len => {
                let (len, len_varint_size) = read_varint(self.buf, self.pos)?;
                let len =
                    usize::try_from(len).map_err(|_| RuntimeError::MalformedInput)?;
                let payload_start = self.pos + len_varint_size;
                if payload_start + len > self.buf.len() {
                    return Err(RuntimeError::MalformedInput);
                }
                self.pos = payload_start + len;

                return Ok(WireField {
                    tag_bytes,
                    field_num,
                    wire_type,
                    value_bytes: &self.buf[value_start..self.pos],
                    len_payload: &self.buf[payload_start..self.pos],
                });
            }
        }

        Ok(WireField {
            tag_bytes,
            field_num,
            wire_type,
            value_bytes: &self.buf[value_start..self.pos],
            len_payload: &[],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::*;

    fn scan_all(buf: &[u8]) -> Result<alloc::vec::Vec<WireField<'_>>, RuntimeError> {
        WireScanner::new(buf).collect()
    }

    #[test]
    fn wire_scan_varint() {
        // Field 1, varint 150
        let buf = encode_varint_field(1, 150);
        let fields = scan_all(&buf).unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].field_num, 1);
        assert_eq!(fields[0].wire_type, WireType::Varint);
        // 150 encodes as [0x96, 0x01]
        assert_eq!(fields[0].value_bytes, &[0x96, 0x01]);
        assert!(fields[0].len_payload.is_empty());
    }

    #[test]
    fn wire_scan_i32() {
        let buf = encode_fixed32_field(2, 0x1234_5678);
        let fields = scan_all(&buf).unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].field_num, 2);
        assert_eq!(fields[0].wire_type, WireType::I32);
        assert_eq!(fields[0].value_bytes, &0x1234_5678_u32.to_le_bytes());
    }

    #[test]
    fn wire_scan_i64() {
        let buf = encode_fixed64_field(3, 0x0123_4567_89AB_CDEF);
        let fields = scan_all(&buf).unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].field_num, 3);
        assert_eq!(fields[0].wire_type, WireType::I64);
        assert_eq!(
            fields[0].value_bytes,
            &0x0123_4567_89AB_CDEF_u64.to_le_bytes()
        );
    }

    #[test]
    fn wire_scan_len() {
        let buf = encode_len_field(4, b"hello");
        let fields = scan_all(&buf).unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].field_num, 4);
        assert_eq!(fields[0].wire_type, WireType::Len);
        assert_eq!(fields[0].len_payload, b"hello");
        // value_bytes includes the length prefix varint (1 byte for len=5) + payload.
        assert_eq!(fields[0].value_bytes.len(), 1 + 5);
        assert_eq!(&fields[0].value_bytes[1..], b"hello");
    }

    #[test]
    fn wire_scan_multi() {
        let mut buf = encode_varint_field(1, 42);
        buf.extend_from_slice(&encode_len_field(2, b"world"));
        buf.extend_from_slice(&encode_fixed32_field(3, 99));

        let fields = scan_all(&buf).unwrap();
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].field_num, 1);
        assert_eq!(fields[0].wire_type, WireType::Varint);
        assert_eq!(fields[1].field_num, 2);
        assert_eq!(fields[1].wire_type, WireType::Len);
        assert_eq!(fields[1].len_payload, b"world");
        assert_eq!(fields[2].field_num, 3);
        assert_eq!(fields[2].wire_type, WireType::I32);
    }

    #[test]
    fn wire_scan_empty() {
        let fields = scan_all(&[]).unwrap();
        assert!(fields.is_empty());
    }

    #[test]
    fn wire_scan_truncated_tag() {
        // 0x80 is a continuation byte with no following byte.
        let result = scan_all(&[0x80]);
        assert_eq!(result, Err(RuntimeError::MalformedInput));
    }

    #[test]
    fn wire_scan_truncated_value() {
        // LEN field (field 1, wire type 2), length=10, but only 3 payload bytes.
        let mut buf = encode_tag(1, 2);
        buf.extend_from_slice(&encode_varint(10));
        buf.extend_from_slice(&[0x00, 0x00, 0x00]);
        let result = scan_all(&buf);
        assert_eq!(result, Err(RuntimeError::MalformedInput));
    }

    #[test]
    fn wire_scan_unknown_wire_type() {
        // Wire type 6 is not valid.
        let buf = encode_tag(1, 6);
        let result = scan_all(&buf);
        assert_eq!(result, Err(RuntimeError::MalformedInput));
    }

    #[test]
    fn wire_scan_overlong_varint() {
        // 11 continuation bytes — exceeds the 10-byte varint limit.
        let result = scan_all(&[0x80; 11]);
        assert_eq!(result, Err(RuntimeError::MalformedInput));
    }

    #[test]
    fn wire_scan_noncanonical_10th_byte() {
        // 10-byte varint where the 10th byte has upper bits set (non-canonical).
        let mut buf = [0x80u8; 10];
        buf[9] = 0x02; // bit 1 set → data loss on decode
        let result = scan_all(&buf);
        assert_eq!(result, Err(RuntimeError::MalformedInput));
    }
}
