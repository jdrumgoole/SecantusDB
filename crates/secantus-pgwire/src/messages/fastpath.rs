//! The Fastpath sub-protocol: `FunctionCall` ('F') and
//! `FunctionCallResponse` ('V'). (SecantusDB local patch: pgjdbc's large
//! object API calls `lo_*` this way.)

use bytes::{Buf, BufMut, Bytes};

use super::{DecodeContext, Message};
use crate::error::PgWireResult;

/// A call of the function with oid `function_oid`.
#[non_exhaustive]
#[derive(PartialEq, Eq, Debug, Default)]
pub struct FunctionCall {
    pub function_oid: u32,
    pub argument_format_codes: Vec<i16>,
    pub arguments: Vec<Option<Bytes>>,
    pub result_format_code: i16,
}

/// Message type byte for FunctionCall
pub const MESSAGE_TYPE_BYTE_FUNCTION_CALL: u8 = b'F';

impl Message for FunctionCall {
    #[inline]
    fn message_type() -> Option<u8> {
        Some(MESSAGE_TYPE_BYTE_FUNCTION_CALL)
    }

    #[inline]
    fn max_message_length() -> usize {
        super::LARGE_PACKET_SIZE_LIMIT
    }

    fn message_length(&self) -> usize {
        4 + 4
            + 2
            + 2 * self.argument_format_codes.len()
            + 2
            + self
                .arguments
                .iter()
                .map(|a| 4 + a.as_ref().map_or(0, |d| d.len()))
                .sum::<usize>()
            + 2
    }

    fn encode_body(&self, buf: &mut bytes::BytesMut) -> PgWireResult<()> {
        buf.put_u32(self.function_oid);
        buf.put_u16(self.argument_format_codes.len() as u16);
        for c in &self.argument_format_codes {
            buf.put_i16(*c);
        }
        buf.put_u16(self.arguments.len() as u16);
        for a in &self.arguments {
            match a {
                Some(d) => {
                    buf.put_i32(d.len() as i32);
                    buf.put_slice(d);
                }
                None => buf.put_i32(-1),
            }
        }
        buf.put_i16(self.result_format_code);
        Ok(())
    }

    fn decode_body(
        buf: &mut bytes::BytesMut,
        _: usize,
        _ctx: &DecodeContext,
    ) -> PgWireResult<Self> {
        let function_oid = buf.get_u32();
        let n = buf.get_u16();
        let argument_format_codes = (0..n).map(|_| buf.get_i16()).collect();
        let n = buf.get_u16();
        let mut arguments = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let len = buf.get_i32();
            arguments.push(if len >= 0 {
                Some(buf.split_to(len as usize).freeze())
            } else {
                None
            });
        }
        let result_format_code = buf.get_i16();
        Ok(FunctionCall {
            function_oid,
            argument_format_codes,
            arguments,
            result_format_code,
        })
    }
}

/// The result of a `FunctionCall`.
#[non_exhaustive]
#[derive(PartialEq, Eq, Debug, Default)]
pub struct FunctionCallResponse {
    pub value: Option<Bytes>,
}

impl FunctionCallResponse {
    pub fn new(value: Option<Bytes>) -> Self {
        FunctionCallResponse { value }
    }
}

/// Message type byte for FunctionCallResponse
pub const MESSAGE_TYPE_BYTE_FUNCTION_CALL_RESPONSE: u8 = b'V';

impl Message for FunctionCallResponse {
    #[inline]
    fn message_type() -> Option<u8> {
        Some(MESSAGE_TYPE_BYTE_FUNCTION_CALL_RESPONSE)
    }

    #[inline]
    fn max_message_length() -> usize {
        super::LARGE_PACKET_SIZE_LIMIT
    }

    fn message_length(&self) -> usize {
        4 + 4 + self.value.as_ref().map_or(0, |d| d.len())
    }

    fn encode_body(&self, buf: &mut bytes::BytesMut) -> PgWireResult<()> {
        match &self.value {
            Some(d) => {
                buf.put_i32(d.len() as i32);
                buf.put_slice(d);
            }
            None => buf.put_i32(-1),
        }
        Ok(())
    }

    fn decode_body(
        buf: &mut bytes::BytesMut,
        _: usize,
        _ctx: &DecodeContext,
    ) -> PgWireResult<Self> {
        let len = buf.get_i32();
        let value = (len >= 0).then(|| buf.split_to(len as usize).freeze());
        Ok(FunctionCallResponse { value })
    }
}
