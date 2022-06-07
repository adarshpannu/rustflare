// pop: Physical operators

#![allow(unused_variables)]

use std::fmt;
use std::fs::File;
use std::io::Write;
use std::process::Command;
use std::rc::Rc;

pub use crate::{bitset::*, csv::*, expr::*, flow::*, graph::*, includes::*, lop::*, metadata::*, pcode::*, pcode::*, qgm::*, row::*, stage::*, task::*};

pub type POPGraph = Graph<POPKey, POP, POPProps>;

#[derive(Debug, Serialize, Deserialize)]
pub struct POPProps {
    pub predicates: Option<Vec<PCode>>,
    pub emitcols: Option<Vec<PCode>>,
    pub npartitions: usize,
}

impl POPProps {
    pub fn new(predicates: Option<Vec<PCode>>, emitcols: Option<Vec<PCode>>, npartitions: usize) -> POPProps {
        POPProps {
            predicates,
            emitcols,
            npartitions,
        }
    }
}

/***************************************************************************************************/
#[derive(Debug, Serialize, Deserialize)]
pub enum POP {
    CSV(CSV),
    CSVDir(CSVDir),
    HashJoin(HashJoin),
    Repartition(Repartition),
    Aggregation(Aggregation),
}

impl POP {
    pub fn is_stage_root(&self) -> bool {
        matches!(self, POP::Repartition { .. })
    }
}

impl POPKey {
    pub fn next(&self, flow: &Flow, stage: &Stage, task: &mut Task, is_head: bool) -> Result<bool, String> {
        let (pop, props, ..) = flow.pop_graph.get3(*self);

        loop {
            let got_row = match pop {
                POP::CSV(inner_node) => inner_node.next(*self, flow, stage, task, is_head)?,
                POP::CSVDir(inner_node) => inner_node.next(*self, flow, stage, task, is_head)?,
                POP::Repartition(inner_node) => inner_node.next(*self, flow, stage, task, is_head)?,
                POP::HashJoin(inner_node) => inner_node.next(*self, flow, stage, task, is_head)?,
                POP::Aggregation(inner_node) => inner_node.next(*self, flow, stage, task, is_head)?,
            };

            // Run predicates and emits, if any
            if got_row {
                let row_passed = Self::eval_predicates(props, &task.task_row);
                if row_passed {
                    let emitrow = Self::eval_emitcols(props, &task.task_row);
                    if let Some(emitrow) = emitrow {
                        debug!("Emit row: {}", emitrow);
                    }
                }
                return Ok(true);
            } else {
                // No more rows to drain
                return Ok(false);
            }
        }
    }

    pub fn eval_predicates(props: &POPProps, registers: &Row) -> bool {
        if let Some(preds) = props.predicates.as_ref() {
            for pred in preds.iter() {
                let result = pred.eval(&registers);
                if let Datum::BOOL(b) = result {
                    if !b {
                        return false; // short circuit
                    }
                } else {
                    panic!("No bool?")
                }
            }
        }
        return true;
    }

    pub fn eval_emitcols(props: &POPProps, registers: &Row) -> Option<Row> {
        if let Some(emitcols) = props.emitcols.as_ref() {
            let emit_output = emitcols
                .iter()
                .map(|emit| {
                    let result = emit.eval(&registers);
                    result
                })
                .collect::<Vec<_>>();
            Some(Row::from(emit_output))
        } else {
            None
        }
    }
}
/***************************************************************************************************/
#[derive(Debug, Serialize, Deserialize)]
pub struct Repartition {
    output_map: Option<Vec<RegisterId>>,
}

impl Repartition {
    fn next(&self, pop_key: POPKey, flow: &Flow, stage: &Stage, task: &mut Task, is_head: bool) -> Result<bool, String> {
        debug!("Repartition:next(): {:?}, is_head: {}", pop_key, is_head);

        todo!()
    }
}

/***************************************************************************************************/
#[derive(Debug, Serialize, Deserialize)]
pub struct HashJoin {}

