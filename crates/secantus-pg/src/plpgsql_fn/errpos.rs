//! Where in a PL/pgSQL body its compile error is.
//!
//! PostgreSQL points a body's grammar error at its token
//! (`plpgsql_scanner_errposition` sets the internal position, which
//! `function_parse_error_transpose` moves into the statement). libpg_query
//! keeps only the error's `cursorpos`, so the internal position is lost.
//! It is found again from the parser itself: an LR parser stops at the
//! first token no valid body can continue with, so the shortest prefix of
//! the body, cut after a token, that fails with the same message ends at
//! that token.

/// The 0-based character offset in `body` of the token `parse` fails on
/// with `message`; `parse` builds and parses the routine around a body
/// text. `None` for an error at end of input or one no prefix reproduces.
pub fn compile_error_offset(
    body: &str,
    message: &str,
    parse: &dyn Fn(&str) -> Result<(), String>,
) -> Option<usize> {
    if message.ends_with("at end of input") {
        return None;
    }
    let tokens = pg_query::scan(body).ok()?.tokens;
    for t in tokens {
        let (start, end) = (t.start as usize, t.end as usize);
        let Some(prefix) = body.get(..end) else {
            continue;
        };
        if parse(prefix).err().as_deref() == Some(message) {
            return Some(body[..start].chars().count());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(body: &str) -> Result<(), String> {
        crate::plpgsql_fn::validate(&format!(
            "CREATE FUNCTION f() RETURNS void AS $b${body}\n$b$ LANGUAGE plpgsql"
        ))
        .map_err(|e| e.message)
    }

    #[test]
    fn the_offset_is_the_failing_token() {
        let body = "declare x int; begin raise notice 'z' end";
        let msg = parse(body).unwrap_err();
        assert_eq!(compile_error_offset(body, &msg, &parse), Some(38));
        let body = "<<blk>> begin raise notice 'z'; end nope";
        let msg = parse(body).unwrap_err();
        assert_eq!(compile_error_offset(body, &msg, &parse), Some(36));
    }
}
