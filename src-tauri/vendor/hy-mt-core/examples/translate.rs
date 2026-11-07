//! Smoke-test the vendored inference stack without launching the app:
//!
//! ```bash
//! cargo run --release --example translate -- /path/to/model.gguf "你好，世界！" English
//! ```
//!
//! Prints the translation to stdout and load/decode timing to stderr.

use std::io::Write;
use std::time::Instant;

use hy_mt_core::device::DeviceCtx;
use hy_mt_core::generate::Generator;
use hy_mt_core::gguf::HyGgufFile;
use hy_mt_core::model::HunyuanDense;
use hy_mt_core::sampling::SamplingParams;
use hy_mt_core::tokenizer::HyTokenizer;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let model_path = args
        .next()
        .expect("usage: translate <model.gguf> <text> [target-language]");
    let text = args.next().expect("usage: translate <model.gguf> <text> [target]");
    let target = args.next().unwrap_or_else(|| "English".to_string());

    let t0 = Instant::now();
    let file = HyGgufFile::load(&model_path)?;
    let tokenizer = HyTokenizer::from_gguf(file.content())?;
    let device = DeviceCtx::cpu();
    let mut model = HunyuanDense::load_from(&file, &device)?;
    eprintln!("model loaded in {:?}", t0.elapsed());
    drop(file);

    let params = SamplingParams {
        temperature: 0.0,
        ..Default::default()
    };
    let mut generator = Generator::new(&mut model, &tokenizer, params, 1024);
    let instruction = args.next();
    let prompt = match &instruction {
        Some(tmpl) => {
            let user = tmpl.replace("{tgt}", &target).replace("{text}", &text);
            tokenizer.apply_chat_template(None, &user)?
        }
        None => tokenizer.build_translate_prompt(&target, &text)?,
    };
    eprintln!("prompt tokens: {}", prompt.len());

    let t1 = Instant::now();
    let mut tokens = 0usize;
    let mut first_token: Option<std::time::Duration> = None;
    generator.generate(&prompt, |step| {
        if !step.text.is_empty() {
            if first_token.is_none() {
                first_token = Some(t1.elapsed());
            }
            print!("{}", step.text);
            let _ = std::io::stdout().flush();
            tokens += 1;
        }
        true
    })?;
    let dt = t1.elapsed();
    eprintln!("first token after {:?}", first_token.unwrap_or_default());
    eprintln!(
        "\n{} tokens in {:?} ({:.1} tok/s)",
        tokens,
        dt,
        tokens as f64 / dt.as_secs_f64().max(f64::EPSILON)
    );
    Ok(())
}
