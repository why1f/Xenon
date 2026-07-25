use sha2::{Digest, Sha256};

pub fn sha256_hex(value: &[u8]) -> String {
    use std::fmt::Write;
    Sha256::digest(value)
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
            output
        })
}
