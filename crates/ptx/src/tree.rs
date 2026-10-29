//! Record walk. Payload bytes stay in the revealed buffer; a record only stores a range.

use crate::Error;

const MAX_DEPTH: usize = 32;
const MAX_RECORDS: usize = 100_000;

#[derive(Debug)]
pub(crate) struct Node {
    pub code: u16,
    pub body_at: usize,
    pub body_len: usize,
    pub span: usize,
    pub children: Vec<Node>,
}

impl Node {
    pub(crate) fn body<'a>(&self, data: &'a [u8]) -> &'a [u8] {
        data.get(self.body_at..self.body_at.saturating_add(self.body_len)).unwrap_or(&[])
    }
}

pub(crate) fn walk(data: &[u8], little_endian: bool) -> Result<Vec<Node>, Error> {
    if data.len() < 20 {
        return Err(Error::new("shorter than the 20-byte header"));
    }
    let mut count = 0usize;
    let mut roots = Vec::new();
    let mut i = 20usize;
    while i < data.len() {
        if data[i] != 0x5a {
            i += 1;
            continue;
        }
        match take(data, i, data.len(), little_endian, 0, &mut count)? {
            Some(node) => {
                i += node.span.max(1);
                roots.push(node);
            }
            None => i += 1,
        }
    }
    Ok(roots)
}

fn take(data: &[u8], pos: usize, limit: usize, little_endian: bool, depth: usize, count: &mut usize) -> Result<Option<Node>, Error> {
    if depth > MAX_DEPTH {
        return Err(Error::new("record nesting is deeper than 32"));
    }
    if pos >= limit || data.get(pos) != Some(&0x5a) || pos + 7 > limit {
        return Ok(None);
    }
    let class = match u16_at(data, pos + 1, little_endian) {
        Some(v) => v,
        None => return Ok(None),
    };
    if class > 0xff {
        return Ok(None);
    }
    let size = match u32_at(data, pos + 3, little_endian) {
        Some(v) => v as usize,
        None => return Ok(None),
    };
    if size == 1 || pos + 7 + size > limit {
        return Ok(None);
    }
    *count += 1;
    if *count > MAX_RECORDS {
        return Err(Error::new("more than 100000 records"));
    }
    if pos == 0x14 && class == 1 && size == 4 {
        return Ok(Some(Node { code: 0, body_at: pos + 7, body_len: 4, span: 11, children: Vec::new() }));
    }
    if size < 2 {
        return Ok(Some(Node { code: 0, body_at: pos + 7, body_len: 0, span: 7, children: Vec::new() }));
    }
    let code = match u16_at(data, pos + 7, little_endian) {
        Some(v) => v,
        None => return Ok(None),
    };
    let body_at = pos + 9;
    let body_len = size - 2;
    let mut children = Vec::new();
    if !flat(code) {
        let skip = lead(data, code, body_at, body_len, little_endian);
        let mut cursor = body_at + skip.min(body_len);
        let end = body_at + body_len;
        while cursor < end {
            if data.get(cursor) != Some(&0x5a) {
                cursor += 1;
                continue;
            }
            match take(data, cursor, end, little_endian, depth + 1, count)? {
                Some(child) => {
                    cursor += child.span.max(1);
                    children.push(child);
                }
                None => cursor += 1,
            }
        }
    }
    Ok(Some(Node { code, body_at, body_len, span: 7 + size, children }))
}

fn flat(code: u16) -> bool {
    matches!(
        code,
        0x0002
            | 0x1001
            | 0x1014
            | 0x1017
            | 0x1028
            | 0x103a
            | 0x104f
            | 0x2000
            | 0x204d
            | 0x2077
            | 0x251a
            | 0x2602
            | 0x260a
            | 0x260e
            | 0x2628
            | 0x262f
            | 0x2637
    )
}

fn lead(data: &[u8], code: u16, body_at: usize, body_len: usize, little_endian: bool) -> usize {
    if matches!(code, 0x1004 | 0x100b | 0x1015 | 0x1018 | 0x1054 | 0x2030 | 0x2603 | 0x262a) {
        return 4.min(body_len);
    }
    if code == 0x1052
        && let Some(after) = counted_end(data, body_at, little_endian)
        && after + 4 <= body_at + body_len
    {
        return after + 4 - body_at;
    }
    0
}

fn counted_end(data: &[u8], at: usize, little_endian: bool) -> Option<usize> {
    let len = u32_at(data, at, little_endian)? as usize;
    if len > 16 * 1024 {
        return None;
    }
    at.checked_add(4)?.checked_add(len).filter(|end| *end <= data.len())
}

pub(crate) fn u16_at(data: &[u8], at: usize, little_endian: bool) -> Option<u16> {
    let b = data.get(at..at + 2)?;
    Some(if little_endian { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) })
}

pub(crate) fn u32_at(data: &[u8], at: usize, little_endian: bool) -> Option<u32> {
    let b = data.get(at..at + 4)?;
    Some(if little_endian { u32::from_le_bytes([b[0], b[1], b[2], b[3]]) } else { u32::from_be_bytes([b[0], b[1], b[2], b[3]]) })
}

pub(crate) fn u64_at(data: &[u8], at: usize, little_endian: bool) -> Option<u64> {
    let b = data.get(at..at + 8)?;
    let bytes = [b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]];
    Some(if little_endian { u64::from_le_bytes(bytes) } else { u64::from_be_bytes(bytes) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_past_the_buffer_is_not_a_record() {
        let mut data = vec![0u8; 40];
        data[20] = 0x5a;
        data[23] = 0xff;
        data[24] = 0xff;
        let nodes = walk(&data, true).unwrap();
        assert!(nodes.is_empty());
    }
}
