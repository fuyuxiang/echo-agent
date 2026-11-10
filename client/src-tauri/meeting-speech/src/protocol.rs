// Shared wire format between the desktop process and the local inference worker.
use serde::{Deserialize, Serialize};

pub const RATE: u32 = 16_000;
pub const CONTEXT: u64 = RATE as u64 * 8;
pub const MAX_WINDOW: usize = RATE as usize * 16;
pub const MAX_REPLY: u64 = 512 * 1024;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Window {
    pub pieces: Vec<Piece>,
    pub turns: Vec<Turn>,
    pub voices: Vec<Voice>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Piece {
    pub start: f32,
    pub end: f32,
    pub speaker: Option<i32>,
    pub text: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Turn {
    pub start: f32,
    pub end: f32,
    pub speaker: i32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Voice {
    pub speaker: i32,
    pub embedding: Vec<f32>,
}
