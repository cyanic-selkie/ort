use std::{
	fs::File,
	io::{Read, Seek, SeekFrom, Write},
	path::Path
};

use kdam::BarExt;
use ort::{
	execution_providers::CUDAExecutionProvider,
	memory::Allocator,
	session::{Session, builder::SessionBuilder},
	training::{CheckpointStrategy, Trainer, TrainerCallbacks, TrainerControl, TrainerState, TrainingArguments},
	value::{Tensor, TensorRef}
};
use rand::RngCore;
use tokenizers::Tokenizer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

const BATCH_SIZE: usize = 16;
const SEQUENCE_LENGTH: usize = 256;

// Include common code for `ort` examples that allows using the various feature flags to enable different EPs and
// backends.
#[path = "../common/mod.rs"]
mod common;

struct LoggerCallback {
	progress_bar: kdam::Bar
}

impl LoggerCallback {
	pub fn new() -> Self {
		Self {
			progress_bar: kdam::Bar::builder().leave(true).build().unwrap()
		}
	}
}

impl TrainerCallbacks for LoggerCallback {
	fn train_step(&mut self, train_loss: f32, state: &TrainerState, _: &mut TrainerControl<'_>) -> ort::Result<()> {
		self.progress_bar.total = state.max_steps;
		self.progress_bar.set_postfix(format!("loss={train_loss:.3}"));
		let _ = self.progress_bar.update_to(state.iter_step);
		Ok(())
	}
}

fn main() -> ort::Result<()> {
	// Initialize tracing to receive log messages from `ort`
	tracing_subscriber::registry()
		.with(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()))
		.with(tracing_subscriber::fmt::layer())
		.init();

	// Register EPs based on feature flags - this isn't crucial for usage and can be removed.
	common::init()?;

	let trainer = Trainer::new_from_artifacts(
		SessionBuilder::new()?.with_execution_providers([CUDAExecutionProvider::default().build()])?,
		Allocator::default(),
		"tools/train-data/mini-clm",
		None
	)?;

	let tokenizer = Tokenizer::from_file(
		Path::new(env!("CARGO_MANIFEST_DIR"))
			.parent()
			.unwrap()
			.join("gpt2")
			.join("data")
			.join("tokenizer.json")
	)
	.unwrap();

	let mut dataset = File::open("train-clm-dataset.bin").unwrap();
	let file_size = dataset.metadata().unwrap().len();
	let num_tokens = (file_size / 2) as usize; // 16-bit tokens
	let mut rng = rand::rng();
	let mut input_buffer = vec![0u16; SEQUENCE_LENGTH * BATCH_SIZE];
	let mut label_buffer = vec![0u16; SEQUENCE_LENGTH * BATCH_SIZE];
	let dataloader = move |_: usize| {
		for batch in 0..BATCH_SIZE {
			let start_idx = rng.next_u64() % (num_tokens - SEQUENCE_LENGTH - 1) as u64;
			dataset.seek(SeekFrom::Start(start_idx * 2)).unwrap();
			dataset
				.read_exact(unsafe {
					std::slice::from_raw_parts_mut(
						input_buffer[batch * SEQUENCE_LENGTH..(batch + 1) * SEQUENCE_LENGTH]
							.as_mut_ptr()
							.cast::<u8>(),
						SEQUENCE_LENGTH * 2
					)
				})
				.unwrap();
			dataset.seek(SeekFrom::Start((start_idx + 1) * 2)).unwrap();
			dataset
				.read_exact(unsafe {
					std::slice::from_raw_parts_mut(
						label_buffer[batch * SEQUENCE_LENGTH..(batch + 1) * SEQUENCE_LENGTH]
							.as_mut_ptr()
							.cast::<u8>(),
						SEQUENCE_LENGTH * 2
					)
				})
				.unwrap();
		}

		let inputs = Tensor::from_array(([BATCH_SIZE, SEQUENCE_LENGTH], input_buffer.iter().map(|c| *c as i64).collect::<Vec<i64>>()))?;
		let labels = Tensor::from_array(([BATCH_SIZE * SEQUENCE_LENGTH], label_buffer.iter().map(|c| *c as i64).collect::<Vec<i64>>()))?;

		Ok((ort::inputs![inputs], ort::inputs![labels]))
	};

	trainer.train(
		TrainingArguments::new(dataloader)
			.with_lr(7e-5)
			.with_max_steps(5000)
			.with_ckpt_strategy(CheckpointStrategy::Steps(500))
			.with_callbacks(LoggerCallback::new())
	)?;

	trainer.export("trained-clm.onnx", ["probs"])?;

	let mut session = Session::builder()?.commit_from_file("trained-clm.onnx")?;

	let mut stdout = std::io::stdout();

	let tokens = tokenizer.encode("<|endoftext|>", false).unwrap();
	let mut tokens = tokens.get_ids().iter().map(|i| *i as i64).collect::<Vec<_>>();

	for _ in 0..50 {
		let input = TensorRef::from_array_view((vec![1, 1, tokens.len() as i64], tokens.as_slice()))?;
		let outputs = session.run(ort::inputs![input])?;
		let (dim, probabilities) = outputs["probs"].try_extract_tensor()?;

		let (seq_len, vocab_size) = (dim[2] as usize, dim[3] as usize);
		let mut probabilities: Vec<(usize, f32)> = probabilities[(seq_len - 1) * vocab_size..].iter().copied().enumerate().collect();
		probabilities.sort_unstable_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Less));

		let token = probabilities[0].0 as i64;
		tokens.push(token);

		let token_str = tokenizer.decode(&[token as _], false).unwrap();
		print!("{}", token_str);
		stdout.flush().unwrap();
	}

	println!();
	Ok(())
}
