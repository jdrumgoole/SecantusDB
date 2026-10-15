//! `wparser_def.c`'s state tables, in its order: for each state, the
//! character tests tried in turn and what the first that holds does.

use super::*;

// ------------------------------------------------------------------------
// The state tables
// ------------------------------------------------------------------------

/// `TParserState`. `Null` is "stay in the current state".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum S {
    Base,
    InNumWord,
    InAsciiWord,
    InWord,
    InUnsignedInt,
    InSignedIntFirst,
    InSignedInt,
    InSpace,
    InUDecimalFirst,
    InUDecimal,
    InDecimalFirst,
    InDecimal,
    InVerVersion,
    InSVerVersion,
    InVersionFirst,
    InVersion,
    InMantissaFirst,
    InMantissaSign,
    InMantissa,
    InXMLEntityFirst,
    InXMLEntity,
    InXMLEntityNumFirst,
    InXMLEntityNum,
    InXMLEntityHexNumFirst,
    InXMLEntityHexNum,
    InXMLEntityEnd,
    InTagFirst,
    InXMLBegin,
    InTagCloseFirst,
    InTagName,
    InTagBeginEnd,
    InTag,
    InTagEscapeK,
    InTagEscapeKK,
    InTagBackSleshed,
    InTagEnd,
    InCommentFirst,
    InCommentLast,
    InComment,
    InCloseCommentFirst,
    InCloseCommentLast,
    InCommentEnd,
    InHostFirstDomain,
    InHostDomainSecond,
    InHostDomain,
    InPortFirst,
    InPort,
    InHostFirstAN,
    InHost,
    InEmail,
    InFileFirst,
    InFileTwiddle,
    InPathFirst,
    InPathFirstFirst,
    InPathSecond,
    InFile,
    InFileNext,
    InURLPathFirst,
    InURLPathStart,
    InURLPath,
    InFURL,
    InProtocolFirst,
    InProtocolSecond,
    InProtocolEnd,
    InHyphenAsciiWordFirst,
    InHyphenAsciiWord,
    InHyphenWordFirst,
    InHyphenWord,
    InHyphenNumWordFirst,
    InHyphenNumWord,
    InHyphenDigitLookahead,
    InParseHyphen,
    InParseHyphenHyphen,
    InHyphenWordPart,
    InHyphenAsciiWordPart,
    InHyphenNumWordPart,
    InHyphenUnsignedInt,
    Null,
}

/// The character tests (`p_is*`); `Any` is the table's closing NULL entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum T {
    Eof,
    Eq(u8),
    Ignore,
    Asclet,
    Alpha,
    Digit,
    Alnum,
    NotAlnum,
    Space,
    Xdigit,
    Special,
    StopHost,
    Host,
    UrlPath,
    UrlChar,
    Any,
}

/// The special handlers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Sp {
    No,
    Tags,
    Furl,
    Hyphen,
    VerVersion,
}

pub(super) const A_NEXT: u8 = 0x00;
pub(super) const A_BINGO: u8 = 0x01;
pub(super) const A_POP: u8 = 0x02;
pub(super) const A_PUSH: u8 = 0x04;
pub(super) const A_RERUN: u8 = 0x08;
pub(super) const A_CLEAR: u8 = 0x10;
pub(super) const A_MERGE: u8 = 0x20;
pub(super) const A_CLRALL: u8 = 0x40;

/// `TParserStateActionItem`: test, flags, next state, token type, handler.
pub(super) type Item = (T, u8, S, u8, Sp);

const fn i(t: T, flags: u8, to: S) -> Item {
    (t, flags, to, 0, Sp::No)
}

const fn b(t: T, flags: u8, to: S, ty: u8) -> Item {
    (t, flags, to, ty, Sp::No)
}

const fn sp(t: T, flags: u8, to: S, ty: u8, h: Sp) -> Item {
    (t, flags, to, ty, h)
}

