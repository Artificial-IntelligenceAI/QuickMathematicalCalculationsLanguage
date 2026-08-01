mod ast;
mod codegen;
mod lexer;
mod parser;

use std::env;
use std::fs;
use std::process::exit;

use inkwell::context::Context;
use inkwell::execution_engine::JitFunction;
use inkwell::OptimizationLevel;

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

    let tokens = lexer::Lexer::new(&source).tokenize().unwrap_or_else(|e| {
        eprintln!("qmcl: lex error: {}", e);
        exit(1);
    });

    let program = parser::Parser::new(tokens)
        .parse_program()
        .unwrap_or_else(|e| {
            eprintln!("qmcl: parse error: {}", e);
            exit(1);
        });

    let context = Context::create();
    let mut codegen = codegen::Codegen::new(&context, "qmcl_module");
    codegen.compile_program(&program);

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
