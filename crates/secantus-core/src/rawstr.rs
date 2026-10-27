//! mongod's byte-level string rules, for strings whose bytes are not UTF-8
//! (RM-222).
//!
//! mongod keeps a BSON string as the bytes it was sent and reads them byte by
//! byte: a code point is counted at every byte that is not a continuation
//! byte, and a code point's length is read off its first byte's leading one
//! bits, whatever follows. Its regular expressions are PCRE2 in UTF mode,
//! which checks the subject and the pattern first and names the first fault
//! it finds. The functions here reproduce those rules so an operator given an
//! invalid string answers what mongod answers; for valid UTF-8 they agree
//! with the ordinary `str` operations.

/// `str::isUTF8ContinuationByte`: `10xxxxxx`.
pub fn is_continuation(b: u8) -> bool {
    b & 0xC0 == 0x80
}

/// `str::getCodePointLength`: the number of leading one bits of a lead byte,
/// or 1 for an ASCII byte. 8 for `0xff`, so a walk that steps by it can run
/// past the end of the string, as mongod's does.
pub fn code_point_length(b: u8) -> usize {
    if b < 0x80 {
        1
    } else {
        b.leading_ones() as usize
    }
}

/// `$strLenCP`: one code point per byte that is not a continuation byte.
pub fn len_cp(bytes: &[u8]) -> usize {
    bytes.iter().filter(|b| !is_continuation(**b)).count()
}

/// The text PCRE2 gives for the first UTF-8 fault in `bytes` (its
/// `valid_utf` check, the messages of `pcre2_error.c`), or `None` when the
/// bytes are valid UTF-8.
pub fn pcre2_utf8_error(bytes: &[u8]) -> Option<&'static str> {
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        i += 1;
        if c < 0x80 {
            continue;
        }
        if c < 0xC0 {
            return Some("UTF-8 error: isolated byte with 0x80 bit set");
        }
        if c >= 0xFE {
            return Some("UTF-8 error: illegal byte (0xfe or 0xff)");
        }
        // Additional bytes the lead byte announces (PCRE2's utf8_table4).
        let ab = match c {
            0xC0..=0xDF => 1,
            0xE0..=0xEF => 2,
            0xF0..=0xF7 => 3,
            0xF8..=0xFB => 4,
            _ => 5,
        };
        let left = bytes.len() - i;
        if left < ab {
            return Some(match ab - left {
                1 => "UTF-8 error: 1 byte missing at end",
                2 => "UTF-8 error: 2 bytes missing at end",
                3 => "UTF-8 error: 3 bytes missing at end",
                4 => "UTF-8 error: 4 bytes missing at end",
                _ => "UTF-8 error: 5 bytes missing at end",
            });
        }
        let d = bytes[i];
        if !is_continuation(d) {
            return Some("UTF-8 error: byte 2 top bits not 0x80");
        }
        const TOP: [&str; 4] = [
            "UTF-8 error: byte 3 top bits not 0x80",
            "UTF-8 error: byte 4 top bits not 0x80",
            "UTF-8 error: byte 5 top bits not 0x80",
            "UTF-8 error: byte 6 top bits not 0x80",
        ];
        for (k, msg) in TOP.iter().enumerate().take(ab - 1) {
            if !is_continuation(bytes[i + 1 + k]) {
                return Some(msg);
            }
        }
        match ab {
            1 if c & 0x3E == 0 => return Some("UTF-8 error: overlong 2-byte sequence"),
            2 if c == 0xE0 && d & 0x20 == 0 => {
                return Some("UTF-8 error: overlong 3-byte sequence")
            }
            2 if c == 0xED && d >= 0xA0 => {
                return Some("UTF-8 error: code points 0xd800-0xdfff are not defined")
            }
            3 if c == 0xF0 && d & 0x30 == 0 => {
                return Some("UTF-8 error: overlong 4-byte sequence")
            }
            3 if c > 0xF4 || (c == 0xF4 && d > 0x8F) => {
                return Some("UTF-8 error: code points greater than 0x10ffff are not defined")
            }
            4 if c == 0xF8 && d & 0x38 == 0 => {
                return Some("UTF-8 error: overlong 5-byte sequence")
            }
            5 if c == 0xFC && d & 0x3C == 0 => {
                return Some("UTF-8 error: overlong 6-byte sequence")
            }
            _ => {}
        }
        if ab == 4 {
            return Some("UTF-8 error: 5-byte character is not allowed (RFC 3629)");
        }
        if ab == 5 {
            return Some("UTF-8 error: 6-byte character is not allowed (RFC 3629)");
        }
        i += ab;
    }
    None
}

