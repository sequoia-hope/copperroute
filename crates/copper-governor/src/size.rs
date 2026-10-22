pub fn parse_byte_size(value: &str) -> Result<u64, String> {
    let trimmed = value.trim();
    let split = trimmed
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(trimmed.len());
    let (number, unit) = trimmed.split_at(split);
    let number: f64 = number
        .parse()
        .map_err(|_| format!("'{value}' is not a size such as 512M or 4G"))?;
    let scale: u64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1 << 10,
        "m" | "mb" | "mib" => 1 << 20,
        "g" | "gb" | "gib" => 1 << 30,
        "t" | "tb" | "tib" => 1 << 40,
        _ => return Err(format!("'{value}' has an unknown unit; use K, M, G or T")),
    };
    Ok((number * scale as f64) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_binary_units() {
        assert_eq!(parse_byte_size("4096"), Ok(4096));
        assert_eq!(parse_byte_size("512M"), Ok(512 << 20));
        assert_eq!(parse_byte_size("4G"), Ok(4 << 30));
        assert_eq!(parse_byte_size("1.5gib"), Ok(3 << 29));
        assert_eq!(parse_byte_size(" 2 GB "), Ok(2 << 30));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_byte_size("").is_err());
        assert!(parse_byte_size("G").is_err());
        assert!(parse_byte_size("4X").is_err());
    }
}
