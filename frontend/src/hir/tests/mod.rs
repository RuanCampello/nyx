use super::*;
use crate::{
    hir::error::{ConstFnViolationKind, HirErrorKind},
    parser::Parser,
};

fn with_lowered<R>(src: &str, f: impl for<'a> FnOnce(Hir<'a>) -> R) -> R {
    let arena = bumpalo::Bump::new();
    let statements = Parser::new(src).parse().expect("parse failed");
    let hir = super::lower(statements, &arena).expect("lowering failed");

    f(hir)
}

fn with_lowered_err<R>(src: &str, f: impl for<'a> FnOnce(HirError<'a>) -> R) -> R {
    let arena = bumpalo::Bump::new();
    let stmts = Parser::new(src).parse().expect("parse failed");
    let err = super::lower(stmts, &arena).reported_errors[0];

    f(err)
}

fn missing_pattern(src: &str) -> Option<String> {
    let arena = bumpalo::Bump::new();
    let statements = Parser::new(src).parse().expect("parse failed");

    super::lower(statements, &arena)
        .reported_errors
        .iter()
        .find_map(|error| match error.kind {
            HirErrorKind::NonExhaustiveMatch { missing, .. } => Some(missing.to_owned()),
            _ => None,
        })
}

#[test]
fn statics_are_laid_out_with_their_initialiser() {
    let source = "static LIMIT: i32 = 10;\nstatic mut CURSOR: i32 = 0;\nfn main(): i32 { LIMIT }";
    with_lowered(source, |hir| {
        assert_eq!(hir.statics.len(), 2);

        let limit = hir.statics[StaticId(0)];
        assert_eq!(limit.init, Literal::Int(10));
        assert!(!limit.is_mut);

        let cursor = hir.statics[StaticId(1)];
        assert_eq!(cursor.init, Literal::Int(0));
        assert!(cursor.is_mut);
    });
}

#[test]
fn mutable_static_needs_an_unsafe_context() {
    let source = "static mut CURSOR: i32 = 0;\nfn main(): i32 { CURSOR }";
    with_lowered_err(source, |err| {
        assert_eq!(err.kind, HirErrorKind::UnsafeStatic { name: "nyx::CURSOR" });
    });
}

#[test]
fn mutable_static_is_allowed_inside_unsafe() {
    let source = "static mut CURSOR: i32 = 0;\n@unsafe\nfn main(): i32 { CURSOR }";
    with_lowered(source, |_| {});
}

#[test]
fn static_initialiser_must_be_known_at_compile_time() {
    let source =
        "const fn seed(): i32 { 7 }\nstatic mut CURSOR: i32 = seed();\nfn main(): i32 { 0 }";
    with_lowered_err(source, |err| {
        assert_eq!(err.kind, HirErrorKind::NonConstStaticInit { name: "CURSOR" });
    });
}

#[test]
fn unknown_identifier() {
    with_lowered_err("fn main() { x + 1; }", |err| {
        assert_eq!(err.kind, HirErrorKind::UndeclaredIdentifier { name: "x" });
    });
}

#[test]
fn missing_return() {
    with_lowered_err("fn answer(): i32 {}", |err| {
        assert_eq!(
            err.kind,
            HirErrorKind::MissingReturn { name: "nyx::answer", expected: TypeKind::I32.into() }
        );
    });

    with_lowered_err(
        r#"
            fn sign(x: i32): i32 {
                if x < 0 {
                    return -1;
                }
            }
        "#,
        |err| assert!(matches!(err.kind, HirErrorKind::MissingReturn { .. })),
    );

    with_lowered(
        r#"
            fn sign(x: i32): i32 {
                if x < 0 {
                    return -1;
                }

                1
            }
        "#,
        |_| {},
    );
}

#[test]
fn valueless_return_must_match_the_declared_return_type() {
    with_lowered_err("fn answer(): i32 { return; }", |error| {
        assert!(matches!(
            error.kind,
            HirErrorKind::TypeAnnotationMismatch { expected, found, .. }
                if expected.kind() == TypeKind::I32 && found.kind() == TypeKind::Unit
        ));
    });

    with_lowered("fn discard() { return; }", |_| {});
}

#[test]
fn expression_body_is_checked_against_the_declared_return_type() {
    with_lowered_err("fn answer(): bool = 42;", |error| {
        assert!(matches!(
            error.kind,
            HirErrorKind::TypeAnnotationMismatch { expected, found, .. }
                if expected.kind() == TypeKind::Bool && found.kind() == TypeKind::I32
        ));
    });
}

#[test]
fn branching_expression_bodies_return_on_every_path() {
    with_lowered(
        r#"
            fn absolute(value: i32): i32 = if value < 0 { -value } else { value };
            fn classify(value: i32): i32 = match value { 0 -> 1, _ -> value, };
            "#,
        |_| {},
    );
}

#[test]
fn mutability() {
    with_lowered_err(
        r#"
            fn main() {
                let x: i32 = 1;
                x = 2;
            }
        "#,
        |err| assert!(matches!(err.kind, HirErrorKind::ImmutableBind { name: "x", .. })),
    );

    with_lowered(
        r#"
            fn main() {
                let mut x: i32 = 1;
                x = 2;
            }
        "#,
        |_| {},
    );
}

#[test]
fn range_endpoints_must_be_integers() {
    with_lowered_err(
        r#"
            fn main() {
                loop 1.0..2.0 { }
            }
        "#,
        |err| {
            assert_eq!(err.kind, HirErrorKind::InvalidRangeType { typ: TypeKind::F64.into() });
        },
    );
}

#[test]
fn loop_over_inferred_array() {
    with_lowered(
        r#"
            fn main(): i32 {
                let values = [2, 3, 5];
                let mut total = 0;
                loop value in values {
                    total = total + value;
                }
                total
            }
        "#,
        |hir| assert_eq!(hir.arrays.get(ArrayId(0)).element, TypeKind::I32.into()),
    );

    for (annotation, expected) in [
        ("u8", TypeKind::U8),
        ("i16", TypeKind::I16),
        ("u32", TypeKind::U32),
        ("i64", TypeKind::I64),
    ] {
        let source = format!(
            r#"
                fn main() {{
                    let values = [1, 2, 3];
                    let mut total: {annotation} = 0;
                    loop value in values {{
                        total = total + value;
                    }}
                }}
            "#
        );
        with_lowered(&source, |hir| {
            assert_eq!(
                hir.arrays.get(ArrayId(0)).element,
                expected.into(),
                "element should be {annotation}"
            );
        });
    }
}

#[test]
fn loop_control_requires_a_loop() {
    with_lowered_err("fn main() { break; }", |err| {
        assert_eq!(err.kind, HirErrorKind::LoopControlOutsideLoop { kind: "break" });
    });
}

#[test]
fn bitwise_and_shifts_typechecking() {
    let source_ok = r#"
            fn main() {
                let a: i32 = 1;
                let b: i32 = 2;
                let c: i32 = a & b;
                let d: i32 = a | b;
                let e: i32 = a ^ b;
                let f: i32 = !a;
                let g: i32 = a << b;
                let h: i32 = a >> b;

                let x: bool = true;
                let y: bool = false;
                let z: bool = x & y;
                let w: bool = x | y;
                let v: bool = x ^ y;
                let u: bool = !x;
            }
        "#;
    with_lowered(source_ok, |_| {});

    let source_err_shift = r#"
            fn main() {
                let a: bool = true;
                let b: i32 = 2;
                let c: bool = a << b;
            }
        "#;
    with_lowered_err(source_err_shift, |err| {
        assert!(matches!(err.kind, HirErrorKind::TypeMismatch { .. }));
    });

    let source_err_not = r#"
            fn main() {
                let a: f64 = 1.0;
                let b: f64 = !a;
            }
        "#;
    with_lowered_err(source_err_not, |err| {
        assert!(matches!(err.kind, HirErrorKind::TypeMismatch { .. }));
    });
}

