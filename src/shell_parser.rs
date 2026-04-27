//! Recursive-descent parser for the shell's control structures.
//!
//! Produces a [`Cmd`] AST so the evaluator can dispatch on real syntactic
//! shape rather than splicing strings on `;`.  Replaces the earlier
//! ad-hoc `command.split(';')`-based handling of `if`, `while`, and
//! `for` loops.
//!
//! The grammar is the POSIX-ish core, intentionally small:
//!
//!     program     := list
//!     list        := pipeline (';' pipeline)*
//!     pipeline    := simple
//!     simple      := WORD WORD*                    -- a single command + args
//!     if-expr     := 'if' list ';' 'then' list (';' 'elif' list ';' 'then' list)*
//!                    (';' 'else' list)? ';' 'fi'
//!     while-expr  := 'while' list ';' 'do' list ';' 'done'
//!     for-expr    := 'for' WORD 'in' WORD* ';' 'do' list ';' 'done'
//!
//! `if`/`while`/`for` may appear in place of a simple command.
//!
//! Limitations:
//!   * No real piping — `|` is left to the existing `execute_pipeline`
//!     fast path.
//!   * No nested heredocs or quoting; words are whitespace-split.
//!   * No grouping `{ ; }` or subshells `(...)`.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    Word(String),
    Semi,            // ;
    Newline,
    KwIf,
    KwThen,
    KwElif,
    KwElse,
    KwFi,
    KwWhile,
    KwDo,
    KwDone,
    KwFor,
    KwIn,
    Eof,
}

pub fn tokenize(input: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut iter = input.chars().peekable();
    let mut buf = String::new();

    let flush_word = |buf: &mut String, out: &mut Vec<Token>| {
        if buf.is_empty() { return; }
        let s = core::mem::take(buf);
        out.push(match s.as_str() {
            "if" => Token::KwIf,
            "then" => Token::KwThen,
            "elif" => Token::KwElif,
            "else" => Token::KwElse,
            "fi" => Token::KwFi,
            "while" => Token::KwWhile,
            "do" => Token::KwDo,
            "done" => Token::KwDone,
            "for" => Token::KwFor,
            "in" => Token::KwIn,
            _ => Token::Word(s),
        });
    };

    while let Some(&c) = iter.peek() {
        match c {
            ' ' | '\t' => { flush_word(&mut buf, &mut out); iter.next(); }
            ';' => { flush_word(&mut buf, &mut out); iter.next(); out.push(Token::Semi); }
            '\n' => { flush_word(&mut buf, &mut out); iter.next(); out.push(Token::Newline); }
            '"' => {
                // Quoted string: take until closing quote.
                iter.next();
                while let Some(&q) = iter.peek() {
                    if q == '"' { iter.next(); break; }
                    buf.push(q);
                    iter.next();
                }
            }
            '\'' => {
                iter.next();
                while let Some(&q) = iter.peek() {
                    if q == '\'' { iter.next(); break; }
                    buf.push(q);
                    iter.next();
                }
            }
            _ => { buf.push(c); iter.next(); }
        }
    }
    flush_word(&mut buf, &mut out);
    out.push(Token::Eof);
    out
}

