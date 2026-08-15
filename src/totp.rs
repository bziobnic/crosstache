use crate::error::{CrosstacheError, Result};
use std::time::{SystemTime, UNIX_EPOCH};
use totp_rs::{Builder, Secret, Totp, TotpError};
use zeroize::Zeroizing;

pub const DEFAULT_TOTP_FIELD: &str = "one-time-code";

pub struct GeneratedTotp {
    pub code: Zeroizing<String>,
    pub expires_in_seconds: u64,
}

fn invalid_material(message: impl Into<String>) -> CrosstacheError {
    CrosstacheError::config(format!("invalid TOTP material: {}", message.into()))
}

fn parse_bare_seed(material: &str) -> Result<Totp> {
    let normalized = Zeroizing::new(
        material
            .chars()
            .filter(|character| !character.is_ascii_whitespace())
            .map(|character| character.to_ascii_uppercase())
            .collect::<String>(),
    );
    if normalized.is_empty() {
        return Err(invalid_material("seed is empty"));
    }
    if !normalized
        .bytes()
        .all(|byte| byte.is_ascii_uppercase() || (b'2'..=b'7').contains(&byte))
    {
        return Err(invalid_material("seed is not unpadded RFC 4648 Base32"));
    }
    let secret = Secret::try_from_base32(normalized.as_str())
        .map_err(|_| invalid_material("seed is not unpadded RFC 4648 Base32"))?;
    Builder::new()
        .with_secret(secret)
        .build()
        .map_err(|error| invalid_material(error.to_string()))
}

fn parse_uri(material: &str) -> Result<Totp> {
    let parsed =
        url::Url::parse(material).map_err(|_| invalid_material("TOTP URI is malformed"))?;
    if parsed.scheme() != "otpauth" || parsed.host_str() != Some("totp") {
        return Err(invalid_material("URI must use otpauth://totp"));
    }
    if parsed.path().trim_matches('/').is_empty() {
        return Err(invalid_material("TOTP URI account label is empty"));
    }
    Totp::from_url(material).map_err(invalid_uri_error)
}

fn invalid_uri_error(error: TotpError) -> CrosstacheError {
    let message = match error {
        TotpError::InvalidAlgorithm { .. } => "algorithm parameter is invalid",
        TotpError::DigitsParse { .. } | TotpError::InvalidDigits { .. } => {
            "digits parameter is invalid"
        }
        TotpError::StepParse { .. } | TotpError::InvalidStepZero => "period parameter is invalid",
        TotpError::InvalidSecret | TotpError::SecretTooShort { .. } => {
            "secret parameter is invalid"
        }
        TotpError::SecretNotSet => "secret parameter is missing",
        TotpError::InvalidAccountName { .. }
        | TotpError::AccountNameDecode { .. }
        | TotpError::AccountNameNotSet => "account label is invalid",
        TotpError::InvalidIssuer { .. }
        | TotpError::IssuerDecode { .. }
        | TotpError::IssuerMismatch { .. } => "issuer parameter is invalid",
        TotpError::InvalidScheme { .. } | TotpError::InvalidHost { .. } => {
            "URI must use otpauth://totp"
        }
        TotpError::UrlParse(_) => "TOTP URI is malformed",
        _ => "TOTP URI parameters are invalid",
    };
    invalid_material(message)
}

fn parse_material(material: &str) -> Result<Totp> {
    let trimmed = material.trim();
    if trimmed.contains("://") {
        parse_uri(trimmed)
    } else {
        parse_bare_seed(trimmed)
    }
}

fn generate_for_totp(totp: &Totp, unix_seconds: u64) -> GeneratedTotp {
    let step = totp.step();
    let expires_in_seconds = step - (unix_seconds % step);
    let code = Zeroizing::new(totp.generate(unix_seconds).to_string());
    GeneratedTotp {
        code,
        expires_in_seconds,
    }
}

pub fn generate_at(material: &str, unix_seconds: u64) -> Result<GeneratedTotp> {
    let totp = parse_material(material)?;
    Ok(generate_for_totp(&totp, unix_seconds))
}

