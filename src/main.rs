use cpu_simulator_rs::assembler::build;
use cpu_simulator_rs::cpu::start_cpu_from_program;
use std::fs;
use std::fs::File;
use std::io::{BufWriter, Write};

/// Port of `Assembler.BuildToSchematic.main`.
///
/// NOTE (carried over from the original): `Build::build` already writes a
/// `.bin` file itself (with `0b` prefixes and trailing commas, one
/// instruction per line). This function then writes to the *same* `.bin`
/// path again, in a different format (no `0b` prefix, no commas, uses the
/// platform newline). Since this runs after `build()`, whatever this
/// function writes is what ends up on disk - the first write is
/// overwritten. That's true of the Java version too; I didn't invent or fix
/// this, just preserved it. Worth deciding whether that's intentional
/// before this file's contents matter to anything downstream.
fn main() {
	use cpu_simulator_rs::assembler::build;
	use cpu_simulator_rs::cpu::start_cpu_from_program;

	let file_name = "./compiled/fib.as";

	let file_contents = fs::read_to_string(file_name).unwrap();

	let program = match build::build(&file_contents) {
		Ok(program) => program,
		Err(e) => {
			eprintln!("Build failed: {e}");
			std::process::exit(1);
		}
	};

	// if let Err(e) = write_to_pure_binary(file_name, &program) {
	// 	eprintln!("Failed to write binary output: {e}");
	// 	std::process::exit(1);
	// }
	//
	// if let Err(e) = write_to_ascii_binary(file_name, &program) {
	// 	eprintln!("Failed to write ascii binary output: {e}");
	// 	std::process::exit(1);
	// }

	start_cpu_from_program(&program);

	// Build::runBuildToSchem(file_name + ".bin");
}

// fn main() {
// 	let mode = if cfg!(debug_assertions) { BuildMode::Dev } else { BuildMode::Release };
//
// 	let file_name = "fib";
//
// 	let compiled_program = compile_to_asm(&format!("./src/compiler/programs/{file_name}.ecc"), mode).unwrap();
//
//
// 	let program = match build::build(&compiled_program) {
// 		Ok(program) => program,
// 		Err(e) => {
// 			eprintln!("Build failed: {e}");
// 			std::process::exit(1);
// 		}
// 	};
//
// 	if let Err(e) = write_to_pure_binary(file_name, &program) {
// 		eprintln!("Failed to write binary output: {e}");
// 		std::process::exit(1);
// 	}
//
// 	if let Err(e) = write_to_ascii_binary(file_name, &program) {
// 		eprintln!("Failed to write ascii binary output: {e}");
// 		std::process::exit(1);
// 	}
//
// 	start_cpu_from_program(&program);
// }

/// Port of `BuildToSchematic.writeToAsciiBinary`.
fn write_to_ascii_binary(file: &str, program: &[u32]) -> std::io::Result<()> {
	let path = format!("{file}.abin");
	let mut writer = BufWriter::new(File::create(path)?);

	for &instruction in program {
		writeln!(writer, "{:032b}", instruction)?;
	}

	Ok(())
}

fn write_to_pure_binary(file: &str, program: &[u32]) -> std::io::Result<()> {
	let path = format!("{file}.bin");
	let mut writer = BufWriter::new(File::create(path)?);

	for &instruction in program {
		// Convert the 32-bit integer directly into an array of 4 raw bytes [u8; 4]
		// Use .to_le_bytes() for Little-Endian (x86/ARM/RISC-V)
		// or .to_be_bytes() for Big-Endian networks/VMs
		let raw_bytes = instruction.to_le_bytes();

		// Write the raw bytes directly to disk with no text formatting or newlines
		writer.write_all(&raw_bytes)?;
	}

	// Flush ensure everything is cleanly written to disk
	writer.flush()?;
	Ok(())
}
