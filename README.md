# QMCL — Quick Mathematical Calculations Language

A small, math-focused programming language that compiles ahead-of-time to real native binaries via LLVM. Written in Rust.

```
declare 'x' = number '1000'.
print["Total: " (x)].
```

## What this is

QMCL is a hobby language project: lexer → parser → LLVM codegen → a real linked executable, with no interpreter or bytecode VM in the loop. It's built around a deliberately distinctive syntax (quoted names/literals, `[]` for calls, `(x)` to dereference a variable) and a real diagnostic system rather than raw compiler panics.

## Building

Requires:
- A Rust toolchain (stable, edition 2024)
- LLVM 22 (`brew install llvm` on macOS — `llvm-sys` auto-detects Homebrew's keg-only install)
- A system C compiler (`cc`) — used to link the final executable, and for libc (`printf`, `pow`, `setlocale`)

```bash
cargo build --release
```

## Running a program

```bash
./target/release/QuickMathematicalCalculationsLanguage task:run file:examples/hello.qmcl outputfilename:hello
```

Arguments are `key:value` pairs, matched by each key's full prefix (so a value can safely contain a colon, e.g. a Windows-style path):

| Task | Behavior |
|---|---|
| `task:compile` | Build only — leaves a standalone binary, doesn't run it |
| `task:run` | Build and run immediately |
| `task:temprun` | Build, run, then delete the binary |

`file:<path>.qmcl` is required. `outputfilename:<name>` is optional — defaults to the input filename with `.qmcl` stripped.

For convenience, symlink the binary somewhere on your `PATH`:
```bash
ln -s "$(pwd)/target/release/QuickMathematicalCalculationsLanguage" /opt/homebrew/bin/qmcl
qmcl task:run file:examples/hello.qmcl outputfilename:hello
```
(Rebuild the release binary after any compiler change — the symlink always points at whatever's currently built.)

## Language tour

### Declaring variables

```
declare '<name>' = <type> <value>.
```

| Type | Notes |
|---|---|
| `number` / `number:16` / `number:32` / `number:64` | Floating point (default 64-bit). Supports decimals, negatives, thousands separators. |
| `integer` / `integer:8` / `integer:16` / `integer:32` / `integer:64` | True fixed-width integers (default 64-bit) — real overflow semantics, not floats. |
| `string` | Text, e.g. `string 'Hello, World!'` |
| `boolean` | `boolean 'true'` or `boolean 'false'` |
| `percentage` | `percentage '100%'` or `percentage '100'` (equivalent) — stored normalized as a fraction, redisplayed with `%` on print |

### Printing

```
print["some text " (variable) " more text"].
```

- `"..."` — literal text. Can span multiple raw lines, and supports escapes: `\'`, `\"`, `\\`, `\n`, `\t`.
- `(x)` — dereferences a variable's value. This is the general "give me the value" syntax everywhere, not print-specific.
- Adjacent parts (text and values) concatenate automatically — no operator needed.

### Operators

`+`  `-`  `*`  `/` (or `÷`)  `^` (or `**`)  `>`  `<`

- Precedence, loosest to tightest: comparison → additive (`+`/`-`) → multiplicative (`*`/`/`) → power (`^`).
- Numeric literals accept thousands separators: `'1,000,000'`.
- Mixing types/widths in arithmetic (e.g. `number:16` with `number:64`, or an `integer` with a `number`) auto-promotes rather than erroring — but always reported via the Informer (see below), never silently.
- Integer `/` truncates (`7 / 2 = 3`); mixing a string or boolean into arithmetic is a compile error.

### Identifiers

Variable and function names can include emoji and `?`.

## Diagnostics

Two named subsystems, both compile-time:

- **QMCL Error Handler** — what went wrong, where (file/line/column plus a source snippet with a caret), the general rule broken, and a concrete suggested fix. Reports every error found in one pass, not just the first.
- **QMCL Informer** — separate, non-blocking notices for things that are fine but worth knowing about: a variable being redeclared, mixed-precision arithmetic auto-promoting, integer division truncating.

## Project status

Actively evolving hobby project. Currently implemented: `declare`/`print`, arithmetic and comparison operators, five value types (number, integer, string, boolean, percentage) with real width/precision control, and full AOT compilation to standalone native executables.

Not yet implemented: loops, functions, grouping parentheses (`(...)` is reserved for variable dereference), bignum.

## License

Apache 2.0 — see [LICENSE](LICENSE).
