use crate::cpu::alu::CpuFlags;
use crate::cpu::io::IoPorts;
use crate::cpu::program_counter::ProgramCounter;
use crate::cpu::program_ram::ProgramRam;
use crate::cpu::program_rom::ProgramMemory;

mod alu;
mod program_counter;
mod program_rom;
mod program_ram;
mod registers;
mod io;

type Word = i16;
type UnsignedWord = u16;
type Long = i32;

type Instruction = i32;

pub const ROM_SIZE: usize = 256; // 8 bits
pub const RAM_SIZE: usize = 256;

struct CpuParts {
	alu_flags: CpuFlags,
	program_counter: ProgramCounter,
	program_memory: ProgramMemory,
	program_ram: ProgramRam,
	io_ports: IoPorts,
}

impl CpuParts {}

impl Default for CpuParts {
	fn default() -> Self {
		Self {
			alu_flags: CpuFlags::default(),
			program_counter: ProgramCounter::default(),
			program_memory: ProgramMemory::default(), // TODO, get file loading going to run programs
			program_ram: ProgramRam::default(),
			io_ports: IoPorts::new().unwrap(),
		}
	}
}