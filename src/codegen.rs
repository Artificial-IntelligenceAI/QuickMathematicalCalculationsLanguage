use std::collections::HashMap;

use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::values::{BasicMetadataValueEnum, FunctionValue, IntValue, PointerValue};
use inkwell::AddressSpace;

use crate::ast::*;
use crate::error::QmclError;

pub struct Codegen<'ctx> {
    context: &'ctx Context,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    vars: HashMap<String, PointerValue<'ctx>>,
}

impl<'ctx> Codegen<'ctx> {
    pub fn new(context: &'ctx Context, module_name: &str) -> Self {
        Codegen {
            context,
            module: context.create_module(module_name),
            builder: context.create_builder(),
            vars: HashMap::new(),
        }
    }

    pub fn module(&self) -> &Module<'ctx> {
        &self.module
    }

    /// QMCL programs have no `fn main`-equivalent — top-level statements just
    /// run in order. This emits them straight into a single LLVM `main`
    /// function, which is compiler plumbing the programmer never writes.
    pub fn compile_program(&mut self, program: &Program) -> Result<(), QmclError> {
        let i8_ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i32_ty = self.context.i32_type();
        let printf_ty = i32_ty.fn_type(&[i8_ptr_ty.into()], true);
        let printf_fn = self.module.add_function("printf", printf_ty, None);

        let main_ty = i32_ty.fn_type(&[], false);
        let main_fn = self.module.add_function("main", main_ty, None);
        let entry = self.context.append_basic_block(main_fn, "entry");
        self.builder.position_at_end(entry);

        for stmt in program {
            self.compile_stmt(stmt, printf_fn)?;
        }

        self.builder
            .build_return(Some(&i32_ty.const_int(0, false)))
            .unwrap();
        Ok(())
    }

    fn compile_stmt(&mut self, stmt: &Stmt, printf_fn: FunctionValue<'ctx>) -> Result<(), QmclError> {
        match stmt {
            Stmt::Declare { name, ty: Type::Number, value } => {
                let val = self.compile_expr(value)?;
                let i64_ty = self.context.i64_type();
                let ptr = self.builder.build_alloca(i64_ty, name).unwrap();
                self.builder.build_store(ptr, val).unwrap();
                self.vars.insert(name.clone(), ptr);
                Ok(())
            }
            Stmt::Print { parts } => self.compile_print(parts, printf_fn),
        }
    }

    fn compile_print(&mut self, parts: &[PrintPart], printf_fn: FunctionValue<'ctx>) -> Result<(), QmclError> {
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
                    fmt.push_str("%lld");
                    args.push(self.compile_expr(expr)?.into());
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
        Ok(())
    }

    fn compile_expr(&mut self, expr: &Expr) -> Result<IntValue<'ctx>, QmclError> {
        match expr {
            Expr::NumberLiteral(n) => Ok(self.context.i64_type().const_int(*n as u64, true)),
            Expr::Var(name, span) => {
                let ptr = *self.vars.get(name).ok_or_else(|| {
                    QmclError::new(format!("undeclared variable '{}'", name))
                        .at(*span)
                        .rule("a variable must be declared before it's referenced")
                        .suggest(format!(
                            "declare it first, e.g. declare '{}' = number '...'.",
                            name
                        ))
                })?;
                let i64_ty = self.context.i64_type();
                Ok(self
                    .builder
                    .build_load(i64_ty, ptr, name)
                    .unwrap()
                    .into_int_value())
            }
        }
    }
}
