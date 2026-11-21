//! Rooms: seats, move clock, computer turns and rematches. Everything lives in memory.

use crate::ai::{self, Difficulty};
use crate::analysis::Analyses;
use crate::game::{Game, Move, Player};
use crate::history::{History, PerColor, Record};
use crate::maps;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

pub const MOVE_TIME: Duration = Duration::from_secs(30);
/// Leeway for network lag before a player loses on time.
const LAG_GRACE: Duration = Duration::from_millis(600);
/// The computer waits at least this long so its move doesn't land mid-animation.
const AI_MIN_DELAY: Duration = Duration::from_millis(750);
pub const MAX_ROOMS: usize = 10_000;

pub struct AppState {
    pub rooms: Mutex<HashMap<String, Room>>,
    pub next_conn: AtomicU64,
    pub history: History,
    pub analyses: Analyses,
}

impl AppState {
    pub fn new(history: History) -> AppState {
        AppState { rooms: Mutex::default(), next_conn: AtomicU64::default(), history, analyses: Analyses::default() }
    }
}

pub type Shared = Arc<AppState>;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Online,
    Local,
    Solo(Difficulty),
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Online => "online",
            Mode::Local => "local",
            Mode::Solo(_) => "solo",
        }
    }

    fn difficulty(self) -> Option<Difficulty> {
        match self {
            Mode::Solo(d) => Some(d),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Waiting,
    Playing,
    Finished,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Reason {
    Score,
    Timeout,
    Resign,
    Aborted,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Spectator,
    Seat(Player),
    /// Pass and play: one screen controls both colours.
    Both,
}

impl Role {
    fn controls(self, p: Player) -> bool {
        match self {
            Role::Both => true,
            Role::Seat(q) => q == p,
            Role::Spectator => false,
        }
    }
}

#[derive(Default, Clone)]
pub struct Seat {
    token: Option<String>,
    ai: bool,
    connections: u32,
}

struct Client {
    tx: UnboundedSender<String>,
    role: Role,
}

#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum ClientMsg {
    Move { from: usize, to: usize },
    Resign,
    Rematch,
    Ping,
}

pub struct Room {
    id: String,
    mode: Mode,
    map: &'static str,
    game: Game,
    status: Status,
    result: Option<(Option<Player>, Reason)>,
    seats: [Seat; 2],
    deadline: Option<Instant>,
    /// Bumped on every turn change so stale timers and computer moves are ignored.
    seq: u64,
    clients: HashMap<u64, Client>,
    rematch: [bool; 2],
    next: Option<String>,
    last_activity: Instant,
}

/// Creates a room for its creator. Returns `(room id, creator token)`.
pub fn create(st: &Shared, mode: Mode) -> Option<(String, String)> {
    let mut rooms = st.rooms.lock().unwrap();
    if rooms.len() >= MAX_ROOMS {
        return None;
    }
    let token = random_id(20);
    let mine = if rand::rng().random_bool(0.5) { Player::Green } else { Player::Red };
    let mut seats: [Seat; 2] = Default::default();
    match mode {
        Mode::Online => seats[mine.idx()].token = Some(token.clone()),
        Mode::Local => seats.iter_mut().for_each(|s| s.token = Some(token.clone())),
        Mode::Solo(_) => {
            seats[mine.idx()].token = Some(token.clone());
            seats[mine.other().idx()].ai = true;
        }
    }
    Some((insert(st, &mut rooms, mode, seats), token))
}

fn insert(st: &Shared, rooms: &mut HashMap<String, Room>, mode: Mode, seats: [Seat; 2]) -> String {
    let id = loop {
        let id = random_id(8);
        if !rooms.contains_key(&id) && !st.history.contains(&id) {
            break id;
        }
    };
    let (map, board) = maps::random();
    let mut room = Room {
        id: id.clone(),
        mode,
        map,
        game: Game::new(board),
        status: Status::Waiting,
        result: None,
        seats,
        deadline: None,
        seq: 0,
        clients: HashMap::new(),
        rematch: [false; 2],
        next: None,
        last_activity: Instant::now(),
    };
    room.start_if_ready(st);
    rooms.insert(id.clone(), room);
    id
}

/// Attaches a websocket to a room. Returns false if the room doesn't exist.
pub fn join(st: &Shared, id: &str, conn: u64, tx: UnboundedSender<String>, token: Option<String>) -> bool {
    let mut rooms = st.rooms.lock().unwrap();
    match rooms.get_mut(id) {
        Some(room) => {
            room.join(st, conn, tx, token);
            true
        }
        None => {
            let _ = tx.send(json!({ "t": "error", "code": "not_found" }).to_string());
            false
        }
    }
}

pub fn leave(st: &Shared, id: &str, conn: u64) {
    if let Some(room) = st.rooms.lock().unwrap().get_mut(id) {
        room.leave(conn);
    }
}

pub fn on_message(st: &Shared, id: &str, conn: u64, msg: ClientMsg) {
    let mut rooms = st.rooms.lock().unwrap();
    let Some(room) = rooms.get_mut(id) else { return };
    let Some(seats) = room.handle(st, conn, msg) else { return };
    // Both players want a rematch: open a new room with colours swapped.
    let mode = room.mode;
    let next = insert(st, &mut rooms, mode, seats);
    if let Some(room) = rooms.get_mut(id) {
        room.next = Some(next);
        room.broadcast();
    }
}

/// Drops rooms nobody uses anymore.
pub fn sweep(st: &Shared) {
    let idle = Duration::from_secs(30 * 60);
    let stale = Duration::from_secs(6 * 3600);
    st.rooms.lock().unwrap().retain(|_, r| {
        let age = r.last_activity.elapsed();
        age < idle || (!r.clients.is_empty() && age < stale)
    });
}

impl Room {
    fn role_for(&self, token: Option<&str>) -> Role {
        let Some(token) = token else { return Role::Spectator };
        let owns = |p: Player| self.seats[p.idx()].token.as_deref() == Some(token);
        match (owns(Player::Green), owns(Player::Red)) {
            (true, true) => Role::Both,
            (true, false) => Role::Seat(Player::Green),
            (false, true) => Role::Seat(Player::Red),
            (false, false) => Role::Spectator,
        }
    }

    fn join(&mut self, st: &Shared, conn: u64, tx: UnboundedSender<String>, token: Option<String>) {
        let mut role = self.role_for(token.as_deref());
        let mut token = token.filter(|_| role != Role::Spectator);

        // Whoever opens the invite link takes the free seat.
        if role == Role::Spectator && self.status == Status::Waiting {
            let free = Player::ALL.into_iter().find(|p| {
                let s = &self.seats[p.idx()];
                !s.ai && s.token.is_none()
            });
            if let Some(p) = free {
                let t = random_id(20);
                self.seats[p.idx()].token = Some(t.clone());
                token = Some(t);
                role = Role::Seat(p);
                self.start_if_ready(st);
            }
        }

        for p in Player::ALL {
            if role.controls(p) {
                self.seats[p.idx()].connections += 1;
            }
        }
        let seat = match role {
            Role::Spectator => Value::Null,
            Role::Seat(p) => json!(p),
            Role::Both => json!("both"),
        };
        let _ = tx.send(json!({ "t": "welcome", "seat": seat, "token": token }).to_string());
        self.clients.insert(conn, Client { tx, role });
        self.last_activity = Instant::now();
        self.broadcast();
    }

    fn leave(&mut self, conn: u64) {
        let Some(client) = self.clients.remove(&conn) else { return };
        for p in Player::ALL {
            if client.role.controls(p) {
                let seat = &mut self.seats[p.idx()];
                seat.connections = seat.connections.saturating_sub(1);
            }
        }
        self.broadcast();
    }

    /// Handles a client message. Returns the seats of a rematch room to create, if any.
    fn handle(&mut self, st: &Shared, conn: u64, msg: ClientMsg) -> Option<[Seat; 2]> {
        let role = self.clients.get(&conn)?.role;
        self.last_activity = Instant::now();
        match msg {
            ClientMsg::Ping => {}
            ClientMsg::Move { from, to } => {
                let p = self.game.turn;
                let result = if !role.controls(p) || self.seats[p.idx()].ai {
                    Err("not your turn")
                } else {
                    self.play(st, p, Move { from, to })
                };
                if let Err(e) = result {
                    // Put the client back in sync with the real board.
                    self.send(conn, json!({ "t": "error", "code": "rejected", "message": e }));
                    self.send(conn, self.state());
                }
            }
            ClientMsg::Resign => {
                if self.status != Status::Playing {
                    return None;
                }
                let loser = match role {
                    Role::Seat(p) => p,
                    Role::Both => self.game.turn,
                    Role::Spectator => return None,
                };
                self.finish(st, Some(loser.other()), Reason::Resign);
                self.broadcast();
            }
            ClientMsg::Rematch => {
                if self.status != Status::Finished || self.next.is_some() {
                    return None;
                }
                for p in Player::ALL {
                    if role.controls(p) {
                        self.rematch[p.idx()] = true;
                    }
                }
                self.broadcast();
                let ready = Player::ALL.iter().all(|p| self.rematch[p.idx()] || self.seats[p.idx()].ai);
                if ready {
                    let swap = |s: &Seat| Seat { token: s.token.clone(), ai: s.ai, connections: 0 };
                    return Some([swap(&self.seats[1]), swap(&self.seats[0])]);
                }
            }
        }
        None
    }

    fn start_if_ready(&mut self, st: &Shared) {
        let ready = self.seats.iter().all(|s| s.ai || s.token.is_some());
        if ready && self.status == Status::Waiting {
            self.status = Status::Playing;
            self.start_turn(st);
        }
    }

    fn start_turn(&mut self, st: &Shared) {
        self.seq += 1;
        self.deadline = Some(Instant::now() + MOVE_TIME);
        let seq = self.seq;

        let (st2, id) = (st.clone(), self.id.clone());
        tokio::spawn(async move {
            tokio::time::sleep(MOVE_TIME + LAG_GRACE).await;
            if let Some(room) = st2.rooms.lock().unwrap().get_mut(&id) {
                room.on_timeout(&st2, seq);
            }
        });

        let me = self.game.turn;
        if let (Mode::Solo(level), true) = (self.mode, self.seats[me.idx()].ai) {
            let board = self.game.board.clone();
            let (st, id) = (st.clone(), self.id.clone());
            tokio::spawn(async move {
                let started = Instant::now();
                let mv = tokio::task::spawn_blocking(move || ai::choose(&board, me, level)).await.ok().flatten();
                if let Some(rest) = AI_MIN_DELAY.checked_sub(started.elapsed()) {
                    tokio::time::sleep(rest).await;
                }
                let Some(mv) = mv else { return };
                let mut rooms = st.rooms.lock().unwrap();
                if let Some(room) = rooms.get_mut(&id) {
                    if room.seq == seq {
                        let _ = room.play(&st, me, mv);
                    }
                }
            });
        }
    }

    fn play(&mut self, st: &Shared, p: Player, m: Move) -> Result<(), &'static str> {
        if self.status != Status::Playing {
            return Err("game is not running");
        }
        self.game.play(p, m)?;
        self.last_activity = Instant::now();
        if self.game.over {
            self.finish(st, self.game.leader(), Reason::Score);
        } else {
            self.start_turn(st);
        }
        self.broadcast();
        Ok(())
    }

    fn on_timeout(&mut self, st: &Shared, seq: u64) {
        if self.seq != seq || self.status != Status::Playing {
            return;
        }
        if self.game.ply < 2 {
            // Someone never made their first move: call the game off, like Lichess.
            self.finish(st, None, Reason::Aborted);
        } else {
            self.finish(st, Some(self.game.turn.other()), Reason::Timeout);
        }
        self.broadcast();
    }

    fn finish(&mut self, st: &Shared, winner: Option<Player>, reason: Reason) {
        self.status = Status::Finished;
        self.result = Some((winner, reason));
        self.deadline = None;
        self.seq += 1;
        if reason != Reason::Aborted && !self.game.moves.is_empty() {
            st.history.save(self.record(winner, reason));
        }
    }

    fn record(&self, winner: Option<Player>, reason: Reason) -> Record {
        let g = &self.game;
        let ended_at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        Record {
            id: self.id.clone(),
            mode: self.mode.name().into(),
            difficulty: self.mode.difficulty(),
            map: self.map.into(),
            w: g.start.w,
            h: g.start.h,
            start: g.start.encode(),
            moves: g.moves.clone(),
            winner,
            reason,
            scores: PerColor { green: g.score(Player::Green), red: g.score(Player::Red) },
            ai: PerColor { green: self.seats[Player::Green.idx()].ai, red: self.seats[Player::Red.idx()].ai },
            ended_at,
        }
    }

    fn send(&self, conn: u64, msg: Value) {
        if let Some(c) = self.clients.get(&conn) {
            let _ = c.tx.send(msg.to_string());
        }
    }

    fn broadcast(&self) {
        let msg = self.state().to_string();
        for c in self.clients.values() {
            let _ = c.tx.send(msg.clone());
        }
    }

    fn state(&self) -> Value {
        let g = &self.game;
        let player = |p: Player| {
            let s = &self.seats[p.idx()];
            json!({ "ai": s.ai, "online": s.ai || s.connections > 0 })
        };
        json!({
            "t": "state",
            "mode": self.mode.name(),
            "difficulty": self.mode.difficulty(),
            "map": self.map,
            "w": g.board.w,
            "h": g.board.h,
            "cells": g.board.encode(),
            "status": match self.status { Status::Waiting => "waiting", Status::Playing => "playing", Status::Finished => "finished" },
            "turn": g.turn,
            "ply": g.ply,
            "scores": { "green": g.score(Player::Green), "red": g.score(Player::Red) },
            "last": g.last.as_ref().map(|l| json!({
                "by": l.by, "from": l.from, "to": l.to, "jump": l.jump,
                "converted": l.converted, "filled": l.filled,
            })),
            "remaining_ms": self.deadline.map(|d| d.saturating_duration_since(Instant::now()).as_millis() as u64),
            "move_ms": MOVE_TIME.as_millis() as u64,
            "result": self.result.map(|(winner, reason)| json!({
                "winner": winner,
                "reason": match reason { Reason::Score => "score", Reason::Timeout => "timeout", Reason::Resign => "resign", Reason::Aborted => "aborted" },
            })),
            "players": { "green": player(Player::Green), "red": player(Player::Red) },
            "rematch": { "green": self.rematch[0], "red": self.rematch[1] },
            "next": self.next,
        })
    }
}

fn random_id(len: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijkmnopqrstuvwxyz23456789";
    let mut rng = rand::rng();
    (0..len).map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char).collect()
}
