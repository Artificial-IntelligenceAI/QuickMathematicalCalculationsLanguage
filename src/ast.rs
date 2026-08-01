#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Number,
}

use crate::error::Span;

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    NumberLiteral(f64),
    /// A variable reference (the `x` in `(x)`), carrying where it was
    /// written so codegen can point at it if the variable turns out to be
    /// undeclared.
    Var(String, Span),
}

#[derive(Debug, Clone, PartialEq)]
pub enum PrintPart {
    Text(String),
    Value(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Declare {
        name: String,
        /// Where the name itself was written, so a redeclaration can be
        /// pointed at.
        name_span: Span,
        ty: Type,
        value: Expr,
    },
    Print {
        parts: Vec<PrintPart>,
    },
}

pub type Program = Vec<Stmt>;
