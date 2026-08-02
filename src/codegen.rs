use std::collections::HashMap;

use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::types::{FloatType, IntType};
use inkwell::values::{BasicMetadataValueEnum, FloatValue, FunctionValue, IntValue, PointerValue};
use inkwell::{AddressSpace, FloatPredicate, IntPredicate};

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
    /// bool: whether this value should print with thousands separators
    /// (e.g. 1,000,000) — true if the literal it came from used them, or
    /// if either side of an arithmetic operation did.
    /// u8: the actual bit width this value currently is (16/32/64).
    Number(FloatValue<'ctx>, bool, u8),
    /// Same underlying float as Number (already normalized to a fraction —
    /// 100% is stored as 1.0), kept as its own variant purely so printing
    /// knows to re-scale by 100 and append '%'. Always 64-bit.
    Percentage(FloatValue<'ctx>, bool),
    /// A genuine integer — separate representation from Number, real
    /// integer arithmetic rather than a float that looks whole. bool/u8
    /// mean the same thing as on Number (grouped-for-print, current width).
    Integer(IntValue<'ctx>, bool, u8),
    Boolean(IntValue<'ctx>),
    Str(PointerValue<'ctx>),
}

/// A value classified as either a true integer or a float-family number
/// (Number/Percentage), for arithmetic — Boolean/Str never reach this,
/// they're rejected before classification.
enum Numeric<'ctx> {
    Int(IntValue<'ctx>, bool, u8),
    Float(FloatValue<'ctx>, bool, u8),
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
    /// e.g. a variable being shadowed, or arithmetic silently promoting a
    /// mismatched pair of number widths. Never blocks compilation.
    notes: Vec<QmclInfo>,
    /// libm's `pow` — LLVM has no native float-exponentiation instruction,
    /// so `^`/`**` compiles to a call to this, same as C/C++ do. Always
    /// double-precision (that's libm's signature), regardless of operand
    /// width.
    pow_fn: Option<FunctionValue<'ctx>>,
    printf_fn: Option<FunctionValue<'ctx>>,
    /// QMCL has no user-defined functions yet — everything compiles into
    /// this one `main`, which loops need a handle to in order to append
    /// new basic blocks (for the condition/body/end of the loop).
    main_fn: Option<FunctionValue<'ctx>>,
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
            printf_fn: None,
            main_fn: None,
        }
    }

    pub fn module(&self) -> &Module<'ctx> {
        &self.module
    }

    fn float_type_for_width(&self, width: u8) -> FloatType<'ctx> {
        match width {
            16 => self.context.f16_type(),
            32 => self.context.f32_type(),
            _ => self.context.f64_type(),
        }
    }

    /// Widens or narrows `f` from `from_width` to `to_width`, or returns it
    /// unchanged if they're already equal.
    fn cast_to_width(&mut self, f: FloatValue<'ctx>, from_width: u8, to_width: u8) -> FloatValue<'ctx> {
        if from_width == to_width {
            return f;
        }
        let target_ty = self.float_type_for_width(to_width);
        if to_width > from_width {
            self.builder.build_float_ext(f, target_ty, "widen").unwrap()
        } else {
            self.builder.build_float_trunc(f, target_ty, "narrow").unwrap()
        }
    }

    fn int_type_for_width(&self, width: u8) -> IntType<'ctx> {
        match width {
            8 => self.context.i8_type(),
            16 => self.context.i16_type(),
            32 => self.context.i32_type(),
            _ => self.context.i64_type(),
        }
    }

    /// Widens or narrows `i` from `from_width` to `to_width` (sign-extend
    /// or truncate), or returns it unchanged if they're already equal.
    fn cast_int_to_width(&mut self, i: IntValue<'ctx>, from_width: u8, to_width: u8) -> IntValue<'ctx> {
        if from_width == to_width {
            return i;
        }
        let target_ty = self.int_type_for_width(to_width);
        if to_width > from_width {
            self.builder.build_int_s_extend(i, target_ty, "iwiden").unwrap()
        } else {
            self.builder.build_int_truncate(i, target_ty, "inarrow").unwrap()
        }
    }

    fn int_to_f64(&mut self, i: IntValue<'ctx>) -> FloatValue<'ctx> {
        self.builder
            .build_signed_int_to_float(i, self.context.f64_type(), "itof")
            .unwrap()
    }

    /// Classifies a Value for arithmetic, rejecting Boolean/Str with a clear
    /// error (rather than emitting broken IR) — those aren't usable in
    /// arithmetic yet.
    fn classify_numeric(&mut self, v: Value<'ctx>, span: Span) -> Numeric<'ctx> {
        match v {
            Value::Number(f, grouped, width) => Numeric::Float(f, grouped, width),
            Value::Percentage(f, grouped) => Numeric::Float(f, grouped, 64),
            Value::Integer(i, grouped, width) => Numeric::Int(i, grouped, width),
            Value::Boolean(_) => {
                self.errors.push(
                    QmclError::new("a boolean value can't be used in arithmetic")
                        .at(span)
                        .rule("arithmetic operators only work on number/integer/percentage values")
                        .suggest("only use +, -, *, /, ^ with number, integer, or percentage values"),
                );
                Numeric::Float(self.context.f64_type().const_float(0.0), false, 64)
            }
            Value::Str(_) => {
                self.errors.push(
                    QmclError::new("a string value can't be used in arithmetic")
                        .at(span)
                        .rule("arithmetic operators only work on number/integer/percentage values")
                        .suggest("only use +, -, *, /, ^ with number, integer, or percentage values"),
                );
                Numeric::Float(self.context.f64_type().const_float(0.0), false, 64)
            }
        }
    }

    /// Converts a Numeric to a float for the mixed/float arithmetic path.
    /// Converting a genuine Int here means an integer got mixed with a
    /// number/percentage — surfaced via the Informer rather than done
    /// silently, since the result is no longer a true integer.
    fn numeric_to_float(&mut self, n: Numeric<'ctx>, op_span: Span) -> (FloatValue<'ctx>, bool, u8) {
        match n {
            Numeric::Float(f, grouped, width) => (f, grouped, width),
            Numeric::Int(i, grouped, _width) => {
                self.notes.push(
                    QmclInfo::new(
                        "mixed integer and number here — automatically computed as a number (float)",
                    )
                    .at(op_span),
                );
                (self.int_to_f64(i), grouped, 64)
            }
        }
    }

    /// Same idea as `numeric_to_float`, but silent — used when a `number`/
    /// `percentage` declare's value happens to resolve to an integer (e.g.
    /// `declare 'y' = number (some_int_var).`), which is plain assignment
    /// rather than an arithmetic operation mixing types, so it's treated
    /// the same as any other widening declare-time conversion (silent).
    fn coerce_to_float(&mut self, v: Value<'ctx>, span: Span) -> (FloatValue<'ctx>, bool, u8) {
        match self.classify_numeric(v, span) {
            Numeric::Float(f, grouped, width) => (f, grouped, width),
            Numeric::Int(i, grouped, _width) => (self.int_to_f64(i), grouped, 64),
        }
    }

    /// Registers `name` with a zero placeholder after a declare-time type
    /// error, so later references to it report as the (already-reported)
    /// wrong-type error instead of cascading into a separate "undeclared
    /// variable" error.
    fn declare_placeholder_integer(&mut self, name: &str, width: u8, ty: Type) {
        let int_ty = self.int_type_for_width(width);
        let ptr = self.builder.build_alloca(int_ty, name).unwrap();
        self.builder.build_store(ptr, int_ty.const_int(0, false)).unwrap();
        self.vars.insert(name.to_string(), (ptr, ty, false));
    }

    /// Requires a Value to be a genuine integer (widened/narrowed to
    /// 64-bit), reporting a clear error — rather than emitting broken IR —
    /// if it turns out to be a float/percentage/boolean/string. Used for a
    /// loop's `from`/`to` bounds, which must be true integers.
    fn require_integer(&mut self, v: Value<'ctx>, span: Span, what: &str) -> IntValue<'ctx> {
        match self.classify_numeric(v, span) {
            Numeric::Int(i, _grouped, width) => self.cast_int_to_width(i, width, 64),
            Numeric::Float(..) => {
                self.errors.push(
                    QmclError::new(format!("{} must be an integer", what))
                        .at(span)
                        .rule("a loop's 'from'/'to' bounds must be genuine integers, not a number/percentage")
                        .suggest("use an integer literal or an integer-typed variable for the loop bounds"),
                );
                self.context.i64_type().const_int(0, false)
            }
        }
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
        self.printf_fn = Some(printf_fn);

        let f64_ty = self.context.f64_type();
        let pow_ty = f64_ty.fn_type(&[f64_ty.into(), f64_ty.into()], false);
        self.pow_fn = Some(self.module.add_function("pow", pow_ty, None));

        let setlocale_ty = i8_ptr_ty.fn_type(&[i32_ty.into(), i8_ptr_ty.into()], false);
        let setlocale_fn = self.module.add_function("setlocale", setlocale_ty, None);

        let main_ty = i32_ty.fn_type(&[], false);
        let main_fn = self.module.add_function("main", main_ty, None);
        self.main_fn = Some(main_fn);
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
            self.compile_stmt(stmt);
        }

        self.builder
            .build_return(Some(&i32_ty.const_int(0, false)))
            .unwrap();

        (std::mem::take(&mut self.errors), std::mem::take(&mut self.notes))
    }

    fn compile_stmt(&mut self, stmt: &Stmt) {
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
                    Type::Number(width) => {
                        let (f, grouped, val_width) = self.coerce_to_float(val, *name_span);
                        let f = self.cast_to_width(f, val_width, *width);
                        let float_ty = self.float_type_for_width(*width);
                        let ptr = self.builder.build_alloca(float_ty, name).unwrap();
                        self.builder.build_store(ptr, f).unwrap();
                        self.vars.insert(name.clone(), (ptr, *ty, grouped));
                    }
                    Type::Percentage => {
                        let (f, grouped, val_width) = self.coerce_to_float(val, *name_span);
                        let f = self.cast_to_width(f, val_width, 64);
                        let f64_ty = self.context.f64_type();
                        let ptr = self.builder.build_alloca(f64_ty, name).unwrap();
                        self.builder.build_store(ptr, f).unwrap();
                        self.vars.insert(name.clone(), (ptr, *ty, grouped));
                    }
                    Type::Integer(width) => match val {
                        Value::Integer(i, grouped, val_width) => {
                            let i = self.cast_int_to_width(i, val_width, *width);
                            let int_ty = self.int_type_for_width(*width);
                            let ptr = self.builder.build_alloca(int_ty, name).unwrap();
                            self.builder.build_store(ptr, i).unwrap();
                            self.vars.insert(name.clone(), (ptr, *ty, grouped));
                        }
                        Value::Number(..) | Value::Percentage(..) => {
                            self.errors.push(
                                QmclError::new("cannot store a number (float) value in an integer variable")
                                    .at(*name_span)
                                    .rule("an integer variable's value must stay a true integer — mixing in a number/percentage anywhere in the expression makes the whole result a float")
                                    .suggest("make sure every part of this expression is an integer, or declare this variable as 'number' instead"),
                            );
                            self.declare_placeholder_integer(name, *width, *ty);
                        }
                        Value::Boolean(_) | Value::Str(_) => {
                            self.errors.push(
                                QmclError::new("cannot store a boolean/string value in an integer variable")
                                    .at(*name_span)
                                    .rule("an integer's value must be an integer expression")
                                    .suggest("declare this as 'boolean'/'string' instead, or fix the expression"),
                            );
                            self.declare_placeholder_integer(name, *width, *ty);
                        }
                    },
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
            Stmt::Print { parts } => self.compile_print(parts),
            Stmt::CountedLoop { var_name, var_name_span, start, end, body } => {
                self.compile_counted_loop(var_name, *var_name_span, start, end, body)
            }
        }
    }

    fn compile_print(&mut self, parts: &[PrintPart]) {
        let printf_fn = self.printf_fn.expect("printf_fn set at start of compile_program");
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
                        Value::Number(f, grouped, width) => {
                            // printf's variadic calling convention always
                            // promotes float args to double — there's no
                            // way to pass a raw 16/32-bit float to it, so
                            // this widens regardless of the declared width.
                            let f = self.cast_to_width(f, width, 64);
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
                        Value::Integer(i, grouped, width) => {
                            // C's variadic default-argument promotion widens
                            // anything narrower than `int` (32-bit) to int
                            // automatically — replicated by hand here since
                            // we're emitting the call directly rather than
                            // going through a C compiler that'd do it for
                            // us. 64-bit needs %lld specifically.
                            if width == 64 {
                                fmt.push_str(if grouped { "%'lld" } else { "%lld" });
                                args.push(i.into());
                            } else {
                                let i32_val = self.cast_int_to_width(i, width, 32);
                                fmt.push_str(if grouped { "%'d" } else { "%d" });
                                args.push(i32_val.into());
                            }
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
            // A bare literal has no inherent declared width; it's evaluated
            // at 64-bit and narrowed later at the point of use (a narrower
            // declare, or mixed-width arithmetic) if needed.
            Expr::NumberLiteral(n, grouped) => {
                Value::Number(self.context.f64_type().const_float(*n), *grouped, 64)
            }
            // Same idea — no inherent declared width until it's stored
            // somewhere or combined with another integer; evaluated at
            // 64-bit and narrowed at the point of use.
            Expr::IntegerLiteral(n, grouped) => {
                Value::Integer(self.context.i64_type().const_int(*n as u64, true), *grouped, 64)
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
                        Type::Number(width) => {
                            let float_ty = self.float_type_for_width(*width);
                            let v = self
                                .builder
                                .build_load(float_ty, ptr, name)
                                .unwrap()
                                .into_float_value();
                            Value::Number(v, grouped, *width)
                        }
                        Type::Percentage => {
                            let v = self
                                .builder
                                .build_load(self.context.f64_type(), ptr, name)
                                .unwrap()
                                .into_float_value();
                            Value::Percentage(v, grouped)
                        }
                        Type::Integer(width) => {
                            let int_ty = self.int_type_for_width(*width);
                            let v = self
                                .builder
                                .build_load(int_ty, ptr, name)
                                .unwrap()
                                .into_int_value();
                            Value::Integer(v, grouped, *width)
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
                    Value::Number(self.context.f64_type().const_float(0.0), false, 64)
                }
            },
            Expr::BinaryOp(op, left, right, op_span) => {
                let l_val = self.compile_expr(left);
                let r_val = self.compile_expr(right);
                let l_num = self.classify_numeric(l_val, *op_span);
                let r_num = self.classify_numeric(r_val, *op_span);

                match (l_num, r_num) {
                    // Both genuine integers — stays true integer math.
                    (Numeric::Int(l, l_grouped, l_width), Numeric::Int(r, r_grouped, r_width)) => {
                        self.compile_integer_binop(op, l, l_grouped, l_width, r, r_grouped, r_width, *op_span)
                    }
                    // At least one side is a number/percentage (or an
                    // integer that needs promoting to sit alongside one) —
                    // the existing float path, unchanged from before
                    // Integer existed.
                    (l_num, r_num) => {
                        let (l, l_grouped, l_width) = self.numeric_to_float(l_num, *op_span);
                        let (r, r_grouped, r_width) = self.numeric_to_float(r_num, *op_span);
                        // If either side wants thousands separators, the
                        // result does too.
                        let grouped = l_grouped || r_grouped;
                        // Mixed precision auto-promotes to the wider side,
                        // but that's surfaced via the Informer rather than
                        // done silently.
                        let target_width = l_width.max(r_width);
                        if l_width != r_width {
                            self.notes.push(
                                QmclInfo::new(format!(
                                    "mixed precision here ({}-bit and {}-bit) — automatically computed at {}-bit",
                                    l_width, r_width, target_width
                                ))
                                .at(*op_span),
                            );
                        }
                        let l = self.cast_to_width(l, l_width, target_width);
                        let r = self.cast_to_width(r, r_width, target_width);
                        match op {
                            BinOp::Add => Value::Number(
                                self.builder.build_float_add(l, r, "addtmp").unwrap(),
                                grouped,
                                target_width,
                            ),
                            BinOp::Sub => Value::Number(
                                self.builder.build_float_sub(l, r, "subtmp").unwrap(),
                                grouped,
                                target_width,
                            ),
                            BinOp::Mul => Value::Number(
                                self.builder.build_float_mul(l, r, "multmp").unwrap(),
                                grouped,
                                target_width,
                            ),
                            BinOp::Div => Value::Number(
                                self.builder.build_float_div(l, r, "divtmp").unwrap(),
                                grouped,
                                target_width,
                            ),
                            BinOp::Pow => {
                                // libm's pow is always (double, double) ->
                                // double, regardless of the operands' actual
                                // width.
                                let l64 = self.cast_to_width(l, target_width, 64);
                                let r64 = self.cast_to_width(r, target_width, 64);
                                let pow_fn = self.pow_fn.expect("pow_fn set at start of compile_program");
                                let result = match self
                                    .builder
                                    .build_call(pow_fn, &[l64.into(), r64.into()], "powtmp")
                                    .unwrap()
                                    .try_as_basic_value()
                                {
                                    inkwell::values::ValueKind::Basic(v) => v.into_float_value(),
                                    inkwell::values::ValueKind::Instruction(_) => {
                                        unreachable!("pow always returns a value, never void")
                                    }
                                };
                                let result = self.cast_to_width(result, 64, target_width);
                                Value::Number(result, grouped, target_width)
                            }
                            // No boolean type is produced here (comparisons
                            // predate Boolean and still yield a plain
                            // number) — 1.0 or 0.0.
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
                                    64,
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
                                    64,
                                )
                            }
                        }
                    }
                }
            }
        }
    }

    /// Real integer arithmetic for when both operands of a BinaryOp are
    /// genuine integers. Mismatched integer widths auto-promote to the
    /// wider one (Informer note, same as floats). `/` truncates (C/Rust
    /// convention) with a static Informer note at every integer-division
    /// site, regardless of the actual runtime values. `^` still goes
    /// through libm's pow (fixed double signature), so it's the one
    /// operator that produces a Number (float) result even when both
    /// inputs were integers — a real, flagged asymmetry with +/-/*//.
    #[allow(clippy::too_many_arguments)]
    fn compile_integer_binop(
        &mut self,
        op: &BinOp,
        l: IntValue<'ctx>,
        l_grouped: bool,
        l_width: u8,
        r: IntValue<'ctx>,
        r_grouped: bool,
        r_width: u8,
        op_span: Span,
    ) -> Value<'ctx> {
        let grouped = l_grouped || r_grouped;
        let target_width = l_width.max(r_width);
        if l_width != r_width {
            self.notes.push(
                QmclInfo::new(format!(
                    "mixed precision here ({}-bit and {}-bit integers) — automatically computed at {}-bit",
                    l_width, r_width, target_width
                ))
                .at(op_span),
            );
        }
        let l = self.cast_int_to_width(l, l_width, target_width);
        let r = self.cast_int_to_width(r, r_width, target_width);
        match op {
            BinOp::Add => Value::Integer(self.builder.build_int_add(l, r, "iaddtmp").unwrap(), grouped, target_width),
            BinOp::Sub => Value::Integer(self.builder.build_int_sub(l, r, "isubtmp").unwrap(), grouped, target_width),
            BinOp::Mul => Value::Integer(self.builder.build_int_mul(l, r, "imultmp").unwrap(), grouped, target_width),
            BinOp::Div => {
                self.notes.push(
                    QmclInfo::new("integer division here truncates any remainder (e.g. 7 / 2 = 3, not 3.5)")
                        .at(op_span),
                );
                Value::Integer(
                    self.builder.build_int_signed_div(l, r, "idivtmp").unwrap(),
                    grouped,
                    target_width,
                )
            }
            BinOp::Pow => {
                let lf = self.int_to_f64(l);
                let rf = self.int_to_f64(r);
                let pow_fn = self.pow_fn.expect("pow_fn set at start of compile_program");
                let result = match self
                    .builder
                    .build_call(pow_fn, &[lf.into(), rf.into()], "ipowtmp")
                    .unwrap()
                    .try_as_basic_value()
                {
                    inkwell::values::ValueKind::Basic(v) => v.into_float_value(),
                    inkwell::values::ValueKind::Instruction(_) => {
                        unreachable!("pow always returns a value, never void")
                    }
                };
                Value::Number(result, grouped, 64)
            }
            BinOp::Gt => {
                let cmp = self.builder.build_int_compare(IntPredicate::SGT, l, r, "igttmp").unwrap();
                Value::Number(
                    self.builder
                        .build_unsigned_int_to_float(cmp, self.context.f64_type(), "booltmp")
                        .unwrap(),
                    false,
                    64,
                )
            }
            BinOp::Lt => {
                let cmp = self.builder.build_int_compare(IntPredicate::SLT, l, r, "ilttmp").unwrap();
                Value::Number(
                    self.builder
                        .build_unsigned_int_to_float(cmp, self.context.f64_type(), "booltmp")
                        .unwrap(),
                    false,
                    64,
                )
            }
        }
    }

    /// `repeat 'i' from <start> to <end> [ body ].` — inclusive range,
    /// step +1, loop variable always a 64-bit integer. The first (and so
    /// far only) construct that needs real branching: LLVM basic blocks
    /// for the condition check, the body, and where control resumes after
    /// the loop ends.
    ///
    /// No lexical scoping exists yet, so `var_name` (and anything the body
    /// declares) is registered in the same flat `self.vars` table as
    /// everything else, and remains accessible — holding its final value —
    /// after the loop finishes.
    fn compile_counted_loop(
        &mut self,
        var_name: &str,
        var_name_span: Span,
        start: &Expr,
        end: &Expr,
        body: &[Stmt],
    ) {
        let main_fn = self.main_fn.expect("main_fn set at start of compile_program");
        let i64_ty = self.context.i64_type();

        let start_val = self.compile_expr(start);
        let start_i = self.require_integer(start_val, var_name_span, "a loop's 'from' bound");
        let end_val = self.compile_expr(end);
        let end_i = self.require_integer(end_val, var_name_span, "a loop's 'to' bound");

        // The loop variable is a plain 64-bit integer, stored like any
        // other declared variable — registering it here is what makes
        // (i) usable inside the body.
        let ptr = self.builder.build_alloca(i64_ty, var_name).unwrap();
        self.builder.build_store(ptr, start_i).unwrap();
        self.vars.insert(var_name.to_string(), (ptr, Type::Integer(64), false));

        let cond_bb = self.context.append_basic_block(main_fn, "loop_cond");
        let body_bb = self.context.append_basic_block(main_fn, "loop_body");
        let end_bb = self.context.append_basic_block(main_fn, "loop_end");

        self.builder.build_unconditional_branch(cond_bb).unwrap();

        // loop_cond: while current <= end, keep going.
        self.builder.position_at_end(cond_bb);
        let current = self.builder.build_load(i64_ty, ptr, var_name).unwrap().into_int_value();
        let keep_going = self
            .builder
            .build_int_compare(IntPredicate::SLE, current, end_i, "loopcond")
            .unwrap();
        self.builder.build_conditional_branch(keep_going, body_bb, end_bb).unwrap();

        // loop_body: run the body, then increment and jump back to the check.
        self.builder.position_at_end(body_bb);
        for stmt in body {
            self.compile_stmt(stmt);
        }
        let current = self.builder.build_load(i64_ty, ptr, var_name).unwrap().into_int_value();
        let one = i64_ty.const_int(1, false);
        let next = self.builder.build_int_add(current, one, "loopinc").unwrap();
        self.builder.build_store(ptr, next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        // Everything after the loop statement continues here.
        self.builder.position_at_end(end_bb);
    }
}
