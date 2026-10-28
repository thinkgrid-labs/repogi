//! Independent benchmark oracle: hashes stdin bytes, without Atlas code.
use std::io::{self, Read};

fn main() -> io::Result<()> {
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let count = io::stdin().read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    println!("blake3:{}", hasher.finalize().to_hex());
    Ok(())
}
