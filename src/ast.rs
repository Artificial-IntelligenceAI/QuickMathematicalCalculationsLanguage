#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Number,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    NumberLiteral(i64),
    Var(String),
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
        ty: Type,
        value: Expr,
    },
    Print {
        parts: Vec<PrintPart>,
    },
}

pub type Program = Vec<Stmt>;
