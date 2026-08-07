use std::fs::File;
use std::io::{BufWriter, Write};

use cpu_simulator_rs::assembler::build;

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
    let file_name = "src/Programs_V2_0/fib";

    let program = match build::build(&format!("{file_name}.as")) {
        Ok(program) => program,
        Err(e) => {
            eprintln!("Build failed: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = write_to_pure_binary(file_name, &program) {
        eprintln!("Failed to write binary output: {e}");
        std::process::exit(1);
    }

	if let Err(e) = write_to_ascii_binary(file_name, &program) {
		eprintln!("Failed to write ascii binary output: {e}");
		std::process::exit(1);
	}

    // Build::runBuildToSchem(file_name + ".bin");
}

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
