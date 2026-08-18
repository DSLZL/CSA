use crate::error::{ManagerError, Result};
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

pub fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn sha256_file(path: &Path) -> Result<(String, u64)> {
    let file = File::open(path)
        .map_err(|error| ManagerError::io(&format!("open {}", path.display()), error))?;
    let size = file
        .metadata()
        .map_err(|error| ManagerError::io(&format!("stat {}", path.display()), error))?
        .len();
    let mut reader = BufReader::new(file);
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| ManagerError::io(&format!("hash {}", path.display()), error))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok((format!("{:x}", digest.finalize()), size))
}

pub fn sha256_os_str(value: &OsStr) -> String {
    sha256_bytes(value.to_string_lossy().as_bytes())
}
