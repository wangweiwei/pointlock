//! `${{ ... }}` surface-text → [`pointlock_ir::Expr`] (02 §8.1 grammar).
//!
//! Recursive descent over the closed grammar; the compiled AST is data —
//! after `check` the YAML surface text no longer exists (spine §7).
//!
//! ```ebnf
//! Expr    ::= Literal | Ref | FnCall
//! FnCall  ::= FnName '(' [ Expr (',' Expr)* ] ')'
//! FnName  ::= 'eq' | 'ne' | 'not' | 'and' | 'or' | 'concat' | 'len'
//!           | 'coalesce' | 'jsonPath' | 'regexMatch'
//! Ref     ::= RefPath                  (* 02 §8.1; validated by RefPath *)
//! Literal ::= String | Number | 'true' | 'false' | 'null'
//! ```
//!
//! Strings are double-quoted with JSON escapes (surrogate pairs included)
//! or single-quoted verbatim; numbers are JSON numbers. Arity, literal-only
//! positions and scope legality are *not* checked here — that is
//! `pointlock_expr::check` plus the compiler `check` stage.

use pointlock_ir::{Expr, PureFn, RefPath};

/// Maximum nesting depth of the expression text (fail-closed).
const MAX_EXPR_DEPTH: usize = 64;

/// Parse failure over `${{ ... }}` inner text, classified for diagnostic
/// code assignment (RF3003/RF3004/RF3005).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExprTextError {
    /// Out-of-grammar text.
    Syntax {
        /// Human-readable reason.
        message: String,
    },
    /// A call to a function outside the ten-function whitelist.
    UnknownFunction {
        /// The unknown function name as written.
        name: String,
    },
    /// A reference path rejected by the [`RefPath`] grammar.
    BadRef {
        /// The path as written.
        path: String,
        /// The grammar's rejection reason.
        reason: String,
    },
}

/// Parses the inner text of a `${{ ... }}` scalar into an [`Expr`].
pub(crate) fn parse_expr_text(inner: &str) -> Result<Expr, ExprTextError> {
    let mut cursor = Cursor {
        text: inner,
        pos: 0,
    };
    let expr = cursor.parse_expr(0)?;
    cursor.skip_ws();
    if !cursor.at_end() {
        return Err(cursor.syntax("unexpected trailing input after the expression"));
    }
    Ok(expr)
}

