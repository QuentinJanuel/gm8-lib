use crate::token::{Keyword, Operator, Separator, Token};

use std::{
    iter::{Copied, Enumerate, Peekable},
    slice,
    slice::SliceIndex,
    str, u64,
};

#[derive(Clone)]
pub struct Lexer<'a> {
    /// GML source code to return references to.
    src: &'a [u8],

    line_hint: usize,

    /// Byte offset where the token most recently returned by `next` begins, past the whitespace
    /// and comments skipped to reach it. With `offset`, this is what gives a statement a span --
    /// which is how a mod points at one. See `TokenStream`.
    token_start: usize,

    /// Iterator over the source code as raw bytes.
    iter: Peekable<Enumerate<Copied<slice::Iter<'a, u8>>>>,
}

impl<'a> Lexer<'a> {
    /// Creates a new Lexer over GML source code.
    pub fn new(src: &'a [u8]) -> Self {
        Lexer { src, line_hint: 1, token_start: 0, iter: src.iter().copied().enumerate().peekable() }
    }

    /// Returns the current line number in the source code.
    pub fn line(&self) -> usize {
        self.line_hint
    }

    /// Where the token most recently returned by `next` began.
    pub fn token_start(&self) -> usize {
        self.token_start
    }

    /// Byte offset of the next byte not yet lexed, i.e. just past the last token returned.
    ///
    /// Read straight after `next`, this is that token's end -- before the whitespace following it,
    /// which is what a statement's span wants: text inserted there lands against the statement
    /// rather than in the middle of the next line's indentation.
    pub fn offset(&mut self) -> usize {
        self.iter.peek().map_or(self.src.len(), |&(index, _)| index)
    }

    /// Fast-forwards the internal iterator to the next token, skipping over whitespace.
    /// Returns how many lines (LF) were skipped in the process.
    fn fast_forward(&mut self) -> usize {
        let mut lines_skipped: usize = 0;
        loop {
            match self.iter.peek() {
                Some(&(_, ch)) if ch <= b' ' => {
                    if ch == b'\n' {
                        lines_skipped += 1;
                    }
                    self.iter.next();
                },
                _ => break lines_skipped,
            }
        }
    }
}

/// A lexer with one token of lookahead that also knows where its tokens are.
///
/// The parser needs both, and `Peekable<Lexer>` can only give the first: peeking pulls a token out
/// of the lexer, so the lexer's own position is then past a token the parser has not consumed yet,
/// and a span taken at that moment is wrong by exactly one token. Keeping the peeked token's start
/// beside the token fixes that -- `start` answers for the next token whether or not anything has
/// looked at it, and `end` for the last one handed out.
#[derive(Clone)]
pub struct TokenStream<'a> {
    lexer: Lexer<'a>,
    /// The token pulled from the lexer but not yet handed out, and where it begins.
    peeked: Option<(Token<'a>, usize)>,
    /// Just past the last token `next` returned.
    end: usize,
    /// Whether the parser should record a span for each statement it reads.
    spans: bool,
}

impl<'a> TokenStream<'a> {
    pub fn new(src: &'a [u8]) -> Self {
        TokenStream { lexer: Lexer::new(src), peeked: None, end: 0, spans: false }
    }

    /// A stream whose parser wraps every statement in `Expr::At`. See `AST::new_with_spans`.
    ///
    /// The flag rides on the stream rather than being an argument, because it would otherwise have
    /// to be threaded through every one of the parser's recursive reads to reach the three places
    /// that act on it.
    pub fn with_spans(src: &'a [u8]) -> Self {
        TokenStream { spans: true, ..TokenStream::new(src) }
    }

    pub fn spans(&self) -> bool {
        self.spans
    }

    pub fn peek(&mut self) -> Option<&Token<'a>> {
        if self.peeked.is_none() {
            let token = self.lexer.next()?;
            self.peeked = Some((token, self.lexer.token_start()));
        }
        self.peeked.as_ref().map(|(token, _)| token)
    }

    /// Where the next token begins, or the end of the source when there is none.
    ///
    /// The start of a statement is this, read before parsing it.
    pub fn start(&mut self) -> usize {
        if self.peek().is_none() {
            return self.lexer.offset();
        }
        self.peeked.as_ref().map_or(self.end, |&(_, start)| start)
    }

    /// Just past the last token handed out. The end of a statement is this, read after parsing it.
    pub fn end(&self) -> usize {
        self.end
    }

    pub fn line(&self) -> usize {
        self.lexer.line()
    }
}