#[test]
fn if_condition_must_be_bool() {
    with_lowered_err(
        r#"
            fn main() {
                let x: i64 = 1;
                if x { }
            }
        "#,
        |err| {
            assert_eq!(
                err.kind,
                HirErrorKind::TypeMismatch {
                    expected: TypeKind::Bool.into(),
                    found: TypeKind::I64.into(),
                }
            );
        },
    );
}

#[test]
fn duplicated_function() {
    with_lowered_err(
        r#"
            fn foo(): i32 { 1 }
            fn foo(): i32 { 2 }
        "#,
        |err| assert!(matches!(err.kind, HirErrorKind::DuplicateFunction { name: "foo", .. })),
    );
}

#[test]
fn arity_mismatch_too_many() {
    with_lowered_err(
        r#"
            fn add(a: i32, b: i32): i32 { a + b }
            fn main() { add(1, 2, 3); }
        "#,
        |err| {
            assert!(matches!(
                err.kind,
                HirErrorKind::ArityMismatch { name: "nyx::add", expected: 2, found: 3, .. }
            ));
        },
    );
}

#[test]
fn unknown_function() {
    with_lowered_err("fn main() { foo(); }", |err| {
        assert_eq!(err.kind, HirErrorKind::UnknownFunction { name: "foo" });
    });
}

#[test]
fn type_mismatch_in_let() {
    with_lowered_err(
        r#"
            fn add(a: i32, b: i32): i32 { a + b }
            fn main() {
                let x: i32 = add(1, 2);
                let y: bool = add(1, 2);
            }
        "#,
        |err| {
            assert!(matches!(
                err.kind,
                HirErrorKind::TypeAnnotationMismatch { expected, found, .. }
                    if expected.kind() == TypeKind::Bool && found.kind() == TypeKind::I32
            ));
        },
    );
}

#[test]
fn type_inference_from_expr() {
    with_lowered("fn main() { let x = 1 + 2; }", |hir| {
        let main = &hir.functions[0];
        assert_eq!(main.locals[0].typ, TypeKind::I32.into());
    });
}

#[test]
fn top_level_non_function() {
    with_lowered_err("let x: i64 = 1;", |err| {
        assert_eq!(err.kind, HirErrorKind::TopLevelNonFunction);
    });
}

#[test]
fn integer_literal_as_function_arg_typed_i64() {
    with_lowered(
        r#"
            fn foo(x: i64): i64 { x }
            fn main() { foo(1); }
        "#,
        |hir| {
            assert_eq!(hir.functions.len(), 2);
            let foo = &hir.functions[0];
            assert_eq!(foo.return_type, TypeKind::I64.into());
            assert_eq!(foo.params.len(), 1);
            assert_eq!(foo.params[0].typ, TypeKind::I64.into());

            let main = &hir.functions[1];
            let call_id = match &main.body.statements[0] {
                Statement::Expr(expr) => *expr,
                other => panic!("expected Expr statement, got {other:?}"),
            };
            assert_eq!(main.typeck.type_of(call_id.id), TypeKind::I64.into());
            let arg = match &call_id.kind {
                ExpressionKind::Call { args, .. } => {
                    assert_eq!(args.len(), 1);
                    args[0]
                },
                other => panic!("expected Call expression, got {other:?}"),
            };
            assert_eq!(main.typeck.type_of(arg.id), TypeKind::I64.into());
            assert_eq!(arg.kind, 1.into());
        },
    );
}

#[test]
fn float_literal_defaults_to_f64() {
    with_lowered("fn main() { let x = 3.14; }", |hir| {
        let func = &hir.functions[0];
        assert_eq!(func.locals.len(), 1);
        assert_eq!(func.locals[0].typ, TypeKind::F64.into());
    });
}

#[test]
fn integer_literal_defaults_to_i32_in_binary_expr() {
    with_lowered("fn main() { let x = 1 + 2; }", |hir| {
        let func = &hir.functions[0];
        assert_eq!(func.locals[0].typ, TypeKind::I32.into());

        let stmt = &func.body.statements[0];
        assert!(matches!(stmt, Statement::LetInit { id: LocalId(0), .. }));
    });
}

#[test]
fn float_literal_widens_to_f32() {
    with_lowered("fn main() { let x: f32 = 3.14; }", |hir| {
        let func = &hir.functions[0];
        assert_eq!(func.locals.len(), 1);
        assert_eq!(func.locals[0].typ, TypeKind::F32.into());
    });
}

#[test]
fn mutable_assign_widens_literal() {
    with_lowered(
        r#"
            fn main() {
                let mut x: i64 = 0;
                x = 99;
            }
        "#,
        |hir| {
            let func = &hir.functions[0];
            assert_eq!(func.locals.len(), 1);
            assert_eq!(func.locals[0].typ, TypeKind::I64.into());
            assert!(func.locals[0].mutable);

            let assign_id = match &func.body.statements[1] {
                Statement::Expr(expr) => *expr,
                other => panic!("expected Expr statement, got {other:?}"),
            };
            assert_eq!(func.typeck.type_of(assign_id.id), TypeKind::I64.into());
            let (target_id, value) = match &assign_id.kind {
                ExpressionKind::Assign { target, value } => match &target.kind {
                    ExpressionKind::Local(id) => (*id, *value),
                    _ => panic!("expected local assignment target"),
                },
                other => panic!("expected Assign expression, got {other:?}"),
            };

            assert_eq!(target_id, LocalId(0));
            assert_eq!(func.typeck.type_of(value.id), TypeKind::I64.into());
            assert_eq!(value.kind, 99.into());
        },
    );
}

#[test]
fn integer_literal_widens_in_binary_with_i64_local() {
    with_lowered(
        r#"
            fn main() {
                let x: i64 = 10;
                let y = x + 1;
            }
        "#,
        |hir| {
            let func = &hir.functions[0];
            assert_eq!(func.locals.len(), 2);
            assert_eq!(func.locals[0].typ, TypeKind::I64.into());
            assert_eq!(func.locals[1].typ, TypeKind::I64.into());

            let y_stmt = &func.body.statements[1];
            assert!(matches!(y_stmt, Statement::LetInit { id: LocalId(1), .. }));
        },
    );
}

#[test]
fn new_integer_types_accepted() {
    let src = r#"
            fn bytes(a: i8, b: u8, c: i16, d: u16): i32 {
                0
            }
        "#;

    with_lowered(src, |_| {});
}

#[test]
fn integer_literal_widens() {
    let src = r#"
            fn main() {
                let x: i16 = 100;
                let y: u8 = 42;
            }
        "#;

    with_lowered(src, |_| {});
}

#[test]
fn uptr_iptr_type_resolution() {
    let src = r#"
            fn main() {
                let a: uptr = 10;
                let b: iptr = 20;
            }
        "#;

    with_lowered(src, |hir| {
        let func = &hir.functions[0];
        assert_eq!(func.locals[0].typ, TypeKind::Uptr.into());
        assert_eq!(func.locals[1].typ, TypeKind::Iptr.into());
    });
}

#[test]
fn uptr_iptr_literal_widening() {
    let src = r#"
            fn main() {
                let a: uptr = 100;
                let b: iptr = 200;
            }
        "#;

    with_lowered(src, |hir| {
        let func = &hir.functions[0];

        let init_a = match &func.body.statements[0] {
            Statement::LetInit { init: e, .. } => *e,
            other => panic!("expected Let with init, got {other:?}"),
        };
        assert_eq!(func.typeck.type_of(init_a.id), TypeKind::Uptr.into());
        assert_eq!(init_a.kind, 100.into());

        let init_b = match &func.body.statements[1] {
            Statement::LetInit { init: e, .. } => *e,
            other => panic!("expected Let with init, got {other:?}"),
        };
        assert_eq!(func.typeck.type_of(init_b.id), TypeKind::Iptr.into());
        assert_eq!(init_b.kind, 200.into());
    });
}

