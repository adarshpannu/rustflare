use crate::includes::*;

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;
use std::thread::JoinHandle;
use typed_arena::Arena;

use std::sync::mpsc;
use std::thread;

use crate::flow::*;

/***************************************************************************************************/
impl Flow {
    fn make_stages(&self) -> Vec<Stage> {
        let stages: Vec<_> = self
            .nodes
            .iter()
            .filter(|node| node.is_endpoint())
            .map(|node| Stage::new(node.id(), self))
            .collect();
        for stage in stages.iter() {
            debug!("Stage: head_node_id = {}", stage.head_node_id)
        }
        stages
    }

    pub fn run(&self, ctx: &Context) {
        let stages = self.make_stages();
        for stage in stages {
            stage.run(ctx, self);
        }
    }
}

/***************************************************************************************************/
#[derive(Debug, Serialize, Deserialize)]
pub struct Stage {
    pub head_node_id: NodeId,
    pub npartitions_producer: usize,
    pub npartitions_consumer: usize,
}

impl Stage {
    fn new(top: NodeId, flow: &Flow) -> Stage {
        let node = flow.get_node(top);
        let npartitions = node.child(flow, 0).npartitions();
        Stage {
            head_node_id: top,
            npartitions_producer: npartitions,
            npartitions_consumer: node.npartitions(),
        }
    }

    fn run(&self, ctx: &Context, flow: &Flow) {
        let node = flow.get_node(self.head_node_id);
        let npartitions = self.npartitions_producer;
        for partition_id in 0..npartitions {
            let mut task = Task::new(partition_id);
            //task.run(flow, self);

            let thread_id = partition_id % (ctx.thread_pool.size());

            //let t2sa = Task2SendAcross { flow: flow.clone() };
            let t2sa = &(flow, self, task);
            let encoded: Vec<u8> = bincode::serialize(&t2sa).unwrap();
            //debug!("Serialized task len = {}", encoded.len());

            let decoded: (Flow, Stage, Task) =
                bincode::deserialize(&encoded[..]).unwrap();

            //dbg!(&decoded.0);

            ctx.thread_pool.s2t_channels_sx[thread_id]
                .send(ThreadPoolMessage::RunTask(encoded));
        }
    }
}

/***************************************************************************************************/
#[derive(Serialize, Deserialize)]
pub struct Task {
    pub partition_id: PartitionId,

    #[serde(skip)]
    pub contexts: HashMap<NodeId, NodeRuntime>,
}

// Tasks write to flow-id / top-id / dest-part-id / source-part-id
impl Task {
    pub fn new(partition_id: PartitionId) -> Task {
        Task {
            partition_id,
            contexts: HashMap::new(),
        }
    }

    pub fn run(&mut self, flow: &Flow, stage: &Stage) {
        debug!(
            "Running task: stage = {}, partition = {}/{}",
            stage.head_node_id, self.partition_id, stage.npartitions_producer
        );
        let node = flow.get_node(stage.head_node_id);
        node.next(flow, stage, self, true);
    }
}

pub enum ThreadPoolMessage {
    RunTask(Vec<u8>),
    EndTask,
    TaskEnded,
}

/***************************************************************************************************/
pub struct ThreadPool {
    threads: Option<Vec<JoinHandle<()>>>,
    s2t_channels_sx: Vec<mpsc::Sender<ThreadPoolMessage>>, // scheduler -> threads (T channels i.e. one per thread)
    t2s_channel_rx: mpsc::Receiver<ThreadPoolMessage>, // threads -> scheduler (1 channel, shared by all threads)
}

impl ThreadPool {
    pub fn join(&mut self) {
        let threads = self.threads.take();
        for thrd in threads.unwrap() {
            thrd.join().unwrap()
        }
    }

    fn size(&self) -> usize {
        if let Some(threads) = &self.threads {
            threads.len()
        } else {
            0
        }
    }

    pub fn close_all(&mut self) {
        for tx in self.s2t_channels_sx.iter() {
            tx.send(ThreadPoolMessage::EndTask);
        }
    }

    pub fn new(ntasks: u32) -> ThreadPool {
        let mut threads = vec![];
        let mut s2t_channels_sx = vec![];

        let (t2s_channel_tx, t2s_channel_rx) =
            mpsc::channel::<ThreadPoolMessage>();

        for i in 0..ntasks {
            let t2s_channel_tx_clone = t2s_channel_tx.clone();

            let (s2t_channel_tx, s2t_channel_rx) =
                mpsc::channel::<ThreadPoolMessage>();

            let thrd = thread::Builder::new()
                .name(format!("thread-{}", i))
                .spawn(move || {
                    for msg in s2t_channel_rx {
                        match msg {
                            ThreadPoolMessage::EndTask => {
                                debug!("End of thread");
                                break;
                            }
                            ThreadPoolMessage::RunTask(encoded) => {
                                let (flow, stage, mut task): (
                                    Flow,
                                    Stage,
                                    Task,
                                ) = bincode::deserialize(&encoded[..]).unwrap();

                                /*
                                debug!(
                                    "Received task, len = {}, stage {}, partition {} ",
                                    encoded.len(),
                                    stage.head_node_id,
                                    task.partition_id
                                );
                                */
                                task.run(&flow, &stage);

                                t2s_channel_tx_clone
                                    .send(ThreadPoolMessage::TaskEnded);
                            }
                            ThreadPoolMessage::TaskEnded => {
                                panic!("Invalid message")
                            }
                        }
                    }
                });
            threads.push(thrd.unwrap());
            s2t_channels_sx.push(s2t_channel_tx);

            //tx_channel.send(WorkerMessage::ShutdownWorker).unwrap();
        }
        ThreadPool {
            threads: Some(threads),
            s2t_channels_sx,
            t2s_channel_rx,
        }
    }
}