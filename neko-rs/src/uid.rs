//! URL-safe unique IDs, ported from server/pkg/utils (uid.go + id.go).
//! Same alphabet and rejection sampling, so IDs keep the same shape.

use rand::Rng;

const ALPHABET: &[u8] = b"_-0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJ\
                          KLMNOPQRSTUVWXYZ";

/// GenerateUID: rejection-sample random bytes into the alphabet.
pub fn generate_uid(size: usize) -> String {
    let mut rng = rand::thread_rng();
    let mut id = String::with_capacity(size);
    let mut buf = [0u8; 1];
    while id.len() < size {
        rng.fill(&mut buf);
        let idx = buf[0] as usize;
        if idx < ALPHABET.len() {
            id.push(ALPHABET[idx] as char);
        }
    }
    id
}

/// NewUID(size) — Go's default session token size is 64; multiuser member
/// ids use 5.
pub fn new_uid(size: usize) -> String {
    generate_uid(size)
}

/// SafeID: keep URL-safe chars, collapse the rest to '-', trim '-', cap at
/// 64 chars, "user" when empty. Mirrors utils.SafeID.
pub fn safe_id(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches(['-', '.']);
    let truncated: String = trimmed.chars().take(64).collect();
    let result = truncated.trim_end_matches(['-', '.']);
    if result.is_empty() {
        "user".to_owned()
    } else {
        result.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uid_alphabet_and_size() {
        let id = new_uid(21);
        assert_eq!(id.len(), 21);
        assert!(id
            .chars()
            .all(|c| ALPHABET.contains(&(c as u8))));
    }

    #[test]
    fn uid_unique() {
        assert_ne!(new_uid(64), new_uid(64));
    }

    #[test]
    fn safe_id_basic() {
        // Go: replace unsafe with '-', then Trim leading/trailing "-."
        assert_eq!(safe_id("张三 admin"), "admin");
        assert_eq!(safe_id("user@host"), "user-host");
        assert_eq!(safe_id("..."), "user");
        assert_eq!(safe_id("ok_name-1.2"), "ok_name-1.2");
        assert_eq!(safe_id("-leading"), "leading");
    }

    #[test]
    fn safe_id_truncates() {
        let long = "a".repeat(100);
        assert_eq!(safe_id(&long).len(), 64);
    }
}