impl HashJoin {
    fn next(&self, pop_key: POPKey, flow: &Flow, stage: &Stage, task: &mut Task, is_head: bool) -> Result<bool, String> {
        let children = flow.pop_graph.get(pop_key).children.as_ref().unwrap();
        let probe_child_key = children[0];
        let build_child_key = children[1];

        // Drain both children for now: todo
        for child_key in vec![probe_child_key, build_child_key] {
            debug!("HashJoin:next(): Drain {:?}", child_key);
            while child_key.next(flow, stage, task, false).unwrap() {}
        }
        Ok(true)
    }
}

/***************************************************************************************************/
#[derive(Debug, Serialize, Deserialize)]
pub struct Aggregation {}

impl Aggregation {
    fn next(&self, pop_key: POPKey, flow: &Flow, stage: &Stage, task: &mut Task, is_head: bool) -> Result<bool, String> {
        todo!()
    }
}

/***************************************************************************************************/
#[derive(Serialize, Deserialize)]
pub struct CSV {
    pathname: String,
    coltypes: Vec<DataType>,
    header: bool,
    separator: char,
    partitions: Vec<TextFilePartition>,
    input_map: HashMap<ColId, RegisterId>,
}

impl CSV {
    fn new(pathname: String, coltypes: Vec<DataType>, header: bool, separator: char, npartitions: usize, input_map: HashMap<ColId, RegisterId>) -> CSV {
        let partitions = compute_partitions(&pathname, npartitions as u64).unwrap();

        CSV {
            pathname,
            coltypes,
            header,
            separator,
            partitions,
            input_map,
        }
    }

    fn next(&self, pop_key: POPKey, flow: &Flow, stage: &Stage, task: &mut Task, is_head: bool) -> Result<bool, String> {
        let partition_id = task.partition_id;
        let runtime = task.contexts.entry(pop_key).or_insert_with(|| {
            let partition = &self.partitions[partition_id];
            let mut iter = CSVPartitionIter::new(&self.pathname, partition).unwrap();
            if partition_id == 0 {
                iter.next(); // Consume the header row (fix: check if header exists though)
            }
            NodeRuntime::CSV { iter }
        });

        if let NodeRuntime::CSV { iter } = runtime {
            if let Some(line) = iter.next() {
                // debug!("line = :{}:", &line.trim_end());
                line.trim_end()
                    .split(self.separator)
                    .enumerate()
                    .filter(|(ix, col)| self.input_map.get(ix).is_some())
                    .for_each(|(ix, col)| {
                        let ttuple_ix = *self.input_map.get(&ix).unwrap();
                        let datum = match self.coltypes[ix] {
                            DataType::INT => {
                                let ival = col.parse::<isize>();
                                if ival.is_err() {
                                    panic!("{} is not an INT", &col);
                                } else {
                                    Datum::INT(ival.unwrap())
                                }
                            }
                            DataType::STR => Datum::STR(Rc::new(col.to_owned())),
                            _ => todo!(),
                        };
                        task.task_row.set_column(ttuple_ix, &datum);
                    });
                return Ok(true);
            } else {
                return Ok(false);
            }
        }
        panic!("Cannot get NodeRuntime::CSV")
    }
}

impl fmt::Debug for CSV {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        let pathname = self.pathname.split("/").last().unwrap();
        //fmt.debug_struct("").field("file", &pathname).field("input_map", &self.input_map).finish()
        fmt.debug_struct("").field("file", &pathname).finish()
    }
}

/***************************************************************************************************/

#[derive(Serialize, Deserialize)]
pub struct CSVDir {
    dirname_prefix: String, // E.g.: $TEMPDIR/flow-99/stage  i.e. everything except the "-{partition#}"
    coltypes: Vec<DataType>,
    header: bool,
    separator: char,
    npartitions: usize,
    input_map: HashMap<ColId, RegisterId>,
}

impl fmt::Debug for CSVDir {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        let dirname = self.dirname_prefix.split("/").last().unwrap();
        //fmt.debug_struct("").field("file", &pathname).field("input_map", &self.input_map).finish()
        fmt.debug_struct("").field("dir", &dirname).finish()
    }
}

impl CSVDir {
    fn new(dirname_prefix: String, coltypes: Vec<DataType>, header: bool, separator: char, npartitions: usize, input_map: HashMap<ColId, RegisterId>) -> Self {
        CSVDir {
            dirname_prefix,
            coltypes,
            header,
            separator,
            npartitions,
            input_map,
        }
    }

