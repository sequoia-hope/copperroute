use std::cell::RefCell;
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use copper_board::Board;
use copper_board::structure::Unit;
use copper_geometry::IntVector;
use copper_morph::frames::{self, ObserverGuard};

/// Writes `NNNNN.svg` per kept move plus `frames.jsonl` describing each, into an empty directory.
pub struct FrameWriter {
    dir: PathBuf,
    index: BufWriter<File>,
    written: usize,
    max: usize,
    overlay: String,
    /// A fixed view in board units, for a board whose shape changes.
    pub view: Option<[f64; 4]>,
}

pub struct Recording {
    pub writer: Rc<RefCell<FrameWriter>>,
    _observer: ObserverGuard,
}

pub fn record(dir: &Path, max: usize, overlay: String) -> io::Result<Recording> {
    fs::create_dir_all(dir)?;
    if fs::read_dir(dir)?.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} is not empty", dir.display()),
        ));
    }
    let writer = Rc::new(RefCell::new(FrameWriter {
        dir: dir.to_path_buf(),
        index: BufWriter::new(File::create_new(dir.join("frames.jsonl"))?),
        written: 0,
        max,
        overlay,
        view: None,
    }));
    let observed = Rc::clone(&writer);
    let observer = frames::observe(move |board, frame| {
        observed
            .borrow_mut()
            .write(board, &frame.operators, &frame.components, &frame.by);
    });
    Ok(Recording {
        writer,
        _observer: observer,
    })
}

impl FrameWriter {
    pub fn write(&mut self, board: &Board, operators: &[&str], components: &[i32], by: &IntVector) {
        self.write_with(board, operators, components, by, serde_json::Value::Null);
    }

    pub fn write_with(
        &mut self,
        board: &Board,
        operators: &[&str],
        components: &[i32],
        by: &IntVector,
        stats: serde_json::Value,
    ) {
        if self.written >= self.max {
            return;
        }
        let file = format!("{:05}.svg", self.written);
        let svg = copper_core::preview::board_svg_in(board, components, &self.overlay, self.view);
        let per_mm = board.communication.get_resolution(Unit::Mm);
        let names: Vec<String> = components
            .iter()
            .map(|id| {
                board
                    .components
                    .get_all()
                    .find(|c| c.id == *id)
                    .map_or_else(|| id.to_string(), |c| c.name.clone())
            })
            .collect();
        let open = open_connections(board);
        let line = serde_json::json!({
            "frame": self.written,
            "file": file,
            "operators": operators,
            "parts": names,
            "by_mm": [f64::from(by.x) / per_mm, f64::from(by.y) / per_mm],
            "unconnected": open.len(),
            "open": open,
            "stats": stats,
        });
        let result = fs::write(self.dir.join(&file), svg)
            .and_then(|()| writeln!(self.index, "{line}"))
            .and_then(|()| self.index.flush());
        match result {
            Ok(()) => self.written += 1,
            Err(error) => {
                tracing::error!("Couldn't write frame {file}: {error}");
                self.max = self.written;
            }
        }
    }
}

/// Each airline as `NET: a - b`, the ends named by part, else by item kind, in name order.
fn open_connections(board: &Board) -> Vec<String> {
    let end = |id| match board.get_item(id) {
        Some(item) if item.component_id() > 0 => board
            .components
            .get_all()
            .find(|c| c.id == item.component_id())
            .map_or_else(String::new, |c| c.name.clone()),
        Some(item) => format!("{:?}", item.kind()).to_lowercase(),
        None => String::new(),
    };
    let mut open: Vec<String> = copper_drc::all_airlines(board)
        .iter()
        .map(|a| {
            let net = board
                .rules
                .nets
                .get(a.net_number)
                .map_or_else(|| a.net_number.to_string(), |n| n.name.clone());
            let mut ends = [end(a.from_item), end(a.to_item)];
            ends.sort();
            format!("{net}: {} - {}", ends[0], ends[1])
        })
        .collect();
    open.sort();
    open
}
