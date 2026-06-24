use anyhow::{bail, Context, Result};

pub fn encode(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn decode(value: &str, name: &str) -> Result<String> {
    if !value.len().is_multiple_of(2) {
        bail!("invalid encoded {name}");
    }
    let bytes = value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).context("invalid hexadecimal argument")?;
            u8::from_str_radix(pair, 16).with_context(|| format!("invalid encoded {name}"))
        })
        .collect::<Result<Vec<_>>>()?;
    String::from_utf8(bytes).with_context(|| format!("encoded {name} is not UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_arguments_with_shell_metacharacters() {
        let value = "Chat name; $(touch nope)";
        assert_eq!(decode(&encode(value), "test").unwrap(), value);
        assert!(encode(value)
            .chars()
            .all(|character| character.is_ascii_hexdigit()));
    }
}