#[test]
fn uptr_arithmetic() {
    let src = r#"
            fn add(a: uptr, b: uptr): uptr { a + b }
        "#;

    with_lowered(src, |hir| {
        let func = &hir.functions[0];
        assert_eq!(func.return_type, TypeKind::Uptr.into());
        assert_eq!(func.params[0].typ, TypeKind::Uptr.into());
        assert_eq!(func.params[1].typ, TypeKind::Uptr.into());
    });
}

#[test]
fn iptr_arithmetic() {
    let src = r#"
            fn scale(base: iptr, factor: iptr): iptr { base * factor }
        "#;

    with_lowered(src, |hir| {
        let func = &hir.functions[0];
        assert_eq!(func.return_type, TypeKind::Iptr.into());
        assert_eq!(func.params[0].typ, TypeKind::Iptr.into());
        assert_eq!(func.params[1].typ, TypeKind::Iptr.into());
    });
}

#[test]
fn uptr_range() {
    let src = r#"
            fn triangle(limit: uptr): uptr {
                let mut acc: uptr = 0;
                loop i in 1..=limit {
                    acc = acc + i;
                }
                acc
            }
        "#;

    with_lowered(src, |_| {});
}

#[test]
fn uptr_iptr_mixed_type_mismatch() {
    let src = r#"
            fn main() {
                let a: uptr = 1;
                let b: iptr = a;
            }
        "#;

    with_lowered_err(src, |err| {
        assert!(matches!(
            err.kind,
            HirErrorKind::TypeAnnotationMismatch { expected, found, .. }
                if expected.kind() == TypeKind::Iptr && found.kind() == TypeKind::Uptr
        ));
    });
}

#[test]
fn bare_int_literal_defaults_to_i32() {
    let src = r#"
            fn main() {
                let x = 0;
                let f = 1.0;
            }
        "#;

    with_lowered(src, |hir| {
        let func = &hir.functions[0];
        assert_eq!(func.locals[0].typ, TypeKind::I32.into(), "unconstrained integer falls back");
        assert_eq!(func.locals[1].typ, TypeKind::F64.into(), "float literal unchanged");
    });
}

#[test]
fn int_binding_back_propagates_from_later_annotation() {
    let src = r#"
            fn main() {
                let x = 0;
                let y: i64 = x;
            }
        "#;

    with_lowered(src, |hir| {
        let func = &hir.functions[0];
        assert_eq!(func.locals[0].typ, TypeKind::I64.into(), "use against i64 pins the literal");
        assert_eq!(func.locals[1].typ, TypeKind::I64.into());
    });
}

#[test]
fn int_binding_conflicting_uses_report_mismatch() {
    let src = r#"
            fn main() {
                let x = 0;
                let y: u8 = x;
                let z: u32 = x;
            }
        "#;

    with_lowered_err(src, |err| {
        assert!(matches!(
            err.kind,
            HirErrorKind::TypeAnnotationMismatch { expected, found, .. }
                if expected.kind() == TypeKind::U32 && found.kind() == TypeKind::U8
        ));
    });
}

#[test]
fn mixed_width_arithmetic_still_errors() {
    let src = r#"
            fn main() {
                let a: i32 = 1;
                let b: uptr = 2;
                let c = a + b;
            }
        "#;

    with_lowered_err(src, |err| {
        assert_eq!(
            err.kind,
            HirErrorKind::TypeMismatch {
                expected: TypeKind::I32.into(),
                found: TypeKind::Uptr.into(),
            }
        );
    });
}

#[test]
fn struct_fields_remain_in_source_order() {
    let src = r#"
            struct Packed {
                a: i8,
                b: i64,
                c: i32,
            }

            fn main() {
                let value: Packed = Packed { a: 1, b: 2, c: 3 };
            }
        "#;

    with_lowered(src, |hir| {
        assert_eq!(hir.adts.iter().filter(|adt| adt.is_struct()).count(), 1);
        let field_names: Vec<_> =
            hir.adts[0].fields().iter().map(|field| hir.symbols.get(field.name)).collect();
        assert_eq!(field_names, vec!["a", "b", "c"]);

        let func = &hir.functions[0];
        assert_eq!(func.locals[0].typ, hir.types.adt(AdtId(0), &[]));
    });
}

#[test]
fn nested_struct_fields_are_resolved() {
    let src = r#"
            struct Inner {
                n: i32,
            }

            struct Outer {
                inner: Inner,
                flag: bool,
            }

            fn main() {
                let value = Outer {
                    inner: Inner { n: 1 },
                    flag: true,
                };
            }
        "#;

    with_lowered(src, |hir| {
        assert_eq!(hir.adts.iter().filter(|adt| adt.is_struct()).count(), 2);

        let outer_inner = hir.adts[1]
            .fields()
            .iter()
            .find(|field| hir.symbols.get(field.name) == "inner")
            .unwrap();
        assert_eq!(outer_inner.typ, hir.types.adt(AdtId(0), &[]));
    });
}

#[test]
fn enum_payload_can_reference_a_later_struct() {
    let src = r#"
            enum Msg {
                ChangeColour(Colour),
            }

            struct Colour {
                r: u8,
                g: u8,
                b: u8,
            }
        "#;

    with_lowered(src, |hir| {
        assert_eq!(hir.adts[1].variants()[0].payload, Some(hir.types.adt(AdtId(0), &[])));
    });
}

#[test]
fn circular_structs_are_rejected() {
    let arena = bumpalo::Bump::new();
    let src = r#"
            struct A {
                b: B,
            }

            struct B {
                a: A,
            }

            fn main() { }
        "#;

    let hir = super::lower(Parser::new(src).parse().unwrap(), &arena);
    assert!(
        hir.diagnostics
            .iter()
            .any(|error| error.message.contains("contains itself by value"))
    );
}

#[test]
fn struct_literal_requires_all_fields() {
    let src = r#"
            struct Point {
                x: i32,
                y: i32,
            }

            fn main() {
                let point = Point { x: 1 };
            }
        "#;

    with_lowered_err(src, |err| {
        assert_eq!(err.kind, HirErrorKind::MissingField { struct_name: "Point", field: "y" });
    });
}

#[test]
fn struct_literal_rejects_unknown_field_with_span() {
    let src = "struct Point{x:i32}\nfn main(){let p=Point{z:1};}";
    with_lowered_err(src, |err| {
        assert_eq!(err.kind, HirErrorKind::UnknownField { struct_name: "Point", field: "z" });
        let mut map = crate::source_map::SourceMap::default();
        map.add_file("t", src);
        assert_eq!(map.loc(err.span.start).col_utf8, 22);
        assert_eq!(map.loc(err.span.end).col_utf8, 25);
    });
}

#[test]
fn struct_literal_rejects_duplicate_field_with_span() {
    let src = "struct Point{x:i32}\nfn main(){let p=Point{x:1,x:2};}";
    with_lowered_err(src, |err| {
        assert_eq!(err.kind, HirErrorKind::DuplicateField { name: "x" });
        let mut map = crate::source_map::SourceMap::default();
        map.add_file("t", src);
        assert_eq!(map.loc(err.span.start).col_utf8, 26);
        assert_eq!(map.loc(err.span.end).col_utf8, 29);
    });
}

#[test]
fn immutable_field_assignment_reports_assignment_span() {
    let src = "struct Point{x:i32}\nfn main(){let p=Point{x:1};p.x=2;}";
    with_lowered_err(src, |err| {
        assert!(matches!(err.kind, HirErrorKind::ImmutableBind { name: "p", .. }));
        let mut map = crate::source_map::SourceMap::default();
        map.add_file("t", src);
        assert_eq!(map.loc(err.span.start).col_utf8, 27);
        assert_eq!(map.loc(err.span.end).col_utf8, 30);
    });
}

