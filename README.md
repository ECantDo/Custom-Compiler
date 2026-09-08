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
`u32`), raw pointers (`*mut u16`), and `store` / `fetch` builtins for RAM.

## Rough edges

- No function calls yet: `main` is expected to inline everything (`-Oz`
  does this for the samples).
- No stack frame — the register allocator errors out instead of spilling if a function needs more than the 16 pool
  registers.
- `caller` / `callee` save classes in `registers.cfg` are defined but unused (everything is parked in `callee`).
- The simulator loop sleeps between instructions and prints state to the terminal; it isn't set up for
  scripted/automated runs.
