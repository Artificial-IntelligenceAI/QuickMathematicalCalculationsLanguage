use std::collections::HashMap;

use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::values::{BasicMetadataValueEnum, FloatValue, FunctionValue, PointerValue};
use inkwell::AddressSpace;

use crate::ast::*;
use crate::error::{QmclError, QmclInfo};

pub struct Codegen<'ctx> {
    context: &'ctx Context,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    vars: HashMap<String, PointerValue<'ctx>>,
    /// Collected instead of aborting at the first one, so a single run can
    /// report every problem in the file, not just the first.
    errors: Vec<QmclError>,
    /// Notable-but-not-wrong things worth telling the programmer about,
    /// e.g. a variable being shadowed. Never blocks compilation.
    notes: Vec<QmclInfo>,
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

        let main_ty = i32_ty.fn_type(&[], false);
        let main_fn = self.module.add_function("main", main_ty, None);
        let entry = self.context.append_basic_block(main_fn, "entry");
        self.builder.position_at_end(entry);

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
            Stmt::Declare { name, name_span, ty: Type::Number, value } => {
                let val = self.compile_expr(value);
                if self.vars.contains_key(name) {
                    self.notes.push(
                        QmclInfo::new(format!(
                            "'{}' is being redeclared — its previous value is no longer accessible",
                            name
                        ))
                        .at(*name_span),
                    );
                }
                let f64_ty = self.context.f64_type();
                let ptr = self.builder.build_alloca(f64_ty, name).unwrap();
                self.builder.build_store(ptr, val).unwrap();
                self.vars.insert(name.clone(), ptr);
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
                    // %.15g: full double precision, but trims trailing
                    // zeros so a whole number like 1000.0 prints as "1000"
                    // rather than "1000.000000000000".
                    fmt.push_str("%.15g");
                    args.push(self.compile_expr(expr).into());
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
    fn compile_expr(&mut self, expr: &Expr) -> FloatValue<'ctx> {
        match expr {
            Expr::NumberLiteral(n) => self.context.f64_type().const_float(*n),
            Expr::Var(name, span) => match self.vars.get(name) {
                Some(ptr) => {
                    let f64_ty = self.context.f64_type();
                    self.builder
                        .build_load(f64_ty, *ptr, name)
                        .unwrap()
                        .into_float_value()
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
                    self.context.f64_type().const_float(0.0)
                }
            },
        }
    }
}
