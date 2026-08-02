#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Type {
    /// Bit width: 16, 32, or 64 (the default when no `:N` suffix is given,
    /// e.g. `number:16` vs plain `number`).
    Number(u8),
    String,
    Boolean,
    /// Stored normalized as a fraction (100% -> 1.0, 50% -> 0.5) so it's
    /// directly usable in arithmetic; printing re-multiplies by 100 and
    /// re-appends '%' for display. Always 64-bit — precision selection isn't
    /// extended to percentage in this pass.
    Percentage,
    /// Bit width: 8, 16, 32, or 64 (default 64, e.g. `integer:16` vs plain
    /// `integer`). A genuinely separate representation from Number — real
    /// integer arithmetic, not a float that looks whole.
    Integer(u8),
}

use crate::error::Span;

#[derive(Debug, Clone, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    /// Right-associative: 2^3^2 = 2^(3^2), not (2^3)^2.
    Pow,
    /// No boolean type yet, so comparisons produce a number: 1.0 (true) or
    /// 0.0 (false) — same simplification C used before it had a real bool.
    Gt,
    Lt,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// Also used for a Percentage literal, already normalized to a fraction
    /// by the time it reaches here (parsing strips the '%' and divides).
    /// The bool is whether the original literal used thousands separators
    /// (e.g. '1,000,000') — carried through so printing can reproduce them.
    NumberLiteral(f64, bool),
    /// Same grouped-flag idea as NumberLiteral, but for a genuine integer.
    IntegerLiteral(i64, bool),
    StringLiteral(String),
    BooleanLiteral(bool),
    /// A variable reference (the `x` in `(x)`), carrying where it was
    /// written so codegen can point at it if the variable turns out to be
    /// undeclared.
    Var(String, Span),
    /// Carries the operator's own span so a type-mismatch error (e.g. `+`
    /// between a string and a number) has somewhere to point.
    BinaryOp(BinOp, Box<Expr>, Box<Expr>, Span),
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
    /// `repeat 'i' from <start> to <end> [ body ].` — inclusive range,
    /// step +1. The loop variable is always a 64-bit integer. No lexical
    /// scoping exists yet, so it (and anything declared in the body)
    /// remains accessible after the loop ends, holding its final value.
    CountedLoop {
        var_name: String,
        /// Where the loop variable's name was written, for error spans.
        var_name_span: Span,
        start: Expr,
        end: Expr,
        body: Vec<Stmt>,
    },
}

pub type Program = Vec<Stmt>;