impl<'a> Iterator for TokenStream<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Token<'a>> {
        let token = match self.peeked.take() {
            // The lexer already ran past this one when it was peeked, so its position is this
            // token's end either way.
            Some((token, _)) => token,
            None => self.lexer.next()?,
        };
        self.end = self.lexer.offset();
        Some(token)
    }
}

impl<'a> Iterator for Lexer<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        // locate next token
        self.line_hint = self.fast_forward();

        /// Helper function to reconstruct our byte slices to a string easily.
        /// This is fine since we operate on something that is a &str in a first place,
        /// and we never (should) use a value not pulled from our iterators as range indices.
        #[inline(always)]
        fn sl<R: SliceIndex<[u8], Output = [u8]>>(src: &[u8], range: R) -> &[u8] {
            unsafe { src.get_unchecked(range) }
        }

        let head = *self.iter.peek()?;
        // Before any branch, so every token records it. A comment restarts this function with
        // `return self.next()`, which overwrites it with the real token's start -- which is right:
        // a statement's span should not begin at a comment above it.
        self.token_start = head.0;

        #[allow(clippy::match_overlapping_arm)] // quotes overlap with the catch-all ASCII
        Some(match head.1 {
            // identifier, keyword or alphanumeric operator/separator
            b'A'..=b'Z' | b'a'..=b'z' | b'_' => {
                let identifier = {
                    loop {
                        match self.iter.peek() {
                            Some(&(tail, ch)) => match ch {
                                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' => {
                                    self.iter.next();
                                },
                                _ => break sl(&self.src, head.0..tail),
                            },
                            None => break sl(&self.src, head.0..),
                        }
                    }
                };

                match identifier {
                    // Keywords
                    b"var" => Token::Keyword(Keyword::Var),
                    b"globalvar" => Token::Keyword(Keyword::GlobalVar),
                    b"if" => Token::Keyword(Keyword::If),
                    b"else" => Token::Keyword(Keyword::Else),
                    b"with" => Token::Keyword(Keyword::With),
                    b"repeat" => Token::Keyword(Keyword::Repeat),
                    b"do" => Token::Keyword(Keyword::Do),
                    b"until" => Token::Keyword(Keyword::Until),
                    b"while" => Token::Keyword(Keyword::While),
                    b"for" => Token::Keyword(Keyword::For),
                    b"switch" => Token::Keyword(Keyword::Switch),
                    b"case" => Token::Keyword(Keyword::Case),
                    b"default" => Token::Keyword(Keyword::Default),
                    b"break" => Token::Keyword(Keyword::Break),
                    b"continue" => Token::Keyword(Keyword::Continue),
                    b"return" => Token::Keyword(Keyword::Return),
                    b"exit" => Token::Keyword(Keyword::Exit),

                    // Operators
                    b"mod" => Token::Operator(Operator::Modulo),
                    b"div" => Token::Operator(Operator::IntDivide),
                    b"and" => Token::Operator(Operator::And),
                    b"or" => Token::Operator(Operator::Or),
                    b"xor" => Token::Operator(Operator::Xor),
                    b"not" => Token::Operator(Operator::Not),
                    b"then" => Token::Separator(Separator::Then),
                    b"begin" => Token::Separator(Separator::BraceLeft),
                    b"end" => Token::Separator(Separator::BraceRight),

                    _ => Token::Identifier(identifier),
                }
            },

            // real literal or . operator
            // in a real literal, every dot after the first one is ignored
            // a number can't begin with `..` - for example, '..1' is read as:
            // - the Period separator
            // - real literal literal 0.1
            // examples of valid real literals, you will lose brain cells reading this:
            // 5.5.5.... => 5.55
            // 6...2...9 => 6.29
            // .7....3.. => 0.73
            // 4.2...0.0 => 4.2
            b'0'..=b'9' | b'.' => {
                let mut point_seen = false;

                if let (_, b'.') = head {
                    self.iter.next();
                    if let Some(&(_, ch)) = self.iter.peek() {
                        match ch {
                            b'0'..=b'9' => point_seen = true,
                            _ => return Some(Token::Separator(Separator::Period)),
                        }
                    }
                }

                let mut result = 0.0f64;
                let mut factor = 1.0f64;
                while let Some(&(_, ch)) = self.iter.peek() {
                    match ch {
                        ch @ b'0'..=b'9' => {
                            let dec = ch - b'0';
                            if point_seen {
                                factor /= 10.0;
                            }
                            result = result * 10.0 + f64::from(dec);
                        },
                        b'.' => point_seen = true,
                        _ => break,
                    }
                    self.iter.next();
                }

                Token::Real(result * factor)
            },

            // string literal
            // note: unclosed string literals at eof are accepted, however each script ends in:
            // newline
            // space
            // space
            // so "asdf would be "asdf\n  "
            // we don't take care of this here, that's the script loader's job
            b'"' | b'\'' => {
                self.iter.next(); // skip over opening quote
                let quote = head.1; // opening quote mark char

                // new head after opening quote
                let head = match self.iter.peek() {
                    Some(&(head, _)) => head,
                    None => return Some(Token::String(b"")),
                };

                let string = loop {
                    // matching next() here implicitly skips closing quote
                    match self.iter.next() {
                        Some((tail, ch)) if ch == quote => break sl(&self.src, head..tail),
                        Some(_) => (),

                        // In GML, if a quote is unclosed it's still a valid string,
                        // but interestingly enough unclosed strings end in CRLF and three spaces.
                        // This is likely due to how the runner allocates scripts and is UB,
                        // and to match the runner behaviour we append that to the end of scripts.
                        None => break sl(&self.src, head..),
                    }
                };
                Token::String(string)
            },

            // hexadecimal real literal.
            // a single $ with no valid hexadecimal chars after it is equivalent to $0.
            b'$' => {
                self.iter.next(); // skip '$'

                // new head after '$'
                let head = match self.iter.peek() {
                    Some(&(i, _)) => i,
                    None => return Some(Token::Real(0.0)),
                };

                let hex = loop {
                    match self.iter.peek() {
                        Some(&(tail, ch)) => match ch {
                            b'0'..=b'9' | b'a'..=b'f' | b'A'..=b'F' => {
                                self.iter.next();
                            },
                            _ => break sl(&self.src, head..tail),
                        },
                        None => break sl(&self.src, head..),
                    }
                };

                if hex.is_empty() {
                    Token::Real(0.0)
                } else {
                    // the only time we actually need strings in the entire lexer
                    // this is guaranteed to be ASCII so this is fine
                    let s = unsafe { str::from_utf8_unchecked(hex) };
                    Token::Real(
                        // if it failed to parse it must be too large, so we return the max value
                        u64::from_str_radix(s, 16).unwrap_or(u64::MAX) as f64,
                    )
                }
            },

            // operator, separator or possibly just an invalid character
            0x00..=b'~' => {
                let op_sep_ch = |ch| match ch & 0b0111_1111 {
                    b'!' => Token::Operator(Operator::Not),
                    b'&' => Token::Operator(Operator::BitwiseAnd),
                    b'(' => Token::Separator(Separator::ParenLeft),
                    b')' => Token::Separator(Separator::ParenRight),
                    b'*' => Token::Operator(Operator::Multiply),
                    b'+' => Token::Operator(Operator::Add),
                    b',' => Token::Separator(Separator::Comma),
                    b'-' => Token::Operator(Operator::Subtract),
                    b'/' => Token::Operator(Operator::Divide),
                    b':' => Token::Separator(Separator::Colon),
                    b';' => Token::Separator(Separator::Semicolon),
                    b'<' => Token::Operator(Operator::LessThan),
                    b'=' => Token::Operator(Operator::Assign),
                    b'>' => Token::Operator(Operator::GreaterThan),
                    b'[' => Token::Separator(Separator::BracketLeft),
                    b']' => Token::Separator(Separator::BracketRight),
                    b'^' => Token::Operator(Operator::BitwiseXor),
                    b'{' => Token::Separator(Separator::BraceLeft),
                    b'|' => Token::Operator(Operator::BitwiseOr),
                    b'}' => Token::Separator(Separator::BraceRight),
                    b'~' => Token::Operator(Operator::Complement),
                    _ => Token::InvalidChar(head.0, head.1),
                };

                let token1 = op_sep_ch(head.1);
                self.iter.next();

                if let Token::Operator(op) = token1 {
                    let ch2 = match self.iter.peek() {
                        Some(&(_, ch)) => ch,
                        None => return Some(Token::Operator(op)),
                    };

                    // boolean operators that are just repeated chars
                    // such as && || ^^
                    if head.1 == ch2 {
                        let repeated_combo = match op {
                            Operator::BitwiseAnd => Operator::And,
                            Operator::BitwiseOr => Operator::Or,
                            Operator::BitwiseXor => Operator::Xor,
                            Operator::LessThan => Operator::BinaryShiftLeft,
                            Operator::GreaterThan => Operator::BinaryShiftRight,

                            Operator::Assign => Operator::Equal,

                            // single line comments
                            Operator::Divide => {
                                self.iter.next();
                                while let Some(&(_, ch)) = self.iter.peek() {
                                    match ch {
                                        b'\n' | b'\r' => break,
                                        _ => {
                                            self.iter.next();
                                        },
                                    }
                                }
                                return self.next()
                            },

                            _ => return Some(Token::Operator(op)),
                        };
                        self.iter.next(); // consume ch2
                        Token::Operator(repeated_combo)
                    } else if ch2 == b'=' {
                        // assignment operator combos such as += -= *= /=

                        let eq_combo = match op {
                            // boolean operators
                            // == is in above match condition since it's a repeated character
                            Operator::Not => Operator::NotEqual,

                            // comparison operators
                            Operator::LessThan => Operator::LessThanOrEqual,
                            Operator::GreaterThan => Operator::GreaterThanOrEqual,

                            // assignment operators
                            Operator::Add => Operator::AssignAdd,
                            Operator::Subtract => Operator::AssignSubtract,
                            Operator::Multiply => Operator::AssignMultiply,
                            Operator::Divide => Operator::AssignDivide,
                            Operator::BitwiseAnd => Operator::AssignBitwiseAnd,
                            Operator::BitwiseOr => Operator::AssignBitwiseOr,
                            Operator::BitwiseXor => Operator::AssignBitwiseXor,

                            _ => return Some(Token::Operator(op)),
                        };
                        self.iter.next(); // consume ch2
                        Token::Operator(eq_combo)
                    } else if op == Operator::Divide && ch2 == b'*' {
                        // multi-line comments

                        self.iter.next();
                        while let Some(&(_, ch)) = self.iter.peek() {
                            match ch {
                                b'*' => {
                                    self.iter.next();
                                    if let Some(&(_, b'/')) = self.iter.peek() {
                                        self.iter.next();
                                        break
                                    }
                                },
                                _ => {
                                    self.iter.next();
                                },
                            }
                        }
                        return self.next()
                    } else if op == Operator::LessThan && ch2 == b'>' {
                        // <> is the same as != (let's call it a diamond)

                        self.iter.next(); // consume ch2
                        Token::Operator(Operator::NotEqual)
                    } else {
                        Token::Operator(op)
                    }
                } else if let Token::Separator(Separator::Colon) = token1 {
                    // pascal-style := init-assignments

                    if self.iter.peek().map(|(_, ch)| *ch == b'=').unwrap_or(false) {
                        self.iter.next();
                        Token::Operator(Operator::Assign)
                    } else {
                        Token::Separator(Separator::Colon)
                    }
                } else {
                    token1
                }
            },

            // invalid unicode
            _ => {
                self.iter.next(); // skip (if possible)
                Token::InvalidChar(head.0, head.1)
            },
        })
    }
}

// The lexer is intrinsically tested via the AST tests.