#[test]
fn chained_field_access() {
    let src = r#"
            struct Point { x: i64, y: i64 }
            struct Rect { top_left: Point, bottom_right: Point }
 
            fn main(): i64 {
                let p1 = Point { x: 0, y: 10 };
                let p2 = Point { x: 10, y: 0 };
                let r = Rect { top_left: p1, bottom_right: p2 };
                r.bottom_right.x
            }
        "#;

    with_lowered(src, |_| {});
}

#[test]
fn impl_blocks_collect_methods_for_same_struct() {
    let src = r#"
            struct Counter { value: i32 }

            impl Counter {
                fn value(&self): i32 { self.value }
            }

            impl Counter {
                fn add(&mut self, delta: i32) {
                    self.value = self.value + delta;
                }
            }

            fn main(): i32 {
                let mut counter = Counter { value: 40 };
                counter.add(2);
                counter.value()
            }
        "#;

    with_lowered(src, |hir| {
        assert_eq!(hir.functions.len(), 3);
        assert!(hir.functions.iter().any(|f| matches!(f.kind, FunctionKind::Method(_))));
    });
}

#[test]
fn duplicate_methods_across_impl_blocks_are_rejected() {
    let src = r#"
            struct Counter { value: i32 }

            impl Counter {
                fn value(&self): i32 { self.value }
            }

            impl Counter {
                fn value(&self): i32 { self.value }
            }
        "#;

    with_lowered_err(src, |err| {
        assert!(matches!(
            err.kind,
            HirErrorKind::DuplicateMethod { struct_name: "Counter", name: "value", .. }
        ));
        let mut map = crate::source_map::SourceMap::default();
        map.add_file("t", src);
        assert_eq!(map.loc(err.span.start).col_utf8, 16);
    });
}

#[test]
fn mut_self_method_requires_mutable_receiver() {
    let src = r#"
            struct Counter { value: i32 }

            impl Counter {
                fn add(&mut self, delta: i32) {
                    self.value = self.value + delta;
                }
            }

            fn main() {
                let counter = Counter { value: 40 };
                counter.add(2);
            }
        "#;

    with_lowered_err(src, |err| {
        assert!(matches!(err.kind, HirErrorKind::ImmutableBind { name: "counter", .. }));
    });
}

#[test]
fn mutable_reference_parameters_can_be_written_through() {
    let src = r#"
            struct Counter { value: i32 }

            fn bump(counter: &mut Counter) {
                counter.value = 1;
            }
        "#;

    with_lowered(src, |_| {});
}

#[test]
fn shared_reference_parameters_cannot_be_written_through() {
    let src = r#"
            struct Counter { value: i32 }

            fn bump(counter: &Counter) {
                counter.value = 1;
            }
        "#;

    with_lowered_err(src, |err| {
        assert!(matches!(err.kind, HirErrorKind::ImmutableBind { name: "counter", .. }));
    });
}

#[test]
fn a_reference_can_point_at_a_reference() {
    let src = "fn main(){let mut v:i32=1;let mut p=&mut v;let pp=&mut p;}";

    with_lowered(src, |hir| {
        let pp = hir.functions[0]
            .locals
            .iter()
            .find(|local| hir.symbols.get(local.name) == "pp")
            .unwrap();
        let TypeKind::Ref { to, .. } = pp.typ.kind() else {
            panic!("pp must be a reference")
        };
        assert!(matches!(to.kind(), TypeKind::Ref { .. }));
    });
}

#[test]
fn dereferencing_a_non_pointer_names_the_type() {
    let src = "fn main():i32{let x:i32=1;*x}";
    with_lowered_err(src, |err| {
        assert!(
            matches!(err.kind, HirErrorKind::InvalidDeref { .. }),
            "dereferencing a non-pointer must not be reported against a made-up reference type"
        );
    });
}

#[test]
fn a_const_requirement_binds_the_implementation() {
    let src = r#"
            interface Bounded { const fn limit(&self): i32; }
            struct Gauge { n: i32 }

            impl Gauge with Bounded {
                fn limit(&self): i32 { 100 }
            }
        "#;

    with_lowered_err(src, |err| {
        assert!(matches!(err.kind, HirErrorKind::NonConstInterfaceMethod { .. }));
    });
}

#[test]
fn an_implementation_may_be_const_without_the_interface() {
    let src = r#"
            interface Plain { fn value(&self): i32; }
            struct Gauge { n: i32 }

            impl Gauge with Plain {
                const fn value(&self): i32 { 100 }
            }
        "#;

    with_lowered(src, |_| {});
}

#[test]
fn shared_self_cannot_assign_fields() {
    let src = r#"
            struct Counter { value: i32 }

            impl Counter {
                fn set(&self, value: i32) {
                    self.value = value;
                }
            }
        "#;

    with_lowered_err(src, |err| {
        assert!(matches!(err.kind, HirErrorKind::ImmutableBind { name: "self", .. }));
    });
}

#[test]
fn wrong_interface_parameters_impl() {
    let src = r#"
        interface StorageEngine {
            fn flush(&self): bool;
            fn read_page(&self): i64;
        }

        struct BTreeStorage {
            page_size: i64,
        }

        impl BTreeStorage with StorageEngine {
            fn flush(&self): bool { true }
            fn read_page(&self, page_id: i64): i64 { self.page_size }
        }
        "#;

    with_lowered_err(src, |err| {
        assert!(matches!(
            err.kind,
            HirErrorKind::InterfaceSignatureMismatch {
                struct_name,
                interface_name,
                method_name,
                ..
            } if struct_name == "BTreeStorage"
                && interface_name == "StorageEngine"
                && method_name == "read_page"
        ));
    });
}

#[test]
fn interface_requires_its_associated_constants() {
    let src = r#"
            interface Buffer { const SIZE: uptr; }
            struct Page {}
            impl Page with Buffer {}
        "#;

    with_lowered_err(src, |err| {
        assert!(matches!(
            err.kind,
            HirErrorKind::MissingInterfaceConstant {
                struct_name: "Page",
                interface_name: "Buffer",
                constant_name: "SIZE",
                ..
            }
        ));
    });
}

#[test]
fn interface_associated_constant_type_must_match() {
    let src = r#"
            interface Buffer { const SIZE: uptr; }
            struct Page {}
            impl Page with Buffer { const SIZE: i32 = 4096; }
        "#;

    with_lowered_err(src, |err| {
        assert!(matches!(
            err.kind,
            HirErrorKind::InterfaceConstantTypeMismatch {
                struct_name: "Page",
                interface_name: "Buffer",
                constant_name: "SIZE",
                expected,
                found,
                ..
            } if expected == TypeKind::Uptr.into() && found == TypeKind::I32.into()
        ));
    });
}

#[test]
fn generic_bound_resolves_its_associated_constant() {
    let src = r#"
            interface HasValue { const VALUE: i32; }
            struct Number {}
            impl Number with HasValue { const VALUE: i32 = 42; }

            fn value<T: HasValue>(): i32 { T::VALUE }
            fn main(): i32 { value::<Number>() }
        "#;

    with_lowered(src, |_| {});
}

#[test]
fn field_shorthand_binds_the_name_it_stands_for() {
    let src = r#"
            struct Point { x: i64, y: i64 }
            fn make(x: i64, y: i64): Point { Point { x, y } }
            fn main(): i64 { let p = make(1, 2); p.x }
        "#;

    with_lowered(src, |hir| {
        let make = hir
            .functions
            .iter()
            .find(|func| hir.symbols.get(func.name).ends_with("make"))
            .expect("make is lowered");

        // the shorthand is the parameter of the same name, not a fresh binding
        assert_eq!(make.params.len(), 2, "no extra local is introduced");
    });
}

#[test]
fn field_shorthand_still_needs_the_binding_to_exist() {
    let src = r#"
            struct Point { x: i64, y: i64 }
            fn main(): i64 { let x: i64 = 1; let p = Point { x, y }; p.x }
        "#;

    with_lowered_err(src, |err| {
        assert_eq!(
            err.kind,
            HirErrorKind::UndeclaredIdentifier { name: "y" },
            "a field named after nothing in scope is an error, not an empty field"
        );
    });
}

