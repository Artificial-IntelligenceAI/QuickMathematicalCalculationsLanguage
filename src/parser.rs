use crate::ast::*;
use crate::lexer::Token;

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Parser { tokens, pos: 0 }
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn advance(&mut self) -> Token {
        let t = self.tokens[self.pos].clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        t
    }

    fn expect(&mut self, expected: &Token) -> Result<(), String> {
        if self.peek() == expected {
            self.advance();
            Ok(())
        } else {
            Err(format!("expected {:?}, found {:?}", expected, self.peek()))
        }
    }

    pub fn parse_program(&mut self) -> Result<Program, String> {
        let mut stmts = Vec::new();
        while *self.peek() != Token::Eof {
            stmts.push(self.parse_stmt()?);
        }
        Ok(stmts)
    }

    fn parse_stmt(&mut self) -> Result<Stmt, String> {
        match self.peek() {
            Token::Declare => self.parse_declare(),
            Token::Print => self.parse_print(),
            other => Err(format!("unexpected token at start of statement: {:?}", other)),
        }
    }

    fn parse_declare(&mut self) -> Result<Stmt, String> {
        self.advance(); // `declare`
        let name = match self.advance() {
            Token::Quoted(s) => s,
            other => return Err(format!("expected a quoted name after 'declare', found {:?}", other)),
        };
        self.expect(&Token::Equals)?;
        let ty = match self.advance() {
            Token::TypeName(t) if t == "number" => Type::Number,
            other => return Err(format!("expected a type name, found {:?}", other)),
        };
        let value = match self.advance() {
            Token::Quoted(s) => {
                let n: i64 = s
                    .parse()
                    .map_err(|_| format!("'{}' is not a valid number literal", s))?;
                Expr::NumberLiteral(n)
            }
            other => return Err(format!("expected a quoted literal value, found {:?}", other)),
        };
        self.expect(&Token::Period)?;
        Ok(Stmt::Declare { name, ty, value })
    }

    fn parse_print(&mut self) -> Result<Stmt, String> {
        self.advance(); // `print`
        self.expect(&Token::LBracket)?;
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
                    let name = match self.advance() {
                        Token::Ident(n) => n,
                        other => {
                            return Err(format!(
                                "expected an identifier inside (), found {:?}",
                                other
                            ))
                        }
                    };
                    self.expect(&Token::RParen)?;
                    parts.push(PrintPart::Value(Expr::Var(name)));
                }
                other => return Err(format!("unexpected token in print[...]: {:?}", other)),
            }
        }
        self.expect(&Token::RBracket)?;
        self.expect(&Token::Period)?;
        Ok(Stmt::Print { parts })
    }
}
