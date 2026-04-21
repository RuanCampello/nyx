//! High-level IR (HIR) produced by semantic analysis.
//!
//! HIR is a tree-structured, fully resolved and typed.
//! Identifiers are lowered to stable numeric IDs.

use crate::{
    hir::{
        error::{HirError, HirErrorKind},
        functions::FunctionBuilder,
        symbols::SymbolTable,
    },
    lexer::token::Span,
    parser::{
        expression::{BinaryOperator, UnaryOperator},
        statement,
    },
};
use lasso::{Key, Spur};

pub mod error;
mod functions;
mod symbols;

#[derive(Debug, Clone, PartialEq)]
pub struct Hir {
    pub symbols: Vec<String>,
    pub functions: Vec<Function>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Statement {
    Let {
        id: LocalId,
        init: Option<Expression>,
    },
    Expr(Expression),
    Return(Option<Expression>),
    If {
        condition: Expression,
        then_block: Block,
        else_block: Option<Block>,
    },
    While {
        condition: Expression,
        body: Block,
    },
    Block(Block),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Expression {
    pub(crate) kind: ExpressionKind,
    pub(crate) typ: Type,
    span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    pub(crate) id: FunctionId,
    pub(crate) name: SymbolId,
    pub(crate) params: Vec<Parameter>,
    pub(crate) locals: Vec<Local>,
    pub(crate) return_type: Type,
    pub(crate) body: Block,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parameter {
    pub(crate) id: LocalId,
    name: SymbolId,
    pub(crate) typ: Type,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Local {
    pub(crate) id: LocalId,
    name: SymbolId,
    pub(crate) typ: Type,
    mutable: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub(crate) statements: Vec<Statement>,
    span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    I32,
    I64,
    F32,
    F64,
    Bool,
    String,
    Unit,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExpressionKind {
    Unit,
    Integer(i64),
    Float(f64),
    String(String),
    Bool(bool),
    Local(LocalId),
    Unary {
        operator: UnaryOperator,
        expr: Box<Expression>,
    },
    Binary {
        operator: BinaryOperator,
        left: Box<Expression>,
        right: Box<Expression>,
    },
    Assign {
        target: LocalId,
        value: Box<Expression>,
    },
    Call {
        function: FunctionId,
        args: Vec<Expression>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FunctionId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SymbolId(pub Spur);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LocalId(pub u32);

/// Lowers the program AST to a HIR program.
pub fn lower<'h>(statements: Vec<statement::Statement<'h>>) -> Result<Hir, HirError<'h>> {
    let mut symbols = SymbolTable::new();
    let (signatures, functions_map) =
        functions::collect_function_signatures(&statements, &mut symbols)?;

    let mut functions = Vec::new();
    for statement in statements {
        let function = match statement {
            statement::Statement::Fn(function) => function,
            other => {
                return Err(HirError {
                    kind: HirErrorKind::TopLevelNonFunction,
                });
            }
        };

        let symbol = symbols.insert(function.name);
        let id = *functions_map
            .get(&symbol)
            .expect("function id assigned during signature collection");

        let function = FunctionBuilder::new(&signatures, &functions_map, &mut symbols, function);
        functions.push(function.lower()?);
    }

    Ok(Hir {
        symbols: symbols.into_symbols(),
        functions,
    })
}

impl Type {
    pub(in crate::hir) const fn is_number(&self) -> bool {
        matches!(self, Self::I32 | Self::I64 | Self::F32 | Self::F64)
    }
}

impl From<statement::Type> for Type {
    fn from(value: statement::Type) -> Self {
        Self::from(&value)
    }
}

impl From<&statement::Type> for Type {
    fn from(value: &statement::Type) -> Self {
        use statement::Type as AstType;
        match value {
            AstType::I32 { .. } => Type::I32,
            AstType::I64 { .. } => Type::I64,
            AstType::F32 { .. } => Type::F32,
            AstType::F64 { .. } => Type::F64,
            AstType::Bool { .. } => Type::Bool,
            AstType::String { .. } => Type::String,
        }
    }
}

impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Type::I32 => "i32",
            Type::I64 => "i64",
            Type::F32 => "f32",
            Type::F64 => "f64",
            Type::Bool => "bool",
            Type::String => "String",
            Type::Unit => "unit",
        };

        f.write_str(s)
    }
}

impl std::fmt::Debug for SymbolId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SymbolId({})", self.0.into_usize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    #[test]
    fn unknown_identifier() {
        let statements = Parser::new("fn main() { x + 1; }").parse().unwrap();
        let err = super::lower(statements).unwrap_err();

        assert_eq!(
            err.kind,
            HirErrorKind::UndeclaredIdentifier {
                name: "x".to_string()
            }
        )
    }

    #[test]
    fn mutability() {
        let statements = Parser::new(
            r#"
            fn main() {
                let x: i32 = 1;
                x = 2;
            }
        "#,
        )
        .parse()
        .unwrap();

        let err = super::lower(statements).unwrap_err();
        assert_eq!(err.kind, HirErrorKind::ImmutableBind { name: "x".into() });

        let statements = Parser::new(
            r#"
            fn main() {
                let mut x: i32 = 1;
                x = 2;
            }
        "#,
        )
        .parse()
        .unwrap();

        assert!(super::lower(statements).is_ok());
    }

    #[test]
    fn while_condition_must_be_bool() {
        let statements = Parser::new(
            r#"
            fn main() {
                let x: i32 = 1;
                while x { }
            }
        "#,
        )
        .parse()
        .unwrap();

        let err = super::lower(statements).unwrap_err();
        assert_eq!(
            err.kind,
            HirErrorKind::TypeMismatch {
                expected: Type::Bool,
                found: Type::I32
            }
        )
    }

    #[test]
    fn if_condition_must_be_bool() {
        let statements = Parser::new(
            r#"
            fn main() {
                let x: i64 = 1;
                if x { }
            }
        "#,
        )
        .parse()
        .unwrap();

        let err = super::lower(statements).unwrap_err();
        assert_eq!(
            err.kind,
            HirErrorKind::TypeMismatch {
                expected: Type::Bool,
                found: Type::I64
            }
        )
    }

    #[test]
    fn duplicated_function() {
        let statements = Parser::new(
            r#"
            fn foo(): i32 { 1 }
            fn foo(): i32 { 2 }
        "#,
        )
        .parse()
        .unwrap();

        let err = super::lower(statements).unwrap_err();

        assert_eq!(
            err.kind,
            HirErrorKind::DuplicateFunction { name: "foo".into() }
        );
    }

    #[test]
    fn arity_mismatch_too_many() {
        let statements = Parser::new(
            r#"
            fn add(a: i32, b: i32): i32 { a + b }
            fn main() { add(1, 2, 3); }
        "#,
        )
        .parse()
        .unwrap();

        let err = super::lower(statements).unwrap_err();

        assert_eq!(
            err.kind,
            HirErrorKind::ArityMismatch {
                name: "add".into(),
                expected: 2,
                found: 3,
            }
        );
    }

    #[test]
    fn unknown_function() {
        let statements = Parser::new("fn main() { foo(); }").parse().unwrap();

        let err = super::lower(statements).unwrap_err();

        assert_eq!(
            err.kind,
            HirErrorKind::UnknownFunction { name: "foo".into() }
        );
    }

    #[test]
    fn type_mismatch_in_let() {
        let statements = Parser::new(
            r#"
            fn add(a: i32, b: i32): i32 { a + b }
            fn main() {
                let x: i32 = add(1, 2);
                let y: bool = add(1, 2);
            }
        "#,
        )
        .parse()
        .unwrap();

        let err = super::lower(statements).unwrap_err();
        assert_eq!(
            err.kind,
            HirErrorKind::TypeMismatch {
                expected: Type::Bool,
                found: Type::I32
            }
        )
    }

    #[test]
    fn type_inference_from_expr() {
        let statements = Parser::new("fn main() { let x = 1 + 2; }").parse().unwrap();
        let hir = super::lower(statements);

        // TODO: exact assertion here of expected state
        assert!(hir.is_ok())
    }

    #[test]
    fn top_level_non_function() {
        let statements = Parser::new("let x: i64 = 1;").parse().unwrap();
        let err = super::lower(statements).unwrap_err();

        assert_eq!(err.kind, HirErrorKind::TopLevelNonFunction)
    }

    #[test]
    fn integer_literal_as_function_arg_typed_i64() {
        let statements = Parser::new(
            r#"
            fn foo(x: i64): i64 { x }
            fn main() { foo(1); }
        "#,
        )
        .parse()
        .unwrap();

        let hir = super::lower(statements).unwrap();

        assert_eq!(hir.functions.len(), 2);
        let foo = &hir.functions[0];
        assert_eq!(foo.return_type, Type::I64);
        assert_eq!(foo.params.len(), 1);
        assert_eq!(foo.params[0].typ, Type::I64);

        let main = &hir.functions[1];
        let call_expr = match &main.body.statements[0] {
            Statement::Expr(expr) => expr,
            other => panic!("expected Expr statement, got {other:?}"),
        };
        assert_eq!(call_expr.typ, Type::I64);
        let arg = match &call_expr.kind {
            ExpressionKind::Call { args, .. } => {
                assert_eq!(args.len(), 1);
                &args[0]
            }
            other => panic!("expected Call expression, got {other:?}"),
        };
        assert_eq!(arg.typ, Type::I64);
        assert!(matches!(arg.kind, ExpressionKind::Integer(1)));
    }

    #[test]
    fn float_literal_defaults_to_f64() {
        let statements = Parser::new("fn main() { let x = 3.14; }").parse().unwrap();
        let hir = super::lower(statements).unwrap();

        let func = &hir.functions[0];
        assert_eq!(func.locals.len(), 1);
        assert_eq!(func.locals[0].typ, Type::F64);
    }

    #[test]
    fn integer_literal_defaults_to_i32_in_binary_expr() {
        let statements = Parser::new("fn main() { let x = 1 + 2; }").parse().unwrap();
        let hir = super::lower(statements).unwrap();

        let func = &hir.functions[0];
        assert_eq!(func.locals[0].typ, Type::I32);

        let stmt = &func.body.statements[0];
        assert!(matches!(stmt, Statement::Let { id: LocalId(0), .. }));
    }

    #[test]
    fn float_literal_widens_to_f32() {
        let statements = Parser::new("fn main() { let x: f32 = 3.14; }")
            .parse()
            .unwrap();
        let hir = super::lower(statements).unwrap();

        let func = &hir.functions[0];
        assert_eq!(func.locals.len(), 1);
        assert_eq!(func.locals[0].typ, Type::F32);
    }

    #[test]
    fn mutable_assign_widens_literal() {
        let statements = Parser::new(
            r#"
            fn main() {
                let mut x: i64 = 0;
                x = 99;
            }
        "#,
        )
        .parse()
        .unwrap();
        let hir = super::lower(statements).unwrap();

        let func = &hir.functions[0];
        assert_eq!(func.locals.len(), 1);
        assert_eq!(func.locals[0].typ, Type::I64);
        assert_eq!(func.locals[0].mutable, true);

        let assign_expr = match &func.body.statements[1] {
            Statement::Expr(expr) => expr,
            other => panic!("expected Expr statement, got {other:?}"),
        };
        assert_eq!(assign_expr.typ, Type::I64);
        let (target_id, value) = match &assign_expr.kind {
            ExpressionKind::Assign { target, value } => (target, value.as_ref()),
            other => panic!("expected Assign expression, got {other:?}"),
        };

        assert_eq!(*target_id, LocalId(0));
        assert_eq!(value.typ, Type::I64);
        assert!(matches!(value.kind, ExpressionKind::Integer(99)));
    }

    #[test]
    fn integer_literal_widens_in_binary_with_i64_local() {
        let statements = Parser::new(
            r#"
            fn main() {
                let x: i64 = 10;
                let y = x + 1;
            }
        "#,
        )
        .parse()
        .unwrap();
        let hir = super::lower(statements).unwrap();

        let func = &hir.functions[0];
        assert_eq!(func.locals.len(), 2);
        assert_eq!(func.locals[0].typ, Type::I64);
        assert_eq!(func.locals[1].typ, Type::I64);

        let y_stmt = &func.body.statements[1];
        assert!(matches!(y_stmt, Statement::Let { id: LocalId(1), .. }));
    }
}
