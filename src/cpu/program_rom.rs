use crate::cpu::{Instruction, UnsignedWord, ROM_SIZE};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};

pub struct ProgramMemory {
	program_memory: Box<[Instruction; ROM_SIZE]>,
}

impl Default for ProgramMemory {
	fn default() -> Self {
		Self {
			program_memory: Box::new([0; ROM_SIZE]),
		}
	}
}

impl ProgramMemory {
	pub fn from_program(program: &[u32]) -> Self {
		let mut memory = Self::default();
		for (i, &instruction) in program.iter().enumerate() {
			if i >= ROM_SIZE {
				panic!("Program size is larger than the ROM size");
			}
			memory.program_memory[i] = instruction as Instruction;
		}

		memory
	}

	/// Takes in any file (with a type) and loads it, uses the `.bin` or `.abin` file types seen
	/// below
	pub fn read_from_file(file_name: &str) -> io::Result<Self> {
		let file_type_idx = file_name.rfind('.').unwrap_or(0);

		if file_type_idx == 0 {
			panic!("No filetype found or no filename found: {}", file_name);
		}

		let file_extension = &file_name[file_type_idx..];
		let file = &file_name[..file_type_idx];

		match file_extension {
			".bin" => { Self::read_from_bin_file(file) }
			".abin" => { Self::read_from_ascii_file(file) }
			_ => { panic!("Unrecognized filetype: {}", file_extension) }
		}
	}


	/// Reads a `.bin` file (raw little-endian 4-byte instructions, no separators)
	/// and builds a ProgramMemory from it.
	pub fn read_from_bin_file(file: &str) -> io::Result<Self> {
		let program: Vec<u32> = read_pure_binary(file)?;
		Ok(Self::from_program(&program))
	}

	/// Reads a `.abin` file (one 32-char '0'/'1' line per instruction) and
	/// builds a ProgramMemory from it. Handy for debugging/inspecting a ROM
	/// dump by eye without needing a hex viewer.
	pub fn read_from_ascii_file(file: &str) -> io::Result<Self> {
		let program = read_ascii_binary(file)?;
		Ok(Self::from_program(&program))
	}

	pub fn fetch(&self, address: UnsignedWord) -> Instruction {
		let address = address as usize;
		assert!(address < ROM_SIZE);
		self.program_memory[address]
	}
}

fn read_pure_binary(file: &str) -> io::Result<Vec<u32>> {
	let path = format!("{file}.bin");
	let mut reader = BufReader::new(File::open(path)?);

	let mut bytes = Vec::new();
	reader.read_to_end(&mut bytes)?;

	if bytes.len() % 4 != 0 {
		return Err(io::Error::new(
			io::ErrorKind::InvalidData,
			"file length is not a multiple of 4 bytes",
		));
	}

	let program = bytes
		.chunks_exact(4)
		.map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
		.collect();

	Ok(program)
}

fn read_ascii_binary(file: &str) -> io::Result<Vec<u32>> {
	let path = format!("{file}.abin");
	let reader = BufReader::new(File::open(path)?);

	let mut program = Vec::new();
	for line in reader.lines() {
		let line = line?;
		let bits = line.trim();
		if bits.is_empty() {
			continue;
		}
		let value = u32::from_str_radix(bits, 2)
			.map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
		program.push(value);
	}

	Ok(program)
}