    fn next(&self, pop_key: POPKey, flow: &Flow, stage: &Stage, task: &mut Task, is_head: bool) -> Result<bool, String> {
        let partition_id = task.partition_id;
        let runtime = task.contexts.entry(pop_key).or_insert_with(|| {
            let full_dirname = format!("{}-{}", self.dirname_prefix, partition_id);
            let iter = CSVDirIter::new(&full_dirname).unwrap();
            NodeRuntime::CSVDir { iter }
        });

        if let NodeRuntime::CSVDir { iter } = runtime {
            if let Some(line) = iter.next() {
                // debug!("line = :{}:", &line.trim_end());
                line.trim_end()
                    .split(self.separator)
                    .enumerate()
                    .filter(|(ix, col)| self.input_map.get(ix).is_some())
                    .for_each(|(ix, col)| {
                        let ttuple_ix = *self.input_map.get(&ix).unwrap();
                        let datum = match self.coltypes[ix] {
                            DataType::INT => {
                                let ival = col.parse::<isize>();
                                if ival.is_err() {
                                    panic!("{} is not an INT", &col);
                                } else {
                                    Datum::INT(ival.unwrap())
                                }
                            }
                            DataType::STR => Datum::STR(Rc::new(col.to_owned())),
                            _ => todo!(),
                        };
                        task.task_row.set_column(ttuple_ix, &datum);
                    });
                return Ok(true);
            } else {
                return Ok(false);
            }
        }
        panic!("Cannot get NodeRuntime::CSV")
    }
}

/***************************************************************************************************/
pub enum NodeRuntime {
    Unused,
    CSV { iter: CSVPartitionIter },
    CSVDir { iter: CSVDirIter },
}

/***************************************************************************************************/
#[derive(Debug, Serialize, Deserialize)]
pub struct HashJoinPOP {}

impl POP {
    pub fn compile(env: &Env, qgm: &mut QGM) -> Result<Flow, String> {
        let (lop_graph, lop_key) = qgm.build_logical_plan(env)?;
        let mut pop_graph: POPGraph = Graph::new();
        let mut stage_graph = StageGraph::new();

        let root_stage_id = stage_graph.add_stage(lop_key, None);

        let root_pop_key = Self::compile_lop(qgm, &lop_graph, lop_key, &mut pop_graph, &mut stage_graph, root_stage_id)?;

        stage_graph.set_pop_key(&pop_graph, root_stage_id, root_pop_key);

        stage_graph.print();

        let plan_pathname = format!("{}/{}", env.output_dir, "pop.dot");
        QGM::write_physical_plan_to_graphviz(qgm, &pop_graph, root_pop_key, &plan_pathname)?;

        let flow = Flow { pop_graph, stage_graph };
        return Ok(flow);
    }

    pub fn compile_lop(
        qgm: &mut QGM, lop_graph: &LOPGraph, lop_key: LOPKey, pop_graph: &mut POPGraph, stage_graph: &mut StageGraph, stage_id: StageId,
    ) -> Result<POPKey, String> {
        let (lop, lopprops, lop_children) = lop_graph.get3(lop_key);

        let child_stage_id = if matches!(lop, LOP::Repartition { .. }) {
            stage_graph.add_stage(lop_key, Some(stage_id))
        } else {
            stage_id
        };

        // Compile children first
        let mut pop_children = vec![];
        if let Some(lop_children) = lop_children {
            for lop_child_key in lop_children {
                let pop_key = Self::compile_lop(qgm, lop_graph, *lop_child_key, pop_graph, stage_graph, child_stage_id)?;
                pop_children.push(pop_key);
            }
        }

        let npartitions = lopprops.partdesc.npartitions;

        let pop_key = match lop {
            LOP::TableScan { input_cols } => Self::compile_scan(qgm, lop_graph, lop_key, pop_graph, stage_graph, stage_id)?,
            LOP::HashJoin { equi_join_preds } => Self::compile_join(qgm, lop_graph, lop_key, pop_graph, pop_children, stage_graph, stage_id)?,
            LOP::Repartition { cpartitions } => {
                Self::compile_repartition(qgm, lop_graph, lop_key, pop_graph, pop_children, stage_graph, stage_id, child_stage_id)?
            }
            LOP::Aggregation { .. } => Self::compile_aggregation(qgm, lop_graph, lop_key, pop_graph, pop_children, stage_graph, stage_id)?,
        };

        if stage_id != child_stage_id {
            stage_graph.set_pop_key(pop_graph, child_stage_id, pop_key)
        }

        Ok(pop_key)
    }

