//! Move-by-move analysis of a finished game, to learn from it.
//!
//! Every move is compared with the best ones a fixed-depth search finds (the hard
//! computer's bitboard alpha-beta with strategic evaluation, without its time limit),
//! so an analysis is deterministic.
//!
//! Scores are in centi-cells (`SCALE = 100` = 1.00 cell equivalent), weighing
//! material, territory control, mobility, and piece safety. Beyond ±`WIN` the game
//! is decided by force. Moves are graded by win-probability drop, like chess engines.

use crate::ai::{self, Search, INF, SCALE, WIN, WIN_PROB_K};
use crate::game::{Board, Game, Move, Player};
use crate::history::Record;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;

/// Base search depth from the root (three moves each). Each candidate in the
/// shortlist is paired with one extra ply (`DEPTH` + `DEPTH + 1`) so odd and even
/// horizons cancel out instead of swinging each turn.
#[cfg(not(test))]
pub const DEPTH: u32 = 6;
#[cfg(test)]
pub const DEPTH: u32 = 4;
const _: () = assert!(DEPTH >= 2);
/// Moves listed per position, best first.
const TOP: usize = 3;
/// Top moves from the base depth pass promoted to the paired-depth pass.
const SHORTLIST: usize = 6;
const _: () = assert!(SHORTLIST >= TOP);
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
    /// Engine score of the move played, in centi-cells, for the player who made it.
    pub score: i32,
    /// How much worse it is than the best move, in centi-cells. 0 when it is the best.
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
    pub scale: i32,
    pub win_k: f64,
    pub moves: Vec<MoveAnalysis>,
}

pub fn analyse(start: Board, moves: &[Move]) -> Analysis {
    let mut game = Game::new(start);
    let mut out = Vec::with_capacity(moves.len());
    let mut search = Search::for_analysis();
    for &m in moves {
        let me = game.turn;
        if !game.board.is_legal(me, m) {
            break; // not a real game: analyse what makes sense
        }
        out.push(analyse_move(&mut search, &game.board, me, m));
        game.play(me, m).expect("legal move");
    }
    Analysis { depth: DEPTH, win: WIN, scale: SCALE, win_k: WIN_PROB_K, moves: out }
}

/// Two moves give the same board: same move, or copies to the same cell.
fn same(board: &Board, a: Move, b: Move) -> bool {
    a == b || (a.to == b.to && !board.is_jump(a) && !board.is_jump(b))
}

/// Combines consecutive even/odd depth evaluations to cancel last-mover horizon
/// bias while trusting the deeper search whenever a forced outcome appears.
fn combine(shallow: i32, deep: i32) -> i32 {
    if deep.abs() >= WIN || shallow.abs() >= WIN {
        deep
    } else {
        (shallow + deep) / 2
    }
}

