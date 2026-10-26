//! The token counts of the primer tiers under nanbench's three tokenizers: `o200k_base`,
//! `cl100k_base` and the tokenizer of Llama 3.

use std::fs;
use std::path::Path;
use std::process::Command;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rustc_hash::FxHashMap;
use sha2::{Digest, Sha256};
use tiktoken_rs::CoreBPE;

/// Meta's published ranks of the Llama 3 tokenizer, at a fixed commit, and their SHA-256.
const LLAMA3_URL: &str = "https://raw.githubusercontent.com/meta-llama/llama-models/5fdf83110cc9daa7435dfba6eb304892cc0041b8/models/llama3/tokenizer.model";
const LLAMA3_SHA256: &str = "82e9d31979e92ab929cd544440f129d9ecd797b69e327f80f17e1c50d5551b55";

/// The pattern splitting text into the pieces Llama 3 encodes.
const LLAMA3_PATTERN: &str = r"(?i:'s|'t|'re|'ve|'m|'ll|'d)|[^\r\n\p{L}\p{N}]?\p{L}+|\p{N}{1,3}| ?[^\s\p{L}\p{N}]+[\r\n]*|\s*[\r\n]+|\s+(?!\S)|\s+";

pub const NAMES: [&str; 3] = ["o200k_base", "cl100k_base", "Llama 3"];

/// The primer tiers: the words `nan primer` takes, none for the core, and each tier's cap.
pub const TIERS: [(&str, usize); 8] = [
    ("core", 8_000),
    ("low-level", 3_000),
    ("concurrency", 3_000),
    ("kernel", 3_000),
    ("packages", 3_000),
    ("contracts", 3_000),
    ("components", 3_000),
    ("tools", 1_000),
];

pub struct Tokenizers([CoreBPE; 3]);

impl Tokenizers {
    /// The three tokenizers, fetching the Llama 3 ranks into `cache` when they are not there.
    pub fn load(cache: &Path) -> Result<Tokenizers, String> {
        let fault = |error: &dyn std::fmt::Display| error.to_string();
        let ranks = llama3_ranks(cache)?;
        let mut encoder = FxHashMap::default();
        for line in ranks.lines() {
            let (token, rank) = line.split_once(' ').ok_or("a malformed rank line")?;
            let token = STANDARD.decode(token).map_err(|error| fault(&error))?;
            encoder.insert(token, rank.parse().map_err(|error| fault(&error))?);
        }
        let llama3 = CoreBPE::new(encoder, FxHashMap::default(), LLAMA3_PATTERN)
            .map_err(|error| fault(&error))?;
        let o200k = tiktoken_rs::o200k_base().map_err(|error| fault(&error))?;
        let cl100k = tiktoken_rs::cl100k_base().map_err(|error| fault(&error))?;
        Ok(Tokenizers([o200k, cl100k, llama3]))
    }

    /// The count each tokenizer gives for the text's UTF-8 bytes, adding no special token.
    pub fn counts(&self, text: &str) -> [usize; 3] {
        self.0
            .each_ref()
            .map(|tokenizer| tokenizer.encode_ordinary(text).len())
    }
}

/// The Llama 3 ranks, read from `cache` or fetched with `curl` and checked against their hash.
fn llama3_ranks(cache: &Path) -> Result<String, String> {
    let path = cache.join("llama3.tiktoken");
    if !path.exists() {
        fs::create_dir_all(cache).map_err(|error| error.to_string())?;
        let partial = cache.join("llama3.tiktoken.partial");
        let fetched = Command::new("curl")
            .args(["-sSfL", "-o"])
            .arg(&partial)
            .arg(LLAMA3_URL)
            .status()
            .map_err(|error| format!("curl: {error}"))?;
        if !fetched.success() {
            return Err(format!("curl could not fetch {LLAMA3_URL}"));
        }
        fs::rename(&partial, &path).map_err(|error| error.to_string())?;
    }
    let bytes = fs::read(&path).map_err(|error| error.to_string())?;
    let digest: String = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if digest != LLAMA3_SHA256 {
        drop(fs::remove_file(&path));
        return Err(format!(
            "{} does not hash to {LLAMA3_SHA256}",
            path.display()
        ));
    }
    String::from_utf8(bytes).map_err(|error| error.to_string())
}

/// The `nan primer` text of a tier, run in `dir`, which no workspace holds.
pub fn primer(tier: &str, dir: &Path) -> String {
    let mut args = vec!["primer".to_owned()];
    if tier != "core" {
        args.push(tier.to_owned());
    }
    nan::run(&args, dir).output
}