#[test]
fn field_shorthand_still_checks_the_bindings_type() {
    let src = r#"
            struct Point { x: i64, y: i64 }
            fn main(): i64 {
                let x: i64 = 1;
                let y: bool = true;
                let p = Point { x, y };
                p.x
            }
        "#;

    with_lowered_err(src, |err| {
        assert!(
            matches!(err.kind, HirErrorKind::TypeMismatch { .. }),
            "sharing a name is not sharing a type: {:?}",
            err.kind
        );
    });
}

#[test]
fn field_shorthand_still_checks_the_field_exists() {
    let src = r#"
            struct Point { x: i64 }
            fn main(): i64 { let x: i64 = 1; let z: i64 = 2; let p = Point { x, z }; p.x }
        "#;

    with_lowered_err(src, |err| {
        assert_eq!(err.kind, HirErrorKind::UnknownField { struct_name: "Point", field: "z" });
    });
}

#[test]
fn field_shorthand_still_requires_every_field() {
    let src = r#"
            struct Point { x: i64, y: i64 }
            fn main(): i64 { let x: i64 = 1; let p = Point { x }; p.x }
        "#;

    with_lowered_err(src, |err| {
        assert_eq!(err.kind, HirErrorKind::MissingField { struct_name: "Point", field: "y" });
    });
}

#[test]
fn primitive_orphan_rule_is_enforced() {
    let src = r#"
            impl i64 {
                fn val(&self): i64 { *self }
            }
        "#;

    with_lowered_err(src, |err| {
        assert_eq!(err.kind, HirErrorKind::OrphanImpl { name: "i64" });
    });
}

fn const_value<'hir>(expr: &Expression<'hir>) -> &'hir Expression<'hir> {
    match expr.kind {
        ExpressionKind::Const(constant) => constant.value,
        ref other => panic!("expected Const node, got {other:?}"),
    }
}

#[test]
fn const_top_level() {
    let src = r#"
            const ANSWER: i32 = 42;
            fn main(): i32 {
                ANSWER
            }
        "#;
    with_lowered(src, |hir| {
        let func = &hir.functions[0];
        let ret_expr = match &func.body.statements[0] {
            Statement::Return(Some(expr)) => *expr,
            other => panic!("expected Return statement, got {other:?}"),
        };
        assert_eq!(func.typeck.type_of(ret_expr.id), TypeKind::I32.into());
        assert_eq!(const_value(ret_expr).kind, 42.into());
    });
}

#[test]
fn const_scoped_and_qualified() {
    let src = r#"
            struct Dummy {}
            impl Dummy {
                pub const VALUE: uptr = 127;
            }
            fn main(): uptr {
                Dummy::VALUE
            }
        "#;
    with_lowered(src, |hir| {
        let func = &hir.functions[0];
        let ret_expr = match &func.body.statements[0] {
            Statement::Return(Some(expr)) => *expr,
            other => panic!("expected Return statement, got {other:?}"),
        };
        assert_eq!(func.typeck.type_of(ret_expr.id), TypeKind::Uptr.into());
        assert_eq!(const_value(ret_expr).kind, 127.into());
    });
}

#[test]
fn const_primitive_scoped_in_std() {
    let src = r#"
            impl i8 {
                pub const MAX: uptr = 127;
            }
            fn main(): uptr {
                i8::MAX
            }
        "#;
    let arena = bumpalo::Bump::new();
    let mut statements = Parser::new(src).parse().unwrap();
    statement::inject_default_methods(&mut statements, |_| None);
    let (declarations, errors) = Declarations::collect_recovering(&statements);
    assert!(errors.is_empty());
    let mut scope = ItemTable::new(&arena);
    scope.in_std.set(true);
    scope.extend(&declarations, &arena);
    let functions = scope.lower_matching_functions(&declarations, |_| true, false, &arena);
    let main_func = &functions[0];
    let ret_expr = match &main_func.body.statements[0] {
        Statement::Return(Some(expr)) => *expr,
        other => panic!("expected Return statement, got {other:?}"),
    };
    assert_eq!(main_func.typeck.type_of(ret_expr.id), TypeKind::Uptr.into());
    assert_eq!(const_value(ret_expr).kind, 127.into());
}

#[test]
fn const_nested_evaluation() {
    let src = r#"
            const A: i32 = 10;
            const B: i32 = A + 2;
            fn main(): i32 {
                B
            }
        "#;
    with_lowered(src, |hir| {
        let func = &hir.functions[0];
        let ret_expr = match &func.body.statements[0] {
            Statement::Return(Some(expr)) => *expr,
            other => panic!("expected Return statement, got {other:?}"),
        };
        assert_eq!(func.typeck.type_of(ret_expr.id), TypeKind::I32.into());
        match &const_value(ret_expr).kind {
            ExpressionKind::Binary { left, operator, right } => {
                assert_eq!(*operator, BinaryOperator::Add);
                assert_eq!(const_value(left).kind, 10.into());
                assert_eq!(right.kind, 2.into());
            },
            other => panic!("expected Binary expression, got {other:?}"),
        };
    });
}

#[test]
fn const_circular_dependency() {
    let src = r#"
            const A: i32 = B;
            const B: i32 = A;
            fn main() {}
        "#;
    with_lowered_err(src, |err| {
        assert!(matches!(
            err.kind,
            HirErrorKind::CircularConstant { name } if name == "A" || name == "B"
        ));
    });
}

#[test]
fn const_duplicate_declaration() {
    let src = r#"
            const X: i32 = 1;
            const X: i32 = 2;
            fn main() {}
        "#;
    with_lowered_err(src, |err| {
        assert!(matches!(err.kind, HirErrorKind::DuplicateConstant { name: "X", .. }));
    });
}

#[test]
fn const_scoped_duplicate_declaration() {
    let src = r#"
            struct Dummy {}
            impl Dummy {
                pub const VALUE: i32 = 1;
                pub const VALUE: i32 = 2;
            }
            fn main() {}
        "#;
    with_lowered_err(src, |err| {
        assert!(matches!(err.kind, HirErrorKind::DuplicateConstant { name: "Dummy::VALUE", .. }));
    });
}

#[test]
fn const_undefined_reference() {
    let src = r#"
            const A: i32 = UNDEFINED;
            fn main() {}
        "#;
    with_lowered_err(src, |err| {
        assert_eq!(err.kind, HirErrorKind::UndeclaredIdentifier { name: "UNDEFINED" });
    });
}

#[test]
fn const_shadowing() {
    let src = r#"
            const ANSWER: i32 = 42;
            fn main(): i32 {
                let ANSWER: i32 = 100;
                ANSWER
            }
        "#;
    with_lowered(src, |hir| {
        let func = &hir.functions[0];
        let ret_expr = match &func.body.statements[1] {
            Statement::Return(Some(expr)) => *expr,
            other => panic!("expected Return statement, got {other:?}"),
        };
        assert_eq!(func.typeck.type_of(ret_expr.id), TypeKind::I32.into());
        assert!(matches!(ret_expr.kind, ExpressionKind::Local(_)));
    });
}

#[test]
fn const_in_function_body_is_a_const_use_not_a_local() {
    let src = r#"
            fn main(): i32 {
                const ANSWER: i32 = 42;
                ANSWER
            }
        "#;
    with_lowered(src, |hir| {
        let func = &hir.functions[0];

        assert_eq!(func.body.statements.len(), 1);
        assert!(func.locals.is_empty());

        let ret_expr = match &func.body.statements[0] {
            Statement::Return(Some(expr)) => *expr,
            other => panic!("expected Return statement, got {other:?}"),
        };
        assert_eq!(func.typeck.type_of(ret_expr.id), TypeKind::I32.into());
        assert_eq!(const_value(ret_expr).kind, 42.into());
    });
}