/// `$indexOfCP` over bytes, as mongod walks them: code points are counted by
/// [`code_point_length`] steps from the start, and `token` is compared byte
/// for byte at each code point's first byte. `start` / `end` are code-point
/// indexes, already validated as nonnegative; `end` is `None` when absent.
/// A step that lands on a continuation byte is mongod's 40095 (measured
/// 8.2.11, 2026-10-27: `"a\x80"` and `"\x80abc"` both refuse).
pub fn index_of_cp(
    input: &[u8],
    token: &[u8],
    start: usize,
    end: Option<usize>,
) -> Result<i32, (i32, &'static str)> {
    // The input's length in code points, and the byte the search starts at.
    let mut length = 0usize;
    let mut start_byte = None;
    let mut at = 0usize;
    while at < input.len() {
        if is_continuation(input[at]) {
            return Err((40095, "$indexOfCP found bad UTF-8 in the input"));
        }
        if length == start {
            start_byte = Some(at);
        }
        at += code_point_length(input[at]);
        length += 1;
    }
    let end = end.map_or(length, |e| e.min(length));
    if start > end {
        return Ok(-1);
    }
    if token.is_empty() {
        return Ok(start as i32);
    }
    let Some(mut at) = start_byte else {
        return Ok(-1);
    };
    let mut cp = start;
    while cp < end && at < input.len() {
        if input[at..].starts_with(token) {
            return Ok(cp as i32);
        }
        at += code_point_length(input[at]);
        cp += 1;
    }
    Ok(-1)
}

/// `$indexOfBytes` over bytes: the first byte offset in `[start, end)` where
/// `token` begins (and fits), or -1.
pub fn index_of_bytes(input: &[u8], token: &[u8], start: usize, end: Option<usize>) -> i32 {
    let end = end.map_or(input.len(), |e| e.min(input.len()));
    if start > end {
        return -1;
    }
    let hay = &input[start.min(input.len())..end];
    if token.is_empty() {
        return start as i32;
    }
    hay.windows(token.len())
        .position(|w| w == token)
        .map_or(-1, |p| (start + p) as i32)
}

/// `$split` over bytes.
pub fn split(input: &[u8], sep: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut from = 0;
    let mut i = 0;
    while i + sep.len() <= input.len() {
        if &input[i..i + sep.len()] == sep {
            out.push(input[from..i].to_vec());
            i += sep.len();
            from = i;
        } else {
            i += 1;
        }
    }
    out.push(input[from..].to_vec());
    out
}

/// `$replaceOne` / `$replaceAll` over bytes.
pub fn replace(input: &[u8], find: &[u8], with: &[u8], all: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    let mut done = false;
    if find.is_empty() {
        // An empty `find` matches before every byte for `$replaceAll` and
        // once at the start for `$replaceOne`, as `std::string::find` does.
        if !all {
            out.extend_from_slice(with);
            out.extend_from_slice(input);
            return out;
        }
        for b in input {
            out.extend_from_slice(with);
            out.push(*b);
        }
        out.extend_from_slice(with);
        return out;
    }
    while i < input.len() {
        if !done && input[i..].starts_with(find) {
            out.extend_from_slice(with);
            i += find.len();
            done = !all;
        } else {
            out.push(input[i]);
            i += 1;
        }
    }
    out
}

/// `ExpressionTrim::numberOfBytesForCodePoint`.
fn trim_code_point_len(b: u8) -> usize {
    if b & 0xF8 == 0xF0 {
        4
    } else if b & 0xF0 == 0xE0 {
        3
    } else if b & 0xE0 == 0xC0 {
        2
    } else {
        1
    }
}

/// `$trim`'s `chars`, split into code points as mongod splits them, or its
/// refusal: (code, the message after `Failed to parse "chars" argument to
/// <op>: `).
pub fn trim_code_points(chars: &[u8]) -> Result<Vec<&[u8]>, (i32, &'static str)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if is_continuation(chars[i]) {
            return Err((
                5156305,
                "Detected invalid UTF-8. Got continuation byte when expecting the start of a \
                 new code point.",
            ));
        }
        let n = trim_code_point_len(chars[i]);
        if i + n > chars.len() {
            return Err((
                5156304,
                "Detected invalid UTF-8. Missing expected continuation byte at end of string.",
            ));
        }
        out.push(&chars[i..i + n]);
        i += n;
    }
    Ok(out)
}

