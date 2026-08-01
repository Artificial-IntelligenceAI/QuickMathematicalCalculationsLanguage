use crate::ast::*;
use crate::error::{QmclError, Span, Spanned};
use crate::lexer::{describe, Token};

pub struct Parser {
    tokens: Vec<Spanned<Token>>,
    pos: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Spanned<Token>>) -> Self {
        Parser { tokens, pos: 0 }
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.pos].node
    }

    fn peek_span(&self) -> Span {
        self.tokens[self.pos].span
    }

    /// The span to blame for an error about the current token. At EOF,
    /// there's no real content at the synthetic "end of file" position (it
    /// may even be a line/column past anything that actually exists in the
    /// source), so point at the last real token instead — that's where the
    /// user actually needs to look to fix things.
    fn error_span(&self) -> Span {
        if *self.peek() == Token::Eof && self.pos > 0 {
            self.tokens[self.pos - 1].span
        } else {
            self.peek_span()
        }
    }

    fn advance(&mut self) -> Spanned<Token> {
        let t = self.tokens[self.pos].clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        t
    }

    fn expect(&mut self, expected: &Token) -> Result<Span, QmclError> {
        if self.peek() == expected {
            let span = self.peek_span();
            self.advance();
            Ok(span)
        } else {
            Err(QmclError::new(format!(
                "expected {}, found {}",
                describe(expected),
                describe(self.peek())
            ))
            .at(self.error_span()))
        }
    }

    /// Returns every statement that parsed successfully alongside every
    /// error found — a parse error doesn't stop the parser, it recovers to
    /// the next likely statement boundary (see `synchronize`) and keeps
    /// going, so a file with several mistakes reports all of them at once
    /// instead of just the first. An empty error list means it's safe to
    /// hand `Program` to codegen.
    pub fn parse_program(&mut self) -> (Program, Vec<QmclError>) {
        let mut stmts = Vec::new();
        let mut errors = Vec::new();
        while *self.peek() != Token::Eof {
            match self.parse_stmt() {
                Ok(stmt) => stmts.push(stmt),
                Err(e) => {
                    errors.push(e);
                    self.synchronize();
                }
            }
        }
        (stmts, errors)
    }

    /// After a parse error, skip tokens until we're past a '.' (the
    /// statement terminator) or sitting right at what looks like the start
    /// of the next statement, so parsing can resume from a sane point
    /// instead of stopping outright.
    fn synchronize(&mut self) {
        loop {
            match self.peek() {
                Token::Eof => return,
                Token::Period => {
                    self.advance();
                    return;
                }
                Token::Declare | Token::Print => return,
                _ => {
                    self.advance();
                }
            }
        }
    }

    fn parse_stmt(&mut self) -> Result<Stmt, QmclError> {
        match self.peek() {
            Token::Declare => self.parse_declare(),
            Token::Print => self.parse_print(),
            other => Err(QmclError::new(format!(
                "expected a statement, found {}",
                describe(other)
            ))
            .at(self.error_span())
            .rule("every statement must start with 'declare' or 'print'")
            .suggest("start this statement with 'declare' or 'print'")),
        }
    }

    /// expr := term (('+' | '-') term)*  — left-associative, lowest precedence.
    fn parse_expr(&mut self) -> Result<Expr, QmclError> {
        let mut left = self.parse_term()?;
        loop {
            let op = match self.peek() {
                Token::Plus => BinOp::Add,
                Token::Minus => BinOp::Sub,
                _ => break,
            };
            self.advance();
            let right = self.parse_term()?;
            left = Expr::BinaryOp(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    /// term := atom (('*' | '/') atom)*  — binds tighter than +/-.
    fn parse_term(&mut self) -> Result<Expr, QmclError> {
        let mut left = self.parse_atom()?;
        loop {
            let op = match self.peek() {
                Token::Star => BinOp::Mul,
                Token::Slash => BinOp::Div,
                _ => break,
            };
            self.advance();
            let right = self.parse_atom()?;
            left = Expr::BinaryOp(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    /// atom := a quoted number literal, or a (variable) reference.
    fn parse_atom(&mut self) -> Result<Expr, QmclError> {
        let tok = self.advance();
        match tok.node {
            Token::Quoted(s) => {
                let n: f64 = s.parse().map_err(|_| {
                    QmclError::new(format!("'{}' is not a valid number literal", s))
                        .at(tok.span)
                        .rule("a quoted number literal must be a valid number")
                        .suggest("use digits, optionally with a decimal point, e.g. '1000' or '3.14'")
                })?;
                Ok(Expr::NumberLiteral(n))
            }
            Token::LParen => {
                let ident_tok = self.advance();
                let (name, span) = match ident_tok.node {
                    Token::Ident(n) => (n, ident_tok.span),
                    other => {
                        return Err(QmclError::new(format!(
                            "expected an identifier inside (), found {}",
                            describe(&other)
                        ))
                        .at(ident_tok.span)
                        .rule("inside ( ), only a bare (unquoted) variable name is allowed")
                        .suggest("reference a variable like (x), with no quotes around it"))
                    }
                };
                self.expect(&Token::RParen).map_err(|e| {
                    e.rule("a '(' opened to reference a variable must be closed with ')'")
                        .suggest("add a ')' right after the variable name")
                })?;
                Ok(Expr::Var(name, span))
            }
            other => Err(QmclError::new(format!(
                "expected a number or (variable), found {}",
                describe(&other)
            ))
            .at(tok.span)
            .rule("expressions are built from quoted numbers and (variable) references")
            .suggest("write a number like '5' or a variable reference like (x)")),
        }
    }

    fn parse_declare(&mut self) -> Result<Stmt, QmclError> {
        self.advance(); // `declare`

        let name_tok = self.advance();
        let name_span = name_tok.span;
        let name = match name_tok.node {
            Token::Quoted(s) => s,
            other => {
                return Err(QmclError::new(format!(
                    "expected a quoted name after 'declare', found {}",
                    describe(&other)
                ))
                .at(name_tok.span)
                .rule("the name being declared must be written in quotes right after 'declare'")
                .suggest("write the name in quotes, e.g. declare 'x' = number '1000'."))
            }
        };

        self.expect(&Token::Equals).map_err(|e| {
            e.rule("'declare' is followed by '=' between the name and its type")
                .suggest("add an '=' here, e.g. declare 'x' = number '1000'.")
        })?;

        let ty_tok = self.advance();
        let ty = match ty_tok.node {
            Token::TypeName(t) if t == "number" => Type::Number,
            other => {
                return Err(QmclError::new(format!(
                    "expected a type name, found {}",
                    describe(&other)
                ))
                .at(ty_tok.span)
                .rule("a type name must follow the '=' in a declaration")
                .suggest("use 'number' — it's currently the only supported type"))
            }
        };

        let value = self.parse_expr()?;

        self.expect(&Token::Period).map_err(|e| {
            e.rule("every statement must end with a '.'")
                .suggest("add a '.' at the end, e.g. declare 'x' = number '1000'.")
        })?;

        Ok(Stmt::Declare { name, name_span, ty, value })
    }

    fn parse_print(&mut self) -> Result<Stmt, QmclError> {
        self.advance(); // `print`

        self.expect(&Token::LBracket).map_err(|e| {
            e.rule("'print' is always followed by its arguments inside [ ]")
                .suggest("add [ ] after 'print', e.g. print[\"hi\"].")
        })?;

        let mut parts = Vec::new();
        loop {
            match self.peek().clone() {
                Token::RBracket => break,
                Token::Str(s) => {
                    self.advance();
                    parts.push(PrintPart::Text(s));
                }
                Token::LParen | Token::Quoted(_) => {
                    let expr = self.parse_expr()?;
                    parts.push(PrintPart::Value(expr));
                }
                Token::Eof => {
                    return Err(QmclError::new("unclosed 'print[...]' — reached end of file")
                        .at(self.error_span())
                        .rule("every 'print[' must be closed with a matching ']'")
                        .suggest("add a ']' to close this print[...] call"))
                }
                other => {
                    return Err(QmclError::new(format!(
                        "unexpected {} inside print[...]",
                        describe(&other)
                    ))
                    .at(self.error_span())
                    .rule("each part of print[...] must start with \"text\", a number, or a (variable) reference")
                    .suggest("wrap text in \"...\" or start an expression with a number or (variable)"))
                }
            }
        }

        self.expect(&Token::RBracket).map_err(|e| {
            e.rule("print's arguments must be closed with a matching ']'")
                .suggest("add a ']' to close the print[...] call")
        })?;
        self.expect(&Token::Period).map_err(|e| {
            e.rule("every statement must end with a '.'")
                .suggest("add a '.' at the end")
        })?;

        Ok(Stmt::Print { parts })
    }
}