#[derive(Debug, Clone)]
pub enum Cmd {
    /// One executable command line, e.g. `echo hello`.
    Simple(Vec<String>),
    /// Sequence of commands separated by `;`.
    Sequence(Vec<Cmd>),
    /// `if cond ; then body ; elif cond ; then body ; else body ; fi`.
    If {
        branches: Vec<(Cmd, Cmd)>, // (condition, body) pairs (one per if/elif)
        else_branch: Option<alloc::boxed::Box<Cmd>>,
    },
    /// `while cond ; do body ; done`.
    While { cond: alloc::boxed::Box<Cmd>, body: alloc::boxed::Box<Cmd> },
    /// `for VAR in W1 W2 ... ; do BODY ; done`.
    For { var: String, items: Vec<String>, body: alloc::boxed::Box<Cmd> },
}

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    pub fn new(input: &str) -> Self {
        Parser { tokens: tokenize(input), pos: 0 }
    }

    fn peek(&self) -> &Token { &self.tokens[self.pos] }
    fn bump(&mut self) -> Token {
        let t = self.tokens[self.pos].clone();
        if self.pos + 1 < self.tokens.len() { self.pos += 1; }
        t
    }
    fn eat(&mut self, t: &Token) -> bool {
        if self.peek() == t { self.bump(); true } else { false }
    }

    /// Skip leading separators (`;` or newline).
    fn skip_sep(&mut self) {
        while matches!(self.peek(), Token::Semi | Token::Newline) {
            self.bump();
        }
    }

    pub fn parse_program(&mut self) -> Result<Cmd, &'static str> {
        self.skip_sep();
        let cmd = self.parse_list()?;
        Ok(cmd)
    }

    fn parse_list(&mut self) -> Result<Cmd, &'static str> {
        let mut items = Vec::new();
        items.push(self.parse_item()?);
        while matches!(self.peek(), Token::Semi | Token::Newline) {
            self.bump();
            // Trailing separator before a terminator keyword is fine.
            if matches!(self.peek(), Token::Eof | Token::KwFi | Token::KwDone
                | Token::KwElse | Token::KwElif | Token::KwThen | Token::KwDo) {
                break;
            }
            items.push(self.parse_item()?);
        }
        Ok(if items.len() == 1 { items.pop().unwrap() } else { Cmd::Sequence(items) })
    }

    fn parse_item(&mut self) -> Result<Cmd, &'static str> {
        match self.peek() {
            Token::KwIf    => self.parse_if(),
            Token::KwWhile => self.parse_while(),
            Token::KwFor   => self.parse_for(),
            _ => self.parse_simple(),
        }
    }

    fn parse_simple(&mut self) -> Result<Cmd, &'static str> {
        let mut words = Vec::new();
        while let Token::Word(w) = self.peek().clone() {
            words.push(w);
            self.bump();
        }
        if words.is_empty() {
            return Err("expected command");
        }
        Ok(Cmd::Simple(words))
    }

    fn parse_if(&mut self) -> Result<Cmd, &'static str> {
        self.bump(); // 'if'
        let mut branches: Vec<(Cmd, Cmd)> = Vec::new();
        let mut else_branch: Option<alloc::boxed::Box<Cmd>> = None;

        let cond = self.parse_list()?;
        self.skip_sep();
        if !self.eat(&Token::KwThen) {
            return Err("if: expected 'then'");
        }
        self.skip_sep();
        let body = self.parse_list()?;
        branches.push((cond, body));

        loop {
            self.skip_sep();
            match self.peek() {
                Token::KwElif => {
                    self.bump();
                    let cond = self.parse_list()?;
                    self.skip_sep();
                    if !self.eat(&Token::KwThen) {
                        return Err("elif: expected 'then'");
                    }
                    self.skip_sep();
                    let body = self.parse_list()?;
                    branches.push((cond, body));
                }
                Token::KwElse => {
                    self.bump();
                    self.skip_sep();
                    let body = self.parse_list()?;
                    else_branch = Some(alloc::boxed::Box::new(body));
                }
                Token::KwFi => {
                    self.bump();
                    return Ok(Cmd::If { branches, else_branch });
                }
                _ => return Err("if: expected elif/else/fi"),
            }
        }
    }

    fn parse_while(&mut self) -> Result<Cmd, &'static str> {
        self.bump(); // 'while'
        let cond = self.parse_list()?;
        self.skip_sep();
        if !self.eat(&Token::KwDo) {
            return Err("while: expected 'do'");
        }
        self.skip_sep();
        let body = self.parse_list()?;
        self.skip_sep();
        if !self.eat(&Token::KwDone) {
            return Err("while: expected 'done'");
        }
        Ok(Cmd::While {
            cond: alloc::boxed::Box::new(cond),
            body: alloc::boxed::Box::new(body),
        })
    }

    fn parse_for(&mut self) -> Result<Cmd, &'static str> {
        self.bump(); // 'for'
        let var = match self.bump() {
            Token::Word(w) => w,
            _ => return Err("for: expected variable name"),
        };
        if !self.eat(&Token::KwIn) {
            return Err("for: expected 'in'");
        }
        let mut items = Vec::new();
        while let Token::Word(w) = self.peek().clone() {
            items.push(w);
            self.bump();
        }
        self.skip_sep();
        if !self.eat(&Token::KwDo) {
            return Err("for: expected 'do'");
        }
        self.skip_sep();
        let body = self.parse_list()?;
        self.skip_sep();
        if !self.eat(&Token::KwDone) {
            return Err("for: expected 'done'");
        }
        Ok(Cmd::For { var, items, body: alloc::boxed::Box::new(body) })
    }
}

