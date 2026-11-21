//! Move-by-move analysis of a finished game, to learn from it.
//!
//! Every move is compared with the best ones a fixed-depth search finds (the hard
//! computer's alpha-beta, without its time limit), so an analysis never changes.
//! Scores are cell differences, mine minus theirs, after `DEPTH` moves of best play
//! from both sides. Beyond ±`WIN` the game is decided: someone ends up stuck.

use crate::ai::{self, Search, INF, WIN};
use crate::game::{Board, Game, Move, Player};
use crate::history::Record;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;

/// Three moves each. Ending on the opponent's answer means a move isn't praised
/// for a capture that gets taken right back. About a second for a whole game.
#[cfg(not(test))]
pub const DEPTH: u32 = 6;
#[cfg(test)]
pub const DEPTH: u32 = 4;
const _: () = assert!(DEPTH >= 2);
/// Moves listed per position, best first.
const TOP: usize = 3;
/// Analyses kept in memory.
const CACHE: usize = 256;
/// Analyses computed at the same time (each one keeps a CPU busy).
const WORKERS: usize = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Class {
    Best,
    Good,
    Inaccuracy,
    Mistake,
    Blunder,
    /// The only legal move.
    Forced,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
pub struct Scored {
    pub from: usize,
    pub to: usize,
    pub score: i32,
}

#[derive(Debug, Serialize)]
pub struct MoveAnalysis {
    pub class: Class,
    /// Score of the move played, for the player who made it.
    pub score: i32,
    /// How much worse it is than the best move. 0 when it is the best.
    pub loss: i32,
    /// Different moves available. Copies to the same cell count once: they give the same board.
    pub choices: usize,
    /// Best moves first, at most `TOP`. Equal to the move played counts as the move played.
    pub top: Vec<Scored>,
    /// The opponent's best answer to the move played, and to the best move.
    pub reply: Option<Move>,
    pub best_reply: Option<Move>,
}

#[derive(Serialize)]
pub struct Analysis {
    pub depth: u32,
    pub win: i32,
    pub moves: Vec<MoveAnalysis>,
}

pub fn analyse(start: Board, moves: &[Move]) -> Analysis {
    let mut game = Game::new(start);
    let mut out = Vec::with_capacity(moves.len());
    for &m in moves {
        let me = game.turn;
        if !game.board.is_legal(me, m) {
            break; // not a real game: analyse what makes sense
        }
        out.push(analyse_move(&game.board, me, m));
        game.play(me, m).expect("legal move");
    }
    Analysis { depth: DEPTH, win: WIN, moves: out }
}

/// Two moves give the same board: same move, or copies to the same cell.
fn same(board: &Board, a: Move, b: Move) -> bool {
    a == b || (a.to == b.to && !board.is_jump(a) && !board.is_jump(b))
}

fn analyse_move(board: &Board, me: Player, played: Move) -> MoveAnalysis {
    let mut moves = vec![played];
    moves.extend(ai::candidates(board, me).into_iter().filter(|&m| !same(board, m, played)));
    ai::order(board, me, &mut moves[1..]);

    // Exact scores for the move played (searched first) and the `TOP` best. Any other
    // move only needs to be proven worse than the current `TOP`-th, which prunes a lot.
    let mut search = Search::new(None);
    let mut scored: Vec<(Move, i32)> = Vec::with_capacity(moves.len());
    for (k, &m) in moves.iter().enumerate() {
        let beta = if k == 0 { INF } else { 1 - kth_best(&scored, TOP) };
        let mut b = board.clone();
        b.apply(me, m);
        scored.push((m, -search.negamax(&b, me.other(), DEPTH - 1, -INF, beta)));
    }

    let score = scored[0].1;
    scored.sort_by_key(|&(_, v)| -v); // stable: the move played stays first among equals
    let (best, best_score) = scored[0];
    let reply = answer(board, me, played);
    MoveAnalysis {
        class: classify(moves.len(), best_score, score),
        score,
        loss: best_score - score,
        choices: moves.len(),
        top: scored.iter().take(TOP).map(|&(m, score)| Scored { from: m.from, to: m.to, score }).collect(),
        reply,
        best_reply: if same(board, best, played) { reply } else { answer(board, me, best) },
    }
}

fn kth_best(scored: &[(Move, i32)], k: usize) -> i32 {
    if scored.len() < k {
        return -INF;
    }
    let mut v: Vec<i32> = scored.iter().map(|s| s.1).collect();
    v.sort_unstable_by(|a, b| b.cmp(a));
    v[k - 1]
}

/// The opponent's best answer to `m`, seen from the same horizon as the scores.
fn answer(board: &Board, me: Player, m: Move) -> Option<Move> {
    let mut b = board.clone();
    b.apply(me, m);
    let them = me.other();
    let mut moves = ai::candidates(&b, them);
    ai::order(&b, them, &mut moves);
    Search::new(None).root(&b, them, &moves, DEPTH - 1).map(|(m, _)| m)
}

fn classify(choices: usize, best: i32, played: i32) -> Class {
    if choices == 1 {
        return Class::Forced;
    }
    if played >= best {
        return Class::Best;
    }
    let decided = |v: i32| v.abs() >= WIN;
    if decided(best) && decided(played) && best.signum() == played.signum() {
        return Class::Good; // won (or lost) either way, only the margin changes
    }
    if decided(best) || decided(played) {
        return Class::Blunder; // let a won game slip, or walked into a lost one
    }
    match best - played {
        1..=2 => Class::Good,
        3..=5 => Class::Inaccuracy,
        6..=10 => Class::Mistake,
        _ => Class::Blunder,
    }
}

/// Analyses of recorded games, computed on first request and then cached as JSON.
pub struct Analyses {
    cache: Mutex<HashMap<String, Arc<String>>>,
    workers: Semaphore,
}

impl Default for Analyses {
    fn default() -> Self {
        Analyses { cache: Mutex::default(), workers: Semaphore::new(WORKERS) }
    }
}

impl Analyses {
    pub async fn get(&self, record: Arc<Record>) -> Option<Arc<String>> {
        if let Some(hit) = self.cached(&record.id) {
            return Some(hit);
        }
        let _worker = self.workers.acquire().await.ok()?;
        if let Some(hit) = self.cached(&record.id) {
            return Some(hit); // someone else asked for it meanwhile
        }
        let id = record.id.clone();
        let json = tokio::task::spawn_blocking(move || {
            let start = Board::decode(record.w, record.h, &record.start)?;
            serde_json::to_string(&analyse(start, &record.moves)).ok()
        })
        .await
        .ok()??;
        let json = Arc::new(json);
        let mut cache = self.cache.lock().unwrap();
        if cache.len() >= CACHE {
            cache.clear();
        }
        cache.insert(id, json.clone());
        Some(json)
    }

    fn cached(&self, id: &str) -> Option<Arc<String>> {
        self.cache.lock().unwrap().get(id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::{choose, Difficulty};

    #[test]
    fn grades_a_capture_against_a_quiet_move() {
        // Green can copy to the centre and convert all three red bacteria.
        let b = Board::parse(&["G.R", "..R", "..R"]);
        let best = analyse(b.clone(), &[Move { from: 0, to: 4 }]).moves.remove(0);
        assert_eq!(best.class, Class::Best);
        assert_eq!(best.loss, 0);
        assert_eq!(best.top[0].to, 4);

        let quiet = analyse(b, &[Move { from: 0, to: 3 }]).moves.remove(0);
        assert!(quiet.loss > 0, "{quiet:?}");
        assert_ne!(quiet.class, Class::Best);
        assert_eq!(quiet.top[0].to, 4);
        assert!(quiet.top.windows(2).all(|w| w[0].score >= w[1].score));
    }

    #[test]
    fn copies_to_the_same_cell_are_the_same_move() {
        // Both green bacteria can copy to the middle: either one is the best move.
        let b = Board::parse(&["G.G", "...", "RRR"]);
        let a = analyse(b, &[Move { from: 2, to: 4 }]).moves.remove(0);
        assert_eq!(a.class, Class::Best);
        assert_eq!(a.reply, a.best_reply);
    }

    #[test]
    fn only_move_is_forced() {
        let b = Board::parse(&["G.#", "###", "#.R"]);
        let a = analyse(b, &[Move { from: 0, to: 1 }]).moves.remove(0);
        assert_eq!(a.class, Class::Forced);
    }

    #[test]
    fn grades() {
        assert_eq!(classify(5, 4, 4), Class::Best);
        assert_eq!(classify(5, 4, 2), Class::Good);
        assert_eq!(classify(5, 4, 0), Class::Inaccuracy);
        assert_eq!(classify(5, 4, -4), Class::Mistake);
        assert_eq!(classify(5, 4, -20), Class::Blunder);
        assert_eq!(classify(5, WIN + 3, 9), Class::Blunder);
        assert_eq!(classify(5, WIN + 9, WIN + 3), Class::Good);
    }

    #[test]
    fn analyses_a_whole_game() {
        let (_, board) = crate::maps::random();
        let mut g = Game::new(board);
        while !g.over && g.ply < 200 {
            let m = choose(&g.board, g.turn, Difficulty::Easy).unwrap();
            g.play(g.turn, m).unwrap();
        }
        let a = analyse(g.start.clone(), &g.moves);
        assert_eq!(a.moves.len(), g.moves.len());
        for (i, m) in a.moves.iter().enumerate() {
            assert!(m.loss >= 0 && !m.top.is_empty(), "move {i}: {m:?}");
            assert_eq!(m.loss, m.top[0].score - m.score, "move {i}");
        }
    }
}
