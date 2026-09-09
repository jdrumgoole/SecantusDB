use bytes::{Bytes, BytesMut};

use super::{DecodeContext, Message, codec};
use crate::error::PgWireResult;

/// A sql query sent from frontend to backend.
#[non_exhaustive]
#[derive(PartialEq, Eq, Debug)]
pub struct Query {
    /// The query text decoded as UTF-8 (lossily: a byte that is not valid
    /// UTF-8 becomes U+FFFD).
    pub query: String,
    /// The query text EXACTLY as it arrived on the wire, in the client's
    /// `client_encoding`. A backend that honours a non-UTF-8 client encoding
    /// decodes this instead of `query` -- see
    /// `SimpleQueryHandler::decode_query_text`. (SecantusDB local patch.)
    pub query_raw: Bytes,
}

impl Query {
    pub fn new(query: String) -> Query {
        let query_raw = Bytes::copy_from_slice(query.as_bytes());
        Query { query, query_raw }
    }
}

/// Message type byte for Query
pub const MESSAGE_TYPE_BYTE_QUERY: u8 = b'Q';

impl Message for Query {
    #[inline]
    fn message_type() -> Option<u8> {
        Some(MESSAGE_TYPE_BYTE_QUERY)
    }

    fn message_length(&self) -> usize {
        5 + self.query.len()
    }

    #[inline]
    fn max_message_length() -> usize {
        super::LARGE_PACKET_SIZE_LIMIT
    }

    fn encode_body(&self, buf: &mut BytesMut) -> PgWireResult<()> {
        codec::put_cstring(buf, &self.query);

        Ok(())
    }

    fn decode_body(buf: &mut BytesMut, _: usize, _ctx: &DecodeContext) -> PgWireResult<Self> {
        let query_raw = codec::get_cstring_raw(buf).unwrap_or_default();
        let query = String::from_utf8_lossy(&query_raw).into_owned();

        Ok(Query { query, query_raw })
    }
}