pub fn generate_current(material: &str) -> Result<GeneratedTotp> {
    let totp = parse_material(material)?;
    let unix_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CrosstacheError::config("system clock is before the Unix epoch"))?
        .as_secs();
    Ok(generate_for_totp(&totp, unix_seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA1_SECRET: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";
    const SHA256_SECRET: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZA";
    const SHA512_SECRET: &str = concat!(
        "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ",
        "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNA"
    );

    fn uri(secret: &str, algorithm: &str, digits: u8, period: u64) -> String {
        format!(
            "otpauth://totp/RFC:alice@example.com?secret={secret}&issuer=RFC&algorithm={algorithm}&digits={digits}&period={period}"
        )
    }

    fn error_text(material: &str) -> String {
        match generate_at(material, 59) {
            Ok(_) => panic!("material unexpectedly generated a code"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn matches_rfc_6238_appendix_b_at_59_seconds() {
        for (material, expected) in [
            (uri(SHA1_SECRET, "SHA1", 8, 30), "94287082"),
            (uri(SHA256_SECRET, "SHA256", 8, 30), "46119246"),
            (uri(SHA512_SECRET, "SHA512", 8, 30), "90693936"),
        ] {
            let generated = generate_at(&material, 59).unwrap();
            assert_eq!(generated.code.as_str(), expected);
            assert_eq!(generated.expires_in_seconds, 1);
        }
    }

    #[test]
    fn bare_seed_uses_six_digit_sha1_defaults() {
        let generated = generate_at(SHA1_SECRET, 59).unwrap();
        assert_eq!(generated.code.as_str(), "287082");
        assert_eq!(generated.expires_in_seconds, 1);
    }

    #[test]
    fn exact_boundary_reports_the_full_period() {
        let generated = generate_at(SHA1_SECRET, 60).unwrap();
        assert_eq!(generated.expires_in_seconds, 30);
    }

    #[test]
    fn custom_period_controls_expiry() {
        let generated = generate_at(&uri(SHA1_SECRET, "SHA1", 6, 60), 61).unwrap();
        assert_eq!(generated.expires_in_seconds, 59);
    }

    #[test]
    fn bare_seed_accepts_lowercase_and_visual_whitespace() {
        let grouped = " gezd gnbv gy3t qojq\n gezd gnbv gy3t qojq ";
        assert_eq!(
            generate_at(grouped, 59).unwrap().code.as_str(),
            generate_at(SHA1_SECRET, 59).unwrap().code.as_str()
        );
    }

    #[test]
    fn rejects_non_totp_and_invalid_parameters() {
        for material in [
            "",
            "ABC!",
            "ABC",
            "https://example.com/totp?secret=AAAA",
            "otpauth://hotp/Test?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&counter=1",
            "otpauth://totp/Test?digits=6",
            "otpauth://totp/Test?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&algorithm=MD5",
            "otpauth://totp/Test?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&digits=5",
            "otpauth://totp/Test?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&period=0",
        ] {
            assert!(!error_text(material).is_empty(), "accepted {material:?}");
        }
    }

    #[test]
    fn parser_errors_never_echo_the_seed_or_full_uri() {
        let sentinel = "MZXW6YTBOI======SENTINEL";
        let material =
            format!("otpauth://totp/Test?secret={sentinel}&algorithm=MD5&digits=6&period=30");
        let message = error_text(&material);
        assert!(!message.contains(sentinel), "{message}");
        assert!(!message.contains(&material), "{message}");
    }

    #[test]
    fn dependency_parser_errors_never_echo_parameter_values() {
        let sentinel = "MZXW6YTBOI======SENTINEL";
        let materials = [
            (
                "malformed period",
                format!("otpauth://totp/Test?secret={SHA1_SECRET}&period={sentinel}"),
            ),
            (
                "malformed digits",
                format!("otpauth://totp/Test?secret={SHA1_SECRET}&digits={sentinel}"),
            ),
            (
                "invalid issuer",
                format!("otpauth://totp/Test?secret={SHA1_SECRET}&issuer={sentinel}:bad"),
            ),
            (
                "invalid label",
                format!("otpauth://totp/Test:{sentinel}:bad?secret={SHA1_SECRET}"),
            ),
            (
                "issuer percent-decoding",
                format!("otpauth://totp/{sentinel}%FF:Test?secret={SHA1_SECRET}"),
            ),
            (
                "label percent-decoding",
                format!("otpauth://totp/Test:{sentinel}%FF?secret={SHA1_SECRET}"),
            ),
        ];

        for (case, material) in materials {
            let message = error_text(&material);
            assert!(!message.contains(sentinel), "{case}: {message}");
            assert!(!message.contains(&material), "{case}: {message}");
        }
    }
}
