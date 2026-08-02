use unicode_segmentation::UnicodeSegmentation;

use crate::error::{QmclError, Span, Spanned};

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Declare,
    Print,
    Repeat,
    From,
    To,
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
    /// `{` — opens a statement block (e.g. a loop body). Distinct from `[]`,
    /// which is the argument-list delimiter (print's args, not full
    /// statements).
    LBrace,
    /// `}` — closes a statement block. Also terminates whatever statement
    /// it belongs to (a loop, etc.) on its own — no trailing `.` needed,
    /// same convention as C/Rust/Java's `{}` blocks.
    RBrace,
    Equals,
    Plus,
    Minus,
    Star,
    /// `/` or `÷` — both produce this same token, `÷` is just an alias.
    Slash,
    /// `^` or `**` — both produce this same token, `**` is just an alias.
    Caret,
    Greater,
    Less,
    /// Statement terminator `.`. Only ever produced outside a quoted span —
    /// numeric literals are always written quoted (e.g. `'1000'`), so a bare
    /// `.` at the top level is unambiguously a terminator, not a decimal point.
    Period,
    Eof,
}

/// Human-readable description of a token, for error messages.
pub fn describe(tok: &Token) -> String {
    match tok {
        Token::Declare => "'declare'".to_string(),
        Token::Print => "'print'".to_string(),
        Token::Repeat => "'repeat'".to_string(),
        Token::From => "'from'".to_string(),
        Token::To => "'to'".to_string(),
        Token::TypeName(t) => format!("type name '{}'", t),
        Token::Quoted(s) => format!("'{}'", s),
        Token::Str(s) => format!("\"{}\"", s),
        Token::Ident(s) => format!("identifier '{}'", s),
        Token::LParen => "'('".to_string(),
        Token::RParen => "')'".to_string(),
        Token::LBracket => "'['".to_string(),
        Token::RBracket => "']'".to_string(),
        Token::LBrace => "'{'".to_string(),
        Token::RBrace => "'}'".to_string(),
        Token::Equals => "'='".to_string(),
        Token::Plus => "'+'".to_string(),
        Token::Minus => "'-'".to_string(),
        Token::Star => "'*'".to_string(),
        Token::Slash => "'/'".to_string(),
        Token::Caret => "'^' (or '**')".to_string(),
        Token::Greater => "'>'".to_string(),
        Token::Less => "'<'".to_string(),
        Token::Period => "'.'".to_string(),
        Token::Eof => "end of file".to_string(),
    }
}

/// Characters that can never be part of a bare identifier or emoji name.
fn is_reserved(g: &str) -> bool {
    matches!(
        g,
        "'" | "\"" | "(" | ")" | "[" | "]" | "{" | "}" | "=" | "." | "\\" | "+" | "-" | "*" | "/"
            | "÷" | "^" | ">" | "<"
    )
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
    line: usize,
    col: usize,
}

const KEYWORDS: &[(&str, fn() -> Token)] = &[
    ("declare", || Token::Declare),
    ("print", || Token::Print),
    ("repeat", || Token::Repeat),
    ("from", || Token::From),
    ("to", || Token::To),
];

