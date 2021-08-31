// includes.rs

pub use log::{error, warn, info, debug};

pub const DATADIR: &str = "/Users/adarshrp/Projects/flare/data";
pub const TEMPDIR: &str = "/Users/adarshrp/Projects/flare/temp";

pub type NodeId = usize;
pub type ColId = usize;
pub type PartitionId = usize;

pub use crate::Env;
pub use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TextFilePartition(pub u64, pub u64);

pub use typed_arena::Arena;

pub type NodeArena = Arena<crate::flow::Node>;

