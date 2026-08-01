use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Declare,
    Print,
    TypeName(String),
    /// Content of a `'...'` span: a declared name or a literal value.
    /// Which one it is depends on where the parser encounters it.
    Quoted(String),
    /// Content of a `"..."` span: string text.
    Str(String),
    /// A bare (unquoted) identifier, e.g. the `x` inside `(x)`.
    Ident(String),
    LParen,
    RParen,
    LBracket,
    RBracket,
    Equals,
    /// Statement terminator `.`. Only ever produced outside a quoted span —
    /// numeric literals are always written quoted (e.g. `'1000'`), so a bare
    /// `.` at the top level is unambiguously a terminator, not a decimal point.
    Period,
    Eof,
}

/// Characters that can never be part of a bare identifier or emoji name.
fn is_reserved(g: &str) -> bool {
    matches!(g, "'" | "\"" | "(" | ")" | "[" | "]" | "=" | "." | "\\")
}

fn is_ascii_digit(g: &str) -> bool {
    g.len() == 1 && g.chars().next().unwrap().is_ascii_digit()
}

fn decode_escapes(raw: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\'') => out.push('\''),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

pub struct Lexer<'a> {
    graphemes: Vec<&'a str>,
    pos: usize,
}

const KEYWORDS: &[(&str, fn() -> Token)] = &[
    ("declare", || Token::Declare),
    ("print", || Token::Print),
];

const TYPE_NAMES: &[&str] = &["number"];

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Lexer {
            graphemes: src.graphemes(true).collect(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<&'a str> {
        self.graphemes.get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<&'a str> {
        let g = self.peek();
        if g.is_some() {
            self.pos += 1;
        }
        g
    }

    fn skip_whitespace(&mut self) {
        while let Some(g) = self.peek() {
            if g.chars().all(char::is_whitespace) {
                self.advance();
            } else {
                break;
            }
        }
    }

    pub fn tokenize(mut self) -> Result<Vec<Token>, String> {
        let mut tokens = Vec::new();
        loop {
            self.skip_whitespace();
            match self.peek() {
                None => {
                    tokens.push(Token::Eof);
                    break;
                }
                Some("'") => tokens.push(self.read_quoted('\'', true)?),
                Some("\"") => tokens.push(self.read_quoted('"', false)?),
                Some("(") => {
                    self.advance();
                    tokens.push(Token::LParen);
                }
                Some(")") => {
                    self.advance();
                    tokens.push(Token::RParen);
                }
                Some("[") => {
                    self.advance();
                    tokens.push(Token::LBracket);
                }
                Some("]") => {
                    self.advance();
                    tokens.push(Token::RBracket);
                }
                Some("=") => {
                    self.advance();
                    tokens.push(Token::Equals);
                }
                Some(".") => {
                    self.advance();
                    tokens.push(Token::Period);
                }
                Some("\\") => {
                    return Err("unexpected '\\' outside a quoted literal".to_string());
                }
                Some(g) => {
                    if is_ascii_digit(g) {
                        return Err(format!(
                            "bare digits aren't allowed outside a quoted literal (found '{}') — numbers must be written like '1000'",
                            g
                        ));
                    }
                    tokens.push(self.read_ident()?);
                }
            }
        }
        Ok(tokens)
    }

    /// Scans a `'...'` or `"..."` span (opening delimiter must be at `pos`),
    /// decoding escapes, and returns it as the appropriate token.
    fn read_quoted(&mut self, quote: char, is_name_or_literal: bool) -> Result<Token, String> {
        self.advance(); // consume opening quote
        let mut raw = String::new();
        loop {
            match self.advance() {
                None => return Err("unterminated quoted literal".to_string()),
                Some("\\") => match self.advance() {
                    None => return Err("unterminated escape sequence".to_string()),
                    Some(esc) => {
                        raw.push('\\');
                        raw.push_str(esc);
                    }
                },
                Some(g) if g.chars().count() == 1 && g.chars().next() == Some(quote) => {
                    let content = decode_escapes(&raw);
                    return Ok(if is_name_or_literal {
                        Token::Quoted(content)
                    } else {
                        Token::Str(content)
                    });
                }
                Some(g) => raw.push_str(g),
            }
        }
    }

    fn read_ident(&mut self) -> Result<Token, String> {
        let mut text = String::new();
        while let Some(g) = self.peek() {
            if g.chars().all(char::is_whitespace) || is_reserved(g) {
                break;
            }
            text.push_str(g);
            self.advance();
        }
        if text.is_empty() {
            return Err("expected an identifier".to_string());
        }
        for (kw, make) in KEYWORDS {
            if *kw == text {
                return Ok(make());
            }
        }
        if TYPE_NAMES.contains(&text.as_str()) {
            return Ok(Token::TypeName(text));
        }
        Ok(Token::Ident(text))
    }
}