    pub fn compile_repartition(
        qgm: &mut QGM, lop_graph: &LOPGraph, lop_key: LOPKey, pop_graph: &mut POPGraph, pop_children: Vec<POPKey>, stage_graph: &mut StageGraph,
        stage_id: StageId, child_stage_id: StageId,
    ) -> Result<POPKey, String> {
        // Repartition split into Repartition + CSVDirScan
        let (lop, lopprops, ..) = lop_graph.get3(lop_key);

        // We shouldn't have any predicates
        let predicates = None;
        assert!(lopprops.preds.len() == 0);

        let ra = stage_graph.get_register_allocator(stage_id);

        // Compile cols or emitcols. We will have one or the other
        let emitcols = Self::compile_emitcols(qgm, lopprops.emitcols.as_ref(), ra);

        let output_map: Option<Vec<RegisterId>> = if emitcols.is_none() {
            let output_map = lopprops
                .cols
                .elements()
                .iter()
                .map(|&quncol| {
                    let regid = ra.get_id(quncol);
                    regid
                })
                .collect();
            Some(output_map)
        } else {
            None
        };

        let props = POPProps::new(predicates, emitcols, lopprops.partdesc.npartitions);

        let pop_inner = Repartition { output_map };
        let pop_key = pop_graph.add_node_with_props(POP::Repartition(pop_inner), props, Some(pop_children));

        Ok(pop_key)
    }

    pub fn compile_join(
        qgm: &mut QGM, lop_graph: &LOPGraph, lop_key: LOPKey, pop_graph: &mut POPGraph, pop_children: Vec<POPKey>, stage_graph: &mut StageGraph,
        stage_id: StageId,
    ) -> Result<POPKey, String> {
        let (lop, lopprops, ..) = lop_graph.get3(lop_key);

        // Compile predicates
        //debug!("Compile predicate for lopkey: {:?}", lop_key);
        let ra = stage_graph.get_register_allocator(stage_id);

        let predicates = Self::compile_predicates(qgm, &lopprops.preds, ra);

        // Compile emitcols
        //debug!("Compile emits for lopkey: {:?}", lop_key);
        let emitcols = Self::compile_emitcols(qgm, lopprops.emitcols.as_ref(), ra);

        let props = POPProps::new(predicates, emitcols, lopprops.partdesc.npartitions);

        let pop_inner = HashJoin {};
        let pop_key = pop_graph.add_node_with_props(POP::HashJoin(pop_inner), props, Some(pop_children));

        Ok(pop_key)
    }

    pub fn compile_aggregation(
        qgm: &mut QGM, lop_graph: &LOPGraph, lop_key: LOPKey, pop_graph: &mut POPGraph, pop_children: Vec<POPKey>, stage_graph: &mut StageGraph,
        stage_id: StageId,
    ) -> Result<POPKey, String> {
        let (lop, lopprops, ..) = lop_graph.get3(lop_key);
        let ra = stage_graph.get_register_allocator(stage_id);

        // Compile predicates
        debug!("Compile predicate for lopkey: {:?}", lop_key);
        let predicates = None; // todo Self::compile_predicates(qgm, &lopprops.preds, ra);

        // Compile emitcols
        debug!("Compile emits for lopkey: {:?}", lop_key);
        let emitcols = None; // todo Self::compile_emitcols(qgm, lopprops.emitcols.as_ref(), ra);

        let props = POPProps::new(predicates, emitcols, lopprops.partdesc.npartitions);

        let pop_inner = Aggregation {};
        let pop_key = pop_graph.add_node_with_props(POP::Aggregation(pop_inner), props, Some(pop_children));

        Ok(pop_key)
    }

