use base64::{Engine, engine::general_purpose::STANDARD};
use x25519_dalek::{PublicKey, StaticSecret};

pub fn decode_key(b64: &str) -> Option<[u8; 32]> {
    let raw = STANDARD.decode(b64.trim()).ok()?;
    raw.try_into().ok()
}

pub fn generate_private() -> String {
    let mut k = [0u8; 32];
    getrandom::fill(&mut k).expect("getrandom");
    k[0] &= 248;
    k[31] &= 127;
    k[31] |= 64;
    STANDARD.encode(k)
}

pub fn public_from_private(private_b64: &str) -> Option<String> {
    let secret = StaticSecret::from(decode_key(private_b64)?);
    Some(STANDARD.encode(PublicKey::from(&secret).as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip() {
        // RFC 7748 test vector (Alice)
        let sk = STANDARD.encode(
            hex("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a"),
        );
        let pk = public_from_private(&sk).unwrap();
        assert_eq!(
            STANDARD.decode(pk).unwrap(),
            hex("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a")
        );
        let g = generate_private();
        assert!(public_from_private(&g).is_some());
    }
    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }
}
