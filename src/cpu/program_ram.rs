use crate::cpu::{UnsignedWord, Word, RAM_SIZE};
use std::fmt;

pub struct ProgramRam {
	ram: Box<[Word; RAM_SIZE]>,
}

impl Default for ProgramRam {
	fn default() -> Self {
		Self {
			ram: Box::new([0; RAM_SIZE])
		}
	}
}

impl ProgramRam {
	pub fn set(&mut self, index: UnsignedWord, value: Word) {
		self.ram[index as usize] = value;
	}

	pub fn get(&self, index: UnsignedWord) -> Word {
		self.ram[index as usize]
	}
}

impl fmt::Display for ProgramRam {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{:?}", self.ram)
	}
}