    pub fn compile_scan(
        qgm: &mut QGM, lop_graph: &LOPGraph, lop_key: LOPKey, pop_graph: &mut POPGraph, stage_graph: &mut StageGraph, stage_id: StageId,
    ) -> Result<POPKey, String> {
        let (lop, lopprops, ..) = lop_graph.get3(lop_key);
        let ra = stage_graph.get_register_allocator(stage_id);

        let qunid = lopprops.quns.elements()[0];
        let tbldesc = qgm.metadata.get_tabledesc(qunid).unwrap();
        let columns = tbldesc.columns();

        let coltypes = columns.iter().map(|col| col.datatype).collect();

        // Build input map
        let input_map: HashMap<ColId, RegisterId> = if let LOP::TableScan { input_cols } = lop {
            input_cols
                .elements()
                .iter()
                .map(|&quncol| {
                    let regid = ra.get_id(quncol);
                    (quncol.1, regid)
                })
                .collect()
        } else {
            return Err(format!("Internal error: compile_scan() received a POP that isn't a TableScan"));
        };

        // Compile predicates
        //debug!("Compile predicate for lopkey: {:?}", lop_key);

        let predicates = Self::compile_predicates(qgm, &lopprops.preds, ra);

        let pop = match tbldesc.get_type() {
            TableType::CSV => {
                let inner = CSV::new(
                    tbldesc.pathname().clone(),
                    coltypes,
                    tbldesc.header(),
                    tbldesc.separator(),
                    lopprops.partdesc.npartitions,
                    input_map,
                );
                POP::CSV(inner)
            }
            TableType::CSVDIR => {
                let inner = CSVDir::new(
                    tbldesc.pathname().clone(),
                    coltypes,
                    tbldesc.header(),
                    tbldesc.separator(),
                    lopprops.partdesc.npartitions,
                    input_map,
                );
                POP::CSVDir(inner)
            }
        };

        // Compile emitcols
        //debug!("Compile emits for lopkey: {:?}", lop_key);
        let emitcols = Self::compile_emitcols(qgm, lopprops.emitcols.as_ref(), ra);

        let props = POPProps::new(predicates, emitcols, lopprops.partdesc.npartitions);

        let pop_key = pop_graph.add_node_with_props(pop, props, None);
        Ok(pop_key)
    }

    pub fn compile_predicates(qgm: &QGM, preds: &Bitset<ExprKey>, register_allocator: &mut RegisterAllocator) -> Option<Vec<PCode>> {
        let mut pcodevec = vec![];
        if preds.len() > 0 {
            for pred_key in preds.elements().iter() {
                let mut pcode = PCode::new();
                pred_key.compile(&qgm.expr_graph, &mut pcode, register_allocator);
                pcodevec.push(pcode);
            }
            Some(pcodevec)
        } else {
            None
        }
    }

    pub fn compile_emitcols(qgm: &QGM, emitcols: Option<&Vec<EmitCol>>, register_allocator: &mut RegisterAllocator) -> Option<Vec<PCode>> {
        if let Some(emitcols) = emitcols {
            let pcode = PCode::new();
            let pcodevec = emitcols
                .iter()
                .map(|ne| {
                    let mut pcode = PCode::new();
                    ne.expr_key.compile(&qgm.expr_graph, &mut pcode, register_allocator);
                    pcode
                })
                .collect::<Vec<_>>();
            Some(pcodevec)
        } else {
            None
        }
    }
}

impl QGM {
    pub fn write_physical_plan_to_graphviz(self: &QGM, pop_graph: &POPGraph, pop_key: POPKey, pathname: &str) -> Result<(), String> {
        let mut file = std::fs::File::create(pathname).map_err(|err| f!("{:?}: {}", err, pathname))?;

        fprint!(file, "digraph example1 {{\n");
        fprint!(file, "    node [shape=record];\n");
        fprint!(file, "    rankdir=BT;\n"); // direction of DAG
        fprint!(file, "    nodesep=0.5;\n");
        fprint!(file, "    ordering=\"in\";\n");

        self.write_pop_to_graphviz(pop_graph, pop_key, &mut file)?;

        fprint!(file, "}}\n");

        drop(file);

        let opathname = format!("{}.jpg", pathname);
        let oflag = format!("-o{}.jpg", pathname);

        // dot -Tjpg -oex.jpg exampl1.dot
        let _cmd = Command::new("dot")
            .arg("-Tjpg")
            .arg(oflag)
            .arg(pathname)
            .status()
            .expect("failed to execute process");

        Ok(())
    }

