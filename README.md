# A Custom Compiler

A toy compiler and CPU simulator for a homemade 16‑bit machine.

You write a small Rust‑like language (`.ecc`), it's lowered to LLVM IR, optimized, then turned into assembly for a
custom RISC‑V‑flavored ISA. A matching assembler produces a machine‑code image, and a bundled simulator can run it.

Importantly, I am trying to design the language around the idea that the programs it is going to produce is for CPUs
built within Minecraft. The key difference here is that Minecraft computers typically have very little memory to store
programs (compared to our Multi-Gigabyte drives we use day-to-day), some do have more than others. Much of what we would
expect to see with any "modern" programming language has the assumption that there are other programs to run on the
device, here we are not constrained to that - as most CPUs are programed with a single program.

> Status: work in progress. It compiles the sample programs end to end, but
> the language, ISA, and codegen are all still moving. See
> [Rough edges](#rough-edges).

## Pipeline

```
.ecc source
  │  lexer / parser / typecheck      src/compiler/{lexer,parser,typecheck}.rs
  ▼
LLVM IR (via inkwell)                src/compiler/codegen.rs
  │  opt passes (default<Oz>)
  ▼
register allocation                  src/compiler/register_allocation/
  │  graph colouring over the interference graph
  ▼
target assembly (.as)                src/compiler/asmgen.rs
  │  assembler                       src/assembler/
  ▼
machine code (.bin / .abin)          src/main.rs
  │
  ▼
simulator                            src/cpu/
```

## Layout

| Path                     | What                                                               |
|--------------------------|--------------------------------------------------------------------|
| `src/compiler/`          | front end, LLVM lowering, register allocation, assembly generation |
| `src/compiler/programs/` | sample `.ecc` programs (`fib`, `std_lib`)                          |
| `src/assembler/`         | text assembly → machine code, ISA opcode/register tables           |
| `src/cpu/`               | the simulator (ALU, registers, RAM/ROM, I/O, screen)               |
| `registers.cfg`          | target register file description read by the allocator             |
| `src/main.rs`            | `build_to_schematic` binary: compile a sample and emit binaries    |

## Build & run

Requires LLVM 18 (the `inkwell` `llvm18-1` feature).

```sh
cargo build
cargo test
cargo run          # compiles src/compiler/programs/std_lib.ecc
```

`cargo run` writes `compiled/std_lib.ll` (IR), `compiled/std_lib.as`
(assembly), and `compiled/bin/std_lib.{bin,abin}` (machine code). Change the
`file_name` in `src/main.rs` to compile a different program; uncomment the
`start_cpu_from_program` call to run it on the simulator.

## The machine it's programed for

- 16‑bit word, 256 words of ROM, 256 words of RAM
- 32 registers, RISC‑V names (`zero`, `ra`, `sp`, `a0..a7`, `s0..s11`, `t0..t6`)
- opcode and register tables live in `src/assembler/operations/`

`registers.cfg` tells the allocator which registers it may use. Right now the allocatable pool is `s0..s11, t0..t3`;
`t4..t6` are reserved as codegen scratch, and `a*` stay out of the pool until calls and a stack frame exist.

## The language (`.ecc`)

Rust‑like surface syntax. Roughly:

```rust
fn fib(fib_num: i16) -> u16 {
	let mut a: u16 = 1;
	let mut b = 0;
	let mut count = 0;

	while count < fib_num {
		let tmp = a + b;
		b = a;
		a = tmp;
		count += 1;
	}
	store(1, a);   // write `a` to RAM address 1
	return a;
}
```

`fn`, `let` / `let mut`, `const`, `if` / `else`, `while`, `break` /
`continue`, `return`, `as` casts, integer types (`i16`, `u16`, `i32`,
`u32`), raw pointers (`*mut u16`), the operators `+ - * / % & | ^ << >>`
plus comparisons and `&&` / `||`, `store` / `fetch` builtins for RAM, and
`port_out(port, value)` / `port_in(port) -> i16` builtins for IO ports
(`port` must be a constant `0..=15`; they lower to the ISA's `out` / `in`).
See `src/compiler/programs/io_ports.ecc` and `arith.ecc`.

The CPU has no multiplier or divider, so `*` / `/` / `%` are lowered in
`asmgen` (`mod muldiv`): `* const` becomes a shift/add chain, `* var`
a shift-and-add loop, `/` and `%` a 16-step restoring-division loop
(`/ 2^k` unsigned shortcuts to a shift). Magic-number division for
constant divisors (skipping the loop) is a possible follow-up.

`i32` / `u32` work too (`mod i32bit`). Wide values don't get registers —
each lives in a pair of RAM scratch slots just under `sp`, and every op is
lowered word-at-a-time: `add`/`sub` use the `addf`/`subf` carry chain,
shifts juggle the two words (the shifter is only 4-bit), `*` is a 32-step
shift-add, `/` and `%` a 32-step restoring division, comparisons test the
high word then fall through to the low. `-Oz` narrows `i32` to `i16`
wherever the top half is dead, so this only kicks in for arithmetic that
genuinely needs 32 bits (fixed-point multiplies, wide accumulators). Wider
ints (`i64`, only from LLVM's own pointer-index math) still just use their
low 16 bits.

## Rough edges

- No function calls in the emitted code. In a program (a module with
  `main`) every other `fn` is forced `alwaysinline` + `internal`, so `-Oz`
  inlines them into `main` and drops the originals. A library module (no
  `main`, e.g. the stdlib) keeps its functions as-is. A call that somehow
  survives to asmgen is an error — only `port_out` / `port_in` are lowered
  at a call site.
- 32-bit scratch slots are a fixed RAM block (`~176..254`); a function
  with dozens of simultaneously-live `i32` temporaries will run out.
- A loop that never terminates *and* has no `port_out` / `store` in it is
  UB to `-Oz`, which collapses it (and everything after) to nothing.
  asmgen detects the collapsed form and errors instead of emitting a
  silent spin loop — but the fix is in your code (usually a missing
  `i += 1`).
- The `run_headless` CPU runner (used by the tests) executes with no IO
  devices and no delay; the interactive `cpu_cycle` still sleeps and
  prints per instruction.
- No stack frame — the register allocator errors out instead of spilling if a function needs more than the 16 pool
  registers.
- `caller` / `callee` save classes in `registers.cfg` are defined but unused (everything is parked in `callee`).
- The simulator loop sleeps between instructions and prints state to the terminal; it isn't set up for
  scripted/automated runs.