const TYPE_NAMES: &[&str] = &["number", "string", "boolean", "percentage", "integer"];

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Lexer {
            graphemes: src.graphemes(true).collect(),
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    fn current_span(&self) -> Span {
        Span::new(self.line, self.col)
    }

    fn peek(&self) -> Option<&'a str> {
        self.graphemes.get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<&'a str> {
        let g = self.peek();
        if let Some(g) = g {
            self.pos += 1;
            if g == "\n" {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
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

    pub fn tokenize(mut self) -> Result<Vec<Spanned<Token>>, QmclError> {
        let mut tokens = Vec::new();
        loop {
            self.skip_whitespace();
            let start = self.current_span();
            match self.peek() {
                None => {
                    tokens.push(Spanned::new(Token::Eof, start));
                    break;
                }
                Some("'") => tokens.push(Spanned::new(self.read_quoted('\'', true)?, start)),
                Some("\"") => tokens.push(Spanned::new(self.read_quoted('"', false)?, start)),
                Some("(") => {
                    self.advance();
                    tokens.push(Spanned::new(Token::LParen, start));
                }
                Some(")") => {
                    self.advance();
                    tokens.push(Spanned::new(Token::RParen, start));
                }
                Some("[") => {
                    self.advance();
                    tokens.push(Spanned::new(Token::LBracket, start));
                }
                Some("]") => {
                    self.advance();
                    tokens.push(Spanned::new(Token::RBracket, start));
                }
                Some("{") => {
                    self.advance();
                    tokens.push(Spanned::new(Token::LBrace, start));
                }
                Some("}") => {
                    self.advance();
                    tokens.push(Spanned::new(Token::RBrace, start));
                }
                Some("=") => {
                    self.advance();
                    tokens.push(Spanned::new(Token::Equals, start));
                }
                Some("+") => {
                    self.advance();
                    tokens.push(Spanned::new(Token::Plus, start));
                }
                Some("-") => {
                    self.advance();
                    tokens.push(Spanned::new(Token::Minus, start));
                }
                Some("*") => {
                    self.advance();
                    if self.peek() == Some("*") {
                        self.advance();
                        tokens.push(Spanned::new(Token::Caret, start));
                    } else {
                        tokens.push(Spanned::new(Token::Star, start));
                    }
                }
                Some("/") | Some("÷") => {
                    self.advance();
                    tokens.push(Spanned::new(Token::Slash, start));
                }
                Some("^") => {
                    self.advance();
                    tokens.push(Spanned::new(Token::Caret, start));
                }
                Some(">") => {
                    self.advance();
                    tokens.push(Spanned::new(Token::Greater, start));
                }
                Some("<") => {
                    self.advance();
                    tokens.push(Spanned::new(Token::Less, start));
                }
                Some(".") => {
                    self.advance();
                    tokens.push(Spanned::new(Token::Period, start));
                }
                Some("\\") => {
                    return Err(QmclError::new("unexpected '\\' outside a quoted literal")
                        .at(start)
                        .rule("a '\\' is only meaningful inside a quoted '...'/\"...\" span, where it starts an escape sequence")
                        .suggest("remove the '\\', or move it inside a quoted span"));
                }
                Some(g) => {
                    if is_ascii_digit(g) {
                        return Err(QmclError::new(format!(
                            "bare digit '{}' isn't allowed outside a quoted literal",
                            g
                        ))
                        .at(start)
                        .rule("numeric literals must always be written inside quotes")
                        .suggest(format!("wrap the number in quotes, e.g. '{}000'", g)));
                    }
                    tokens.push(Spanned::new(self.read_ident()?, start));
                }
            }
        }
        Ok(tokens)
    }

    /// Scans a `'...'` or `"..."` span (opening delimiter must be at `pos`),
    /// decoding escapes, and returns it as the appropriate token.
    fn read_quoted(&mut self, quote: char, is_name_or_literal: bool) -> Result<Token, QmclError> {
        let start = self.current_span();
        self.advance(); // consume opening quote
        let mut raw = String::new();
        loop {
            match self.advance() {
                None => {
                    return Err(QmclError::new("unterminated quoted literal")
                        .at(start)
                        .rule("a quoted name/literal must be closed with a matching quote before the line/file ends")
                        .suggest(format!("add a closing {} to end it", quote)))
                }
                Some("\\") => match self.advance() {
                    None => {
                        return Err(QmclError::new("unterminated escape sequence")
                            .at(start)
                            .rule("a '\\' inside quotes must be followed by a valid escape character")
                            .suggest("use one of: \\' \\\" \\\\ \\n \\t"))
                    }
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

    fn read_ident(&mut self) -> Result<Token, QmclError> {
        let start = self.current_span();
        let mut text = String::new();
        while let Some(g) = self.peek() {
            if g.chars().all(char::is_whitespace) || is_reserved(g) {
                break;
            }
            text.push_str(g);
            self.advance();
        }
        if text.is_empty() {
            return Err(QmclError::new("expected an identifier").at(start));
        }
        for (kw, make) in KEYWORDS {
            if *kw == text {
                return Ok(make());
            }
        }
        if TYPE_NAMES.contains(&text.as_str()) {
            return Ok(Token::TypeName(text));
        }
        // number:16 / number:32 / number:64 and integer:8/16/32/64 — a
        // precision suffix on those types. ':' and digits aren't reserved
        // characters, so this already scans as a single identifier-like
        // token above; the parser is what actually validates/parses the
        // width and rejects anything else that happens to contain a colon.
        for prefix in ["number:", "integer:"] {
            if let Some(width) = text.strip_prefix(prefix) {
                if !width.is_empty() && width.chars().all(|c| c.is_ascii_digit()) {
                    return Ok(Token::TypeName(text));
                }
            }
        }
        Ok(Token::Ident(text))
    }
}