    pub fn write_pop_to_graphviz(self: &QGM, pop_graph: &POPGraph, pop_key: POPKey, file: &mut File) -> Result<(), String> {
        let id = pop_key.printable_key();
        let (pop, props, children) = pop_graph.get3(pop_key);

        if let Some(children) = children {
            for &child_key in children.iter() {
                let child_name = child_key.printable_key();
                fprint!(file, "    popkey{} -> popkey{};\n", child_name, id);
                self.write_pop_to_graphviz(pop_graph, child_key, file)?;
            }
        }

        let (label, extrastr) = match &pop {
            POP::CSV(csv) => {
                let pathname = csv.pathname.split("/").last().unwrap_or(&csv.pathname);
                let mut input_map = csv.input_map.iter().collect::<Vec<_>>();
                input_map.sort_by(|a, b| a.cmp(b));
                let extrastr = format!("file: {}, map: {:?}", pathname, input_map).replace("{", "(").replace("}", ")");
                (String::from("CSV"), extrastr)
            }
            POP::CSVDir(csvdir) => {
                let dirname = csvdir.dirname_prefix.split("/").last().unwrap_or(&csvdir.dirname_prefix);
                let mut input_map = csvdir.input_map.iter().collect::<Vec<_>>();
                input_map.sort_by(|a, b| a.cmp(b));
                let extrastr = format!("file: {}, map: {:?}", dirname, input_map).replace("{", "(").replace("}", ")");
                (String::from("CSVDir"), extrastr)
            }
            POP::HashJoin { .. } => {
                let extrastr = format!("");
                (String::from("HashJoin"), extrastr)
            }
            POP::Repartition(inner) => {
                let extrastr = format!("output_map = {:?}", inner.output_map);
                (String::from("Repartition"), extrastr)
            }
            POP::Aggregation(inner) => {
                let extrastr = format!("");
                (String::from("Aggregation"), extrastr)
            }
        };

        let label = label.replace("\"", "").replace("{", "").replace("}", "");
        fprint!(
            file,
            "    popkey{}[label=\"{}-{}|p = {}|{}\"];\n",
            id,
            label,
            pop_key.printable_id(),
            props.npartitions,
            extrastr
        );

        Ok(())
    }
}

use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterAllocator {
    pub hashmap: HashMap<QunCol, RegisterId>,
    next_id: RegisterId,
}

impl std::default::Default for RegisterAllocator {
    fn default() -> Self {
        RegisterAllocator::new()
    }
}

impl RegisterAllocator {
    pub fn new() -> RegisterAllocator {
        RegisterAllocator {
            hashmap: HashMap::new(),
            next_id: 0,
        }
    }

    pub fn get_id(&mut self, quncol: QunCol) -> RegisterId {
        let next_id = self.next_id;
        let e = self.hashmap.entry(quncol).or_insert(next_id);
        if *e == next_id {
            self.next_id = next_id + 1;
        }
        //debug!("Assigned {:?} -> {}", &quncol, *e);
        *e
    }
}

use regex::Regex;

impl POPKey {
    pub fn printable_key(&self) -> String {
        format!("{:?}", *self).replace("(", "").replace(")", "")
    }

    pub fn printable(&self, pop_graph: &POPGraph) -> String {
        let pop = &pop_graph.get(*self).value;
        format!("{:?}-{:?}", *pop, *self)
    }

    pub fn printable_id(&self) -> String {
        let re1 = Regex::new(r"^.*\(").unwrap();
        let re2 = Regex::new(r"\).*$").unwrap();

        let id = format!("{:?}", *self);
        let id = re1.replace_all(&id, "");
        let id = re2.replace_all(&id, "");
        id.to_string()
    }
}
