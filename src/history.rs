//! Finished games, kept so they can be listed, replayed and reviewed.
//!
//! Each game is one JSON line appended to `games.jsonl` in the data directory, and the
//! whole file is loaded into memory at startup. There is no account: every game is public.

use crate::ai::Difficulty;
use crate::game::{Move, Player};
use crate::room::Reason;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const FILE: &str = "games.jsonl";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerColor<T> {
    pub green: T,
    pub red: T,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    /// `online`, `local` or `solo`.
    pub mode: String,
    pub difficulty: Option<Difficulty>,
    pub map: String,
    pub w: usize,
    pub h: usize,
    /// The board before the first move, as `Board::encode` writes it.
    pub start: String,
    pub moves: Vec<Move>,
    pub winner: Option<Player>,
    pub reason: Reason,
    pub scores: PerColor<usize>,
    /// Which colours the computer played.
    pub ai: PerColor<bool>,
    /// Unix time, in seconds.
    pub ended_at: u64,
}

#[derive(Default)]
struct Games {
    /// Oldest first. Positions never change, so they work as page cursors.
    list: Vec<Arc<Record>>,
    by_id: HashMap<String, usize>,
}

impl Games {
    fn push(&mut self, r: Record) {
        self.by_id.insert(r.id.clone(), self.list.len());
        self.list.push(Arc::new(r));
    }
}

pub struct History {
    /// `None`: games are only kept until the server restarts.
    file: Option<PathBuf>,
    games: Mutex<Games>,
}

impl History {
    /// Keeps games in memory only.
    #[cfg(test)]
    pub fn in_memory() -> History {
        History { file: None, games: Mutex::default() }
    }

    /// Loads the games saved in `dir`, creating it if needed.
    pub fn open(dir: PathBuf) -> History {
        let mut games = Games::default();
        if let Err(e) = fs::create_dir_all(&dir) {
            eprintln!("history: cannot use {}: {e}. Finished games won't be saved.", dir.display());
            return History { file: None, games: Mutex::new(games) };
        }
        let file = dir.join(FILE);
        let mut skipped = 0;
        if let Ok(f) = fs::File::open(&file) {
            for line in BufReader::new(f).lines().map_while(Result::ok) {
                match serde_json::from_str::<Record>(&line) {
                    Ok(r) => games.push(r),
                    Err(_) if line.trim().is_empty() => {}
                    Err(_) => skipped += 1,
                }
            }
        }
        println!("history: {} games in {}", games.list.len(), file.display());
        if skipped > 0 {
            eprintln!("history: skipped {skipped} unreadable lines");
        }
        History { file: Some(file), games: Mutex::new(games) }
    }

    pub fn save(&self, r: Record) {
        if let Some(path) = &self.file {
            let line = serde_json::to_string(&r).expect("records serialize") + "\n";
            let written = OpenOptions::new().create(true).append(true).open(path).and_then(|mut f| f.write_all(line.as_bytes()));
            if let Err(e) = written {
                eprintln!("history: cannot save game {}: {e}", r.id);
            }
        }
        self.games.lock().unwrap().push(r);
    }

    pub fn get(&self, id: &str) -> Option<Arc<Record>> {
        let games = self.games.lock().unwrap();
        games.by_id.get(id).map(|&i| games.list[i].clone())
    }

    pub fn contains(&self, id: &str) -> bool {
        self.games.lock().unwrap().by_id.contains_key(id)
    }

    /// Newest first: up to `limit` games older than the cursor `before`, and the cursor for the next page.
    pub fn page(&self, before: Option<usize>, limit: usize) -> (Vec<Arc<Record>>, Option<usize>) {
        let games = self.games.lock().unwrap();
        let end = before.unwrap_or(usize::MAX).min(games.list.len());
        let start = end.saturating_sub(limit);
        let page = games.list[start..end].iter().rev().cloned().collect();
        (page, (start > 0).then_some(start))
    }

    /// The games among `ids` that exist, newest first.
    pub fn some(&self, ids: &[&str]) -> Vec<Arc<Record>> {
        let games = self.games.lock().unwrap();
        let mut found: Vec<usize> = ids.iter().filter_map(|id| games.by_id.get(*id).copied()).collect();
        found.sort_unstable_by(|a, b| b.cmp(a));
        found.dedup();
        found.into_iter().map(|i| games.list[i].clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str) -> Record {
        Record {
            id: id.into(),
            mode: "local".into(),
            difficulty: None,
            map: "Test".into(),
            w: 3,
            h: 1,
            start: "G.R".into(),
            moves: vec![Move { from: 0, to: 1 }],
            winner: Some(Player::Green),
            reason: Reason::Score,
            scores: PerColor { green: 3, red: 0 },
            ai: PerColor::default(),
            ended_at: 1,
        }
    }

    #[test]
    fn pages_newest_first() {
        let h = History::in_memory();
        for i in 0..5 {
            h.save(record(&format!("g{i}")));
        }
        let ids = |v: Vec<Arc<Record>>| v.iter().map(|r| r.id.clone()).collect::<Vec<_>>();
        let (page, next) = h.page(None, 2);
        assert_eq!(ids(page), ["g4", "g3"]);
        let (page, next) = h.page(next, 2);
        assert_eq!(ids(page), ["g2", "g1"]);
        let (page, next) = h.page(next, 2);
        assert_eq!(ids(page), ["g0"]);
        assert_eq!(next, None);
        assert_eq!(ids(h.some(&["g1", "nope", "g3", "g1"])), ["g3", "g1"]);
    }

    #[test]
    fn survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("bacteria-history-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        History::open(dir.clone()).save(record("abc"));
        fs::write(dir.join(FILE), fs::read_to_string(dir.join(FILE)).unwrap() + "not json\n").unwrap();
        let h = History::open(dir.clone());
        let r = h.get("abc").expect("reloaded");
        assert_eq!(r.moves, vec![Move { from: 0, to: 1 }]);
        assert_eq!(r.reason, Reason::Score);
        let _ = fs::remove_dir_all(&dir);
    }
}
