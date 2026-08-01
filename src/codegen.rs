use std::collections::HashMap;

use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::values::{BasicMetadataValueEnum, FloatValue, FunctionValue, IntValue, PointerValue};
use inkwell::{AddressSpace, FloatPredicate};

use crate::ast::*;
use crate::error::{QmclError, QmclInfo, Span};

/// macOS/BSD libc's LC_NUMERIC value (from <locale.h>) — needed to call
/// setlocale() so printf's `'` thousands-grouping flag actually does
/// anything (it's a no-op in the default "C" locale). This is
/// platform-specific; glibc uses a different constant, so this would need
/// adjusting if QMCL ever targets Linux.
const LC_NUMERIC: i32 = 4;

/// The result of compiling an expression. Not every expression is a number
/// anymore, so callers (print formatting, arithmetic) need to know which
/// kind they actually got.
enum Value<'ctx> {
    /// The bool is whether this value should print with thousands
    /// separators (e.g. 1,000,000) — true if the literal it came from used
    /// them, or if either side of an arithmetic operation did.
    Number(FloatValue<'ctx>, bool),
    /// Same underlying f64 as Number (already normalized to a fraction —
    /// 100% is stored as 1.0), kept as its own variant purely so printing
    /// knows to re-scale by 100 and append '%'.
    Percentage(FloatValue<'ctx>, bool),
    Boolean(IntValue<'ctx>),
    Str(PointerValue<'ctx>),
}

pub struct Codegen<'ctx> {
    context: &'ctx Context,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    /// Type, plus whether the variable's value should print grouped.
    vars: HashMap<String, (PointerValue<'ctx>, Type, bool)>,
    /// Collected instead of aborting at the first one, so a single run can
    /// report every problem in the file, not just the first.
    errors: Vec<QmclError>,
    /// Notable-but-not-wrong things worth telling the programmer about,
    /// e.g. a variable being shadowed. Never blocks compilation.
    notes: Vec<QmclInfo>,
    /// libm's `pow` — LLVM has no native float-exponentiation instruction,
    /// so `^`/`**` compiles to a call to this, same as C/C++ do.
    pow_fn: Option<FunctionValue<'ctx>>,
}

impl<'ctx> Codegen<'ctx> {
    pub fn new(context: &'ctx Context, module_name: &str) -> Self {
        Codegen {
            context,
            module: context.create_module(module_name),
            builder: context.create_builder(),
            vars: HashMap::new(),
            errors: Vec::new(),
            notes: Vec::new(),
            pow_fn: None,
        }
    }

    pub fn module(&self) -> &Module<'ctx> {
        &self.module
    }

    /// QMCL programs have no `fn main`-equivalent — top-level statements just
    /// run in order. This emits them straight into a single LLVM `main`
    /// function, which is compiler plumbing the programmer never writes.
    ///
    /// Returns every semantic error found across the whole program (empty
    /// means it's safe to run) alongside any informational notes. Keeps
    /// compiling subsequent statements after hitting an error, using
    /// placeholder values where needed, purely so more errors can surface in
    /// the same pass — the resulting module is never executed if the error
    /// list isn't empty.
    pub fn compile_program(&mut self, program: &Program) -> (Vec<QmclError>, Vec<QmclInfo>) {
        let i8_ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i32_ty = self.context.i32_type();
        let printf_ty = i32_ty.fn_type(&[i8_ptr_ty.into()], true);
        let printf_fn = self.module.add_function("printf", printf_ty, None);

        let f64_ty = self.context.f64_type();
        let pow_ty = f64_ty.fn_type(&[f64_ty.into(), f64_ty.into()], false);
        self.pow_fn = Some(self.module.add_function("pow", pow_ty, None));

        let setlocale_ty = i8_ptr_ty.fn_type(&[i32_ty.into(), i8_ptr_ty.into()], false);
        let setlocale_fn = self.module.add_function("setlocale", setlocale_ty, None);

        let main_ty = i32_ty.fn_type(&[], false);
        let main_fn = self.module.add_function("main", main_ty, None);
        let entry = self.context.append_basic_block(main_fn, "entry");
        self.builder.position_at_end(entry);

        // printf's `'` thousands-grouping flag is a no-op under the default
        // "C" locale, so a locale with actual digit grouping has to be set
        // before any print statement that might use it.
        let locale_name = self
            .builder
            .build_global_string_ptr("en_US.UTF-8", "locale_name")
            .unwrap();
        self.builder
            .build_call(
                setlocale_fn,
                &[
                    i32_ty.const_int(LC_NUMERIC as u64, true).into(),
                    locale_name.as_pointer_value().into(),
                ],
                "setlocale_call",
            )
            .unwrap();

        for stmt in program {
            self.compile_stmt(stmt, printf_fn);
        }

        self.builder
            .build_return(Some(&i32_ty.const_int(0, false)))
            .unwrap();

        (std::mem::take(&mut self.errors), std::mem::take(&mut self.notes))
    }

    fn compile_stmt(&mut self, stmt: &Stmt, printf_fn: FunctionValue<'ctx>) {
        match stmt {
            Stmt::Declare { name, name_span, ty, value } => {
                if self.vars.contains_key(name) {
                    self.notes.push(
                        QmclInfo::new(format!(
                            "'{}' is being redeclared — its previous value is no longer accessible",
                            name
                        ))
                        .at(*name_span),
                    );
                }

                let val = self.compile_expr(value);
                match ty {
                    Type::Number | Type::Percentage => {
                        let (f, grouped) = self.as_number(val, *name_span);
                        let f64_ty = self.context.f64_type();
                        let ptr = self.builder.build_alloca(f64_ty, name).unwrap();
                        self.builder.build_store(ptr, f).unwrap();
                        self.vars.insert(name.clone(), (ptr, *ty, grouped));
                    }
                    Type::Boolean => {
                        // Guaranteed to already be a Value::Boolean — the
                        // parser only ever produces a BooleanLiteral here.
                        let b = match val {
                            Value::Boolean(b) => b,
                            _ => unreachable!("parser only allows a boolean literal here"),
                        };
                        let bool_ty = self.context.bool_type();
                        let ptr = self.builder.build_alloca(bool_ty, name).unwrap();
                        self.builder.build_store(ptr, b).unwrap();
                        self.vars.insert(name.clone(), (ptr, *ty, false));
                    }
                    Type::String => {
                        // Guaranteed to already be a Value::Str — the
                        // parser only ever produces a StringLiteral here.
                        let s = match val {
                            Value::Str(s) => s,
                            _ => unreachable!("parser only allows a string literal here"),
                        };
                        let ptr_ty = self.context.ptr_type(AddressSpace::default());
                        let ptr = self.builder.build_alloca(ptr_ty, name).unwrap();
                        self.builder.build_store(ptr, s).unwrap();
                        self.vars.insert(name.clone(), (ptr, *ty, false));
                    }
                }
            }
            Stmt::Print { parts } => self.compile_print(parts, printf_fn),
        }
    }

    fn compile_print(&mut self, parts: &[PrintPart], printf_fn: FunctionValue<'ctx>) {
        let mut fmt = String::new();
        let mut args: Vec<BasicMetadataValueEnum> = Vec::new();

        for part in parts {
            match part {
                PrintPart::Text(s) => {
                    // printf treats '%' specially, so literal text can't
                    // pass one through unescaped.
                    fmt.push_str(&s.replace('%', "%%"));
                }
                PrintPart::Value(expr) => {
                    let val = self.compile_expr(expr);
                    match val {
                        Value::Number(f, grouped) => {
                            // %.15g: full double precision, but trims
                            // trailing zeros so a whole number like 1000.0
                            // prints as "1000" rather than "1000.000...".
                            // The `'` flag (needs the locale set up above)
                            // adds thousands separators.
                            fmt.push_str(if grouped { "%'.15g" } else { "%.15g" });
                            args.push(f.into());
                        }
                        Value::Percentage(f, grouped) => {
                            let hundred = self.context.f64_type().const_float(100.0);
                            let scaled = self.builder.build_float_mul(f, hundred, "pcttmp").unwrap();
                            fmt.push_str(if grouped { "%'.15g%%" } else { "%.15g%%" }); // %% is a literal '%' to printf
                            args.push(scaled.into());
                        }
                        Value::Boolean(b) => {
                            let true_str = self
                                .builder
                                .build_global_string_ptr("true", "true_str")
                                .unwrap()
                                .as_pointer_value();
                            let false_str = self
                                .builder
                                .build_global_string_ptr("false", "false_str")
                                .unwrap()
                                .as_pointer_value();
                            let selected = self
                                .builder
                                .build_select(b, true_str, false_str, "boolstr")
                                .unwrap();
                            fmt.push_str("%s");
                            args.push(selected.into());
                        }
                        Value::Str(s) => {
                            fmt.push_str("%s");
                            args.push(s.into());
                        }
                    }
                }
            }
        }
        fmt.push('\n');

        let fmt_global = self.builder.build_global_string_ptr(&fmt, "fmt").unwrap();
        let mut call_args: Vec<BasicMetadataValueEnum> = vec![fmt_global.as_pointer_value().into()];
        call_args.extend(args);
        self.builder
            .build_call(printf_fn, &call_args, "printf_call")
            .unwrap();
    }

    /// Never fails outright — on a semantic error, records it in `self.errors`
    /// and returns a placeholder value so codegen can keep walking the rest
    /// of the program looking for more problems.
    fn compile_expr(&mut self, expr: &Expr) -> Value<'ctx> {
        match expr {
            Expr::NumberLiteral(n, grouped) => {
                Value::Number(self.context.f64_type().const_float(*n), *grouped)
            }
            Expr::StringLiteral(s) => {
                let ptr = self
                    .builder
                    .build_global_string_ptr(s, "strlit")
                    .unwrap()
                    .as_pointer_value();
                Value::Str(ptr)
            }
            Expr::BooleanLiteral(b) => {
                Value::Boolean(self.context.bool_type().const_int(*b as u64, false))
            }
            Expr::Var(name, span) => match self.vars.get(name) {
                Some((ptr, ty, grouped)) => {
                    let ptr = *ptr;
                    let grouped = *grouped;
                    match ty {
                        Type::Number => {
                            let v = self
                                .builder
                                .build_load(self.context.f64_type(), ptr, name)
                                .unwrap()
                                .into_float_value();
                            Value::Number(v, grouped)
                        }
                        Type::Percentage => {
                            let v = self
                                .builder
                                .build_load(self.context.f64_type(), ptr, name)
                                .unwrap()
                                .into_float_value();
                            Value::Percentage(v, grouped)
                        }
                        Type::Boolean => {
                            let v = self
                                .builder
                                .build_load(self.context.bool_type(), ptr, name)
                                .unwrap()
                                .into_int_value();
                            Value::Boolean(v)
                        }
                        Type::String => {
                            let v = self
                                .builder
                                .build_load(self.context.ptr_type(AddressSpace::default()), ptr, name)
                                .unwrap()
                                .into_pointer_value();
                            Value::Str(v)
                        }
                    }
                }
                None => {
                    self.errors.push(
                        QmclError::new(format!("undeclared variable '{}'", name))
                            .at(*span)
                            .rule("a variable must be declared before it's referenced")
                            .suggest(format!(
                                "declare it first, e.g. declare '{}' = number '...'.",
                                name
                            )),
                    );
                    Value::Number(self.context.f64_type().const_float(0.0), false)
                }
            },
            Expr::BinaryOp(op, left, right, op_span) => {
                let l_val = self.compile_expr(left);
                let r_val = self.compile_expr(right);
                let (l, l_grouped) = self.as_number(l_val, *op_span);
                let (r, r_grouped) = self.as_number(r_val, *op_span);
                // If either side wants thousands separators, the result
                // does too.
                let grouped = l_grouped || r_grouped;
                match op {
                    BinOp::Add => {
                        Value::Number(self.builder.build_float_add(l, r, "addtmp").unwrap(), grouped)
                    }
                    BinOp::Sub => {
                        Value::Number(self.builder.build_float_sub(l, r, "subtmp").unwrap(), grouped)
                    }
                    BinOp::Mul => {
                        Value::Number(self.builder.build_float_mul(l, r, "multmp").unwrap(), grouped)
                    }
                    BinOp::Div => {
                        Value::Number(self.builder.build_float_div(l, r, "divtmp").unwrap(), grouped)
                    }
                    BinOp::Pow => {
                        let pow_fn = self.pow_fn.expect("pow_fn set at start of compile_program");
                        let result = match self
                            .builder
                            .build_call(pow_fn, &[l.into(), r.into()], "powtmp")
                            .unwrap()
                            .try_as_basic_value()
                        {
                            inkwell::values::ValueKind::Basic(v) => v.into_float_value(),
                            inkwell::values::ValueKind::Instruction(_) => {
                                unreachable!("pow always returns a value, never void")
                            }
                        };
                        Value::Number(result, grouped)
                    }
                    // No boolean type is produced here (comparisons predate
                    // Boolean and still yield a plain number) — 1.0 or 0.0.
                    BinOp::Gt => {
                        let cmp = self
                            .builder
                            .build_float_compare(FloatPredicate::OGT, l, r, "gttmp")
                            .unwrap();
                        Value::Number(
                            self.builder
                                .build_unsigned_int_to_float(cmp, self.context.f64_type(), "booltmp")
                                .unwrap(),
                            false,
                        )
                    }
                    BinOp::Lt => {
                        let cmp = self
                            .builder
                            .build_float_compare(FloatPredicate::OLT, l, r, "lttmp")
                            .unwrap();
                        Value::Number(
                            self.builder
                                .build_unsigned_int_to_float(cmp, self.context.f64_type(), "booltmp")
                                .unwrap(),
                            false,
                        )
                    }
                }
            }
        }
    }

    /// Coerces a `Value` into a float (plus its "should print grouped"
    /// flag) for arithmetic, reporting a clear error (rather than emitting
    /// broken IR) if it's actually a string or boolean — those aren't
    /// usable in arithmetic yet.
    fn as_number(&mut self, v: Value<'ctx>, span: Span) -> (FloatValue<'ctx>, bool) {
        match v {
            Value::Number(f, grouped) | Value::Percentage(f, grouped) => (f, grouped),
            Value::Boolean(_) => {
                self.errors.push(
                    QmclError::new("a boolean value can't be used in arithmetic")
                        .at(span)
                        .rule("arithmetic operators only work on number/percentage values")
                        .suggest("only use +, -, *, /, ^ with number or percentage values"),
                );
                (self.context.f64_type().const_float(0.0), false)
            }
            Value::Str(_) => {
                self.errors.push(
                    QmclError::new("a string value can't be used in arithmetic")
                        .at(span)
                        .rule("arithmetic operators only work on number/percentage values")
                        .suggest("only use +, -, *, /, ^ with number or percentage values"),
                );
                (self.context.f64_type().const_float(0.0), false)
            }
        }
    }
}