#[test]
fn const_in_function_body_folds_binary_use() {
    let src = r#"
            fn main(): i32 {
                const BASE: i32 = 10;
                BASE + 2
            }
        "#;
    with_lowered(src, |hir| {
        let func = &hir.functions[0];

        let ret_expr = match &func.body.statements[0] {
            Statement::Return(Some(expr)) => *expr,
            other => panic!("expected Return statement, got {other:?}"),
        };
        match &ret_expr.kind {
            ExpressionKind::Binary { left, right, .. } => {
                assert_eq!(const_value(left).kind, 10.into());
                assert_eq!(right.kind, 2.into());
            },
            other => panic!("expected Binary expression, got {other:?}"),
        }
    });
}

#[test]
fn const_in_function_body_references_earlier_const() {
    let src = r#"
            fn main(): i32 {
                const A: i32 = 10;
                const B: i32 = A + 5;
                B
            }
        "#;
    with_lowered(src, |hir| {
        let func = &hir.functions[0];

        let ret_expr = match &func.body.statements[0] {
            Statement::Return(Some(expr)) => *expr,
            other => panic!("expected Return statement, got {other:?}"),
        };
        assert_eq!(func.typeck.type_of(ret_expr.id), TypeKind::I32.into());
        // B's value is `A + 5`, with A itself a nested constant reference
        match &const_value(ret_expr).kind {
            ExpressionKind::Binary { left, right, .. } => {
                assert_eq!(const_value(left).kind, 10.into());
                assert_eq!(right.kind, 5.into());
            },
            other => panic!("expected Binary expression, got {other:?}"),
        }
    });
}

#[test]
fn const_in_function_body_cannot_capture_local() {
    let src = r#"
            fn main(): i32 {
                let x: i32 = 5;
                const BAD: i32 = x;
                0
            }
        "#;
    with_lowered_err(src, |err| {
        assert_eq!(err.kind, HirErrorKind::NonConstValue { name: "x" });
    });
}

#[test]
fn const_in_function_body_rejects_duplicate() {
    let src = r#"
            fn main(): i32 {
                const N: i32 = 1;
                const N: i32 = 2;
                N
            }
        "#;
    with_lowered_err(src, |err| {
        assert!(matches!(err.kind, HirErrorKind::DuplicateConstant { name: "N", .. }));
    });
}

#[test]
fn nested_non_const_item_is_rejected() {
    let src = r#"
            fn main() {
                struct Inner {}
            }
        "#;
    with_lowered_err(src, |err| {
        assert_eq!(err.kind, HirErrorKind::NestedItem { kind: "struct" });
    });
}

#[test]
fn const_fn_calling_non_const_fn_is_rejected() {
    let src = "fn helper(): i32 { 1 }\nconst fn seed(): i32 { helper() }\nfn main(): i32 { 0 }";
    with_lowered_err(src, |err| {
        assert!(matches!(
            err.kind,
            HirErrorKind::ConstFnViolation(ConstFnViolationKind::NonConstCall {
                name: "nyx::helper"
            })
        ));
    });
}

#[test]
fn const_calling_non_const_fn_in_its_initialiser_is_rejected() {
    let src = "fn helper(): i32 { 1 }\nconst SEED: i32 = helper();\nfn main(): i32 { SEED }";
    with_lowered_err(src, |err| {
        assert!(matches!(
            err.kind,
            HirErrorKind::ConstFnViolation(ConstFnViolationKind::NonConstCall {
                name: "nyx::helper"
            })
        ));
    });
}

#[test]
fn body_local_const_calling_non_const_fn_is_rejected() {
    let src = r#"
            fn helper(): i32 { 1 }
            fn main(): i32 {
                const LOCAL: i32 = helper();
                LOCAL
            }
        "#;
    with_lowered_err(src, |err| {
        assert!(matches!(
            err.kind,
            HirErrorKind::ConstFnViolation(ConstFnViolationKind::NonConstCall {
                name: "nyx::helper"
            })
        ));
    });
}

#[test]
fn literal_pattern_integer() {
    let src = r#"
            fn classify(x: i32): i32 {
                match x {
                    0 -> 10,
                    1 -> 20,
                    _ -> 30,
                }
            }
        "#;
    with_lowered(src, |hir| {
        let func = &hir.functions[0];
        let match_expr = match &func.body.statements[0] {
            Statement::Return(Some(expr)) => *expr,
            other => panic!("expected return, got {other:?}"),
        };
        let arms = match &match_expr.kind {
            ExpressionKind::Match { arms, .. } => *arms,
            other => panic!("expected Match, got {other:?}"),
        };
        assert_eq!(arms.len(), 3);
        assert!(matches!(arms[0].pattern.kind, PatternKind::Literal(Literal::Int(0))));
        assert!(matches!(arms[1].pattern.kind, PatternKind::Literal(Literal::Int(1))));
        assert!(matches!(arms[2].pattern.kind, PatternKind::Wildcard));
    });
}

#[test]
fn literal_pattern_bool() {
    let src = r#"
            fn negate(b: bool): bool {
                match b {
                    true -> false,
                    false -> true,
                }
            }
        "#;
    with_lowered(src, |hir| {
        let func = &hir.functions[0];
        let match_expr = match &func.body.statements[0] {
            Statement::Return(Some(expr)) => *expr,
            other => panic!("expected return, got {other:?}"),
        };
        let arms = match &match_expr.kind {
            ExpressionKind::Match { arms, .. } => *arms,
            other => panic!("expected Match, got {other:?}"),
        };
        assert!(matches!(arms[0].pattern.kind, PatternKind::Literal(Literal::Bool(true))));
        assert!(matches!(arms[1].pattern.kind, PatternKind::Literal(Literal::Bool(false))));
    });
}

#[test]
fn or_pattern_folds_into_single_or_node() {
    let src = r#"
            enum Dir { N = 0, S = 1, E = 2, W = 3 } as u8
            fn is_horizontal(d: Dir): bool {
                match d {
                    Dir::E | Dir::W -> true,
                    _ -> false,
                }
            }
        "#;

    with_lowered(src, |hir| {
        let func = &hir.functions[0];
        let match_expr = match &func.body.statements[0] {
            Statement::Return(Some(expr)) => *expr,
            other => panic!("expected return, got {other:?}"),
        };
        let arms = match &match_expr.kind {
            ExpressionKind::Match { arms, .. } => *arms,
            other => panic!("expected Match, got {other:?}"),
        };
        assert_eq!(arms.len(), 2);
        assert!(matches!(arms[0].pattern.kind, PatternKind::Or(pats) if pats.len() == 2));
        assert!(matches!(arms[1].pattern.kind, PatternKind::Wildcard));
    });
}

#[test]
fn match_arm_guard_attached() {
    let src = r#"
            fn sign(x: i32): i32 {
                match x {
                    n if n > 0 -> 1,
                    _ -> 0,
                }
            }
        "#;

    with_lowered(src, |hir| {
        let func = &hir.functions[0];
        let match_expr = match &func.body.statements[0] {
            Statement::Return(Some(expr)) => *expr,
            other => panic!("expected return, got {other:?}"),
        };
        let arms = match &match_expr.kind {
            ExpressionKind::Match { arms, .. } => *arms,
            other => panic!("expected Match, got {other:?}"),
        };
        assert_eq!(arms.len(), 2);
        assert!(arms[0].guard.is_some(), "first arm must have a guard");
        assert!(arms[1].guard.is_none(), "wildcard arm must have no guard");
    });
}

