use std::str;

use bytes::{Buf, BufMut, Bytes, BytesMut};

use crate::error::{PgWireError, PgWireResult};

/// Get null-terminated string, returns None when empty or
/// non null-terminated cstring read.
///
/// Note that this implementation will also advance cursor by 1 after reading
/// empty cstring. This behaviour works for how postgres wire protocol handling
/// key-value pairs, which is ended by a single `\0`
pub(crate) fn get_cstring(buf: &mut BytesMut) -> Option<String> {
    let mut i = 0;

    // with bound check to prevent invalid format
    while i < buf.remaining() && buf[i] != b'\0' {
        i += 1;
    }

    if i == buf.remaining() {
        return None;
    }

    // i+1: include the '\0'
    // move cursor to the end of cstring
    let string_buf = buf.split_to(i + 1);

    if i == 0 {
        None
    } else {
        Some(String::from_utf8_lossy(&string_buf[..i]).into_owned())
    }
}

/// Get the RAW bytes of a null-terminated string, without any character
/// decoding. Same cursor / empty-string semantics as `get_cstring`. Needed
/// because the query text of `Query` and `Parse` is in the client's
/// `client_encoding`, which is not necessarily UTF-8: `get_cstring`'s lossy
/// UTF-8 decode replaces every non-UTF-8 byte with U+FFFD before a backend can
/// see it. (SecantusDB local patch.)
pub(crate) fn get_cstring_raw(buf: &mut BytesMut) -> Option<Bytes> {
    let mut i = 0;
    while i < buf.remaining() && buf[i] != b'\0' {
        i += 1;
    }
    if i == buf.remaining() {
        return None;
    }
    let string_buf = buf.split_to(i + 1);
    if i == 0 {
        None
    } else {
        Some(string_buf.freeze().slice(..i))
    }
}

/// Put null-termianted string
///
/// You can put empty string by giving `""` as input.
///
/// SecantusDB patch: the string is written up to its first NUL, as a C
/// string is. A NUL inside (an error message quoting a binary parameter that
/// was read as text) used to be written verbatim, and the receiver -- which
/// reads C strings -- then found bytes left over after the message's fields
/// and dropped the connection with "message contents do not agree with
/// length in message type E".
pub(crate) fn put_cstring(buf: &mut BytesMut, input: &str) {
    buf.put_slice(&input.as_bytes()[..cstring_body_len(input)]);
    buf.put_u8(b'\0');
}

/// The number of bytes `put_cstring` writes BEFORE the terminator: the input
/// up to its first NUL. A message's `message_length` must count a string the
/// way `put_cstring` writes it, or the frame's length disagrees with its
/// body. (SecantusDB local patch, with `put_cstring`.)
pub(crate) fn cstring_body_len(input: &str) -> usize {
    input.find('\0').unwrap_or(input.len())
}

pub(crate) fn put_option_cstring(buf: &mut BytesMut, input: &Option<String>) {
    if let Some(input) = input {
        put_cstring(buf, input);
    } else {
        buf.put_u8(b'\0');
    }
}

/// Try to read message length from buf, without actually move the cursor
pub(crate) fn get_length(buf: &BytesMut, offset: usize) -> Option<usize> {
    if buf.remaining() >= 4 + offset {
        Some((&buf[offset..4 + offset]).get_i32() as usize)
    } else {
        None
    }
}

/// Validate an on-wire element count before it is used to pre-allocate a
/// collection. Counts are unsigned; `count` elements of `elem_size` bytes each
/// must fit in the remaining buffer, or the message cannot be real.
pub(crate) fn ensure_count(count: usize, elem_size: usize, buf: &BytesMut) -> PgWireResult<()> {
    let remaining = buf.remaining();
    let needed = count.saturating_mul(elem_size);
    if needed > remaining {
        Err(PgWireError::InvalidElementCount(count, needed, remaining))
    } else {
        Ok(())
    }
}

/// Check if message_length matches and move the cursor to right position then
/// call the `decode_fn` for the body
pub(crate) fn decode_packet<T, F>(
    buf: &mut BytesMut,
    offset: usize,
    max_size: usize,
    decode_fn: F,
) -> PgWireResult<Option<T>>
where
    F: Fn(&mut BytesMut, usize) -> PgWireResult<T>,
{
    if let Some(msg_len) = get_length(buf, offset) {
        if msg_len > max_size {
            return Err(PgWireError::MessageTooLarge(max_size, msg_len));
        }

        if buf.remaining() >= msg_len + offset {
            buf.advance(offset + 4);
            return decode_fn(buf, msg_len).map(|r| Some(r));
        }
    }

    Ok(None)
}

// pub(crate) fn get_and_ensure_message_type(buf: &mut BytesMut, t: u8) -> PgWireResult<()> {
//     let msg_type = buf[0];
//     // ensure the type is corrent
//     if msg_type != t {
//         return Err(PgWireError::InvalidMessageType(t, msg_type));
//     }

//     Ok(())
// }

pub(crate) fn option_string_len(s: &Option<String>) -> usize {
    1 + s.as_ref().map(|s| s.len()).unwrap_or(0)
}

#[cfg(test)]
mod test {
    use super::{cstring_body_len, get_cstring, put_cstring};
    use bytes::{BufMut, BytesMut};

    #[test]
    fn put_cstring_stops_at_the_first_nul() {
        // A C string ends at its first NUL; bytes after it would be read by
        // the receiver as the NEXT field and break the message's length.
        let mut buf = BytesMut::new();
        put_cstring(&mut buf, "abc\0def");
        assert_eq!(&buf[..], b"abc\0");
        let mut buf = BytesMut::new();
        put_cstring(&mut buf, "");
        assert_eq!(&buf[..], b"\0");
        let mut buf = BytesMut::new();
        put_cstring(&mut buf, "plain");
        assert_eq!(&buf[..], b"plain\0");
        assert_eq!(cstring_body_len("abc\0def"), 3);
        assert_eq!(cstring_body_len(""), 0);
        assert_eq!(cstring_body_len("plain"), 5);
    }

    #[test]
    fn get_cstring_valid() {
        let mut buf = BytesMut::new();
        buf.put(&b"a cstring\0"[..]);
        buf.put(&b"\0"[..]);

        assert_eq!(Some("a cstring".into()), get_cstring(&mut buf));
        assert_eq!(None, get_cstring(&mut buf));
    }

    #[test]
    fn get_cstring_empty() {
        let mut buf = BytesMut::new();

        assert_eq!(None, get_cstring(&mut buf));
    }

    #[test]
    fn get_cstring_without_null() {
        let mut buf = BytesMut::new();
        buf.put(&b"a cstring"[..]);
        assert_eq!(None, get_cstring(&mut buf));
    }
}
