use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub line: usize, // 1-indexed
    pub col: usize,  // 1-indexed, in grapheme clusters
}

impl Span {
    pub fn new(line: usize, col: usize) -> Self {
        Span { line, col }
    }
}

#[derive(Debug, Clone)]
pub struct Spanned<T> {
    pub node: T,
    pub span: Span,
}

impl<T> Spanned<T> {
    pub fn new(node: T, span: Span) -> Self {
        Spanned { node, span }
    }
}

/// The QMCL Error Handler's diagnostic type: what went wrong, where, and
/// what to do about it.
#[derive(Debug, Clone)]
pub struct QmclError {
    pub message: String,
    pub span: Option<Span>,
    pub suggestions: Vec<String>,
}

impl QmclError {
    pub fn new(message: impl Into<String>) -> Self {
        QmclError {
            message: message.into(),
            span: None,
            suggestions: Vec::new(),
        }
    }

    pub fn at(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    pub fn suggest(mut self, suggestion: impl Into<String>) -> Self {
        self.suggestions.push(suggestion.into());
        self
    }

    /// Rust-compiler-style rendering: the message, a `-->` file:line:col,
    /// the offending source line with a caret under the exact spot, then
    /// suggested fix(es) under `help:`.
    pub fn render(&self, filename: &str, source: &str) -> String {
        let mut out = String::new();
        out.push_str("QMCL Error Handler\n");
        out.push_str(&format!("error: {}\n", self.message));

        if let Some(span) = self.span {
            out.push_str(&format!("  --> {}:{}:{}\n", filename, span.line, span.col));
            let lines: Vec<&str> = source.lines().collect();
            if let Some(line_text) = lines.get(span.line.saturating_sub(1)) {
                let gutter = span.line.to_string();
                let pad = " ".repeat(gutter.len());
                out.push_str(&format!("{} |\n", pad));
                out.push_str(&format!("{} | {}\n", gutter, line_text));
                let caret_pad = " ".repeat(span.col.saturating_sub(1));
                out.push_str(&format!("{} | {}^\n", pad, caret_pad));
            }
        }

        for (i, suggestion) in self.suggestions.iter().enumerate() {
            if i == 0 {
                out.push_str(&format!("help: {}\n", suggestion));
            } else {
                out.push_str(&format!("      {}\n", suggestion));
            }
        }

        out
    }
}

impl fmt::Display for QmclError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}