#[test]
fn generic_free_function_is_monomorphised() {
    let src = r#"
            fn pick<T>(a: T, b: T): T { a }
            fn main(): i32 { pick(7, 9) }
        "#;
    with_lowered(src, |hir| {
        let name = |f: &Function| hir.symbols.get(f.name).to_owned();

        let pick = hir
            .functions
            .iter()
            .find(|f| name(f).contains("pick$i32"))
            .expect("specialised pick$i32 instance");
        assert_eq!(pick.params[0].typ, TypeKind::I32.into());
        assert_eq!(pick.return_type, TypeKind::I32.into());
        assert!(
            hir.functions.iter().all(|f| !name(f).ends_with("pick")),
            "the open template body must not be emitted"
        );

        let main = hir.functions.iter().find(|f| name(f) == "nyx::main").unwrap();
        let call = match &main.body.statements[0] {
            Statement::Return(Some(expr)) => *expr,
            other => panic!("expected return, got {other:?}"),
        };
        assert!(matches!(call.kind, ExpressionKind::Call { .. }));
        assert_eq!(main.typeck.type_dependent_def(call.id), Some(Res::Function(pick.id)));
    });
}

#[test]
fn an_unsafe_function_is_only_callable_from_an_unsafe_one() {
    let src = r#"
            @unsafe fn danger(): i32 { 1 }
            fn main(): i32 { danger() }
        "#;
    with_lowered_err(src, |err| {
        assert!(matches!(err.kind, HirErrorKind::UnsafeCall { name: "nyx::danger", .. }));
    });

    let src = r#"
            @unsafe fn danger(): i32 { 1 }
            @unsafe fn main(): i32 { danger() }
        "#;
    with_lowered(src, |_| {});
}

#[test]
fn an_unsafe_block_lets_safe_code_wrap_an_unsafe_operation() {
    let src = r#"
            @unsafe fn danger(): i32 { 1 }
            fn wrapper(p: *i32): i32 { @unsafe { danger() + *p } }
            fn main(): i32 { 0 }
        "#;
    with_lowered(src, |hir| assert!(hir.diagnostics.is_empty(), "{:?}", hir.diagnostics));
}

#[test]
fn an_unsafe_block_that_grants_nothing_warns() {
    let src = r#"
            @unsafe fn danger(): i32 { 1 }
            fn pointless(): i32 { @unsafe { 5 } }
            @unsafe fn redundant(): i32 { @unsafe { danger() } }
            fn main(): i32 { 0 }
        "#;
    with_lowered(src, |hir| {
        assert_eq!(hir.diagnostics.len(), 2, "{:?}", hir.diagnostics);
        for diagnostic in &hir.diagnostics {
            assert_eq!(diagnostic.severity, crate::diagnostic::Severity::Warning);
            assert_eq!(diagnostic.lint, Some(crate::lints::Lint::UnusedUnsafe));
            assert!(diagnostic.code.is_none(), "a lint carries no error code");
        }
    });
}

#[test]
fn a_raw_pointer_is_only_dereferenceable_in_an_unsafe_function() {
    let src = r#"
            fn read(p: *i32): i32 { *p }
            fn main(): i32 { 0 }
        "#;
    with_lowered_err(src, |err| {
        assert!(matches!(err.kind, HirErrorKind::UnsafeDeref { .. }));
    });

    let src = r#"
            @unsafe fn read(p: *i32): i32 { *p }
            fn main(): i32 { 0 }
        "#;
    with_lowered(src, |_| {});
}

#[test]
fn a_reference_stands_in_for_a_raw_pointer_but_not_the_reverse() {
    let src = r#"
            @unsafe fn main(): i32 {
                let v: i32 = 7;
                let p: *i32 = &v;
                *p
            }
        "#;
    with_lowered(src, |_| {});

    let src = r#"
            @unsafe fn main(): i32 {
                let v: i32 = 7;
                let p: *i32 = &v;
                let r: &i32 = p;
                *r
            }
        "#;
    with_lowered_err(src, |err| {
        assert!(matches!(err.kind, HirErrorKind::TypeAnnotationMismatch { .. }));
    });
}

#[test]
fn an_intrinsic_the_compiler_does_not_implement_is_rejected() {
    let src = r#"
            @intrinsic
            fn reversed(): i32 {}
            fn main() { }
        "#;
    with_lowered_err(src, |err| {
        assert!(matches!(err.kind, HirErrorKind::UnknownIntrinsic { name: "reversed" }));
    });
}

#[test]
fn an_intrinsic_body_is_empty_by_design() {
    let src = r#"
            struct Counter { n: i32 }
            impl Counter {
                @intrinsic
                pub const fn wrapping_add(&self, rhs: i32): i32 {}
            }
            fn main() { }
        "#;
    with_lowered(src, |hir| {
        assert!(
            !hir.functions.iter().any(|f| matches!(f.kind, FunctionKind::Intrinsic(_))),
            "a batch compile lowers no body for an intrinsic"
        );
    });
}

#[test]
fn a_raw_pointer_can_point_at_another_pointer() {
    let src = "fn read(p: **i32): i32 { 0 }";
    with_lowered(src, |hir| {
        let TypeKind::Raw { to, .. } = hir.functions[0].params[0].typ.kind() else {
            panic!("p must be a raw pointer")
        };
        assert!(matches!(to.kind(), TypeKind::Raw { .. }));
    });
}

#[test]
fn a_signature_instantiating_a_generic_keeps_its_own_id() {
    let src = r#"
            enum Res<S, F> { Ok(S), Bad(F) }
            impl Res<S, F> {
                fn first(self): S { self.second() }
                fn second(self): S { self.first() }
            }

            struct Layout { size: uptr }
            struct Failed {}
            impl Layout {
                fn make(size: uptr): Res<Layout, Failed> { Res::Ok(Layout { size: size }) }
            }

            fn main(): i32 { Layout::make(1); 0 }
        "#;
    with_lowered(src, |hir| {
        let name = |f: &Function| hir.symbols.get(f.name).to_owned();

        let make = hir
            .functions
            .iter()
            .find(|f| name(f) == "nyx::Layout::make")
            .expect("Layout::make must keep the id it registered");
        assert_eq!(make.params.len(), 1, "the size parameter must survive");
        assert!(matches!(make.return_type.kind(), TypeKind::Adt(_, _)));
    });
}

#[test]
fn generic_turbofish_selects_instance() {
    let src = r#"
            fn id<T>(x: T): T { x }
            fn main(): i64 { id::<i64>(5) }
        "#;
    with_lowered(src, |hir| {
        let name = |f: &Function| hir.symbols.get(f.name).to_owned();

        let instance = hir
            .functions
            .iter()
            .find(|f| name(f).contains("id$i64"))
            .expect("specialised id$i64 instance");
        assert_eq!(instance.params[0].typ, TypeKind::I64.into());
    });
}

#[test]
fn generic_free_fn_resolves_generic_method() {
    let src = r#"
            struct Box<T> { val: T }
            impl Box<T> { fn get(&self): T { self.val } }
            fn unwrap<T>(b: &Box<T>): T { b.get() }
            fn main(): i64 { unwrap::<i64>(&Box::<i64> { val: 7 }) }
        "#;
    with_lowered(src, |hir| {
        let name = |f: &Function| hir.symbols.get(f.name).to_owned();

        let box_id = hir
            .adts
            .iter()
            .position(|adt| hir.symbols.get(adt.name).ends_with("Box"))
            .map(|index| AdtId(index as u32))
            .expect("Box definition");
        let concrete_box = hir.types.adt(box_id, &[hir.types.common.i64]);
        assert!(
            hir.functions.iter().any(|function| matches!(
                function.kind,
                FunctionKind::Method(method) if method.receiver == concrete_box
            ) && name(function).contains("get")),
            "expected a specialised get method on structural Box<i64>: {:?}",
            hir.functions
                .iter()
                .map(|function| (name(function), function.kind))
                .collect::<Vec<_>>()
        );
    });
}

fn span_text(src: &str, span: Span) -> &str {
    &src[span.start.0 as usize..span.end.0 as usize]
}