/// Parse a complete program.  Returns Err with a static message on syntax
/// errors.
pub fn parse(input: &str) -> Result<Cmd, &'static str> {
    Parser::new(input).parse_program()
}

/// Reassemble a Simple cmd back into a single string for the existing
/// dispatcher to run.
pub fn join_simple(words: &[String]) -> String {
    let mut s = String::new();
    for (i, w) in words.iter().enumerate() {
        if i > 0 { s.push(' '); }
        // Re-quote any word containing whitespace; otherwise pass through.
        if w.contains(' ') || w.contains('\t') {
            s.push('"'); s.push_str(w); s.push('"');
        } else {
            s.push_str(w);
        }
    }
    s
}

/// Pretty-print an AST for debugging (used by the `parse` shell command).
pub fn pretty(cmd: &Cmd, indent: usize) -> String {
    let pad: String = (0..indent).map(|_| ' ').collect();
    match cmd {
        Cmd::Simple(words) => alloc::format!("{}Simple({:?})", pad, words),
        Cmd::Sequence(items) => {
            let mut s = alloc::format!("{}Sequence:", pad);
            for it in items {
                s.push('\n');
                s.push_str(&pretty(it, indent + 2));
            }
            s
        }
        Cmd::If { branches, else_branch } => {
            let mut s = alloc::format!("{}If:", pad);
            for (i, (c, b)) in branches.iter().enumerate() {
                s.push('\n');
                s.push_str(&alloc::format!("{}  branch {}:", pad, i));
                s.push('\n');
                s.push_str(&alloc::format!("{}    cond:\n{}", pad, pretty(c, indent + 6)));
                s.push('\n');
                s.push_str(&alloc::format!("{}    body:\n{}", pad, pretty(b, indent + 6)));
            }
            if let Some(eb) = else_branch {
                s.push('\n');
                s.push_str(&alloc::format!("{}  else:\n{}", pad, pretty(eb, indent + 4)));
            }
            s
        }
        Cmd::While { cond, body } => {
            alloc::format!("{}While:\n{}  cond:\n{}\n{}  body:\n{}",
                pad, pad, pretty(cond, indent + 4), pad, pretty(body, indent + 4))
        }
        Cmd::For { var, items, body } => {
            alloc::format!("{}For {} in {:?}:\n{}",
                pad, var, items, pretty(body, indent + 2))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_if_else() {
        let ast = parse("if test 1 -eq 1 ; then echo yes ; else echo no ; fi").unwrap();
        match ast {
            Cmd::If { branches, else_branch } => {
                assert_eq!(branches.len(), 1);
                assert!(else_branch.is_some());
            }
            _ => panic!("expected If"),
        }
    }

    #[test]
    fn parse_while() {
        let ast = parse("while test x = x ; do echo loop ; done").unwrap();
        assert!(matches!(ast, Cmd::While { .. }));
    }

    #[test]
    fn parse_for() {
        let ast = parse("for x in a b c ; do echo $x ; done").unwrap();
        match ast {
            Cmd::For { var, items, .. } => {
                assert_eq!(var, "x");
                assert_eq!(items, &["a", "b", "c"]);
            }
            _ => panic!("expected For"),
        }
    }
}
