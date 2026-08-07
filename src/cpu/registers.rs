use crate::cpu::Word;

pub struct Registers {
	registers: Box<[Word; Self::NUM]>,
}

impl Default for Registers {
	fn default() -> Self {
		Self {
			registers: Box::new([0; Self::NUM])
		}
	}
}

impl Registers {
	const NUM: usize = 32; // Not in `mod` since this should not be changing; RAM and ROM can get away with more dynamic

	pub fn set(&mut self, index: u8, value: Word) {
		self.registers[index as usize] = value;
	}

	pub fn get(&self, index: u8) -> Word {
		self.registers[index as usize]
	}
}