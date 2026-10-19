use burst_groups::{registry::MAX_DIRECTORY_BYTES, GroupRegistry};
use std::{
    error::Error,
    fs::File,
    io::{Read, Write},
    path::PathBuf,
};

fn main() -> Result<(), Box<dyn Error>> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() != 2 {
        return Err(
            "usage: burst-group-directory INPUT.json OUTPUT.json (output must not exist)".into(),
        );
    }
    let input = PathBuf::from(&arguments[0]);
    let output = PathBuf::from(&arguments[1]);
    let mut bytes = Vec::new();
    File::open(input)?
        .take(MAX_DIRECTORY_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    let registry = GroupRegistry::from_directory(&bytes)?;
    let exported = registry.to_directory()?;
    let mut file = File::options().write(true).create_new(true).open(&output)?;
    file.write_all(&exported)?;
    file.sync_all()?;
    eprintln!(
        "Wrote {} groups to {}. Nothing was uploaded or trusted.",
        registry.list().len(),
        output.display()
    );
    Ok(())
}
