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
            .at(self.peek_span()))
        }
    }

    pub fn parse_program(&mut self) -> Result<Program, QmclError> {
        let mut stmts = Vec::new();
        while *self.peek() != Token::Eof {
            stmts.push(self.parse_stmt()?);
        }
        Ok(stmts)
    }

    fn parse_stmt(&mut self) -> Result<Stmt, QmclError> {
        match self.peek() {
            Token::Declare => self.parse_declare(),
            Token::Print => self.parse_print(),
            other => Err(QmclError::new(format!(
                "expected a statement, found {}",
                describe(other)
            ))
            .at(self.peek_span())
            .suggest("statements start with 'declare' or 'print'")),
        }
    }

    fn parse_declare(&mut self) -> Result<Stmt, QmclError> {
        self.advance(); // `declare`

        let name_tok = self.advance();
        let name = match name_tok.node {
            Token::Quoted(s) => s,
            other => {
                return Err(QmclError::new(format!(
                    "expected a quoted name after 'declare', found {}",
                    describe(&other)
                ))
                .at(name_tok.span)
                .suggest("write the name in quotes, e.g. declare 'x' = number '1000'."))
            }
        };

        self.expect(&Token::Equals)
            .map_err(|e| e.suggest("declarations look like: declare 'x' = number '1000'."))?;

        let ty_tok = self.advance();
        let ty = match ty_tok.node {
            Token::TypeName(t) if t == "number" => Type::Number,
            other => {
                return Err(QmclError::new(format!(
                    "expected a type name, found {}",
                    describe(&other)
                ))
                .at(ty_tok.span)
                .suggest("currently only 'number' is a supported type"))
            }
        };

        let value_tok = self.advance();
        let value = match value_tok.node {
            Token::Quoted(s) => {
                let n: i64 = s.parse().map_err(|_| {
                    QmclError::new(format!("'{}' is not a valid number literal", s))
                        .at(value_tok.span)
                        .suggest("numbers must be plain digits, e.g. '1000'")
                })?;
                Expr::NumberLiteral(n)
            }
            other => {
                return Err(QmclError::new(format!(
                    "expected a quoted literal value, found {}",
                    describe(&other)
                ))
                .at(value_tok.span)
                .suggest("write the value in quotes, e.g. number '1000'"))
            }
        };

        self.expect(&Token::Period)
            .map_err(|e| e.suggest("statements end with a '.', e.g. declare 'x' = number '1000'."))?;

        Ok(Stmt::Declare { name, ty, value })
    }

    fn parse_print(&mut self) -> Result<Stmt, QmclError> {
        self.advance(); // `print`

        self.expect(&Token::LBracket)
            .map_err(|e| e.suggest("print's arguments go inside [ ], e.g. print[\"hi\"]."))?;

        let mut parts = Vec::new();
        loop {
            match self.peek().clone() {
                Token::RBracket => break,
                Token::Str(s) => {
                    self.advance();
                    parts.push(PrintPart::Text(s));
                }
                Token::LParen => {
                    self.advance();
                    let ident_tok = self.advance();
                    let (name, span) = match ident_tok.node {
                        Token::Ident(n) => (n, ident_tok.span),
                        other => {
                            return Err(QmclError::new(format!(
                                "expected an identifier inside (), found {}",
                                describe(&other)
                            ))
                            .at(ident_tok.span)
                            .suggest("reference a variable like (x), with no quotes around it"))
                        }
                    };
                    self.expect(&Token::RParen)?;
                    parts.push(PrintPart::Value(Expr::Var(name, span)));
                }
                other => {
                    return Err(QmclError::new(format!(
                        "unexpected {} inside print[...]",
                        describe(&other)
                    ))
                    .at(self.peek_span())
                    .suggest("print[...] only accepts \"text\" and (variable) references"))
                }
            }
        }

        self.expect(&Token::RBracket)?;
        self.expect(&Token::Period)
            .map_err(|e| e.suggest("statements end with a '.'"))?;

        Ok(Stmt::Print { parts })
    }
}
