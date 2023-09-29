use std::{
    path::PathBuf,
    fs::{File, self},
    io::{self, Write},
};

pub fn run(path: PathBuf, ledger_bytes: &[u8], blocks: impl Iterator<Item = impl io::Read>) {
    fs::create_dir_all(&path).unwrap_or_default();

    let mut ledger_file = File::create(path.join("ledger")).unwrap();
    ledger_file.write_all(ledger_bytes).unwrap();

    let mut blocks_file = File::create(path.join("blocks")).unwrap();
    for mut transitions in blocks {
        io::copy(&mut transitions, &mut blocks_file).unwrap();
    }
}
