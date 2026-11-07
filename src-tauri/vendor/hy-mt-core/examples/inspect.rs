//! Print the parsed model configuration and special-token ids.
//!
//! ```bash
//! cargo run --release --example inspect -- /path/to/model.gguf
//! ```

use hy_mt_core::gguf::HyGgufFile;
use hy_mt_core::tokenizer::HyTokenizer;
use hy_mt_core::ModelSource;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).expect("usage: inspect <model.gguf>");
    let file = HyGgufFile::load(&path)?;
    let cfg = file.config();
    println!("{:#?}", cfg);

    let tok = HyTokenizer::from_gguf(file.content())?;
    println!(
        "tokenizer: family={:?} vocab={} bos={} turn_end={} assistant={}",
        tok.family(),
        tok.vocab_size(),
        tok.bos_id(),
        tok.turn_end_id(),
        tok.assistant_id()
    );

    let text = "你好，世界！";
    let prompt = tok.build_translate_prompt("English", text)?;
    println!("prompt ids ({}): {:?}", prompt.len(), prompt);
    let decoded = tok.decode_with_special(&prompt)?;
    println!("prompt decoded: {decoded:?}");

    // Raw special-token metadata, as the converter wrote it.
    for key in [
        "tokenizer.ggml.bos_token_id",
        "tokenizer.ggml.eos_token_id",
        "tokenizer.ggml.eot_token_id",
        "tokenizer.ggml.padding_token_id",
        "tokenizer.chat_template",
    ] {
        let v = file.content().metadata.get(key);
        let short = v.map(|v| {
            let s = format!("{v:?}");
            s.chars().take(200).collect::<String>()
        });
        println!("{key} = {short:?}");
    }
    Ok(())
}
