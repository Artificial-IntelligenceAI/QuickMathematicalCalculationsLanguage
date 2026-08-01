mod ast;
mod codegen;
mod error;
mod lexer;
mod parser;

use std::env;
use std::fs;
use std::process::exit;

use inkwell::context::Context;
use inkwell::execution_engine::JitFunction;
use inkwell::OptimizationLevel;

use error::QmclError;

fn fail(err: QmclError, path: &str, source: &str) -> ! {
    eprint!("{}", err.render(path, source));
    exit(1);
}

/// Prints every error found in one pass, then exits — used whenever a stage
/// can report more than one problem instead of stopping at the first.
fn fail_all(errors: &[QmclError], path: &str, source: &str) -> ! {
    for e in errors {
        eprint!("{}", e.render(path, source));
    }
    eprintln!(
        "{} error{} found.",
        errors.len(),
        if errors.len() == 1 { "" } else { "s" }
    );
    exit(1);
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let Some(path) = args.get(1) else {
        eprintln!("usage: qmcl <file.qmcl>");
        exit(1);
    };

    let source = fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("qmcl: couldn't read '{}': {}", path, e);
        exit(1);
    });

    let tokens = lexer::Lexer::new(&source)
        .tokenize()
        .unwrap_or_else(|e| fail(e, path, &source));

    let (program, parse_errors) = parser::Parser::new(tokens).parse_program();
    if !parse_errors.is_empty() {
        fail_all(&parse_errors, path, &source);
    }

    let context = Context::create();
    let mut codegen = codegen::Codegen::new(&context, "qmcl_module");
    let codegen_errors = codegen.compile_program(&program);
    if !codegen_errors.is_empty() {
        fail_all(&codegen_errors, path, &source);
    }

    let execution_engine = codegen
        .module()
        .create_jit_execution_engine(OptimizationLevel::None)
        .unwrap_or_else(|e| {
            eprintln!("qmcl: failed to set up JIT: {}", e);
            exit(1);
        });

    unsafe {
        type MainFn = unsafe extern "C" fn() -> i32;
        let main_fn: JitFunction<MainFn> = execution_engine
            .get_function("main")
            .expect("codegen did not produce a main function");
        main_fn.call();
    }
}
