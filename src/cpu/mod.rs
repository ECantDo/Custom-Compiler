use crate::assembler::operations::opcodes::OPCODE_MAP;
use crate::cpu::alu::{run_alu_operation, CpuFlags, UPPER_OP_MASK};
use crate::cpu::cpu_io::IoPorts;
use crate::cpu::instruction_parser::{parse_instruction, InstructionInfo};
use crate::cpu::program_counter::ProgramCounter;
use crate::cpu::program_ram::ProgramRam;
use crate::cpu::program_rom::ProgramMemory;
use crate::cpu::registers::Registers;
use std::io::{self, Write};
use std::thread;
use std::time::Duration;

mod alu;
mod program_counter;
mod program_rom;
mod program_ram;
mod registers;
mod cpu_io;
mod instruction_parser;

type Word = i16;
type UnsignedWord = u16;
type Long = i32;

type Instruction = u32;

pub const ROM_SIZE: usize = 256; // 8 bits
pub const RAM_SIZE: usize = 256;

struct CpuParts {
	cpu_flags: CpuFlags,
	program_counter: ProgramCounter,
	program_memory: ProgramMemory,
	program_ram: ProgramRam,
	io_ports: IoPorts,
	registers: Registers,
}

impl CpuParts {}

impl Default for CpuParts {
	fn default() -> Self {
		Self {
			cpu_flags: CpuFlags::default(),
			program_counter: ProgramCounter::default(),
			program_memory: ProgramMemory::default(), // TODO, get file loading going to run programs
			program_ram: ProgramRam::default(),
			io_ports: IoPorts::new().unwrap(),
			registers: Registers::default(),
		}
	}
}

pub fn start_cpu_from_file(file_name: &str) {
	let mut parts = CpuParts::default();
	parts.program_memory = ProgramMemory::read_from_file(file_name).unwrap();
	cpu_cycle(&mut parts);
}

pub fn start_cpu_from_program(program: &[u32]) {
	let mut parts = CpuParts::default();
	parts.program_memory = ProgramMemory::from_program(program);
	cpu_cycle(&mut parts);
}

/// Execute one already-decoded instruction (everything except fetch, the
/// `hlt` check, and any per-cycle bookkeeping). Shared by the interactive
/// loop and [`run_headless`].
fn execute(cpu_parts: &mut CpuParts, info: &InstructionInfo) {
		match info.opcode & !UPPER_OP_MASK {
			1 | 2 | 3 => {
				// ALU, shifter, ADDF
				let val1 = cpu_parts.registers.get(info.reg1);
				let val2 = cpu_parts.registers.get(info.reg2);

				let result = run_alu_operation(info.opcode, val1, val2, &mut cpu_parts.cpu_flags);
				cpu_parts.registers.set(info.write_back_reg, result);
			}
			17 | 18 => {
				// ALU, shifter => with imm
				let val1 = cpu_parts.registers.get(info.reg1);

				let result = run_alu_operation(info.opcode, val1, info.immediate, &mut cpu_parts.cpu_flags);
				cpu_parts.registers.set(info.write_back_reg, result);
			}
			4 => {
				// Branch
				// Perform math
				let val1 = cpu_parts.registers.get(info.reg1);
				let val2 = cpu_parts.registers.get(info.reg2);

				run_alu_operation(OPCODE_MAP.get("sub").unwrap().1 as u8, val1, val2, &mut cpu_parts.cpu_flags);

				if branch_taken((info.opcode & UPPER_OP_MASK) >> 5, &cpu_parts.cpu_flags) {
					cpu_parts.program_counter.add(info.immediate);
				}
			}
			25 => {
				cpu_parts.registers.set(info.write_back_reg, info.immediate);
			}
			7 => {
				let addr = cpu_parts.registers.get(info.write_back_reg) + info.immediate;
				if (info.opcode & UPPER_OP_MASK) >> 5 == 0 {
					// str
					let value = cpu_parts.registers.get(info.reg1); // This seems dumb
					cpu_parts.program_ram.set(addr as UnsignedWord, value);
				} else {
					// lod
					cpu_parts.registers.set(info.reg1,
					                        cpu_parts.program_ram.get(addr as UnsignedWord));
				}
			}
			5 => {
				cpu_parts.registers.set(info.write_back_reg, cpu_parts.program_counter.get() as Word);
				cpu_parts.program_counter.set(info.immediate as UnsignedWord);
			}
			6 => {
				cpu_parts.registers.set(info.write_back_reg, cpu_parts.program_counter.get() as Word);
				cpu_parts.program_counter.set(
					(info.immediate + cpu_parts.registers.get(info.reg1)) as UnsignedWord
				);
			}
			8 => {
				let port = info.immediate as UnsignedWord;
				if info.opcode >> 5 == 0 {
					let val_in = cpu_parts.io_ports.input(port);
					cpu_parts.registers.set(info.write_back_reg, val_in);
				} else {
					cpu_parts.io_ports.output(
						cpu_parts.registers.get(info.reg1) as UnsignedWord, port,
					);
				}
			}
			_ => {
				panic!("Unknown function -> {}", info.opcode);
			}
		}
}

fn cpu_cycle(cpu_parts: &mut CpuParts) {
	println!("Program Started");
	let mut running = true;
	let mut step: usize = 0;

	while running {
		let instruction: Instruction = cpu_parts.program_memory.fetch(cpu_parts.program_counter.get());
		cpu_parts.program_counter.increment();

		let info: InstructionInfo = parse_instruction(instruction);

		if info.opcode == 0 {
			running = false;
			continue;
		}

		execute(cpu_parts, &info);

		print!("{:4} : {} : S{}\r", cpu_parts.program_counter.get(), cpu_parts.registers, step);
		io::stdout().flush().unwrap();
		step += 1;

		let sleep_time = time::Duration::milliseconds(200);
		thread::sleep(Duration::try_from(sleep_time).unwrap());
	}
	println!("\n{}", cpu_parts.program_ram)
}

/// Run `program` with no IO devices, no delay, and no terminal output,
/// until `hlt` or `max_steps` instructions - then return a snapshot of RAM
/// indexed by address. Intended for tests and scripted runs.
pub fn run_headless(program: &[u32], max_steps: usize) -> Vec<Word> {
	let mut parts = CpuParts {
		cpu_flags: CpuFlags::default(),
		program_counter: ProgramCounter::default(),
		program_memory: ProgramMemory::from_program(program),
		program_ram: ProgramRam::default(),
		io_ports: IoPorts::empty(),
		registers: Registers::default(),
	};

	for _ in 0..max_steps {
		let instruction = parts.program_memory.fetch(parts.program_counter.get());
		parts.program_counter.increment();
		let info = parse_instruction(instruction);
		if info.opcode == 0 {
			break;
		}
		execute(&mut parts, &info);
	}

	(0..RAM_SIZE as UnsignedWord).map(|a| parts.program_ram.get(a)).collect()
}

fn branch_taken(condition: u8, flags: &CpuFlags) -> bool {
	match condition {
		0 => flags.is_zero,                              // BEQ:  a - b == 0
		1 => !flags.is_zero,                              // BNE:  a - b != 0
		2 => flags.sign_flag != flags.overflow_flag,      // BLT:  signed a < b
		3 => flags.sign_flag == flags.overflow_flag,      // BGE:  signed a >= b
		4 => flags.carry_flag,                            // BLTU: unsigned a < b (borrow occurred)
		5 => !flags.carry_flag,                           // BGEU: unsigned a >= b (no borrow)
		_ => false,
	}
}