/// Trim `points` from the left and/or right of `input`, byte for byte.
pub fn trim<'a>(input: &'a [u8], points: &[&[u8]], left: bool, right: bool) -> &'a [u8] {
    let mut s = 0;
    if left {
        while let Some(p) = points
            .iter()
            .find(|p| !p.is_empty() && input[s..].starts_with(p))
        {
            s += p.len();
        }
    }
    let mut e = input.len();
    if right {
        while let Some(p) = points
            .iter()
            .find(|p| !p.is_empty() && e - s >= p.len() && input[s..e].ends_with(p))
        {
            e -= p.len();
        }
    }
    &input[s..e]
}

/// `$substrCP` over bytes, mongod's walk and its refusals.
pub fn substr_cp(input: &[u8], start: usize, length: usize) -> Result<&[u8], (i32, &'static str)> {
    const INVALID: &str = "$substrCP: invalid UTF-8 string";
    let mut s = 0;
    for _ in 0..start {
        if s >= input.len() {
            return Ok(&[]);
        }
        if is_continuation(input[s]) {
            return Err((34456, INVALID));
        }
        let n = code_point_length(input[s]);
        if n > 4 {
            return Err((34457, INVALID));
        }
        s += n;
    }
    let mut e = s;
    for _ in 0..length {
        if e >= input.len() {
            break;
        }
        if is_continuation(input[e]) {
            return Err((34458, INVALID));
        }
        let n = code_point_length(input[e]);
        if n > 4 {
            return Err((34459, INVALID));
        }
        e += n;
    }
    Ok(input
        .get(s.min(input.len())..e.min(input.len()))
        .unwrap_or(&[]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcre2_names_the_first_fault() {
        assert_eq!(pcre2_utf8_error("é€😀".as_bytes()), None);
        let cases: [(&[u8], &str); 12] = [
            (b"\xff\xfe", "illegal byte (0xfe or 0xff)"),
            (b"a\x80", "isolated byte with 0x80 bit set"),
            (b"\xc3", "1 byte missing at end"),
            (b"\xe2", "2 bytes missing at end"),
            (b"\xc3x", "byte 2 top bits not 0x80"),
            (b"\xe2\x82x", "byte 3 top bits not 0x80"),
            (b"\xc0\xaf", "overlong 2-byte sequence"),
            (b"\xe0\x80\x80", "overlong 3-byte sequence"),
            (b"\xed\xa0\x80", "code points 0xd800-0xdfff are not defined"),
            (b"\xf0\x80\x80\x80", "overlong 4-byte sequence"),
            (
                b"\xf4\x90\x80\x80",
                "code points greater than 0x10ffff are not defined",
            ),
            (
                b"\xf8\x88\x80\x80\x80",
                "5-byte character is not allowed (RFC 3629)",
            ),
        ];
        for (bytes, want) in cases {
            assert_eq!(
                pcre2_utf8_error(bytes).map(str::to_string),
                Some(format!("UTF-8 error: {want}")),
                "{bytes:?}"
            );
        }
    }

    #[test]
    fn code_points_are_counted_as_mongod_counts_them() {
        assert_eq!(len_cp(b"\xff\xfe"), 2);
        assert_eq!(len_cp(b"a\xffb"), 3);
        assert_eq!(len_cp("é".as_bytes()), 1);
        // mongod 8.2.11: `{$indexOfCP: ["a\xffb", "b"]}` is -1, the walk
        // stepping eight bytes at 0xff.
        assert_eq!(index_of_cp(b"a\xffb", b"b", 0, None), Ok(-1));
        assert_eq!(index_of_cp("aéb".as_bytes(), b"b", 0, None), Ok(2));
        assert_eq!(index_of_cp(b"ab\xe2\x82", b"b", 0, None), Ok(1));
        assert!(index_of_cp(b"a\x80", b"b", 0, None).is_err());
        assert_eq!(index_of_bytes(b"a\xffb", b"b", 0, None), 2);
    }

    #[test]
    fn byte_operations() {
        assert_eq!(split(b"a\xffb", b"b"), vec![b"a\xff".to_vec(), vec![]]);
        assert_eq!(replace(b"a\xffb", b"b", b"c", true), b"a\xffc");
        assert_eq!(trim(b"a\xffb", &[b"a"], true, true), b"\xffb");
        assert_eq!(
            substr_cp(b"\xff\xfe", 0, 1),
            Err((34459, "$substrCP: invalid UTF-8 string"))
        );
        assert_eq!(
            substr_cp(b"\xff\xfe", 1, 1),
            Err((34457, "$substrCP: invalid UTF-8 string"))
        );
    }
}
