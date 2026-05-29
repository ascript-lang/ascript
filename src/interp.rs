//! Async tree-walking evaluator. `eval_expr`/`exec` are async to establish
//! the event-loop seam from spec §7, even though the skeleton never suspends.

use crate::ast::{BinOp, Expr, ExprKind, Stmt, UnOp};
use crate::error::AsError;
use crate::value::Value;
use async_recursion::async_recursion;

pub struct Interp {
    /// Captured program output (what `print` writes). Exposed for testing and
    /// flushed to stdout by the CLI.
    pub output: String,
}

impl Interp {
    pub fn new() -> Self {
        Interp { output: String::new() }
    }

    pub async fn exec(&mut self, program: &[Stmt]) -> Result<(), AsError> {
        for stmt in program {
            match stmt {
                Stmt::Expr(e) => {
                    self.eval_expr(e).await?;
                }
            }
        }
        Ok(())
    }

    #[async_recursion(?Send)]
    pub async fn eval_expr(&mut self, expr: &Expr) -> Result<Value, AsError> {
        match &expr.kind {
            ExprKind::Number(n) => Ok(Value::Number(*n)),
            ExprKind::Str(s) => Ok(Value::Str(s.as_str().into())),
            ExprKind::Bool(b) => Ok(Value::Bool(*b)),
            ExprKind::Nil => Ok(Value::Nil),
            ExprKind::Ident(name) => Err(AsError::at(
                format!("undefined variable '{}'", name),
                expr.span,
            )),
            ExprKind::Unary { op, expr: operand } => {
                let v = self.eval_expr(operand).await?;
                match op {
                    UnOp::Neg => match v {
                        Value::Number(n) => Ok(Value::Number(-n)),
                        _ => Err(AsError::at("cannot negate a non-number", operand.span)),
                    },
                    UnOp::Not => Ok(Value::Bool(!v.is_truthy())),
                }
            }
            ExprKind::Binary { op, lhs, rhs } => {
                match op {
                    BinOp::And => {
                        let l = self.eval_expr(lhs).await?;
                        return if l.is_truthy() { self.eval_expr(rhs).await } else { Ok(l) };
                    }
                    BinOp::Or => {
                        let l = self.eval_expr(lhs).await?;
                        return if l.is_truthy() { Ok(l) } else { self.eval_expr(rhs).await };
                    }
                    BinOp::Coalesce => {
                        let l = self.eval_expr(lhs).await?;
                        return if l == Value::Nil { self.eval_expr(rhs).await } else { Ok(l) };
                    }
                    _ => {}
                }

                let l = self.eval_expr(lhs).await?;
                let r = self.eval_expr(rhs).await?;

                match op {
                    BinOp::Eq => return Ok(Value::Bool(l == r)),
                    BinOp::Ne => return Ok(Value::Bool(l != r)),
                    _ => {}
                }

                let (a, b) = match (&l, &r) {
                    (Value::Number(a), Value::Number(b)) => (*a, *b),
                    _ => return Err(AsError::at("operator requires two numbers", expr.span)),
                };
                let result = match op {
                    BinOp::Add => Value::Number(a + b),
                    BinOp::Sub => Value::Number(a - b),
                    BinOp::Mul => Value::Number(a * b),
                    BinOp::Div => Value::Number(a / b),
                    BinOp::Mod => Value::Number(a % b),
                    BinOp::Pow => Value::Number(a.powf(b)),
                    BinOp::Lt => Value::Bool(a < b),
                    BinOp::Le => Value::Bool(a <= b),
                    BinOp::Gt => Value::Bool(a > b),
                    BinOp::Ge => Value::Bool(a >= b),
                    BinOp::Eq | BinOp::Ne | BinOp::And | BinOp::Or | BinOp::Coalesce => {
                        unreachable!("handled above")
                    }
                };
                Ok(result)
            }
            ExprKind::Call { callee, args } => {
                let name = match &callee.kind {
                    ExprKind::Ident(n) => n.clone(),
                    _ => return Err(AsError::new("only named builtins are callable in the skeleton")),
                };
                let mut values = Vec::new();
                for a in args {
                    values.push(self.eval_expr(a).await?);
                }
                self.call_builtin(&name, &values)
            }
        }
    }

    fn call_builtin(&mut self, name: &str, args: &[Value]) -> Result<Value, AsError> {
        match name {
            "print" => {
                let parts: Vec<String> = args.iter().map(|v| v.to_string()).collect();
                self.output.push_str(&parts.join(" "));
                self.output.push('\n');
                Ok(Value::Nil)
            }
            other => Err(AsError::new(format!("'{}' is not a function", other))),
        }
    }
}

impl Default for Interp {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;
    use crate::parser::parse;

    async fn eval_to_value(src: &str) -> Value {
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let Stmt::Expr(e) = &stmts[0];
        interp.eval_expr(e).await.unwrap()
    }

    #[tokio::test]
    async fn evaluates_arithmetic_with_precedence() {
        match eval_to_value("1 + 2 * 3").await {
            Value::Number(n) => assert_eq!(n, 7.0),
            other => panic!("expected number, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn print_writes_to_the_output_buffer() {
        let stmts = parse(&lex("print(1 + 2 * 3)").unwrap()).unwrap();
        let mut interp = Interp::new();
        interp.exec(&stmts).await.unwrap();
        assert_eq!(interp.output, "7\n");
    }

    #[tokio::test]
    async fn comparison_and_equality() {
        assert_eq!(eval_to_value("1 < 2").await, Value::Bool(true));
        assert_eq!(eval_to_value("2 == 2").await, Value::Bool(true));
        assert_eq!(eval_to_value("1 != 2").await, Value::Bool(true));
        assert_eq!(eval_to_value("\"a\" == \"a\"").await, Value::Bool(true));
    }

    #[tokio::test]
    async fn exponent_evaluates() {
        assert_eq!(eval_to_value("2 ** 10").await, Value::Number(1024.0));
    }

    #[tokio::test]
    async fn short_circuit_and_coalesce() {
        assert_eq!(eval_to_value("false && nope").await, Value::Bool(false));
        assert_eq!(eval_to_value("true || nope").await, Value::Bool(true));
        assert_eq!(eval_to_value("nil ?? 5").await, Value::Number(5.0));
        assert_eq!(eval_to_value("3 ?? nope").await, Value::Number(3.0));
        assert_eq!(eval_to_value("!0").await, Value::Bool(false));
    }

    #[tokio::test]
    async fn calling_a_non_builtin_is_an_error() {
        let stmts = parse(&lex("nope(1)").unwrap()).unwrap();
        let mut interp = Interp::new();
        let err = interp.exec(&stmts).await.unwrap_err();
        assert!(err.message.contains("is not a function"));
    }
}