struct Cursor<'a> {
    text: &'a str,
    pos: usize,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<char> {
        self.text[self.pos..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn at_end(&self) -> bool {
        self.pos >= self.text.len()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(c) if c.is_whitespace()) {
            self.bump();
        }
    }

    fn syntax(&self, message: impl Into<String>) -> ExprTextError {
        ExprTextError::Syntax {
            message: format!("{} (at offset {})", message.into(), self.pos),
        }
    }

    fn parse_expr(&mut self, depth: usize) -> Result<Expr, ExprTextError> {
        if depth >= MAX_EXPR_DEPTH {
            return Err(self.syntax(format!(
                "expression nesting exceeds the limit of {MAX_EXPR_DEPTH}"
            )));
        }
        self.skip_ws();
        match self.peek() {
            None => Err(self.syntax("expected an expression, found end of input")),
            Some('"') => self.parse_double_quoted().map(Expr::lit),
            Some('\'') => self.parse_single_quoted().map(Expr::lit),
            Some(c) if c.is_ascii_digit() || c == '-' => self.parse_number(),
            Some(c) if c.is_ascii_alphabetic() || c == '_' => self.parse_word(depth),
            Some(c) => Err(self.syntax(format!("unexpected character '{c}'"))),
        }
    }

    /// A word is a keyword literal, a function call, or a reference path.
    fn parse_word(&mut self, depth: usize) -> Result<Expr, ExprTextError> {
        let start = self.pos;
        while matches!(
            self.peek(),
            Some(c) if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')
        ) {
            self.bump();
        }
        let word = &self.text[start..self.pos];
        match word {
            "true" => return Ok(Expr::lit(true)),
            "false" => return Ok(Expr::lit(false)),
            "null" => return Ok(Expr::lit(serde_json::Value::Null)),
            _ => {}
        }
        self.skip_ws();
        if self.peek() == Some('(') {
            let Some(function) = pure_fn_by_name(word) else {
                return Err(ExprTextError::UnknownFunction {
                    name: word.to_owned(),
                });
            };
            return self.parse_call(function, depth);
        }
        match RefPath::new(word) {
            Ok(path) => Ok(Expr::reference(path)),
            Err(err) => Err(ExprTextError::BadRef {
                path: word.to_owned(),
                reason: err.reason.to_owned(),
            }),
        }
    }

    fn parse_call(&mut self, function: PureFn, depth: usize) -> Result<Expr, ExprTextError> {
        self.bump(); // consume '('
        let mut args = Vec::new();
        self.skip_ws();
        if self.peek() == Some(')') {
            self.bump();
            return Ok(Expr::call(function, args));
        }
        loop {
            args.push(self.parse_expr(depth + 1)?);
            self.skip_ws();
            match self.bump() {
                Some(',') => {}
                Some(')') => return Ok(Expr::call(function, args)),
                Some(c) => {
                    return Err(self.syntax(format!(
                        "expected ',' or ')' in the argument list, found '{c}'"
                    )));
                }
                None => return Err(self.syntax("unterminated argument list")),
            }
        }
    }

    fn parse_number(&mut self) -> Result<Expr, ExprTextError> {
        let start = self.pos;
        while matches!(
            self.peek(),
            Some(c) if c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E')
        ) {
            self.bump();
        }
        let text = &self.text[start..self.pos];
        let number: serde_json::Number = text
            .parse()
            .map_err(|_| self.syntax(format!("'{text}' is not a JSON number")))?;
        Ok(Expr::lit(serde_json::Value::Number(number)))
    }

    fn parse_double_quoted(&mut self) -> Result<String, ExprTextError> {
        self.bump(); // consume '"'
        let mut out = String::new();
        loop {
            match self.bump() {
                None => return Err(self.syntax("unterminated string literal")),
                Some('"') => return Ok(out),
                Some('\\') => out.push(self.parse_escape()?),
                Some(c) => out.push(c),
            }
        }
    }

    fn parse_escape(&mut self) -> Result<char, ExprTextError> {
        match self.bump() {
            Some('"') => Ok('"'),
            Some('\\') => Ok('\\'),
            Some('/') => Ok('/'),
            Some('n') => Ok('\n'),
            Some('t') => Ok('\t'),
            Some('r') => Ok('\r'),
            Some('b') => Ok('\u{0008}'),
            Some('f') => Ok('\u{000C}'),
            Some('u') => self.parse_unicode_escape(),
            Some(c) => Err(self.syntax(format!("unsupported escape '\\{c}'"))),
            None => Err(self.syntax("unterminated escape sequence")),
        }
    }

    fn parse_unicode_escape(&mut self) -> Result<char, ExprTextError> {
        let first = self.parse_hex4()?;
        // Surrogate pair handling, JSON-style.
        if (0xD800..=0xDBFF).contains(&first) {
            if self.bump() != Some('\\') || self.bump() != Some('u') {
                return Err(self.syntax("high surrogate not followed by a \\u escape"));
            }
            let second = self.parse_hex4()?;
            if !(0xDC00..=0xDFFF).contains(&second) {
                return Err(self.syntax("invalid low surrogate in \\u escape pair"));
            }
            let combined = 0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00);
            return char::from_u32(combined)
                .ok_or_else(|| self.syntax("invalid \\u surrogate pair"));
        }
        if (0xDC00..=0xDFFF).contains(&first) {
            return Err(self.syntax("lone low surrogate in \\u escape"));
        }
        char::from_u32(first).ok_or_else(|| self.syntax("invalid \\u escape"))
    }

    fn parse_hex4(&mut self) -> Result<u32, ExprTextError> {
        let mut value = 0u32;
        for _ in 0..4 {
            let Some(c) = self.bump() else {
                return Err(self.syntax("truncated \\u escape (expected 4 hex digits)"));
            };
            let Some(digit) = c.to_digit(16) else {
                return Err(self.syntax(format!("'{c}' is not a hex digit in a \\u escape")));
            };
            value = value * 16 + digit;
        }
        Ok(value)
    }

    fn parse_single_quoted(&mut self) -> Result<String, ExprTextError> {
        self.bump(); // consume '\''
        let mut out = String::new();
        loop {
            match self.bump() {
                None => return Err(self.syntax("unterminated string literal")),
                Some('\'') => return Ok(out),
                Some(c) => out.push(c),
            }
        }
    }
}

