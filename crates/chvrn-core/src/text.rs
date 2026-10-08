use std::ops::Range;

pub fn line_range(text: &str, line: usize) -> Option<Range<usize>> {
    let bytes = text.as_bytes();
    let mut remaining = line;
    let mut start = 0;
    let mut position = 0;
    while position < bytes.len() {
        if matches!(bytes[position], b'\r' | b'\n') {
            if remaining == 0 {
                return Some(start..position);
            }
            let carriage_return = bytes[position] == b'\r';
            position += 1;
            if carriage_return && bytes.get(position) == Some(&b'\n') {
                position += 1;
            }
            start = position;
            remaining -= 1;
        } else {
            position += 1;
        }
    }
    (remaining == 0).then_some(start..bytes.len())
}
