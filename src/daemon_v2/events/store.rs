use super::DomainEvent;
use crate::daemon_v2::Projections;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

#[path = "store_tests.rs"]
#[cfg(test)]
mod tests;

#[path = "event_store_spec_tests.rs"]
#[cfg(test)]
mod spec_tests;

pub struct EventStore {
    dir: PathBuf,
    sequence: u64,
    snapshot_sequence: u64,
    writer: Option<io::BufWriter<fs::File>>,
}

impl EventStore {
    pub fn new(dir: PathBuf) -> Self {
        fs::create_dir_all(&dir).expect("failed to create event store directory");
        let log_path = dir.join("log-0000.jsonl");
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .expect("failed to open event log");

        Self {
            dir,
            sequence: 0,
            snapshot_sequence: 0,
            writer: Some(io::BufWriter::new(file)),
        }
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn append(&mut self, event: &DomainEvent) -> io::Result<()> {
        let writer = self.writer.as_mut().expect("event store not open");
        let json = serde_json::to_string(event)?;
        writeln!(writer, "{json}")?;
        writer.flush()?;
        self.sequence += 1;
        Ok(())
    }

    pub fn events_since(&self, since_sequence: u64) -> io::Result<Vec<DomainEvent>> {
        let log_path = self.log_path_for_snapshot(self.snapshot_sequence);
        if !log_path.exists() {
            return Ok(vec![]);
        }

        let file = fs::File::open(&log_path)?;
        let reader = io::BufReader::new(file);
        let mut events = Vec::new();

        for (i, line) in reader.lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let absolute_seq = self.snapshot_sequence + i as u64;
            if absolute_seq < since_sequence {
                continue;
            }
            match serde_json::from_str::<DomainEvent>(&line) {
                Ok(event) => events.push(event),
                Err(_) => break,
            }
        }

        Ok(events)
    }

    pub fn save_snapshot(&mut self, projections: &Projections) -> io::Result<()> {
        let snapshot_path = self.dir.join(format!("snapshot-{:04}.json", self.sequence));
        let json = serde_json::to_string_pretty(projections)?;
        fs::write(&snapshot_path, json)?;
        self.snapshot_sequence = self.sequence;

        drop(self.writer.take());
        let log_path = self.log_path_for_snapshot(self.sequence);
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        self.writer = Some(io::BufWriter::new(file));

        Ok(())
    }

    pub fn recover(dir: PathBuf) -> io::Result<(Self, Option<Projections>, Vec<DomainEvent>)> {
        if !dir.exists() {
            fs::create_dir_all(&dir)?;
            let store = Self::new(dir);
            return Ok((store, None, vec![]));
        }

        let (snapshot, snapshot_seq) = Self::load_latest_snapshot(&dir)?;

        let log_path = dir.join(format!("log-{snapshot_seq:04}.jsonl"));
        let mut events = Vec::new();

        if log_path.exists() {
            let contents = fs::read_to_string(&log_path)?;
            for line in contents.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<DomainEvent>(line) {
                    Ok(event) => events.push(event),
                    Err(_) => break,
                }
            }

            let mut file = fs::File::create(&log_path)?;
            for event in &events {
                let json = serde_json::to_string(event)?;
                writeln!(file, "{json}")?;
            }
        }

        let total_sequence = snapshot_seq + events.len() as u64;

        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;

        let store = Self {
            dir,
            sequence: total_sequence,
            snapshot_sequence: snapshot_seq,
            writer: Some(io::BufWriter::new(file)),
        };

        Ok((store, snapshot, events))
    }

    fn load_latest_snapshot(dir: &PathBuf) -> io::Result<(Option<Projections>, u64)> {
        let mut entries: Vec<_> = fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .map(|n| n.starts_with("snapshot-") && n.ends_with(".json"))
                    .unwrap_or(false)
            })
            .collect();

        entries.sort_by_key(|e| e.file_name());

        if let Some(latest) = entries.last() {
            let name = latest.file_name();
            let name_str = name.to_str().unwrap_or("snapshot-0000.json");
            let seq_str = name_str
                .strip_prefix("snapshot-")
                .and_then(|s| s.strip_suffix(".json"))
                .unwrap_or("0000");
            let seq: u64 = seq_str.parse().unwrap_or(0);

            let content = fs::read_to_string(latest.path())?;
            let projections: Projections = serde_json::from_str(&content)?;
            Ok((Some(projections), seq))
        } else {
            Ok((None, 0))
        }
    }

    fn log_path_for_snapshot(&self, snapshot_seq: u64) -> PathBuf {
        self.dir.join(format!("log-{snapshot_seq:04}.jsonl"))
    }
}