#[test]
fn array_constant_index_out_of_bounds() {
    let src = "fn main(){let a:[i32;3]=[1,2,3];a[5];}";
    with_lowered_err(src, |err| {
        assert_eq!(err.kind, HirErrorKind::IndexOutOfBounds { index: 5, len: 3 });
        assert_eq!(span_text(src, err.span), "a[5]");
    });
}

#[test]
fn indexing_a_non_indexable_type_is_rejected() {
    let src = "fn main(){let x:i32=1;x[0];}";
    with_lowered_err(src, |err| {
        assert!(matches!(err.kind, HirErrorKind::NotIndexable { .. }));
        assert_eq!(span_text(src, err.span), "x[0]");
    });
}

#[test]
fn a_non_integer_index_is_rejected() {
    let src = "fn main(){let a:[i32;2]=[1,2];let b:bool=true;a[b];}";
    with_lowered_err(src, |err| {
        assert!(matches!(err.kind, HirErrorKind::TypeMismatch { .. }));
    });
}

#[test]
fn overloaded_indexing_normalises_to_contextual_method_calls() {
    let src = r#"
            interface Index<Idx> {
                type Output;
                fn index(&self, index: Idx): &Self::Output;
            }
            interface IndexMutable<Idx>: Index {
                fn index_mut(&mut self, index: Idx): &mut Self::Output;
            }
            struct Bag { value: i32 }
            impl Bag with Index<bool> {
                type Output = i32;
                fn index(&self, index: bool): &Self::Output { &self.value }
            }
            impl Bag with IndexMutable<bool> {
                fn index_mut(&mut self, index: bool): &mut Self::Output { &mut self.value }
            }
            fn read(bag: &Bag): i32 { bag[false] }
            fn write(bag: &mut Bag) { bag[true] = 9; }
        "#;
    with_lowered(src, |hir| {
        let function = |name| {
            hir.functions
                .iter()
                .find(|function| hir.symbols.get(function.name).ends_with(name))
                .expect("function must be lowered")
        };

        fn index_call<'hir>(expr: &'hir Expression<'hir>) -> &'hir Expression<'hir> {
            let ExpressionKind::Unary { operator: UnaryOperator::Deref, expr: call } = expr.kind
            else {
                panic!("overloaded indexing must normalise to a dereferenced call")
            };
            assert!(matches!(call.kind, ExpressionKind::MethodCall { .. }));
            call
        }

        let resolved_name = |owner: &Function, call: &Expression| {
            let id = owner
                .typeck
                .type_dependent_def(call.id)
                .and_then(Res::function)
                .expect("normalised call must have a resolved method");
            hir.symbols.get(hir.functions[id].name)
        };

        let read = function("read");
        let Statement::Return(Some(read_index)) = read.body.statements[0] else {
            panic!("read must return its index expression")
        };
        assert!(resolved_name(read, index_call(read_index)).ends_with("Index::index"));

        let write = function("write");
        let Statement::Expr(assign) = write.body.statements[0] else {
            panic!("write must contain an assignment")
        };
        let ExpressionKind::Assign { target: write_index, .. } = assign.kind else {
            panic!("write expression must be an assignment")
        };
        assert!(resolved_name(write, index_call(write_index)).ends_with("IndexMutable::index_mut"));
    });
}

#[test]
fn mutable_indexing_requires_index_mutable() {
    let src = r#"
            interface Index<Idx> {
                type Output;
                fn index(&self, index: Idx): &Self::Output;
            }
            struct Bag { value: i32 }
            impl Bag with Index<i32> {
                type Output = i32;
                fn index(&self, index: i32): &Self::Output = &self.value;
            }
            fn write(bag: &mut Bag) { bag[0] = 9; }
        "#;

    with_lowered_err(src, |err| {
        assert!(matches!(err.kind, HirErrorKind::NotMutablyIndexable { .. }));
    });
}

#[test]
fn writing_an_immutable_array_element_is_rejected() {
    let src = "fn main(){let a:[i32;2]=[1,2];a[0]=9;}";
    with_lowered_err(src, |err| {
        assert!(matches!(err.kind, HirErrorKind::ImmutableBind { name: "a", .. }));
        assert_eq!(span_text(src, err.span), "a[0]");
    });
}

#[test]
fn writing_through_a_shared_slice_is_rejected() {
    let src = "fn set(s:&[i32]){s[0]=9;}";
    with_lowered_err(src, |err| {
        assert_eq!(err.kind, HirErrorKind::AssignBehindSharedRef);
        assert_eq!(span_text(src, err.span), "s[0]");
    });
}

#[test]
fn a_match_missing_an_enum_variant_is_reported() {
    let source = "enum Signal { Halt, Skip, Take }\n\
        fn code(s: Signal): i32 { match s { Signal::Halt -> 0, Signal::Skip -> 1, } }\n\
        fn main(): i32 { code(Signal::Halt) }";

    assert_eq!(missing_pattern(source).as_deref(), Some("Signal::Take"));
}

#[test]
fn a_match_covering_every_variant_is_accepted() {
    let source = "enum Signal { Halt, Skip }\n\
        fn code(s: Signal): i32 { match s { Signal::Halt -> 0, Signal::Skip -> 1, } }\n\
        fn main(): i32 { code(Signal::Halt) }";

    assert_eq!(missing_pattern(source), None);
}

#[test]
fn a_wildcard_arm_covers_what_is_left() {
    let source = "enum Signal { Halt, Skip, Take }\n\
        fn code(s: Signal): i32 { match s { Signal::Halt -> 0, _ -> 1, } }\n\
        fn main(): i32 { code(Signal::Halt) }";

    assert_eq!(missing_pattern(source), None);
}

#[test]
fn an_or_pattern_covers_each_of_its_alternatives() {
    let source = "enum Signal { Halt, Skip }\n\
        fn code(s: Signal): i32 { match s { Signal::Halt | Signal::Skip -> 0, } }\n\
        fn main(): i32 { code(Signal::Halt) }";

    assert_eq!(missing_pattern(source), None);
}

#[test]
fn a_guarded_arm_does_not_count_towards_coverage() {
    let source = "fn code(b: bool): i32 { match b { x if x -> 0, false -> 1, } }\n\
        fn main(): i32 { code(false) }";

    assert_eq!(missing_pattern(source).as_deref(), Some("true"));
}

#[test]
fn both_booleans_are_needed() {
    let source = "fn code(b: bool): i32 { match b { false -> 0, } }\n\
        fn main(): i32 { code(false) }";

    assert_eq!(missing_pattern(source).as_deref(), Some("true"));
}

#[test]
fn a_range_spanning_the_whole_type_is_exhaustive() {
    let source = "fn code(n: u8): i32 { match n { 0..=255 -> 0, } }\n\
        fn main(): i32 { code(0) }";

    assert_eq!(missing_pattern(source), None);
}

#[test]
fn a_range_leaving_one_value_out_names_it() {
    let source = "fn code(n: u8): i32 { match n { 0..=254 -> 0, } }\n\
        fn main(): i32 { code(0) }";

    assert_eq!(missing_pattern(source).as_deref(), Some("255"));
}

#[test]
fn a_missing_payload_is_reported_through_the_variant() {
    let source = "enum Opt { None, Some(bool) }\n\
        fn code(o: Opt): i32 { match o { Opt::None -> 0, Opt::Some(true) -> 1, } }\n\
        fn main(): i32 { code(Opt::None) }";

    assert_eq!(missing_pattern(source).as_deref(), Some("Opt::Some(false)"));
}

#[test]
fn a_binding_covers_everything() {
    let source = "fn code(n: i32): i32 { match n { x -> x, } }\nfn main(): i32 { code(1) }";

    assert_eq!(missing_pattern(source), None);
}

#[test]
fn a_float_match_always_needs_a_wildcard() {
    let source = "fn code(x: f64): i32 { match x { 1.0 -> 0, } }\nfn main(): i32 { code(1.0) }";
    assert_eq!(missing_pattern(source).as_deref(), Some("_"));
}
