use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;

/// Standard FIPS 180-4 SHA-256 implementation
pub struct Sha256 {
    state: [u32; 8],
    count: u64,
    buffer: [u8; 64],
}

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

impl Sha256 {
    pub fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            count: 0,
            buffer: [0u8; 64],
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        let buf_idx = (self.count as usize) & 63;
        self.count += data.len() as u64;

        if buf_idx > 0 {
            let space = 64 - buf_idx;
            if data.len() < space {
                self.buffer[buf_idx..buf_idx + data.len()].copy_from_slice(data);
                return;
            }
            self.buffer[buf_idx..64].copy_from_slice(&data[..space]);
            Self::transform(&mut self.state, &self.buffer);
            data = &data[space..];
        }

        while data.len() >= 64 {
            let block: &[u8; 64] = data[..64].try_into().unwrap();
            Self::transform(&mut self.state, block);
            data = &data[64..];
        }

        if !data.is_empty() {
            self.buffer[..data.len()].copy_from_slice(data);
        }
    }

    fn transform(state: &mut [u32; 8], block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(block[i * 4..(i + 1) * 4].try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut a = state[0];
        let mut b = state[1];
        let mut c = state[2];
        let mut d = state[3];
        let mut e = state[4];
        let mut f = state[5];
        let mut g = state[6];
        let mut h = state[7];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    pub fn finalize(mut self) -> [u8; 32] {
        let bit_len = self.count * 8;
        // Append single 1 bit (0x80)
        self.update(&[0x80]);

        // Pad with zeros until buffer length is 56 mod 64
        while (self.count as usize & 63) != 56 {
            self.update(&[0x00]);
        }

        // Append 64-bit big-endian length (8 bytes)
        let len_bytes = bit_len.to_be_bytes();
        self.update(&len_bytes);

        let mut digest = [0u8; 32];
        for (i, val) in self.state.iter().enumerate() {
            digest[i * 4..(i + 1) * 4].copy_from_slice(&val.to_be_bytes());
        }
        digest
    }
}

pub fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize()
}

pub fn sha256_file<P: AsRef<Path>>(path: P) -> std::io::Result<[u8; 32]> {
    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hasher.finalize())
}

pub fn to_hex(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// Target record storing content digests and recipe fingerprint
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetRecord {
    pub target_hash: String,
    pub recipe_hash: String,
    pub prereq_hashes: HashMap<String, String>,
}

/// Lightweight persistent database storing target build hashes in `.makeyd.db`
#[derive(Debug, Clone, Default)]
pub struct BuildDatabase {
    pub records: HashMap<String, TargetRecord>,
}

impl BuildDatabase {
    pub const DB_FILENAME: &'static str = ".makeyd.db";

    pub fn load<P: AsRef<Path>>(path: P) -> Self {
        let mut db = Self::default();
        let file = match File::open(path) {
            Ok(f) => f,
            Err(_) => return db,
        };
        let reader = BufReader::new(file);

        let mut current_target: Option<String> = None;
        let mut current_target_hash = String::new();
        let mut current_recipe_hash = String::new();
        let mut current_prereqs = HashMap::new();

        for line in reader.lines().map_while(Result::ok) {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(target_name) = line.strip_prefix("TARGET ") {
                if let Some(prev) = current_target.take() {
                    db.records.insert(
                        prev,
                        TargetRecord {
                            target_hash: current_target_hash.clone(),
                            recipe_hash: current_recipe_hash.clone(),
                            prereq_hashes: current_prereqs.clone(),
                        },
                    );
                    current_prereqs.clear();
                }
                current_target = Some(target_name.to_string());
            } else if let Some(thash) = line.strip_prefix("TARGET_HASH ") {
                current_target_hash = thash.to_string();
            } else if let Some(rhash) = line.strip_prefix("RECIPE_HASH ") {
                current_recipe_hash = rhash.to_string();
            } else if let Some(prereq_entry) = line.strip_prefix("PREREQ ") {
                if let Some((name, hash)) = prereq_entry.split_once(' ') {
                    current_prereqs.insert(name.to_string(), hash.to_string());
                }
            }
        }

        if let Some(prev) = current_target {
            db.records.insert(
                prev,
                TargetRecord {
                    target_hash: current_target_hash,
                    recipe_hash: current_recipe_hash,
                    prereq_hashes: current_prereqs,
                },
            );
        }

        db
    }

    pub fn save<P: AsRef<Path>>(&self, path: P) -> std::io::Result<()> {
        let mut file = fs::File::create(path)?;
        writeln!(file, "# makeyd Cryptographic Hash Database v1")?;
        for (target, record) in &self.records {
            writeln!(file, "TARGET {}", target)?;
            writeln!(file, "TARGET_HASH {}", record.target_hash)?;
            writeln!(file, "RECIPE_HASH {}", record.recipe_hash)?;
            for (pname, phash) in &record.prereq_hashes {
                writeln!(file, "PREREQ {} {}", pname, phash)?;
            }
        }
        file.flush()
    }

    pub fn get_record(&self, target: &str) -> Option<&TargetRecord> {
        self.records.get(target)
    }

    pub fn update_record(&mut self, target: String, record: TargetRecord) {
        self.records.insert(target, record);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha256_nist_vectors() {
        // NIST test vector: "" (empty string)
        let empty_digest = sha256_bytes(b"");
        assert_eq!(
            to_hex(&empty_digest),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );

        // NIST test vector: "abc"
        let abc_digest = sha256_bytes(b"abc");
        assert_eq!(
            to_hex(&abc_digest),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
