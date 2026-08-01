mod ast;
mod codegen;
mod error;
mod lexer;
mod parser;

use std::env;
use std::fs;
use std::path::Path;
use std::process::{exit, Command};

use inkwell::context::Context;
use inkwell::targets::{CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine};
use inkwell::OptimizationLevel;

use error::{QmclError, QmclInfo};

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

fn print_notes(notes: &[QmclInfo], path: &str, source: &str) {
    for n in notes {
        eprint!("{}", n.render(path, source));
    }
}

enum Task {
    Compile,
    Run,
}

struct Args {
    task: Task,
    file: String,
    output: Option<String>,
}

const USAGE: &str = "usage:\n  qmcl task:compile file:<path>.qmcl [outputfilename:<name>]\n  qmcl task:run file:<path>.qmcl [outputfilename:<name>]";

/// Parses `key:value` arguments (e.g. `task:compile`, `file:hello.qmcl`).
/// Splits on the *known key name plus its colon* as a fixed prefix, not on
/// the first colon anywhere in the argument — a naive first-colon split
/// would misparse a Windows path like `file:C:\Users\you\hello.qmcl`
/// (the drive letter's colon would get mistaken for the key/value
/// separator). Matching on `"file:"` as a whole prefix sidesteps that.
fn parse_args(raw: &[String]) -> Result<Args, String> {
    const KNOWN_KEYS: &[&str] = &["task", "file", "outputfilename"];

    let mut task: Option<String> = None;
    let mut file: Option<String> = None;
    let mut output: Option<String> = None;

    for arg in raw {
        let mut matched = false;
        for key in KNOWN_KEYS {
            let prefix = format!("{}:", key);
            if let Some(value) = arg.strip_prefix(prefix.as_str()) {
                match *key {
                    "task" => task = Some(value.to_string()),
                    "file" => file = Some(value.to_string()),
                    "outputfilename" => output = Some(value.to_string()),
                    _ => unreachable!(),
                }
                matched = true;
                break;
            }
        }
        if !matched {
            return Err(format!("unrecognized argument '{}'", arg));
        }
    }

    let task = match task.as_deref() {
        Some("compile") => Task::Compile,
        Some("run") => Task::Run,
        Some(other) => return Err(format!("unknown task '{}' — expected 'compile' or 'run'", other)),
        None => return Err("missing 'task:compile' or 'task:run'".to_string()),
    };
    let file = file.ok_or_else(|| "missing 'file:<path>.qmcl'".to_string())?;

    Ok(Args { task, file, output })
}

fn default_output_name(qmcl_path: &str) -> String {
    Path::new(qmcl_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("a")
        .to_string()
}

/// A bare filename (no '/') would make `Command::new` search $PATH instead
/// of running the binary we just built in the current directory — so a
/// bare name needs an explicit "./" to actually run it.
fn runnable_path(output_path: &str) -> String {
    if output_path.contains('/') {
        output_path.to_string()
    } else {
        format!("./{}", output_path)
    }
}

fn main() {
    let raw_args: Vec<String> = env::args().skip(1).collect();
    let args = parse_args(&raw_args).unwrap_or_else(|e| {
        eprintln!("qmcl: {}", e);
        eprintln!("{}", USAGE);
        exit(1);
    });

    let source = fs::read_to_string(&args.file).unwrap_or_else(|e| {
        eprintln!("qmcl: couldn't read '{}': {}", args.file, e);
        exit(1);
    });
    // Some editors (notably on Windows) save UTF-8 files with a leading
    // byte-order-mark. It's invisible but isn't whitespace, so left in
    // place it would glue itself onto the very first token and corrupt it.
    // Stripped once here (rather than inside the lexer) so every downstream
    // consumer, including error rendering, sees identical text — otherwise
    // column numbers would drift by one from what render() displays.
    let source = source.strip_prefix('\u{FEFF}').unwrap_or(&source).to_string();

    let tokens = lexer::Lexer::new(&source)
        .tokenize()
        .unwrap_or_else(|e| fail(e, &args.file, &source));

    let (program, parse_errors) = parser::Parser::new(tokens).parse_program();
    if !parse_errors.is_empty() {
        fail_all(&parse_errors, &args.file, &source);
    }

    let context = Context::create();
    let mut codegen = codegen::Codegen::new(&context, "qmcl_module");
    let (codegen_errors, notes) = codegen.compile_program(&program);
    print_notes(&notes, &args.file, &source);
    if !codegen_errors.is_empty() {
        fail_all(&codegen_errors, &args.file, &source);
    }

    let output_path = args
        .output
        .clone()
        .unwrap_or_else(|| default_output_name(&args.file));

    Target::initialize_native(&InitializationConfig::default()).unwrap_or_else(|e| {
        eprintln!("qmcl: failed to initialize the native target: {}", e);
        exit(1);
    });

    let triple = TargetMachine::get_default_triple();
    let cpu = TargetMachine::get_host_cpu_name().to_string();
    let features = TargetMachine::get_host_cpu_features().to_string();
    let target = Target::from_triple(&triple).unwrap_or_else(|e| {
        eprintln!("qmcl: failed to look up the native target: {}", e);
        exit(1);
    });
    let target_machine = target
        .create_target_machine(
            &triple,
            &cpu,
            &features,
            OptimizationLevel::None,
            RelocMode::Default,
            CodeModel::Default,
        )
        .unwrap_or_else(|| {
            eprintln!("qmcl: failed to create a target machine for '{}'", triple);
            exit(1);
        });

    let obj_path = format!("{}.o", output_path);
    target_machine
        .write_to_file(codegen.module(), FileType::Object, Path::new(&obj_path))
        .unwrap_or_else(|e| {
            eprintln!("qmcl: failed to emit object file: {}", e);
            exit(1);
        });

    // Link with the system C compiler — this is also how we get libc for
    // free, since printf comes from there.
    let link_result = Command::new("cc")
        .arg(&obj_path)
        .arg("-o")
        .arg(&output_path)
        .status();
    let _ = fs::remove_file(&obj_path);

    match link_result {
        Ok(status) if status.success() => {}
        Ok(status) => {
            eprintln!("qmcl: linking failed (exit code {:?})", status.code());
            exit(1);
        }
        Err(e) => {
            eprintln!("qmcl: couldn't run the system linker ('cc'): {}", e);
            exit(1);
        }
    }

    if let Task::Run = args.task {
        let run_status = Command::new(runnable_path(&output_path))
            .status()
            .unwrap_or_else(|e| {
                eprintln!("qmcl: couldn't run '{}': {}", output_path, e);
                exit(1);
            });
        exit(run_status.code().unwrap_or(1));
    }
}