/// The surface-name → [`PureFn`] table (wire names, spine A.4).
fn pure_fn_by_name(name: &str) -> Option<PureFn> {
    match name {
        "eq" => Some(PureFn::Eq),
        "ne" => Some(PureFn::Ne),
        "not" => Some(PureFn::Not),
        "and" => Some(PureFn::And),
        "or" => Some(PureFn::Or),
        "concat" => Some(PureFn::Concat),
        "len" => Some(PureFn::Len),
        "coalesce" => Some(PureFn::Coalesce),
        "jsonPath" => Some(PureFn::JsonPath),
        "regexMatch" => Some(PureFn::RegexMatch),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn r(path: &str) -> Expr {
        Expr::reference(RefPath::new(path).expect("valid ref path"))
    }

    #[test]
    fn parses_the_grammar() {
        assert_eq!(parse_expr_text("params.ssid"), Ok(r("params.ssid")));
        assert_eq!(parse_expr_text(" true "), Ok(Expr::lit(true)));
        assert_eq!(
            parse_expr_text("null"),
            Ok(Expr::lit(serde_json::Value::Null))
        );
        assert_eq!(parse_expr_text("-3.5"), Ok(Expr::lit(json!(-3.5))));
        assert_eq!(
            parse_expr_text(r#""a\"bA😀""#),
            Ok(Expr::lit(json!("a\"bA😀")))
        );
        assert_eq!(
            parse_expr_text("'raw text'"),
            Ok(Expr::lit(json!("raw text")))
        );
        assert_eq!(
            parse_expr_text("eq(steps.probe.output.matched, true)"),
            Ok(Expr::call(
                PureFn::Eq,
                vec![r("steps.probe.output.matched"), Expr::lit(true)]
            ))
        );
        assert_eq!(
            parse_expr_text("and(not(false), regexMatch(params.ssid, \"^lab\", 'i'))"),
            Ok(Expr::call(
                PureFn::And,
                vec![
                    Expr::call(PureFn::Not, vec![Expr::lit(false)]),
                    Expr::call(
                        PureFn::RegexMatch,
                        vec![
                            r("params.ssid"),
                            Expr::lit(json!("^lab")),
                            Expr::lit(json!("i"))
                        ]
                    ),
                ]
            ))
        );
    }

    #[test]
    fn classifies_failures() {
        assert!(matches!(
            parse_expr_text("size(params.ssid)"),
            Err(ExprTextError::UnknownFunction { name }) if name == "size"
        ));
        assert!(matches!(
            parse_expr_text("secrets.token"),
            Err(ExprTextError::BadRef { path, .. }) if path == "secrets.token"
        ));
        assert!(matches!(
            parse_expr_text("steps.x"),
            Err(ExprTextError::BadRef { .. })
        ));
        assert!(matches!(
            parse_expr_text("eq(1, 2) trailing"),
            Err(ExprTextError::Syntax { .. })
        ));
        assert!(matches!(
            parse_expr_text("eq(1,"),
            Err(ExprTextError::Syntax { .. })
        ));
        assert!(matches!(
            parse_expr_text(""),
            Err(ExprTextError::Syntax { .. })
        ));
        assert!(matches!(
            parse_expr_text("\"unterminated"),
            Err(ExprTextError::Syntax { .. })
        ));
    }

    #[test]
    fn nesting_depth_fails_closed() {
        let mut deep = String::new();
        for _ in 0..70 {
            deep.push_str("not(");
        }
        deep.push_str("true");
        deep.push_str(&")".repeat(70));
        assert!(matches!(
            parse_expr_text(&deep),
            Err(ExprTextError::Syntax { .. })
        ));
    }
}