#[rustfmt::skip]
pub(super) fn actions(s: S) -> &'static [Item] {
    use S::*;
    use T::*;
    match s {
        Base => const { &[
            i(Eof, A_NEXT, Null), i(Eq(b'<'), A_PUSH, InTagFirst), i(Ignore, A_NEXT, InSpace),
            i(Asclet, A_NEXT, InAsciiWord), i(Alpha, A_NEXT, InWord),
            i(Digit, A_NEXT, InUnsignedInt), i(Eq(b'-'), A_PUSH, InSignedIntFirst),
            i(Eq(b'+'), A_PUSH, InSignedIntFirst), i(Eq(b'&'), A_PUSH, InXMLEntityFirst),
            i(Eq(b'~'), A_PUSH, InFileTwiddle), i(Eq(b'/'), A_PUSH, InFileFirst),
            i(Eq(b'.'), A_PUSH, InPathFirstFirst), i(Any, A_NEXT, InSpace),
        ] },
        InNumWord => const { &[
            b(Eof, A_BINGO, Base, NUMWORD), i(Alnum, A_NEXT, InNumWord),
            i(Special, A_NEXT, InNumWord), i(Eq(b'@'), A_PUSH, InEmail),
            i(Eq(b'/'), A_PUSH, InFileFirst), i(Eq(b'.'), A_PUSH, InFileNext),
            i(Eq(b'-'), A_PUSH, InHyphenNumWordFirst), b(Any, A_BINGO, Base, NUMWORD),
        ] },
        InAsciiWord => const { &[
            b(Eof, A_BINGO, Base, ASCIIWORD), i(Asclet, A_NEXT, Null),
            i(Eq(b'.'), A_PUSH, InHostFirstDomain), i(Eq(b'.'), A_PUSH, InFileNext),
            i(Eq(b'-'), A_PUSH, InHostFirstAN), i(Eq(b'-'), A_PUSH, InHyphenAsciiWordFirst),
            i(Eq(b'_'), A_PUSH, InHostFirstAN), i(Eq(b'@'), A_PUSH, InEmail),
            i(Eq(b':'), A_PUSH, InProtocolFirst), i(Eq(b'/'), A_PUSH, InFileFirst),
            i(Digit, A_PUSH, InHost), i(Digit, A_NEXT, InNumWord), i(Alpha, A_NEXT, InWord),
            i(Special, A_NEXT, InWord), b(Any, A_BINGO, Base, ASCIIWORD),
        ] },
        InWord => const { &[
            b(Eof, A_BINGO, Base, WORD_T), i(Alpha, A_NEXT, Null), i(Special, A_NEXT, Null),
            i(Digit, A_NEXT, InNumWord), i(Eq(b'-'), A_PUSH, InHyphenWordFirst),
            b(Any, A_BINGO, Base, WORD_T),
        ] },
        InUnsignedInt => const { &[
            b(Eof, A_BINGO, Base, UNSIGNEDINT), i(Digit, A_NEXT, Null),
            i(Eq(b'.'), A_PUSH, InHostFirstDomain), i(Eq(b'.'), A_PUSH, InUDecimalFirst),
            i(Eq(b'e'), A_PUSH, InMantissaFirst), i(Eq(b'E'), A_PUSH, InMantissaFirst),
            i(Eq(b'-'), A_PUSH, InHostFirstAN), i(Eq(b'_'), A_PUSH, InHostFirstAN),
            i(Eq(b'@'), A_PUSH, InEmail), i(Asclet, A_PUSH, InHost),
            i(Alpha, A_NEXT, InNumWord), i(Special, A_NEXT, InNumWord),
            i(Eq(b'/'), A_PUSH, InFileFirst), b(Any, A_BINGO, Base, UNSIGNEDINT),
        ] },
        InSignedIntFirst => const { &[
            i(Eof, A_POP, Null), i(Digit, A_NEXT | A_CLEAR, InSignedInt), i(Any, A_POP, Null),
        ] },
        InSignedInt => const { &[
            b(Eof, A_BINGO, Base, SIGNEDINT), i(Digit, A_NEXT, Null),
            i(Eq(b'.'), A_PUSH, InDecimalFirst), i(Eq(b'e'), A_PUSH, InMantissaFirst),
            i(Eq(b'E'), A_PUSH, InMantissaFirst), b(Any, A_BINGO, Base, SIGNEDINT),
        ] },
        InSpace => const { &[
            b(Eof, A_BINGO, Base, SPACE), b(Eq(b'<'), A_BINGO, Base, SPACE),
            i(Ignore, A_NEXT, Null), b(Eq(b'-'), A_BINGO, Base, SPACE),
            b(Eq(b'+'), A_BINGO, Base, SPACE), b(Eq(b'&'), A_BINGO, Base, SPACE),
            b(Eq(b'/'), A_BINGO, Base, SPACE), i(NotAlnum, A_NEXT, InSpace),
            b(Any, A_BINGO, Base, SPACE),
        ] },
        InUDecimalFirst => const { &[
            i(Eof, A_POP, Null), i(Digit, A_CLEAR, InUDecimal), i(Any, A_POP, Null),
        ] },
        InUDecimal => const { &[
            b(Eof, A_BINGO, Base, DECIMAL_T), i(Digit, A_NEXT, InUDecimal),
            i(Eq(b'.'), A_PUSH, InVersionFirst), i(Eq(b'e'), A_PUSH, InMantissaFirst),
            i(Eq(b'E'), A_PUSH, InMantissaFirst), b(Any, A_BINGO, Base, DECIMAL_T),
        ] },
        InDecimalFirst => const { &[
            i(Eof, A_POP, Null), i(Digit, A_CLEAR, InDecimal), i(Any, A_POP, Null),
        ] },
        InDecimal => const { &[
            b(Eof, A_BINGO, Base, DECIMAL_T), i(Digit, A_NEXT, InDecimal),
            i(Eq(b'.'), A_PUSH, InVerVersion), i(Eq(b'e'), A_PUSH, InMantissaFirst),
            i(Eq(b'E'), A_PUSH, InMantissaFirst), b(Any, A_BINGO, Base, DECIMAL_T),
        ] },
        InVerVersion => const { &[
            i(Eof, A_POP, Null), sp(Digit, A_RERUN, InSVerVersion, 0, Sp::VerVersion),
            i(Any, A_POP, Null),
        ] },
        InSVerVersion => const { &[
            i(Eof, A_POP, Null), b(Digit, A_BINGO | A_CLRALL, InUnsignedInt, SPACE),
            i(Any, A_NEXT, Null),
        ] },
        InVersionFirst => const { &[
            i(Eof, A_POP, Null), i(Digit, A_CLEAR, InVersion), i(Any, A_POP, Null),
        ] },
        InVersion => const { &[
            b(Eof, A_BINGO, Base, VERSIONNUMBER), i(Digit, A_NEXT, InVersion),
            i(Eq(b'.'), A_PUSH, InVersionFirst), b(Any, A_BINGO, Base, VERSIONNUMBER),
        ] },
        InMantissaFirst => const { &[
            i(Eof, A_POP, Null), i(Digit, A_CLEAR, InMantissa),
            i(Eq(b'+'), A_NEXT, InMantissaSign), i(Eq(b'-'), A_NEXT, InMantissaSign),
            i(Any, A_POP, Null),
        ] },
        InMantissaSign => const { &[
            i(Eof, A_POP, Null), i(Digit, A_CLEAR, InMantissa), i(Any, A_POP, Null),
        ] },
        InMantissa => const { &[
            b(Eof, A_BINGO, Base, SCIENTIFIC), i(Digit, A_NEXT, InMantissa),
            b(Any, A_BINGO, Base, SCIENTIFIC),
        ] },
        InXMLEntityFirst => const { &[
            i(Eof, A_POP, Null), i(Eq(b'#'), A_NEXT, InXMLEntityNumFirst),
            i(Asclet, A_NEXT, InXMLEntity), i(Eq(b':'), A_NEXT, InXMLEntity),
            i(Eq(b'_'), A_NEXT, InXMLEntity), i(Any, A_POP, Null),
        ] },
        InXMLEntity => const { &[
            i(Eof, A_POP, Null), i(Alnum, A_NEXT, InXMLEntity), i(Eq(b':'), A_NEXT, InXMLEntity),
            i(Eq(b'_'), A_NEXT, InXMLEntity), i(Eq(b'.'), A_NEXT, InXMLEntity),
            i(Eq(b'-'), A_NEXT, InXMLEntity), i(Eq(b';'), A_NEXT, InXMLEntityEnd),
            i(Any, A_POP, Null),
        ] },
        InXMLEntityNumFirst => const { &[
            i(Eof, A_POP, Null), i(Eq(b'x'), A_NEXT, InXMLEntityHexNumFirst),
            i(Eq(b'X'), A_NEXT, InXMLEntityHexNumFirst), i(Digit, A_NEXT, InXMLEntityNum),
            i(Any, A_POP, Null),
        ] },
        InXMLEntityHexNumFirst => const { &[
            i(Eof, A_POP, Null), i(Xdigit, A_NEXT, InXMLEntityHexNum), i(Any, A_POP, Null),
        ] },
        InXMLEntityNum => const { &[
            i(Eof, A_POP, Null), i(Digit, A_NEXT, InXMLEntityNum),
            i(Eq(b';'), A_NEXT, InXMLEntityEnd), i(Any, A_POP, Null),
        ] },
        InXMLEntityHexNum => const { &[
            i(Eof, A_POP, Null), i(Xdigit, A_NEXT, InXMLEntityHexNum),
            i(Eq(b';'), A_NEXT, InXMLEntityEnd), i(Any, A_POP, Null),
        ] },
        InXMLEntityEnd => const { &[b(Any, A_BINGO | A_CLEAR, Base, XMLENTITY)] },
        InTagFirst => const { &[
            i(Eof, A_POP, Null), i(Eq(b'/'), A_PUSH, InTagCloseFirst),
            i(Eq(b'!'), A_PUSH, InCommentFirst), i(Eq(b'?'), A_PUSH, InXMLBegin),
            i(Asclet, A_PUSH, InTagName), i(Eq(b':'), A_PUSH, InTagName),
            i(Eq(b'_'), A_PUSH, InTagName), i(Any, A_POP, Null),
        ] },
        InXMLBegin => const { &[i(Eof, A_POP, Null), i(Eq(b'x'), A_NEXT, InTag), i(Any, A_POP, Null)] },
        InTagCloseFirst => const { &[
            i(Eof, A_POP, Null), i(Asclet, A_NEXT, InTagName), i(Any, A_POP, Null),
        ] },
        InTagName => const { &[
            i(Eof, A_POP, Null), i(Eq(b'/'), A_NEXT, InTagBeginEnd),
            sp(Eq(b'>'), A_NEXT, InTagEnd, 0, Sp::Tags), sp(Space, A_NEXT, InTag, 0, Sp::Tags),
            i(Alnum, A_NEXT, Null), i(Eq(b':'), A_NEXT, Null), i(Eq(b'_'), A_NEXT, Null),
            i(Eq(b'.'), A_NEXT, Null), i(Eq(b'-'), A_NEXT, Null), i(Any, A_POP, Null),
        ] },
        InTagBeginEnd => const { &[
            i(Eof, A_POP, Null), i(Eq(b'>'), A_NEXT, InTagEnd), i(Any, A_POP, Null),
        ] },
        InTag => const { &[
            i(Eof, A_POP, Null), sp(Eq(b'>'), A_NEXT, InTagEnd, 0, Sp::Tags),
            i(Eq(b'\''), A_NEXT, InTagEscapeK), i(Eq(b'"'), A_NEXT, InTagEscapeKK),
            i(Asclet, A_NEXT, Null), i(Digit, A_NEXT, Null), i(Eq(b'='), A_NEXT, Null),
            i(Eq(b'-'), A_NEXT, Null), i(Eq(b'_'), A_NEXT, Null), i(Eq(b'#'), A_NEXT, Null),
            i(Eq(b'/'), A_NEXT, Null), i(Eq(b':'), A_NEXT, Null), i(Eq(b'.'), A_NEXT, Null),
            i(Eq(b'&'), A_NEXT, Null), i(Eq(b'?'), A_NEXT, Null), i(Eq(b'%'), A_NEXT, Null),
            i(Eq(b'~'), A_NEXT, Null), sp(Space, A_NEXT, Null, 0, Sp::Tags), i(Any, A_POP, Null),
        ] },
        InTagEscapeK => const { &[
            i(Eof, A_POP, Null), i(Eq(b'\\'), A_PUSH, InTagBackSleshed),
            i(Eq(b'\''), A_NEXT, InTag), i(Any, A_NEXT, InTagEscapeK),
        ] },
        InTagEscapeKK => const { &[
            i(Eof, A_POP, Null), i(Eq(b'\\'), A_PUSH, InTagBackSleshed),
            i(Eq(b'"'), A_NEXT, InTag), i(Any, A_NEXT, InTagEscapeKK),
        ] },
        InTagBackSleshed => const { &[i(Eof, A_POP, Null), i(Any, A_MERGE, Null)] },
        InTagEnd => const { &[b(Any, A_BINGO | A_CLRALL, Base, TAG_T)] },
        InCommentFirst => const { &[
            i(Eof, A_POP, Null), i(Eq(b'-'), A_NEXT, InCommentLast),
            i(Eq(b'D'), A_NEXT, InTag), i(Eq(b'd'), A_NEXT, InTag), i(Any, A_POP, Null),
        ] },
        InCommentLast => const { &[i(Eof, A_POP, Null), i(Eq(b'-'), A_NEXT, InComment), i(Any, A_POP, Null)] },
        InComment => const { &[
            i(Eof, A_POP, Null), i(Eq(b'-'), A_NEXT, InCloseCommentFirst), i(Any, A_NEXT, Null),
        ] },
        InCloseCommentFirst => const { &[
            i(Eof, A_POP, Null), i(Eq(b'-'), A_NEXT, InCloseCommentLast),
            i(Any, A_NEXT, InComment),
        ] },
        InCloseCommentLast => const { &[
            i(Eof, A_POP, Null), i(Eq(b'-'), A_NEXT, Null), i(Eq(b'>'), A_NEXT, InCommentEnd),
            i(Any, A_NEXT, InComment),
        ] },
        InCommentEnd => const { &[b(Any, A_BINGO | A_CLRALL, Base, TAG_T)] },
        InHostFirstDomain => const { &[
            i(Eof, A_POP, Null), i(Asclet, A_NEXT, InHostDomainSecond),
            i(Digit, A_NEXT, InHost), i(Any, A_POP, Null),
        ] },
        InHostDomainSecond => const { &[
            i(Eof, A_POP, Null), i(Asclet, A_NEXT, InHostDomain), i(Digit, A_PUSH, InHost),
            i(Eq(b'-'), A_PUSH, InHostFirstAN), i(Eq(b'_'), A_PUSH, InHostFirstAN),
            i(Eq(b'.'), A_PUSH, InHostFirstDomain), i(Eq(b'@'), A_PUSH, InEmail),
            i(Any, A_POP, Null),
        ] },
        InHostDomain => const { &[
            b(Eof, A_BINGO | A_CLRALL, Base, HOST), i(Asclet, A_NEXT, InHostDomain),
            i(Digit, A_PUSH, InHost), i(Eq(b':'), A_PUSH, InPortFirst),
            i(Eq(b'-'), A_PUSH, InHostFirstAN), i(Eq(b'_'), A_PUSH, InHostFirstAN),
            i(Eq(b'.'), A_PUSH, InHostFirstDomain), i(Eq(b'@'), A_PUSH, InEmail),
            i(Digit, A_POP, Null), b(StopHost, A_BINGO | A_CLRALL, InURLPathStart, HOST),
            i(Eq(b'/'), A_PUSH, InFURL), b(Any, A_BINGO | A_CLRALL, Base, HOST),
        ] },
        InPortFirst => const { &[i(Eof, A_POP, Null), i(Digit, A_NEXT, InPort), i(Any, A_POP, Null)] },
        InPort => const { &[
            b(Eof, A_BINGO | A_CLRALL, Base, HOST), i(Digit, A_NEXT, InPort),
            b(StopHost, A_BINGO | A_CLRALL, InURLPathStart, HOST),
            i(Eq(b'/'), A_PUSH, InFURL), b(Any, A_BINGO | A_CLRALL, Base, HOST),
        ] },
        InHostFirstAN => const { &[
            i(Eof, A_POP, Null), i(Digit, A_NEXT, InHost), i(Asclet, A_NEXT, InHost),
            i(Any, A_POP, Null),
        ] },
        InHost => const { &[
            i(Eof, A_POP, Null), i(Digit, A_NEXT, InHost), i(Asclet, A_NEXT, InHost),
            i(Eq(b'@'), A_PUSH, InEmail), i(Eq(b'.'), A_PUSH, InHostFirstDomain),
            i(Eq(b'-'), A_PUSH, InHostFirstAN), i(Eq(b'_'), A_PUSH, InHostFirstAN),
            i(Any, A_POP, Null),
        ] },
        InEmail => const { &[
            i(StopHost, A_POP, Null), b(Host, A_BINGO | A_CLRALL, Base, EMAIL),
            i(Any, A_POP, Null),
        ] },
        InFileFirst => const { &[
            i(Eof, A_POP, Null), i(Asclet, A_NEXT, InFile), i(Digit, A_NEXT, InFile),
            i(Eq(b'.'), A_NEXT, InPathFirst), i(Eq(b'_'), A_NEXT, InFile),
            i(Eq(b'~'), A_PUSH, InFileTwiddle), i(Any, A_POP, Null),
        ] },
        InFileTwiddle => const { &[
            i(Eof, A_POP, Null), i(Asclet, A_NEXT, InFile), i(Digit, A_NEXT, InFile),
            i(Eq(b'_'), A_NEXT, InFile), i(Eq(b'/'), A_NEXT, InFileFirst), i(Any, A_POP, Null),
        ] },
        InPathFirst => const { &[
            i(Eof, A_POP, Null), i(Asclet, A_NEXT, InFile), i(Digit, A_NEXT, InFile),
            i(Eq(b'_'), A_NEXT, InFile), i(Eq(b'.'), A_NEXT, InPathSecond),
            i(Eq(b'/'), A_NEXT, InFileFirst), i(Any, A_POP, Null),
        ] },
        InPathFirstFirst => const { &[
            i(Eof, A_POP, Null), i(Eq(b'.'), A_NEXT, InPathSecond),
            i(Eq(b'/'), A_NEXT, InFileFirst), i(Any, A_POP, Null),
        ] },
        InPathSecond => const { &[
            b(Eof, A_BINGO | A_CLEAR, Base, FILEPATH), i(Eq(b'/'), A_NEXT | A_PUSH, InFileFirst),
            b(Eq(b'/'), A_BINGO | A_CLEAR, Base, FILEPATH),
            b(Space, A_BINGO | A_CLEAR, Base, FILEPATH), i(Any, A_POP, Null),
        ] },
        InFile => const { &[
            b(Eof, A_BINGO, Base, FILEPATH), i(Asclet, A_NEXT, InFile), i(Digit, A_NEXT, InFile),
            i(Eq(b'.'), A_PUSH, InFileNext), i(Eq(b'_'), A_NEXT, InFile),
            i(Eq(b'-'), A_NEXT, InFile), i(Eq(b'/'), A_PUSH, InFileFirst),
            b(Any, A_BINGO, Base, FILEPATH),
        ] },
        InFileNext => const { &[
            i(Eof, A_POP, Null), i(Asclet, A_CLEAR, InFile), i(Digit, A_CLEAR, InFile),
            i(Eq(b'_'), A_CLEAR, InFile), i(Any, A_POP, Null),
        ] },
        InURLPathFirst => const { &[i(Eof, A_POP, Null), i(UrlChar, A_NEXT, InURLPath), i(Any, A_POP, Null)] },
        InURLPathStart => const { &[i(Any, A_NEXT, InURLPath)] },
        InURLPath => const { &[
            b(Eof, A_BINGO, Base, URLPATH), i(UrlChar, A_NEXT, InURLPath),
            b(Any, A_BINGO, Base, URLPATH),
        ] },
        InFURL => const { &[
            i(Eof, A_POP, Null), sp(UrlPath, A_BINGO | A_CLRALL, Base, URL_T, Sp::Furl),
            i(Any, A_POP, Null),
        ] },
        InProtocolFirst => const { &[i(Eof, A_POP, Null), i(Eq(b'/'), A_NEXT, InProtocolSecond), i(Any, A_POP, Null)] },
        InProtocolSecond => const { &[i(Eof, A_POP, Null), i(Eq(b'/'), A_NEXT, InProtocolEnd), i(Any, A_POP, Null)] },
        InProtocolEnd => const { &[b(Any, A_BINGO | A_CLRALL, Base, PROTOCOL)] },
        InHyphenAsciiWordFirst => const { &[
            i(Eof, A_POP, Null), i(Asclet, A_NEXT, InHyphenAsciiWord),
            i(Alpha, A_NEXT, InHyphenWord), i(Digit, A_NEXT, InHyphenDigitLookahead),
            i(Any, A_POP, Null),
        ] },
        InHyphenAsciiWord => const { &[
            sp(Eof, A_BINGO | A_CLRALL, InParseHyphen, ASCIIHWORD, Sp::Hyphen),
            i(Asclet, A_NEXT, InHyphenAsciiWord), i(Alpha, A_NEXT, InHyphenWord),
            i(Special, A_NEXT, InHyphenWord), i(Digit, A_NEXT, InHyphenNumWord),
            i(Eq(b'-'), A_PUSH, InHyphenAsciiWordFirst),
            sp(Any, A_BINGO | A_CLRALL, InParseHyphen, ASCIIHWORD, Sp::Hyphen),
        ] },
        InHyphenWordFirst => const { &[
            i(Eof, A_POP, Null), i(Alpha, A_NEXT, InHyphenWord),
            i(Digit, A_NEXT, InHyphenDigitLookahead), i(Any, A_POP, Null),
        ] },
        InHyphenWord => const { &[
            sp(Eof, A_BINGO | A_CLRALL, InParseHyphen, HWORD, Sp::Hyphen),
            i(Alpha, A_NEXT, InHyphenWord), i(Special, A_NEXT, InHyphenWord),
            i(Digit, A_NEXT, InHyphenNumWord), i(Eq(b'-'), A_PUSH, InHyphenWordFirst),
            sp(Any, A_BINGO | A_CLRALL, InParseHyphen, HWORD, Sp::Hyphen),
        ] },
        InHyphenNumWordFirst => const { &[
            i(Eof, A_POP, Null), i(Alpha, A_NEXT, InHyphenNumWord),
            i(Digit, A_NEXT, InHyphenDigitLookahead), i(Any, A_POP, Null),
        ] },
        InHyphenNumWord => const { &[
            sp(Eof, A_BINGO | A_CLRALL, InParseHyphen, NUMHWORD, Sp::Hyphen),
            i(Alnum, A_NEXT, InHyphenNumWord), i(Special, A_NEXT, InHyphenNumWord),
            i(Eq(b'-'), A_PUSH, InHyphenNumWordFirst),
            sp(Any, A_BINGO | A_CLRALL, InParseHyphen, NUMHWORD, Sp::Hyphen),
        ] },
        InHyphenDigitLookahead => const { &[
            i(Eof, A_POP, Null), i(Digit, A_NEXT, InHyphenDigitLookahead),
            i(Alpha, A_NEXT, InHyphenNumWord), i(Special, A_NEXT, InHyphenNumWord),
            i(Any, A_POP, Null),
        ] },
        InParseHyphen => const { &[
            i(Eof, A_RERUN, Base), i(Asclet, A_NEXT, InHyphenAsciiWordPart),
            i(Alpha, A_NEXT, InHyphenWordPart), i(Digit, A_PUSH, InHyphenUnsignedInt),
            i(Eq(b'-'), A_PUSH, InParseHyphenHyphen), i(Any, A_RERUN, Base),
        ] },
        InParseHyphenHyphen => const { &[
            i(Eof, A_POP, Null), b(Alnum, A_BINGO | A_CLEAR, InParseHyphen, SPACE),
            b(Special, A_BINGO | A_CLEAR, InParseHyphen, SPACE), i(Any, A_POP, Null),
        ] },
        InHyphenWordPart => const { &[
            b(Eof, A_BINGO, Base, PARTHWORD), i(Alpha, A_NEXT, InHyphenWordPart),
            i(Special, A_NEXT, InHyphenWordPart), i(Digit, A_NEXT, InHyphenNumWordPart),
            b(Any, A_BINGO, InParseHyphen, PARTHWORD),
        ] },
        InHyphenAsciiWordPart => const { &[
            b(Eof, A_BINGO, Base, ASCIIPARTHWORD), i(Asclet, A_NEXT, InHyphenAsciiWordPart),
            i(Alpha, A_NEXT, InHyphenWordPart), i(Special, A_NEXT, InHyphenWordPart),
            i(Digit, A_NEXT, InHyphenNumWordPart), b(Any, A_BINGO, InParseHyphen, ASCIIPARTHWORD),
        ] },
        InHyphenNumWordPart => const { &[
            b(Eof, A_BINGO, Base, NUMPARTHWORD), i(Alnum, A_NEXT, InHyphenNumWordPart),
            i(Special, A_NEXT, InHyphenNumWordPart), b(Any, A_BINGO, InParseHyphen, NUMPARTHWORD),
        ] },
        InHyphenUnsignedInt => const { &[
            i(Eof, A_POP, Null), i(Digit, A_NEXT, Null), i(Alpha, A_CLEAR, InHyphenNumWordPart),
            i(Special, A_CLEAR, InHyphenNumWordPart), i(Any, A_POP, Null),
        ] },
        Null => &[],
    }
}