fn analyse_move(search: &mut Search, board: &Board, me: Player, played: Move) -> MoveAnalysis {
    let mut moves = vec![played];
    moves.extend(ai::candidates(board, me).into_iter().filter(|&m| !same(board, m, played)));
    ai::order(board, me, &mut moves[1..]);

    // Pass 1 (depth DEPTH - 1 after the move = DEPTH total): exact scores for the
    // played move and the top `SHORTLIST` candidates.
    let mut pass1: Vec<(Move, i32)> = Vec::with_capacity(moves.len());
    for (k, &m) in moves.iter().enumerate() {
        let beta = if k == 0 || pass1.len() < SHORTLIST { INF } else { 1 - kth_best(&pass1, SHORTLIST) };
        let mut b = board.clone();
        b.apply(me, m);
        pass1.push((m, -search.negamax_board(&b, me.other(), DEPTH - 1, -INF, beta)));
    }

    // Build shortlist: top `SHORTLIST` moves best-first (for tight pruning in Pass 2),
    // always guaranteeing the played move is included.
    let played_p1 = pass1[0];
    let mut shortlist = pass1;
    shortlist.sort_by_key(|&(_, v)| -v); // stable: played stays ahead of ties
    shortlist.truncate(SHORTLIST);
    if !shortlist.iter().any(|&(m, _)| m == played) {
        shortlist.push(played_p1);
    }

    // Pass 2 (depth DEPTH after the move = DEPTH + 1 total): pair with Pass 1 to cancel
    // odd/even horizon swings. Exact scores for `played` and the `TOP` best paired moves.
    let mut scored: Vec<(Move, i32)> = Vec::with_capacity(shortlist.len());
    for &(m, s_shallow) in &shortlist {
        let is_played = m == played;
        let beta = if is_played || scored.len() < TOP {
            INF
        } else {
            let thr = kth_best(&scored, TOP);
            let need_deep = if thr >= WIN || s_shallow.abs() >= WIN {
                thr
            } else {
                (2 * thr - s_shallow - 1).min(WIN).clamp(-INF, INF)
            };
            (1 - need_deep).clamp(-INF, INF)
        };
        let mut b = board.clone();
        b.apply(me, m);
        let s_deep = -search.negamax_board(&b, me.other(), DEPTH, -INF, beta);
        scored.push((m, combine(s_shallow, s_deep)));
    }

    let score = scored.iter().find(|&&(m, _)| m == played).unwrap().1;
    // Best first; the move played wins ties so it is graded Best with loss 0.
    scored.sort_by_key(|&(m, v)| (-v, m != played));
    let (best, best_score) = scored[0];
    let reply = answer(search, board, me, played);
    MoveAnalysis {
        class: classify(moves.len(), best_score, score),
        score,
        loss: best_score - score,
        choices: moves.len(),
        top: scored.iter().take(TOP).map(|&(m, score)| Scored { from: m.from, to: m.to, score }).collect(),
        reply,
        best_reply: if same(board, best, played) { reply } else { answer(search, board, me, best) },
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
fn answer(search: &mut Search, board: &Board, me: Player, m: Move) -> Option<Move> {
    let mut b = board.clone();
    b.apply(me, m);
    let them = me.other();
    let mut moves = ai::candidates(&b, them);
    if moves.is_empty() {
        return None;
    }
    if let Some(best) = search.tt_best_move(&b, them, DEPTH) {
        return Some(best);
    }
    ai::order(&b, them, &mut moves);
    search.root(&b, them, &moves, DEPTH).map(|(m, _)| m)
}

/// Classifies a move by win-probability drop (chess-engine style).
fn classify(choices: usize, best: i32, played: i32) -> Class {
    if choices == 1 {
        return Class::Forced;
    }
    if played >= best {
        return Class::Best;
    }
    let drop = (ai::win_prob(best) - ai::win_prob(played)).max(0.0);
    if drop < 0.05 {
        Class::Good
    } else if drop < 0.10 {
        Class::Inaccuracy
    } else if drop < 0.20 {
        Class::Mistake
    } else {
        Class::Blunder
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
        assert_eq!(classify(5, 100, 100), Class::Best);
        // Small dip around even: < 5% win-prob drop -> Good
        assert_eq!(classify(5, 0, -40), Class::Good);
        // ~7.5% win-prob drop -> Inaccuracy
        assert_eq!(classify(5, 0, -80), Class::Inaccuracy);
        // ~14% win-prob drop -> Mistake
        assert_eq!(classify(5, 0, -150), Class::Mistake);
        // >20% win-prob drop -> Blunder
        assert_eq!(classify(5, 0, -260), Class::Blunder);
        // Throwing away a forced win into a small lead -> Blunder
        assert_eq!(classify(5, WIN + 300, 200), Class::Blunder);
        // Forced win to forced win (smaller margin) -> Good
        assert_eq!(classify(5, WIN + 900, WIN + 300), Class::Good);
        // Already overwhelmingly winning (+20.0 -> +15.0) -> still Good
        assert_eq!(classify(5, 2000, 1500), Class::Good);
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

    #[test]
    fn best_moves_stay_stable_across_turns() {
        // Playing the engine's best move each turn should not zigzag wildly between odd and even plies.
        let start = Board::parse(crate::maps::MAPS[0].rows);
        let mut g = Game::new(start.clone());
        let mut search = Search::for_analysis();
        let mut played = Vec::new();
        for _ in 0..16 {
            if g.over {
                break;
            }
            let me = g.turn;
            let mut moves = ai::candidates(&g.board, me);
            ai::order(&g.board, me, &mut moves);
            let a = analyse_move(&mut search, &g.board, me, moves[0]);
            let best = Move { from: a.top[0].from, to: a.top[0].to };
            played.push(best);
            g.play(me, best).unwrap();
        }
        let analysis = analyse(start, &played);
        assert_eq!(analysis.moves.len(), played.len());
        let mut green_scores = Vec::new();
        for (k, m) in analysis.moves.iter().enumerate() {
            assert_eq!(m.class, Class::Best, "ply {k} should be Best, got {m:?}");
            assert_eq!(m.loss, 0);
            let gs = if k % 2 == 0 { m.score } else { -m.score };
            if gs.abs() < WIN {
                green_scores.push(gs);
            }
        }
        let swings: Vec<i32> = green_scores.windows(2).map(|w| (w[1] - w[0]).abs()).collect();
        let mean_swing = swings.iter().map(|&s| s as f64).sum::<f64>() / swings.len().max(1) as f64;
        assert!(mean_swing < 90.0, "expected stable best-move evals, mean |swing|={mean_swing:.1}, scores={green_scores:?}");
    }

    #[test]
    fn combine_handles_forced_outcomes() {
        assert_eq!(combine(100, -100), 0);
        assert_eq!(combine(200, 100), 150);
        // Never average a forced win/loss with a non-forced score; always trust deeper depth.
        assert_eq!(combine(300, WIN + 500), WIN + 500);
        assert_eq!(combine(WIN + 500, 300), 300);
        assert_eq!(combine(-300, -(WIN + 500)), -(WIN + 500));
        assert_eq!(combine(-(WIN + 500), -300), -300);
    }
}
